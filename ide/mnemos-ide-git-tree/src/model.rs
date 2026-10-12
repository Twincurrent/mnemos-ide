// SPDX-License-Identifier: AGPL-3.0-only
//! Commit graph data model.
//!
//! `GitGraphModel` is a stateful, refreshable model that opens a repo and
//! produces `Vec<CommitNode>` ordered topologically + by committer date
//! (newest first), plus the branch/tag list. Exposes `refresh()`,
//! `commits()` and `refs()` so the caller can drive updates: the app runs
//! `compute_layout` over the commits and pushes the result into
//! `GitTreeView::set_layout` and `GitTreeView::set_refs`.
//!
//! The walk is seeded from HEAD, every local branch and each local branch's
//! upstream, not from every remote branch: a fork carrying thousands of
//! `upstream/*` refs would otherwise fill the `MAX_COMMITS` window with
//! history HEAD never touched. Remote branches still appear in `refs()` and
//! as labels on commits that are in the walk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{build::CheckoutBuilder, BranchType, Repository, Sort};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

/// Maximum commits to walk. Caps memory use on deep histories.
const MAX_COMMITS: usize = 1000;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git2 error: {0}")]
    Git(#[from] git2::Error),
    #[error("repo not set")]
    NoRepo,
    #[error("{0}")]
    Checkout(String),
}

/// What kind of ref a [`RefNode`] describes. Drives grouping and iconography in
/// the tree, and which checkout path applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
}

/// A branch or tag, as shown in the tree above the commit graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefNode {
    /// Short display name: `main`, `origin/main`, `v1.2.0`.
    pub name: String,
    pub kind: RefKind,
    /// Full 40-hex OID this ref points at.
    pub tip_oid: String,
    /// True when HEAD currently resolves to this ref.
    pub is_head: bool,
    /// Commits this branch has that its upstream does not. Zero when there is
    /// no upstream, or for remotes and tags.
    pub ahead: usize,
    /// Commits the upstream has that this branch does not.
    pub behind: usize,
}

/// A single commit node in the rendered graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitNode {
    /// Full 40-hex OID.
    pub oid: String,
    /// Short 7-hex prefix for display.
    pub short_oid: String,
    /// Commit summary (first line of message).
    pub summary: String,
    /// Author display name.
    pub author: String,
    /// Committer unix timestamp (seconds since epoch).
    pub committer_time: i64,
    /// Full OIDs of direct parent commits.
    pub parent_oids: Vec<String>,
    /// Short names of branch refs whose tip is this commit.
    pub branch_refs: Vec<String>,
    /// True when this commit is the current HEAD.
    pub is_head: bool,
    /// Lane index written by `lane_router::assign_lanes`; 0 until routed. The
    /// app reads lanes from `compute_layout` instead and leaves this at 0.
    pub lane: usize,
}

/// Stateful commit-graph model. Holds the repo path and last-fetched commit
/// list. Call `refresh()` to re-read from disk, then `commits()` to render.
pub struct GitGraphModel {
    repo_path: Option<PathBuf>,
    commits: Vec<CommitNode>,
    refs: Vec<RefNode>,
    head_label: Option<String>,
    head_detached: bool,
}

impl GitGraphModel {
    pub fn new() -> Self {
        Self {
            repo_path: None,
            commits: Vec::new(),
            refs: Vec::new(),
            head_label: None,
            head_detached: false,
        }
    }

    /// Set (or change) the repo path. Does NOT trigger a fetch — call
    /// `refresh()` afterward.
    pub fn set_repo_path(&mut self, path: PathBuf) {
        self.repo_path = Some(path);
    }

    /// Re-open the repo and rebuild the commit list. Returns `Err` on git
    /// failures; the previous `commits()` slice is left unchanged.
    pub fn refresh(&mut self) -> Result<(), GitError> {
        let path = self.repo_path.as_deref().ok_or(GitError::NoRepo)?;
        self.commits = load_commits(path)?;
        // Refs are secondary: a failure here should not blank the commit graph,
        // which is the part the diff view depends on.
        match load_refs(path) {
            Ok((refs, head_label, detached)) => {
                self.refs = refs;
                self.head_label = head_label;
                self.head_detached = detached;
            }
            Err(err) => warn!("mnemos-ide-git-tree: failed to load refs: {err}"),
        }
        Ok(())
    }

