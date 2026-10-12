// SPDX-License-Identifier: AGPL-3.0-only
//! Tabs <-> registry reconcile: the pure planner behind "every machine shows
//! the same project tabs".
//!
//! The open set is the registry's records with `open == true`. The app feeds
//! [`plan`] a snapshot of every tab it has (all windows) and executes the
//! returned [`ReconcilePlan`]: relink tabs to new record ids, open tabs for
//! open records that have none, close tabs whose record was closed elsewhere.
//! The planner mutates the store (open flags, learned paths, new records) and
//! says so via `store_changed`; the caller saves it through the locked update.
//!
//! Tab ids from older builds are unreliable: the Mac had twelve tabs carrying
//! nine ids, most pointing at records the old whole-blob sync had erased, and
//! two pairs of unrelated tabs sharing one id. So identity is re-derived from
//! evidence (the tab's working directory and git remote, then its title)
//! whenever the stored id is dead or shared by tabs that disagree.
//!
//! Running [`plan`] twice in a row yields an empty second plan.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mnemos_ide_core::registry::{git_repo_key, path_file_name, same_path};
use mnemos_ide_core::types::Project;

use crate::resolve::{tab_root, FsEnv, LocalResolver};
use crate::store::ProjectStore;

/// What the planner needs to know about one tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSnapshot {
    /// Caller's handle for the tab; echoed back in the plan.
    pub key: usize,
    /// The tab's `tcb_project_id`, if any.
    pub project_id: Option<String>,
    /// The tab's user-visible title, if it has a custom one.
    pub title: Option<String>,
    /// Working directories of the tab's terminal panes, most relevant first.
    pub cwds: Vec<PathBuf>,
    /// No running command or agent in any pane, so closing loses nothing.
    pub safe_to_close: bool,
}

/// Which pass this is. Both plan closes the same way: a tab of a project
/// closed elsewhere is closed only when `safe_to_close`, and a busy one is
/// reported in [`ReconcilePlan::kept`]. Startup used to close unconditionally
/// on the theory that a freshly restored tab runs nothing yet, but restored
/// panes can auto-resume a Claude session at launch (agent-restore), so that
/// theory no longer holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileMode {
    /// Window creation, right after the tab restore.
    Startup,
    /// `projects.json` changed while running.
    Live,
}

/// A tab the app should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTab {
    pub project_id: String,
    /// The project's checkout here; `None` when there is none on this
    /// machine (the app opens the tab in its default directory).
    pub cwd: Option<PathBuf>,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcilePlan {
    /// `(tab key, new tcb_project_id)` for tabs whose id changes. `None`
    /// unlinks the tab (a plain terminal tab).
    pub relink: Vec<(usize, Option<String>)>,
    /// Tabs to open in the primary window, in registry order.
    pub open: Vec<OpenTab>,
    /// Keys of tabs to close.
    pub close: Vec<usize>,
    /// Keys of tabs whose project is closed but which stay open because
    /// something runs in them (either mode). Informational: the caller
    /// remembers them so that a later reconcile, run for some unrelated
    /// change, does not close the tab under the user once it goes idle.
    pub kept: Vec<usize>,
    /// The store was mutated; the caller must save it.
    pub store_changed: bool,
}

impl ReconcilePlan {
    /// Nothing to do: no tab action and no store change. `kept` is not an
    /// action.
    pub fn is_empty(&self) -> bool {
        self.relink.is_empty() && self.open.is_empty() && self.close.is_empty() && !self.store_changed
    }
}

