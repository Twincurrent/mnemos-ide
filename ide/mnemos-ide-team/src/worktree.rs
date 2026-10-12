// SPDX-License-Identifier: AGPL-3.0-only
//! Git worktrees for spawned sessions: create one per session, report on all
//! of a repository's worktrees, and remove only those that are safe to lose.
//!
//! "Safe" means a clean working tree whose branch is already contained in the
//! base branch (`origin/HEAD`, else `main` or `master`). Anything else is kept
//! and reported with the reason. Branches are deleted with `git branch -d`,
//! which refuses unmerged work on its own as a second guard. Squash merges are
//! not detected: such a branch reads as unmerged and is kept.
//!
//! Worktrees a spawn created are recorded in `<git common dir>/mnemos-team/
//! worktrees.json`, next to the team rules and board (see the mnemos-agents
//! plugin's `team.sh`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use command::blocking::Command;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::names::{validate_branch, validate_name, NameError};

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("not inside a git repository: {0}")]
    NotARepo(PathBuf),
    #[error("{0} already exists")]
    Exists(PathBuf),
    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("{0}")]
    Refused(String),
}

/// One worktree as `list` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// None for a detached HEAD.
    pub branch: Option<String>,
    /// The repository's main working tree, never removed.
    pub main: bool,
    pub dirty: bool,
    /// The branch is contained in the base branch.
    pub merged: bool,
    /// Commits not on the branch's upstream; None without an upstream.
    pub unpushed: Option<u32>,
    pub size_bytes: u64,
    /// The spawned session that created it, if one did.
    pub session: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub path: PathBuf,
    pub branch: String,
}

/// `<git common dir>/mnemos-team/worktrees.json`: session name -> worktree.
pub type Records = BTreeMap<String, Record>;

fn git(dir: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    } else {
        Err(WorktreeError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        })
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The top of the working tree around `dir`.
pub fn toplevel(dir: &Path) -> Result<PathBuf, WorktreeError> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .map_err(|_| WorktreeError::NotARepo(dir.to_path_buf()))
}

fn team_dir(dir: &Path) -> Result<PathBuf, WorktreeError> {
    let common = git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map_err(|_| WorktreeError::NotARepo(dir.to_path_buf()))?;
    Ok(PathBuf::from(common).join("mnemos-team"))
}

/// `<git common dir>/mnemos-team` for the repository around `dir`.
pub fn team_dir_of(dir: &Path) -> Option<PathBuf> {
    team_dir(dir).ok()
}