    /// Borrow the last-fetched commit slice. Empty until `refresh()` succeeds.
    pub fn commits(&self) -> &[CommitNode] {
        &self.commits
    }

    /// Branches and tags, unsorted by kind — the view groups them.
    pub fn refs(&self) -> &[RefNode] {
        &self.refs
    }

    /// Branch name HEAD points at, or the short OID when detached.
    pub fn head_label(&self) -> Option<&str> {
        self.head_label.as_deref()
    }

    pub fn is_head_detached(&self) -> bool {
        self.head_detached
    }

    /// Check out a local branch, or a remote branch by creating a local tracking
    /// branch for it first (what VS Code does when you click `origin/foo`).
    ///
    /// Uses a *safe* checkout, so this refuses rather than clobbering
    /// uncommitted work; the error is surfaced to the user instead.
    ///
    /// Does not refresh the model. Callers re-sync afterwards (the app builds
    /// a fresh model for the tree once the checkout returns), so a checkout
    /// costs one walk rather than two.
    pub fn checkout_branch(&mut self, name: &str) -> Result<(), GitError> {
        let path = self.repo_path.as_deref().ok_or(GitError::NoRepo)?;
        let repo = Repository::open(path)?;

        // Prefer an existing local branch of that name.
        if let Ok(branch) = repo.find_branch(name, BranchType::Local) {
            let reference = branch.into_reference();
            let full_name = reference
                .name()
                .ok_or_else(|| GitError::Checkout(format!("branch {name} has no ref name")))?
                .to_owned();
            let object = reference.peel(git2::ObjectType::Any)?;
            repo.checkout_tree(&object, Some(CheckoutBuilder::new().safe()))?;
            repo.set_head(&full_name)?;
            return Ok(());
        }

        // A remote branch: create the local tracking branch, then check it out.
        if let Ok(remote) = repo.find_branch(name, BranchType::Remote) {
            // "origin/feature/x" -> "feature/x". Only the first segment is the
            // remote name, so split_once keeps slashes inside the branch name.
            let local_name = name
                .split_once('/')
                .map(|(_, rest)| rest)
                .ok_or_else(|| GitError::Checkout(format!("cannot derive a local name from {name}")))?
                .to_owned();

            let target = remote
                .get()
                .peel_to_commit()
                .map_err(|e| GitError::Checkout(format!("{name} has no commit: {e}")))?;

            let mut local = match repo.find_branch(&local_name, BranchType::Local) {
                Ok(existing) => existing,
                Err(_) => repo.branch(&local_name, &target, false)?,
            };
            let _ = local.set_upstream(Some(name));

            let reference = local.into_reference();
            let full_name = reference
                .name()
                .ok_or_else(|| GitError::Checkout(format!("branch {local_name} has no ref name")))?
                .to_owned();
            let object = reference.peel(git2::ObjectType::Any)?;
            repo.checkout_tree(&object, Some(CheckoutBuilder::new().safe()))?;
            repo.set_head(&full_name)?;
            return Ok(());
        }

        Err(GitError::Checkout(format!("no branch named {name}")))
    }

    /// Check out a bare commit, leaving HEAD detached, same as
    /// `git checkout <sha>`. Does not refresh the model; callers re-sync.
    pub fn checkout_commit(&mut self, oid: &str) -> Result<(), GitError> {
        let path = self.repo_path.as_deref().ok_or(GitError::NoRepo)?;
        let repo = Repository::open(path)?;
        let id = git2::Oid::from_str(oid)
            .map_err(|e| GitError::Checkout(format!("bad commit id {oid}: {e}")))?;
        let object = repo.find_object(id, None)?;
        repo.checkout_tree(&object, Some(CheckoutBuilder::new().safe()))?;
        repo.set_head_detached(id)?;
        Ok(())
    }