/// Evidence about one tab, gathered once per plan.
struct TabFacts {
    /// Stored id, if non-empty.
    raw: Option<String>,
    /// `raw` mapped through `replacedBy` to a live record.
    live: Option<String>,
    /// Project root from the first cwd that has one.
    root: Option<String>,
    /// `owner/repo` of `root`.
    remote: Option<String>,
    /// Last component of `root`.
    dir_name: Option<String>,
    title: Option<String>,
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn tab_facts(tab: &TabSnapshot, store: &ProjectStore, env: &dyn FsEnv) -> TabFacts {
    let platform = env.platform();
    let raw = tab.project_id.clone().filter(|s| !s.trim().is_empty());
    let live = raw.as_deref().and_then(|id| store.resolve_id(id));
    let root_path = tab.cwds.iter().find_map(|c| tab_root(env, c));
    let remote = root_path.as_deref().and_then(|r| env.git_remote(r));
    let root = root_path.as_deref().map(path_string);
    let dir_name = root.as_deref().and_then(|r| path_file_name(r, platform));
    let title = tab
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    TabFacts {
        raw,
        live,
        root,
        remote,
        dir_name,
        title,
    }
}

/// The record's name matches the tab's title or its root folder's name.
fn name_matches(p: &Project, f: &TabFacts) -> bool {
    f.title.as_deref().is_some_and(|t| same_name(&p.name, t))
        || f.dir_name.as_deref().is_some_and(|d| same_name(&p.name, d))
}

/// Both sides know their repo and the repos differ: provably not the same
/// project.
fn repo_conflict(p: &Project, remote: Option<&str>) -> bool {
    match (p.git_repo(), remote) {
        (Some(g), Some(r)) => git_repo_key(&g) != git_repo_key(r),
        _ => false,
    }
}

fn is_local_path(p: &Project, platform: &str, root: &str) -> bool {
    p.platform_path(platform)
        .is_some_and(|lp| same_path(&path_string(&lp), root, platform))
}

/// A record identified from a tab's evidence. `claim` means the tab's root
/// becomes the record's checkout on this machine once the match is taken.
struct Detection {
    id: String,
    claim: bool,
}

/// Steps b and c of the identity rules, without mutating anything:
/// b1. a record whose checkout here is the tab's root;
/// b2. a record of the root's repo with no checkout here yet (claim; name
///     match first, then oldest);
/// b3. a record named like the tab or its folder with no checkout here yet
///     and no conflicting repo (claim; oldest);
/// c.  a record named like the tab's title (no claim).
fn detect(store: &ProjectStore, f: &TabFacts, platform: &str) -> Option<Detection> {
    if let Some(root) = &f.root {
        if let Some(p) = store.projects.iter().find(|p| is_local_path(p, platform, root)) {
            return Some(Detection {
                id: p.id.clone(),
                claim: false,
            });
        }
        if let Some(remote) = &f.remote {
            let key = git_repo_key(remote);
            let pick = store
                .projects
                .iter()
                .filter(|p| {
                    p.platform_path(platform).is_none()
                        && p.git_repo().is_some_and(|g| git_repo_key(&g) == key)
                })
                .min_by(|a, b| {
                    (!name_matches(a, f), a.created_at, &a.id).cmp(&(
                        !name_matches(b, f),
                        b.created_at,
                        &b.id,
                    ))
                });
            if let Some(p) = pick {
                return Some(Detection {
                    id: p.id.clone(),
                    claim: true,
                });
            }
        }
        let pick = store
            .projects
            .iter()
            .filter(|p| {
                p.platform_path(platform).is_none()
                    && name_matches(p, f)
                    && !repo_conflict(p, f.remote.as_deref())
            })
            .min_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        if let Some(p) = pick {
            return Some(Detection {
                id: p.id.clone(),
                claim: true,
            });
        }
    }
    let title = f.title.as_deref()?;
    store
        .projects
        .iter()
        .find(|p| same_name(&p.name, title))
        .map(|p| Detection {
            id: p.id.clone(),
            claim: false,
        })
}

/// Make `root` record `id`'s checkout here, and fill its repo if missing.
fn claim(store: &mut ProjectStore, id: &str, platform: &str, root: &str, remote: Option<&str>) {
    if let Some(p) = store.get_mut(id) {
        p.set_platform_path(platform, Path::new(root));
        if p.git_repo().is_none() {
            if let Some(r) = remote {
                p.set_git_repo(r);
            }
        }
    }
}

fn take(store: &mut ProjectStore, f: &TabFacts, platform: &str, d: Detection) -> String {
    if d.claim {
        if let Some(root) = &f.root {
            claim(store, &d.id, platform, root, f.remote.as_deref());
        }
    }
    d.id
}

/// Step e: a former project tab (it carried an id) with a real project root
/// that matches nothing becomes a new open record. It keeps its old id when
/// that id is unused (not live, not
/// tombstoned, not shared with another tab), so sessions keyed on it stay
/// attached.
///
/// A tab whose custom title names a different folder than the one it sits
/// in is not registered (returns `None`, the tab ends up unlinked). A dead
/// id plus a folder that contradicts the title is no evidence of which
/// project it was: the Mac's "notes app" tab sat in `~/code/qa`,
/// and registering that would have synced the wrong checkout to every
/// machine. A tab without a custom title is named after its folder and
/// cannot disagree.
fn create_record(
    store: &mut ProjectStore,
    f: &TabFacts,
    platform: &str,
    groups: &HashMap<String, Vec<usize>>,
    taken: &HashSet<String>,
    now: DateTime<Utc>,
) -> Option<String> {
    let root = f.root.as_ref()?;
    if let (Some(title), Some(dir_name)) = (&f.title, &f.dir_name) {
        if !same_name(title, dir_name) {
            tracing::debug!(
                %root,
                %title,
                %dir_name,
                "tab title contradicts its folder; not registering it"
            );
            return None;
        }
    }
    let id = match &f.raw {
        Some(raw)
            if !store.is_known_id(raw) && groups.get(raw).map_or(0, Vec::len) == 1 =>
        {
            raw.clone()
        }
        _ => store.mint_project_id(now, taken),
    };
    let name = f
        .title
        .clone()
        .or_else(|| f.dir_name.clone())
        .unwrap_or_else(|| root.clone());
    let mut project = Project {
        id: id.clone(),
        name,
        path: PathBuf::from(root),
        created_at: now,
        last_opened_at: Some(now),
        extra: serde_json::Map::new(),
    };
    project.set_platform_path(platform, Path::new(root));
    if let Some(r) = &f.remote {
        project.set_git_repo(r);
    }
    project.set_open(true, now);
    store.add(project);
    Some(id)
}

/// Step 1: which record each tab is.
fn identify(
    store: &mut ProjectStore,
    facts: &[TabFacts],
    platform: &str,
    now: DateTime<Utc>,
) -> Vec<Option<String>> {
    let n = facts.len();
    let mut out: Vec<Option<String>> = vec![None; n];
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        if let Some(raw) = &f.raw {
            groups.entry(raw.clone()).or_default().push(i);
        }
    }