pub fn load_records(repo: &Path) -> Records {
    team_dir(repo)
        .ok()
        .and_then(|d| std::fs::read(d.join("worktrees.json")).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_records(repo: &Path, records: &Records) -> Result<(), WorktreeError> {
    let dir = team_dir(repo)?;
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join("worktrees.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(records).unwrap_or_default())?;
    std::fs::rename(&tmp, dir.join("worktrees.json"))?;
    Ok(())
}

/// The branch finished work is merged into: `origin/HEAD`'s target, else a
/// local `main` or `master`.
pub fn default_base(repo: &Path) -> Option<String> {
    if let Ok(remote_head) = git(repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        if !remote_head.is_empty() {
            return Some(remote_head);
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|b| git_ok(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")]))
        .map(str::to_string)
}

/// Where a session's worktree goes: next to the main checkout, named after it.
pub fn path_for(repo: &Path, name: &str) -> Result<PathBuf, WorktreeError> {
    validate_name(name)?;
    let top = toplevel(repo)?;
    let main = main_worktree(&top)?;
    let dir_name = main
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let parent = main.parent().unwrap_or(&main).to_path_buf();
    Ok(parent.join(format!("{dir_name}-{name}")))
}

fn main_worktree(repo: &Path) -> Result<PathBuf, WorktreeError> {
    let porcelain = git(repo, &["worktree", "list", "--porcelain"])?;
    porcelain
        .lines()
        .find_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .ok_or_else(|| WorktreeError::NotARepo(repo.to_path_buf()))
}

/// Creates the worktree for session `name` on `branch` (new from `base`, or
/// the existing branch) and records it.
pub fn create(
    repo: &Path,
    name: &str,
    branch: &str,
    base: Option<&str>,
) -> Result<PathBuf, WorktreeError> {
    validate_branch(branch)?;
    let path = path_for(repo, name)?;
    if path.exists() {
        return Err(WorktreeError::Exists(path));
    }
    let path_str = path.to_string_lossy().into_owned();
    let branch_exists = git_ok(
        repo,
        &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")],
    );
    if branch_exists {
        git(repo, &["worktree", "add", &path_str, branch])?;
    } else {
        let base = match base {
            Some(b) => b.to_string(),
            None => default_base(repo).unwrap_or_else(|| "HEAD".to_string()),
        };
        git(repo, &["worktree", "add", "-b", branch, &path_str, &base])?;
    }
    let mut records = load_records(repo);
    records.insert(
        name.to_string(),
        Record {
            path: path.clone(),
            branch: branch.to_string(),
        },
    );
    save_records(repo, &records)?;
    Ok(path)
}

fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn inspect(path: &Path, branch: Option<String>, main: bool, base: Option<&str>) -> WorktreeInfo {
    let dirty = git(path, &["status", "--porcelain", "--untracked-files=normal"])
        .map(|s| !s.is_empty())
        .unwrap_or(true);
    let merged = match (&branch, base) {
        (Some(b), Some(base)) => git_ok(path, &["merge-base", "--is-ancestor", b, base]),
        _ => false,
    };
    let unpushed = branch.as_ref().and_then(|b| {
        git(path, &["rev-list", "--count", &format!("{b}@{{upstream}}..{b}")])
            .ok()
            .and_then(|n| n.parse().ok())
    });
    WorktreeInfo {
        path: path.to_path_buf(),
        branch,
        main,
        dirty,
        merged,
        unpushed,
        size_bytes: if main { 0 } else { dir_size(path) },
        session: None,
    }
}

/// Every worktree of the repository around `repo`, the main one first. The
/// main working tree's size is not computed (it is the project itself).
pub fn list(repo: &Path) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    let top = toplevel(repo)?;
    let base = default_base(&top);
    let records = load_records(&top);
    let porcelain = git(&top, &["worktree", "list", "--porcelain"])?;
    let mut out = Vec::new();
    for (i, block) in porcelain.split("\n\n").enumerate() {
        let mut path = None;
        let mut branch = None;
        let mut prunable = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_string());
            } else if line.starts_with("prunable") {
                prunable = true;
            }
        }
        let Some(path) = path else { continue };
        if prunable || !path.exists() {
            continue;
        }
        let mut info = inspect(&path, branch, i == 0, base.as_deref());
        info.session = records
            .iter()
            .find(|(_, r)| same_path(&r.path, &info.path))
            .map(|(n, _)| n.clone());
        out.push(info);
    }
    Ok(out)
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Why a worktree must be kept, or None when it is safe to remove.
pub fn keep_reason(info: &WorktreeInfo, base: Option<&str>) -> Option<String> {
    if info.main {
        return Some("it is the main working tree".to_string());
    }
    if info.dirty {
        return Some("it has uncommitted or untracked changes".to_string());
    }
    let Some(branch) = &info.branch else {
        return Some("its HEAD is detached".to_string());
    };
    if !info.merged {
        let base = base.unwrap_or("the base branch");
        return Some(match info.unpushed {
            Some(n) if n > 0 => {
                format!("branch {branch} is not merged into {base} and has {n} unpushed commit(s)")
            }
            None => format!("branch {branch} is not merged into {base} and has no upstream"),
            Some(_) => format!("branch {branch} is not merged into {base}"),
        });
    }
    None
}

/// Removes one worktree when [`keep_reason`] finds nothing, deletes its branch
/// with `git branch -d`, prunes stale worktree metadata and drops its record.
pub fn remove(repo: &Path, path: &Path) -> Result<WorktreeInfo, WorktreeError> {
    // Run from the main working tree: `repo` may be the very worktree that is
    // about to disappear.
    let top = main_worktree(&toplevel(repo)?)?;
    let base = default_base(&top);
    let info = list(&top)?
        .into_iter()
        .find(|w| same_path(&w.path, path))
        .ok_or_else(|| WorktreeError::Refused(format!("{} is not a worktree of this repository", path.display())))?;
    if let Some(reason) = keep_reason(&info, base.as_deref()) {
        return Err(WorktreeError::Refused(format!(
            "kept {}: {reason}",
            info.path.display()
        )));
    }
    let path_str = info.path.to_string_lossy().into_owned();
    git(&top, &["worktree", "remove", &path_str])?;
    if let Some(branch) = &info.branch {
        git(&top, &["branch", "-d", branch])?;
    }
    git(&top, &["worktree", "prune"])?;
    let mut records = load_records(&top);
    let before = records.len();
    records.retain(|_, r| !same_path(&r.path, &info.path) && r.path != info.path);
    if records.len() != before {
        save_records(&top, &records)?;
    }
    Ok(info)
}

/// The outcome of [`prune`].
#[derive(Debug, Default, Serialize)]
pub struct PruneReport {
    pub removed: Vec<WorktreeInfo>,
    /// Path and why it was kept.
    pub kept: Vec<(PathBuf, String)>,
    pub freed_bytes: u64,
}

/// Removes every worktree that is safe to remove (all of them when `dry_run`
/// is false; none when it is true, which only reports).
pub fn prune(repo: &Path, dry_run: bool) -> Result<PruneReport, WorktreeError> {
    let top = toplevel(repo)?;
    let base = default_base(&top);
    let mut report = PruneReport::default();
    for info in list(&top)? {
        if info.main {
            continue;
        }
        if let Some(reason) = keep_reason(&info, base.as_deref()) {
            report.kept.push((info.path.clone(), reason));
            continue;
        }
        if dry_run {
            report.freed_bytes += info.size_bytes;
            report.removed.push(info);
            continue;
        }
        match remove(&top, &info.path) {
            Ok(info) => {
                report.freed_bytes += info.size_bytes;
                report.removed.push(info);
            }
            Err(err) => report.kept.push((info.path.clone(), err.to_string())),
        }
    }
    if !dry_run {
        git(&top, &["worktree", "prune"])?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.org"])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("proj");
        std::fs::create_dir(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        (tmp, repo)
    }

    #[test]
    fn create_list_and_refuse_unmerged() {
        let (_tmp, repo) = repo();
        let path = create(&repo, "tester", "team/tester", None).unwrap();
        assert_eq!(path.file_name().unwrap(), "proj-tester");
        assert!(matches!(
            create(&repo, "tester", "team/other", None),
            Err(WorktreeError::Exists(_))
        ));

        std::fs::write(path.join("b.txt"), "b").unwrap();
        let err = remove(&repo, &path).unwrap_err().to_string();
        assert!(err.contains("uncommitted"), "{err}");

        run(&path, &["add", "."]);
        run(&path, &["commit", "-q", "-m", "work"]);
        let listed = list(&repo).unwrap();
        let wt = listed.iter().find(|w| !w.main).unwrap();
        assert_eq!(wt.session.as_deref(), Some("tester"));
        assert!(!wt.dirty && !wt.merged && wt.size_bytes > 0);
        let err = remove(&repo, &path).unwrap_err().to_string();
        assert!(err.contains("not merged into main"), "{err}");

        run(&repo, &["merge", "-q", "--ff-only", "team/tester"]);
        // Called from inside the worktree itself, as retire does.
        let removed = remove(&path, &path).unwrap();
        assert_eq!(removed.branch.as_deref(), Some("team/tester"));
        assert!(!path.exists());
        assert!(load_records(&repo).is_empty());
        assert!(!git_ok(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/team/tester"]));
    }

    #[test]
    fn prune_removes_only_merged_clean_trees() {
        let (_tmp, repo) = repo();
        let merged = create(&repo, "done", "team/done", None).unwrap();
        let busy = create(&repo, "busy", "team/busy", None).unwrap();
        std::fs::write(busy.join("wip.txt"), "x").unwrap();

        let dry = prune(&repo, true).unwrap();
        assert_eq!(dry.removed.len(), 1);
        assert!(merged.exists(), "a dry run removes nothing");

        let report = prune(&repo, false).unwrap();
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.kept.len(), 1);
        assert!(!merged.exists());
        assert!(busy.exists());
        assert!(report.kept[0].1.contains("uncommitted"));
    }

    #[test]
    fn main_tree_is_never_removed() {
        let (_tmp, repo) = repo();
        let err = remove(&repo, &repo).unwrap_err().to_string();
        assert!(err.contains("main working tree"), "{err}");
    }
}