    /// Create a branch at a commit without checking it out. Does not refresh
    /// the model; callers re-sync.
    pub fn create_branch_at(&mut self, name: &str, oid: &str) -> Result<(), GitError> {
        let path = self.repo_path.as_deref().ok_or(GitError::NoRepo)?;
        let repo = Repository::open(path)?;
        let id = git2::Oid::from_str(oid)
            .map_err(|e| GitError::Checkout(format!("bad commit id {oid}: {e}")))?;
        let commit = repo.find_commit(id)?;
        repo.branch(name, &commit, false)?;
        Ok(())
    }
}

impl Default for GitGraphModel {
    fn default() -> Self {
        Self::new()
    }
}

// ── internals ────────────────────────────────────────────────────────────────

/// Collect local branches, remote branches and tags, plus a label for HEAD.
///
/// Returns `(refs, head_label, head_is_detached)`. `head_label` is the branch
/// name normally, or the short OID when detached, so the view can always show
/// where HEAD is without re-opening the repo.
fn load_refs(repo_path: &Path) -> Result<(Vec<RefNode>, Option<String>, bool), GitError> {
    let repo = Repository::open(repo_path)?;

    let head = repo.head().ok();
    let head_detached = repo.head_detached().unwrap_or(false);
    let head_oid = head.as_ref().and_then(|h| h.target());
    let head_branch = if head_detached {
        None
    } else {
        head.as_ref()
            .and_then(|h| h.shorthand())
            .map(|s| s.to_owned())
    };

    let head_label = match (&head_branch, head_oid) {
        (Some(name), _) => Some(name.clone()),
        (None, Some(oid)) => Some(oid.to_string()[..7].to_owned()),
        _ => None,
    };

    let mut refs = Vec::new();

    for (branch, kind) in repo.branches(None)?.flatten() {
        let Some(tip) = branch.get().target() else {
            continue;
        };
        let Some(name) = branch.name().ok().flatten() else {
            continue;
        };
        let name = name.to_owned();

        let is_local = kind == BranchType::Local;
        // Ahead/behind only means something for a local branch with an upstream.
        let (ahead, behind) = if is_local {
            branch
                .upstream()
                .ok()
                .and_then(|up| up.get().target())
                .and_then(|up_oid| repo.graph_ahead_behind(tip, up_oid).ok())
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };

        refs.push(RefNode {
            is_head: is_local && head_branch.as_deref() == Some(name.as_str()),
            name,
            kind: if is_local {
                RefKind::LocalBranch
            } else {
                RefKind::RemoteBranch
            },
            tip_oid: tip.to_string(),
            ahead,
            behind,
        });
    }

    // Tags. peel_to_commit resolves annotated tags to the commit they wrap, so
    // both lightweight and annotated tags land on a real commit OID.
    if let Ok(names) = repo.tag_names(None) {
        for name in names.iter().flatten() {
            let Ok(reference) = repo.find_reference(&format!("refs/tags/{name}")) else {
                continue;
            };
            let Ok(commit) = reference.peel_to_commit() else {
                continue;
            };
            refs.push(RefNode {
                name: name.to_owned(),
                kind: RefKind::Tag,
                tip_oid: commit.id().to_string(),
                is_head: false,
                ahead: 0,
                behind: 0,
            });
        }
    }

    // Current branch first, then alphabetical — stable ordering across refreshes.
    refs.sort_by(|a, b| {
        b.is_head
            .cmp(&a.is_head)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    Ok((refs, head_label, head_detached))
}

fn load_commits(repo_path: &Path) -> Result<Vec<CommitNode>, GitError> {
    let repo = Repository::open(repo_path)?;

    let head_oid = repo.head().ok().and_then(|h| h.target());

    // Map OID → branch short names (branches only, not every ref).
    let mut branch_ref_map: HashMap<String, Vec<String>> = HashMap::new();
    for branch_result in repo.branches(None)?.flatten() {
        let (branch, _kind) = branch_result;
        let Some(tip) = branch.get().target() else {
            continue;
        };
        if let Some(name) = branch.name().ok().flatten() {
            branch_ref_map
                .entry(tip.to_string())
                .or_default()
                .push(name.to_string());
        }
    }

    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;

    // Seed with HEAD first: that alone covers a detached HEAD and a repo
    // with no branches (the old fallback). Then every local branch tip and
    // the upstream each one tracks, so ahead/behind history is visible.
    // Deliberately not every remote branch: see the module docs.
    if let Some(oid) = head_oid {
        revwalk.push(oid)?;
    }
    for (branch, _kind) in repo.branches(Some(BranchType::Local))?.flatten() {
        if let Some(tip) = branch.get().target() {
            revwalk.push(tip)?;
        }
        // One stale remote-tracking ref must not blank the whole graph.
        if let Some(up) = branch.upstream().ok().and_then(|u| u.get().target()) {
            if let Err(err) = revwalk.push(up) {
                let name = branch.name().ok().flatten().unwrap_or("?");
                warn!("mnemos-ide-git-tree: skipping upstream of {name}: {err}");
            }
        }
    }

    let mut nodes = Vec::with_capacity(MAX_COMMITS.min(256));
    for oid_result in revwalk.take(MAX_COMMITS) {
        let oid = oid_result?;
        let commit = repo.find_commit(oid)?;

        let oid_str = oid.to_string();
        let short_oid = oid_str[..7].to_string();
        let summary = commit.summary().unwrap_or("").to_string();
        let author = commit.author().name().unwrap_or("").to_string();
        let committer_time = commit.time().seconds();

        let parent_oids = commit
            .parent_ids()
            .map(|p| p.to_string())
            .collect::<Vec<_>>();

        let branch_refs = branch_ref_map
            .get(&oid_str)
            .cloned()
            .unwrap_or_default();

        let is_head = head_oid.map(|h| h == oid).unwrap_or(false);

        nodes.push(CommitNode {
            oid: oid_str,
            short_oid,
            summary,
            author,
            committer_time,
            parent_oids,
            branch_refs,
            is_head,
            lane: 0,
        });
    }

    Ok(nodes)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use git2::{Commit, Oid, Signature, Time};

    use super::*;

    /// A throwaway repository under the system temp dir, removed on drop so a
    /// failing assertion still cleans up. Built with git2 only: no git CLI and
    /// no user config (explicit signatures, commits via HEAD).
    struct TempRepo {
        dir: PathBuf,
        repo: Repository,
    }

    impl TempRepo {
        fn new(test_name: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "mnemos-ide-git-tree-{test_name}-{}-{nanos}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let repo = Repository::init(&dir).expect("init temp repo");
            Self { dir, repo }
        }

        /// Commit an empty tree at an explicit time, so "newer" is deterministic.
        fn commit(&self, update_ref: Option<&str>, parents: &[Oid], time: i64, msg: &str) -> Oid {
            let sig = Signature::new("Test", "test@example.invalid", &Time::new(time, 0))
                .expect("signature");
            let tree_id = self
                .repo
                .treebuilder(None)
                .and_then(|b| b.write())
                .expect("empty tree");
            let tree = self.repo.find_tree(tree_id).expect("find tree");
            let parents: Vec<Commit<'_>> = parents
                .iter()
                .map(|id| self.repo.find_commit(*id).expect("parent"))
                .collect();
            let parent_refs: Vec<&Commit<'_>> = parents.iter().collect();
            self.repo
                .commit(update_ref, &sig, &sig, msg, &tree, &parent_refs)
                .expect("commit")
        }

        fn reference(&self, name: &str, oid: Oid) {
            self.repo
                .reference(name, oid, true, "mnemos-ide-git-tree test")
                .expect("create reference");
        }

        fn model(&self) -> GitGraphModel {
            let mut model = GitGraphModel::new();
            model.set_repo_path(self.dir.clone());
            model
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn oids(model: &GitGraphModel) -> Vec<String> {
        model.commits().iter().map(|c| c.oid.clone()).collect()
    }

    /// A remote-only branch whose commits are newer than HEAD must not enter
    /// the walk (in a fork it would crowd HEAD's history out of the
    /// MAX_COMMITS window), while the current branch's upstream still does.
    #[test]
    fn walk_skips_remote_only_branches_but_keeps_upstream() {
        let t = TempRepo::new("seed");
        let c1 = t.commit(Some("HEAD"), &[], 1_000, "c1");
        let c2 = t.commit(Some("HEAD"), &[c1], 2_000, "c2");

        // The current branch tracks origin/trunk, which is one commit ahead.
        t.repo
            .remote("origin", "https://example.invalid/repo.git")
            .expect("add remote");
        let u1 = t.commit(None, &[c2], 3_000, "u1 upstream ahead");
        t.reference("refs/remotes/origin/trunk", u1);
        let head_branch = t
            .repo
            .head()
            .expect("head")
            .shorthand()
            .expect("head name")
            .to_owned();
        t.repo
            .find_branch(&head_branch, BranchType::Local)
            .expect("local branch")
            .set_upstream(Some("origin/trunk"))
            .expect("set upstream");

        // A remote-only branch with commits newer than anything on HEAD.
        let r1 = t.commit(None, &[c1], 5_000, "r1 remote only");
        let r2 = t.commit(None, &[r1], 6_000, "r2 remote only");
        t.reference("refs/remotes/upstream/feature", r2);
        // A remote branch pointing into HEAD's history still labels it.
        t.reference("refs/remotes/upstream/old", c1);

        let mut model = t.model();
        model.refresh().expect("refresh");
        let walked = oids(&model);

        for (oid, what) in [(c1, "c1"), (c2, "c2"), (u1, "upstream-ahead u1")] {
            assert!(walked.contains(&oid.to_string()), "{what} should be walked");
        }
        for (oid, what) in [(r1, "r1"), (r2, "r2")] {
            assert!(
                !walked.contains(&oid.to_string()),
                "remote-only {what} should not be walked"
            );
        }

        let c1_node = model
            .commits()
            .iter()
            .find(|c| c.oid == c1.to_string())
            .expect("c1 node");
        assert!(
            c1_node.branch_refs.iter().any(|r| r == "upstream/old"),
            "remote branch labels kept: {:?}",
            c1_node.branch_refs
        );
        let head_node = model
            .commits()
            .iter()
            .find(|c| c.is_head)
            .expect("head node");
        assert_eq!(head_node.oid, c2.to_string());

        // Remote branches are still listed in the ref tree.
        let feature = model
            .refs()
            .iter()
            .find(|r| r.name == "upstream/feature")
            .expect("remote-only branch still listed");
        assert_eq!(feature.kind, RefKind::RemoteBranch);
        let local = model
            .refs()
            .iter()
            .find(|r| r.name == head_branch)
            .expect("local branch listed");
        assert!(local.is_head);
        assert_eq!((local.ahead, local.behind), (0, 1));
    }

    /// Checking out `origin/feature/x` creates the local `feature/x` (slashes
    /// after the remote name survive) and does not refresh the model itself.
    #[test]
    fn checkout_remote_branch_creates_local_tracking_branch() {
        let t = TempRepo::new("checkout");
        let c1 = t.commit(Some("HEAD"), &[], 1_000, "c1");
        let f1 = t.commit(None, &[c1], 2_000, "f1");
        t.reference("refs/remotes/origin/feature/x", f1);

        let mut model = t.model();
        model
            .checkout_branch("origin/feature/x")
            .expect("checkout remote branch");
        assert!(model.commits().is_empty(), "checkout leaves refreshing to the caller");

        let head = t.repo.head().expect("head");
        assert_eq!(head.shorthand(), Some("feature/x"));
        assert_eq!(head.target(), Some(f1));

        model.refresh().expect("refresh");
        assert_eq!(model.head_label(), Some("feature/x"));
    }
}