    // A live id is contested when several tabs carry it and the evidence says
    // at least one of them is a different project. Only then does evidence
    // override a stored id: a lone tab keeps its project even after `cd`ing
    // somewhere else.
    let contested: HashSet<&str> = groups
        .iter()
        .filter(|(_, idxs)| idxs.len() >= 2)
        .filter_map(|(raw, idxs)| {
            let live = facts[idxs[0]].live.as_deref()?;
            idxs.iter()
                .any(|&i| detect(store, &facts[i], platform).is_some_and(|d| d.id != live))
                .then_some(raw.as_str())
        })
        .collect();

    // a (keep), then b/c from evidence.
    for (i, f) in facts.iter().enumerate() {
        if let Some(live) = &f.live {
            let raw = f.raw.as_deref().unwrap_or_default();
            if !contested.contains(raw) {
                out[i] = Some(live.clone());
                continue;
            }
            // Contested: evidence decides; a tab without evidence keeps the
            // id it already had.
            out[i] = Some(match detect(store, f, platform) {
                Some(d) => take(store, f, platform, d),
                None => live.clone(),
            });
            continue;
        }
        if let Some(d) = detect(store, f, platform) {
            out[i] = Some(take(store, f, platform, d));
        }
    }

    // d: a tab sharing its dead id with a resolved tab is that tab's project,
    // unless its own checkout is provably a different repo.
    let sharer_target = |store: &ProjectStore, out: &[Option<String>], i: usize| -> Option<String> {
        let raw = facts[i].raw.as_ref()?;
        let target = groups
            .get(raw)?
            .iter()
            .filter(|&&j| j != i)
            .find_map(|&j| out[j].clone())?;
        let record = store.get(&target)?;
        (!repo_conflict(record, facts[i].remote.as_deref())).then(|| record.id.clone())
    };
    for i in 0..n {
        if out[i].is_none() {
            out[i] = sharer_target(store, &out, i);
        }
    }

    // e: new records for tabs with a root nothing matched. Evidence is
    // checked again first, since a record created for an earlier tab may be
    // this tab's project too.
    //
    // Only a tab that carried a project id is registered: it was a project
    // tab whose record got lost (erased by the old whole-blob sync, or an id
    // two tabs collided on). A plain terminal tab says nothing about wanting a
    // synced project, and registering it would put `/tmp` or `~/Downloads` on
    // every machine's tab strip. Nor is a tab whose title names a different
    // folder than its root: the title and the folder must agree (see
    // `create_record`).
    let taken: HashSet<String> = facts.iter().filter_map(|f| f.raw.clone()).collect();
    for (i, f) in facts.iter().enumerate() {
        if out[i].is_some() || f.root.is_none() {
            continue;
        }
        out[i] = match detect(store, f, platform) {
            Some(d) => Some(take(store, f, platform, d)),
            None if f.raw.is_some() => create_record(store, f, platform, &groups, &taken, now),
            None => None,
        };
    }

    // d again for tabs whose sharer only got a record in e; the rest stay
    // unlinked (f).
    for i in 0..n {
        if out[i].is_none() {
            out[i] = sharer_target(store, &out, i);
        }
    }
    out
}

/// Step 4: learn each linked record's checkout here. A tab sitting in a
/// checkout of the record's repo is direct evidence; otherwise the resolver
/// checks the record's own path and falls back to discovery.
fn learn(
    store: &mut ProjectStore,
    facts: &[TabFacts],
    assigned: &[Option<String>],
    env: &dyn FsEnv,
    resolver: &mut LocalResolver,
) {
    let platform = env.platform();
    let mut seen: HashSet<String> = HashSet::new();
    for (f, id) in facts.iter().zip(assigned) {
        let Some(id) = id.as_deref().and_then(|id| store.resolve_id(id)) else {
            continue;
        };
        if let (Some(root), Some(remote)) = (&f.root, &f.remote) {
            // A slot pointing at a folder that no longer exists here (the
            // checkout moved) is as good as empty: the tab is better evidence.
            let claimable = store.get(&id).is_some_and(|p| {
                p.platform_path(platform).is_none_or(|lp| !env.is_dir(&lp))
                    && p.git_repo().is_some_and(|g| git_repo_key(&g) == git_repo_key(remote))
            });
            let taken = store
                .projects
                .iter()
                .any(|p| p.id != id && is_local_path(p, platform, root));
            if claimable && !taken {
                claim(store, &id, platform, root, Some(remote));
            }
        }
        if seen.insert(id.clone()) {
            resolver.resolve_local_path(store, &id, env);
        }
    }
}

/// Reconcile the tab strip with the registry. See the design's "Tabs -
/// reconcile" section:
///
/// 1. Identity per tab: keep a live, uncontested id; else detect from the
///    tab's root and title; else inherit from a tab sharing its old id; else,
///    for a tab that carried an id and whose title (if any) agrees with its
///    root folder's name, register its root as a new open record; else
///    unlink (a plain tab stays plain).
/// 2. Open flags: a record with no flag that a project tab (one that carried
///    an id) links to becomes open, without a clock so any real open/close
///    from another machine outranks it; a tab linked to a closed record is
///    closed when it is safe to close, in either mode, and a busy one is
///    reported in `kept`. A plain tab (no stored id) only ever joins an open
///    record; otherwise it stays plain.
/// 3. Every open record without a tab gets one, at its resolved checkout.
/// 4. Checkouts and repos seen along the way are stored on the records.
/// 5. The registry is normalized; ids folded into another record are
///    relinked.
///
/// The platform acted out is `env.platform()`. `_mode` says which pass this
/// is; both plan alike (see [`ReconcileMode`]).
pub fn plan(
    store: &mut ProjectStore,
    tabs: &[TabSnapshot],
    _mode: ReconcileMode,
    env: &dyn FsEnv,
    resolver: &mut LocalResolver,
    now: DateTime<Utc>,
) -> ReconcilePlan {
    let platform = env.platform();
    let before = store.registry_value();
    store.normalize();

    let facts: Vec<TabFacts> = tabs.iter().map(|t| tab_facts(t, store, env)).collect();
    let assigned = identify(store, &facts, platform, now);
    learn(store, &facts, &assigned, env, resolver);
    // Learning a repo can fold two registrations together; settle ids before
    // looking at open flags.
    store.normalize();
    let assigned: Vec<Option<String>> = assigned
        .into_iter()
        .map(|a| a.and_then(|id| store.resolve_id(&id)))
        .collect();

    // A project tab linked to a record from before the synced flag says the
    // project is open. Adopted without a clock (no `openChangedAt`, no
    // `updatedAt` bump): this runs at startup on whatever `projects.json`
    // the last session left, so a real open/close from another machine,
    // however old, must still win the merge, and so must that machine's
    // copy of the record's other fields.
    //
    // Only tabs that carried an id: a plain tab the user `cd`d into an
    // unflagged project's folder says nothing about the project, and
    // flagging it would open it on every machine.
    for (tab, id) in tabs.iter().zip(&assigned) {
        let Some(id) = id else { continue };
        if !carried_id(tab) {
            continue;
        }
        if let Some(p) = store.get_mut(id).filter(|p| p.is_open().is_none()) {
            p.extra.insert("open".to_string(), serde_json::Value::Bool(true));
        }
    }

    let linked: HashSet<String> = assigned.iter().flatten().cloned().collect();
    let mut open = Vec::new();
    for id in store.open_project_ids() {
        if linked.contains(&id) {
            continue;
        }
        let cwd = resolver.resolve_local_path(store, &id, env);
        open.push(OpenTab {
            project_id: id,
            cwd,
            title: String::new(),
        });
    }

    // Discovery may have filled a repo that folds records together.
    store.normalize();
    let assigned: Vec<Option<String>> = tabs
        .iter()
        .zip(assigned)
        .map(|(tab, a)| {
            let id = a.and_then(|id| store.resolve_id(&id))?;
            // A plain tab (no stored id) only joins an open project. In a
            // closed project's folder, linking it would close it on the spot
            // (the user just opened it); in an unflagged one's, it would be
            // a project tab nobody asked for. Neither is evidence that the
            // project should open everywhere.
            let open = store.get(&id).and_then(Project::is_open) == Some(true);
            (carried_id(tab) || open).then_some(id)
        })
        .collect();

    // Closes are decided on the final ids: a fold above can hand a tab's
    // closed record over to an open one. A busy tab is kept in every mode:
    // at startup a restored pane may already be resuming a Claude session.
    let of_closed = |id: &Option<String>| {
        id.as_deref().and_then(|id| store.get(id)).and_then(Project::is_open) == Some(false)
    };
    let mut close: Vec<usize> = Vec::new();
    let mut kept: Vec<usize> = Vec::new();
    for (tab, id) in tabs.iter().zip(&assigned) {
        if !of_closed(id) {
            continue;
        }
        if tab.safe_to_close {
            close.push(tab.key);
        } else {
            kept.push(tab.key);
        }
    }

    let linked: HashSet<String> = assigned.iter().flatten().cloned().collect();
    let mut opening: HashSet<String> = HashSet::new();
    let open: Vec<OpenTab> = open
        .into_iter()
        .filter_map(|o| {
            let id = store.resolve_id(&o.project_id)?;
            let record = store.get(&id)?;
            if record.is_open() != Some(true) || linked.contains(&id) || !opening.insert(id.clone())
            {
                return None;
            }
            let title = record.name.clone();
            Some(OpenTab {
                project_id: id,
                cwd: o.cwd,
                title,
            })
        })
        .collect();

    let relink = tabs
        .iter()
        .zip(&assigned)
        .filter(|(tab, id)| {
            tab.project_id.as_deref().filter(|s| !s.trim().is_empty()) != id.as_deref()
        })
        .map(|(tab, id)| (tab.key, id.clone()))
        .collect();

    ReconcilePlan {
        relink,
        open,
        close,
        kept,
        store_changed: store.registry_value() != before,
    }
}

/// The tab carried a project id (it was a project tab), as opposed to a
/// plain terminal tab.
fn carried_id(tab: &TabSnapshot) -> bool {
    tab.project_id.as_deref().is_some_and(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mnemos_ide_core::registry::{path_parent, same_path};

    /// Minimal Linux-shaped machine: a home and a list of plain directories.
    struct Fs {
        dirs: Vec<&'static str>,
    }

    impl FsEnv for Fs {
        fn platform(&self) -> &'static str {
            "linux"
        }
        fn is_dir(&self, path: &Path) -> bool {
            let s = path.to_string_lossy();
            self.dirs.iter().any(|d| {
                let mut cur = Some(d.to_string());
                while let Some(c) = cur {
                    if same_path(&c, &s, "linux") {
                        return true;
                    }
                    cur = path_parent(&c, "linux");
                }
                false
            })
        }
        fn home(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/home/h"))
        }
        fn git_remote(&self, _dir: &Path) -> Option<String> {
            None
        }
        fn git_root(&self, _cwd: &Path) -> Option<PathBuf> {
            None
        }
        fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
            let s = dir.to_string_lossy();
            self.dirs
                .iter()
                .filter(|d| path_parent(d, "linux").is_some_and(|p| same_path(&p, &s, "linux")))
                .map(PathBuf::from)
                .collect()
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 8, 0, 0).unwrap()
    }

    fn store() -> (tempfile::TempDir, ProjectStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::load_blocking_from(
            dir.path().join("projects.json"),
            dir.path().join("workspace_state.json"),
        )
        .unwrap();
        (dir, store)
    }

    fn tab(key: usize, id: Option<&str>, title: Option<&str>, cwds: &[&str]) -> TabSnapshot {
        TabSnapshot {
            key,
            project_id: id.map(str::to_string),
            title: title.map(str::to_string),
            cwds: cwds.iter().map(PathBuf::from).collect(),
            safe_to_close: true,
        }
    }

    #[test]
    fn dead_id_without_evidence_unlinks_and_fresh_machine_opens_everything() {
        let fs = Fs {
            dirs: vec!["/home/h/code/a"],
        };
        let (_dir, mut store) = store();
        let mut a = Project::new("a", PathBuf::from("/home/h/code/a"));
        a.id = "a".into();
        a.set_open(true, now());
        store.add(a);
        let mut b = Project::new("b", PathBuf::from("/Users/h/code/b"));
        b.id = "b".into();
        b.set_open(true, now());
        store.add(b);
        let mut resolver = LocalResolver::new();

        // A tab carrying an id nobody knows, sitting in home, titled nothing:
        // unlinked. Both open records get tabs; `b` has no checkout here.
        let tabs = vec![tab(1, Some("ghost"), None, &["/home/h"])];
        let p = plan(&mut store, &tabs, ReconcileMode::Startup, &fs, &mut resolver, now());
        assert_eq!(p.relink, vec![(1, None)]);
        assert!(p.close.is_empty());
        let opened: Vec<(&str, Option<&Path>)> = p
            .open
            .iter()
            .map(|o| (o.project_id.as_str(), o.cwd.as_deref()))
            .collect();
        assert_eq!(
            opened,
            vec![("a", Some(Path::new("/home/h/code/a"))), ("b", None)]
        );
        assert!(p.store_changed, "a's linux checkout was learned");
    }

    #[test]
    fn plain_tab_in_a_new_folder_stays_plain() {
        let fs = Fs {
            dirs: vec!["/home/h/code/fresh", "/home/h/Downloads/junk"],
        };
        let (_dir, mut store) = store();
        let mut resolver = LocalResolver::new();
        let tabs = vec![
            tab(1, None, None, &["/home/h/code/fresh"]),
            tab(2, None, Some("scratch"), &["/home/h/Downloads/junk"]),
        ];
        for mode in [ReconcileMode::Startup, ReconcileMode::Live] {
            let p = plan(&mut store, &tabs, mode, &fs, &mut resolver, now());
            assert!(p.is_empty(), "a plain tab must not register its folder: {p:?}");
            assert!(store.projects.is_empty());
        }
    }

    #[test]
    fn lost_project_tab_is_registered_and_a_plain_tab_there_joins_it() {
        let fs = Fs {
            dirs: vec!["/home/h/code/fresh"],
        };
        let (_dir, mut store) = store();
        let mut resolver = LocalResolver::new();
        // Tab 1 was a project tab whose record the old sync erased; tab 2 is
        // a plain tab the user opened in the same folder.
        let tabs = vec![
            tab(1, Some("project-lost"), None, &["/home/h/code/fresh"]),
            tab(2, None, None, &["/home/h/code/fresh"]),
        ];
        let p = plan(&mut store, &tabs, ReconcileMode::Live, &fs, &mut resolver, now());
        assert_eq!(
            p.relink,
            vec![(2, Some("project-lost".to_string()))],
            "the lost id is reused and the plain tab in its folder is linked to it"
        );
        let record = store.get("project-lost").unwrap();
        assert_eq!(record.name, "fresh");
        assert_eq!(record.is_open(), Some(true));
        assert!(p.open.is_empty() && p.close.is_empty());

        let tabs: Vec<TabSnapshot> = tabs
            .into_iter()
            .map(|mut t| {
                t.project_id = Some("project-lost".to_string());
                t
            })
            .collect();
        let again = plan(&mut store, &tabs, ReconcileMode::Live, &fs, &mut resolver, now());
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn lost_project_tab_whose_title_contradicts_its_folder_is_not_registered() {
        let fs = Fs {
            dirs: vec!["/home/h/code/y"],
        };
        let (_dir, mut store) = store();
        let mut resolver = LocalResolver::new();
        // A dead id, titled "X", sitting in folder "y": nothing says which
        // project this was, so the folder must not be synced as "X".
        let tabs = vec![tab(1, Some("ghost-x"), Some("X"), &["/home/h/code/y"])];
        let p = plan(&mut store, &tabs, ReconcileMode::Startup, &fs, &mut resolver, now());
        assert_eq!(p.relink, vec![(1, None)]);
        assert!(store.projects.is_empty(), "{:?}", store.projects);
        assert!(!p.store_changed);
        assert!(p.open.is_empty() && p.close.is_empty());

        // The unlinked tab stays plain on the next pass.
        let tabs = vec![tab(1, None, Some("X"), &["/home/h/code/y"])];
        let again = plan(&mut store, &tabs, ReconcileMode::Live, &fs, &mut resolver, now());
        assert!(again.is_empty(), "{again:?}");
        assert!(store.projects.is_empty());
    }

    #[test]
    fn lost_project_tab_whose_title_matches_its_folder_is_registered() {
        let fs = Fs {
            dirs: vec!["/home/h/code/z"],
        };
        let (_dir, mut store) = store();
        let mut resolver = LocalResolver::new();
        // Title "Z" and folder "z" agree (case-insensitively).
        let tabs = vec![tab(1, Some("project-z"), Some("Z"), &["/home/h/code/z"])];
        let p = plan(&mut store, &tabs, ReconcileMode::Startup, &fs, &mut resolver, now());
        assert!(p.relink.is_empty(), "the lost id is reused: {p:?}");
        assert!(p.store_changed);
        let record = store.get("project-z").expect("record re-registered under its old id");
        assert_eq!(record.name, "Z");
        assert_eq!(
            record.platform_path("linux"),
            Some(PathBuf::from("/home/h/code/z"))
        );
        assert_eq!(record.is_open(), Some(true));
        let again = plan(&mut store, &tabs, ReconcileMode::Startup, &fs, &mut resolver, now());
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn plain_tab_in_a_closed_projects_folder_stays_plain() {
        let fs = Fs {
            dirs: vec!["/home/h/code/a"],
        };
        let (_dir, mut store) = store();
        let mut a = Project::new("a", PathBuf::from("/home/h/code/a"));
        a.id = "a".into();
        a.set_open(false, now());
        store.add(a);
        let mut resolver = LocalResolver::new();
        // A tab the user just opened and `cd`d into the closed project.
        let plain = vec![tab(1, None, None, &["/home/h/code/a"])];
        for mode in [ReconcileMode::Live, ReconcileMode::Startup] {
            let p = plan(&mut store, &plain, mode, &fs, &mut resolver, now());
            assert!(p.close.is_empty() && p.relink.is_empty() && p.open.is_empty(), "{p:?}");
            assert_eq!(store.get("a").unwrap().is_open(), Some(false));
        }
        // A project tab of that record still closes.
        let linked = vec![tab(1, Some("a"), None, &["/home/h/code/a"])];
        let p = plan(&mut store, &linked, ReconcileMode::Live, &fs, &mut resolver, now());
        assert_eq!(p.close, vec![1]);
    }

    #[test]
    fn plain_tab_in_an_unflagged_projects_folder_neither_links_nor_opens_it() {
        let fs = Fs {
            dirs: vec!["/home/h/code/games"],
        };
        let (_dir, mut store) = store();
        let mut games = Project::new("games", PathBuf::from("/home/h/code/games"));
        games.id = "games".into();
        store.add(games);
        let mut resolver = LocalResolver::new();
        let tabs = vec![tab(1, None, None, &["/home/h/code/games"])];
        for mode in [ReconcileMode::Startup, ReconcileMode::Live] {
            let p = plan(&mut store, &tabs, mode, &fs, &mut resolver, now());
            assert!(p.relink.is_empty() && p.open.is_empty() && p.close.is_empty(), "{p:?}");
            assert_eq!(store.get("games").unwrap().is_open(), None, "must not open it everywhere");
        }
    }

    #[test]
    fn a_project_tab_adopts_an_unflagged_record_without_a_clock() {
        let fs = Fs {
            dirs: vec!["/home/h/code/q"],
        };
        let (_dir, mut store) = store();
        let mut q = Project::new("q", PathBuf::from("/home/h/code/q"));
        q.id = "q".into();
        q.created_at = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        store.add(q);
        let mut resolver = LocalResolver::new();
        let tabs = vec![tab(1, Some("q"), None, &["/home/h/code/q"])];
        let p = plan(&mut store, &tabs, ReconcileMode::Startup, &fs, &mut resolver, now());
        assert!(p.close.is_empty() && p.relink.is_empty(), "{p:?}");
        let adopted = store.get("q").unwrap();
        assert_eq!(adopted.is_open(), Some(true));
        assert_eq!(adopted.open_changed_at(), None);
        assert_eq!(adopted.updated_at(), None, "adopting must not make this copy the merge base");

        // The other machine closed it at 07:00, before this (stale) startup
        // at 08:00. Its close still wins, and so does its copy's name.
        let mut remote = store.registry_value();
        let r = &mut remote["projects"][0];
        r["name"] = serde_json::json!("q (renamed)");
        r["open"] = serde_json::json!(false);
        r["openChangedAt"] = serde_json::json!("2026-09-24T07:00:00Z");
        r["updatedAt"] = serde_json::json!("2026-09-24T07:00:00Z");
        let merged = mnemos_ide_core::registry::merge(&store.registry_value(), &remote, "linux");
        assert_eq!(merged["projects"][0]["open"], false);
        assert_eq!(merged["projects"][0]["name"], "q (renamed)");
    }

    #[test]
    fn a_busy_tab_of_a_closed_project_is_reported_as_kept() {
        let fs = Fs {
            dirs: vec!["/home/h/code/a"],
        };
        let (_dir, mut store) = store();
        let mut a = Project::new("a", PathBuf::from("/home/h/code/a"));
        a.id = "a".into();
        a.set_open(false, now());
        store.add(a);
        let mut resolver = LocalResolver::new();
        let mut busy = tab(1, Some("a"), None, &["/home/h/code/a"]);
        busy.safe_to_close = false;
        let p = plan(&mut store, &[busy.clone()], ReconcileMode::Live, &fs, &mut resolver, now());
        assert!(p.close.is_empty());
        assert_eq!(p.kept, vec![1]);
        // Again, now that the checkout is learned: still kept, and a plan
        // with only `kept` in it counts as empty.
        let p = plan(&mut store, &[busy.clone()], ReconcileMode::Live, &fs, &mut resolver, now());
        assert_eq!(p.kept, vec![1]);
        assert!(p.is_empty(), "kept is not an action: {p:?}");
        // Startup keeps it too. It used to close regardless, assuming a
        // restored tab runs nothing yet, but agent-restore can resume a
        // Claude session in a restored pane at launch.
        let p = plan(&mut store, &[busy.clone()], ReconcileMode::Startup, &fs, &mut resolver, now());
        assert!(p.close.is_empty(), "{p:?}");
        assert_eq!(p.kept, vec![1]);
        // Idle, it closes in either mode.
        busy.safe_to_close = true;
        for mode in [ReconcileMode::Startup, ReconcileMode::Live] {
            let p = plan(&mut store, &[busy.clone()], mode, &fs, &mut resolver, now());
            assert_eq!(p.close, vec![1], "{mode:?}");
            assert!(p.kept.is_empty());
        }
    }

    /// Linux machine whose checkouts all have `origin` = `o/<dir name>`.
    struct GitFs {
        dirs: Vec<&'static str>,
    }

    impl FsEnv for GitFs {
        fn platform(&self) -> &'static str {
            "linux"
        }
        fn is_dir(&self, path: &Path) -> bool {
            Fs { dirs: self.dirs.clone() }.is_dir(path)
        }
        fn home(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/home/h"))
        }
        fn git_remote(&self, dir: &Path) -> Option<String> {
            let s = dir.to_string_lossy();
            self.dirs
                .iter()
                .any(|d| same_path(d, &s, "linux"))
                .then(|| format!("o/{}", path_file_name(&s, "linux").unwrap_or_default()))
        }
        fn git_root(&self, cwd: &Path) -> Option<PathBuf> {
            let s = cwd.to_string_lossy();
            self.dirs
                .iter()
                .find(|d| same_path(d, &s, "linux"))
                .map(PathBuf::from)
        }
        fn list_dirs(&self, _dir: &Path) -> Vec<PathBuf> {
            Vec::new()
        }
    }

    #[test]
    fn learn_replaces_a_slot_whose_folder_is_gone() {
        // The checkout moved from ~/old/x to ~/work/x; the tab sits in the new one.
        let fs = GitFs {
            dirs: vec!["/home/h/work/x"],
        };
        let (_dir, mut store) = store();
        let mut x = Project::new("x", PathBuf::from("/home/h/old/x"));
        x.id = "x".into();
        x.set_git_repo("o/x");
        x.set_open(true, now());
        store.add(x);
        let mut resolver = LocalResolver::new();
        let tabs = vec![tab(1, Some("x"), None, &["/home/h/work/x"])];
        let p = plan(&mut store, &tabs, ReconcileMode::Live, &fs, &mut resolver, now());
        assert!(p.relink.is_empty() && p.open.is_empty() && p.close.is_empty(), "{p:?}");
        assert_eq!(
            store.get("x").unwrap().platform_path("linux"),
            Some(PathBuf::from("/home/h/work/x"))
        );
        assert!(plan(&mut store, &tabs, ReconcileMode::Live, &fs, &mut resolver, now()).is_empty());
    }
}
