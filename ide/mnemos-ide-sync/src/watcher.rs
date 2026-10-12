// SPDX-License-Identifier: AGPL-3.0-only
//! `notify`-based file watcher feeding sync events to the manager.
//!
//! Debouncing happens after the kernel-level watch fires
//! (`notify-debouncer-full`, 500 ms), then the manager applies its own
//! per-key debounce on top before pushing.
//!
//! Robustness rules (each one fixed a way sync used to go quiet):
//!
//! - **Every target is watched, present or not.** A target that does not
//!   exist yet (no `~/.claude/agents` on a fresh machine) is anchored on its
//!   nearest existing ancestor, watched non-recursively, and re-armed as soon
//!   as it appears; a synthetic event then announces it so its contents are
//!   pushed. A target that disappears falls back to its anchor the same way.
//!   A periodic recheck catches anything the anchor watch missed.
//! - **Files are watched through their parent directory** (except on macOS,
//!   where FSEvents follows a file path across atomic replaces), so an
//!   editor's write-to-temp-then-rename does not drop the watch.
//! - **Event paths come back in the configured spelling.** FSEvents reports
//!   canonical paths (`/private/var/...`, a symlinked `~/.claude` resolved),
//!   so both sides are canonicalized here and the event path is mapped back
//!   onto the target's configured path before the manager classifies it.
//! - **One bad path never takes the watcher down.** A path whose watch fails
//!   is logged once and retried on the periodic recheck; the others keep
//!   working. A path refused for want of kernel resources (inotify's
//!   `max_user_watches`, ENOSPC, EMFILE) or an unsupported backend moves to a
//!   polling watcher instead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::time::Duration;

use notify::{PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{
    new_debouncer_opt, DebounceEventResult, DebouncedEvent, Debouncer, FileIdMap,
};
use tokio::sync::mpsc as tokio_mpsc;

use crate::error::IdeError;

/// How long `notify-debouncer-full` waits for a path to settle.
const DEBOUNCE: Duration = Duration::from_millis(500);
/// How often pending targets and failed watches are rechecked even when no
/// event arrives.
const RECHECK_INTERVAL: Duration = Duration::from_secs(30);
/// The polling watcher's scan interval, for paths the native backend could
/// not take.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Most files announced when a directory target appears with content.
const MAX_APPEARED_FILES: usize = 2000;

/// One filesystem change observed by the watcher.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub paths: Vec<PathBuf>,
    pub is_remove: bool,
}

/// Whether a target is a single file or a directory tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    File,
    Dir,
}

/// A path the manager wants change events for, whether or not it exists yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchTarget {
    pub path: PathBuf,
    pub kind: TargetKind,
}

impl WatchTarget {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: TargetKind::File,
        }
    }

    pub fn dir(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: TargetKind::Dir,
        }
    }

    /// Whether `path` (in the configured spelling) is this target or, for a
    /// directory, inside it.
    fn covers(&self, path: &Path) -> bool {
        match self.kind {
            TargetKind::File => path == self.path,
            TargetKind::Dir => path.starts_with(&self.path),
        }
    }
}

/// Knobs, for tests.
#[derive(Debug, Clone)]
pub struct WatcherOptions {
    pub recheck_interval: Duration,
    pub poll_interval: Duration,
    /// Use the polling backend for everything (as when the native one cannot
    /// start).
    pub force_poll: bool,
}

impl Default for WatcherOptions {
    fn default() -> Self {
        Self {
            recheck_interval: RECHECK_INTERVAL,
            poll_interval: POLL_INTERVAL,
            force_poll: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Native,
    Poll,
}

/// Adding and removing kernel watches. A trait so the per-path degradation
/// logic is tested without provoking real kernel errors.
trait WatchOps: Send {
    fn watch(
        &mut self,
        path: &Path,
        mode: RecursiveMode,
        backend: Backend,
    ) -> Result<(), notify::Error>;
    fn unwatch(&mut self, path: &Path, backend: Backend);
    fn has_native(&self) -> bool;
}

/// The real backends: the platform's native watcher, and a polling one
/// created the first time a path needs it. Both feed the same channel.
struct Debouncers {
    native: Option<Debouncer<RecommendedWatcher, FileIdMap>>,
    poll: Option<Debouncer<PollWatcher, FileIdMap>>,
    raw_tx: mpsc::Sender<DebounceEventResult>,
    poll_interval: Duration,
}

impl WatchOps for Debouncers {
    fn watch(
        &mut self,
        path: &Path,
        mode: RecursiveMode,
        backend: Backend,
    ) -> Result<(), notify::Error> {
        match backend {
            Backend::Native => {
                let Some(d) = self.native.as_mut() else {
                    return Err(notify::Error::generic("no native watcher"));
                };
                d.watcher().watch(path, mode)?;
                d.cache().add_root(path, mode);
            }
            Backend::Poll => {
                if self.poll.is_none() {
                    let config = notify::Config::default().with_poll_interval(self.poll_interval);
                    self.poll = Some(new_debouncer_opt::<_, PollWatcher, FileIdMap>(
                        DEBOUNCE,
                        None,
                        self.raw_tx.clone(),
                        FileIdMap::new(),
                        config,
                    )?);
                }
                let Some(d) = self.poll.as_mut() else {
                    return Err(notify::Error::generic("no poll watcher"));
                };
                d.watcher().watch(path, mode)?;
                d.cache().add_root(path, mode);
            }
        }
        Ok(())
    }

    fn unwatch(&mut self, path: &Path, backend: Backend) {
        let removed = match backend {
            Backend::Native => self.native.as_mut().map(|d| {
                let r = d.watcher().unwatch(path);
                d.cache().remove_root(path);
                r
            }),
            Backend::Poll => self.poll.as_mut().map(|d| {
                let r = d.watcher().unwatch(path);
                d.cache().remove_root(path);
                r
            }),
        };
        if let Some(Err(e)) = removed {
            // A watch on a path that is gone may already be gone itself.
            tracing::debug!(target: "mnemos_ide_sync::watcher", path = ?path, "unwatch: {e}");
        }
    }

    fn has_native(&self) -> bool {
        self.native.is_some()
    }
}

/// Whether a watch failed for want of kernel resources or backend support,
/// which polling works around.
fn needs_poll_fallback(err: &notify::Error) -> bool {
    match &err.kind {
        notify::ErrorKind::MaxFilesWatch => true,
        notify::ErrorKind::Io(io) => {
            if io.kind() == std::io::ErrorKind::Unsupported {
                return true;
            }
            // ENOSPC (inotify watch limit), EMFILE/ENFILE (no descriptors),
            // ENOSYS (no inotify): the same numbers on Linux and macOS.
            #[cfg(unix)]
            {
                matches!(io.raw_os_error(), Some(28 | 24 | 23 | 38))
            }
            #[cfg(not(unix))]
            {
                false
            }
        }
        _ => false,
    }
}

/// The nearest existing directory above `path`, never the filesystem root.
fn anchor_of(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .skip(1)
        .find(|a| a.is_dir())
        .filter(|a| a.parent().is_some())
        .map(Path::to_path_buf)
}

/// `path` with symlinks resolved, as FSEvents reports it. For a path that
/// does not exist, its nearest existing ancestor is resolved and the rest
/// appended.
fn canonical_of(path: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c;
    }
    for anc in path.ancestors().skip(1) {
        if let Ok(c) = std::fs::canonicalize(anc) {
            if let Ok(rest) = path.strip_prefix(anc) {
                return c.join(rest);
            }
        }
    }
    path.to_path_buf()
}

/// One target as the planner sees it.
#[derive(Debug, Clone)]
struct PlanInput {
    path: PathBuf,
    kind: TargetKind,
    present: bool,
    canonical: PathBuf,
    anchor: Option<PathBuf>,
}

/// Platform behavior the plan depends on.
#[derive(Debug, Clone, Default)]
struct PlanRules {
    /// Watch an existing file directly instead of through its parent
    /// (FSEvents follows a path across atomic replaces).
    direct_file_watch: bool,
    /// Directories never watched non-recursively as an anchor or parent.
    /// FSEvents streams a whole tree even for a non-recursive watch, so on
    /// macOS the home directory is left to the periodic recheck.
    avoid: Vec<PathBuf>,
}

/// Which paths to watch, and how, for these targets. Pure.
fn plan_watches(targets: &[PlanInput], rules: &PlanRules) -> BTreeMap<PathBuf, RecursiveMode> {
    let mut plan: BTreeMap<PathBuf, RecursiveMode> = BTreeMap::new();
    let mut add = |p: PathBuf, mode: RecursiveMode| {
        if p.parent().is_none() {
            return; // never the filesystem root
        }
        if mode == RecursiveMode::NonRecursive && rules.avoid.contains(&p) {
            return;
        }
        let slot = plan.entry(p).or_insert(mode);
        if mode == RecursiveMode::Recursive {
            *slot = RecursiveMode::Recursive;
        }
    };
    for t in targets {
        match (t.kind, t.present) {
            (TargetKind::Dir, true) if t.path.is_dir() => {
                add(t.path.clone(), RecursiveMode::Recursive)
            }
            (TargetKind::Dir, true) => add(t.path.clone(), RecursiveMode::NonRecursive),
            (TargetKind::File, true) if rules.direct_file_watch => {
                add(t.path.clone(), RecursiveMode::NonRecursive)
            }
            (TargetKind::File, true) => {
                if let Some(parent) = t.path.parent() {
                    add(parent.to_path_buf(), RecursiveMode::NonRecursive);
                }
                // A symlinked file changes where it points, not where it is
                // linked, so its real parent is watched too.
                if t.canonical != t.path {
                    if let Some(parent) = t.canonical.parent() {
                        add(parent.to_path_buf(), RecursiveMode::NonRecursive);
                    }
                }
            }
            (_, false) => {
                if let Some(anchor) = &t.anchor {
                    add(anchor.clone(), RecursiveMode::NonRecursive);
                }
            }
        }
    }
    plan
}

/// Map an event path onto the configured spelling of the target it falls
/// under: the longest canonical prefix wins. Paths under no target come back
/// unchanged.
fn translate(path: &Path, targets: &[TargetState]) -> PathBuf {
    let mut best: Option<(&TargetState, usize)> = None;
    for t in targets {
        if t.canonical == t.target.path || !path.starts_with(&t.canonical) {
            continue;
        }
        let len = t.canonical.as_os_str().len();
        if best.is_none_or(|(_, l)| len > l) {
            best = Some((t, len));
        }
    }
    let Some((t, _)) = best else {
        return path.to_path_buf();
    };
    match path.strip_prefix(&t.canonical) {
        Ok(rest) if rest.as_os_str().is_empty() => t.target.path.clone(),
        Ok(rest) => t.target.path.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// Files under a directory that just appeared, so their contents reach the
/// manager even if they were written before the new watch was armed.
fn files_under(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= limit {
                return out;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
struct TargetState {
    target: WatchTarget,
    present: bool,
    canonical: PathBuf,
}

struct Inner {
    ops: Box<dyn WatchOps>,
    targets: Vec<TargetState>,
    rules: PlanRules,
    /// Watched path -> how and with which backend.
    active: BTreeMap<PathBuf, (RecursiveMode, Backend)>,
    /// Watched paths that go to the polling backend.
    poll_paths: HashSet<PathBuf>,
    /// Paths whose watch failed, with the error, so it is logged once and
    /// only retried on the periodic recheck.
    failed: HashMap<PathBuf, String>,
    force_poll: bool,
}

impl Inner {
    fn new(
        ops: Box<dyn WatchOps>,
        targets: &[WatchTarget],
        rules: PlanRules,
        force_poll: bool,
    ) -> Self {
        let targets = targets
            .iter()
            .map(|t| TargetState {
                target: t.clone(),
                present: t.path.exists(),
                canonical: canonical_of(&t.path),
            })
            .collect();
        Self {
            ops,
            targets,
            rules,
            active: BTreeMap::new(),
            poll_paths: HashSet::new(),
            failed: HashMap::new(),
            force_poll,
        }
    }

    fn backend_for(&self, path: &Path) -> Backend {
        if self.force_poll || !self.ops.has_native() || self.poll_paths.contains(path) {
            Backend::Poll
        } else {
            Backend::Native
        }
    }

    /// Recheck every target, adjust the watches, and return the synthetic
    /// events for targets that appeared since the last recheck.
    /// `retry_failed`: also retry watches that failed before.
    fn reconcile(&mut self, retry_failed: bool) -> Vec<ChangeEvent> {
        let mut appeared = Vec::new();
        for t in &mut self.targets {
            let present = t.target.path.exists();
            if present && !t.present {
                tracing::info!(
                    target: "mnemos_ide_sync::watcher",
                    path = ?t.target.path,
                    "watch target appeared; arming it",
                );
                appeared.push(ChangeEvent {
                    paths: vec![t.target.path.clone()],
                    is_remove: false,
                });
                if t.target.kind == TargetKind::Dir {
                    for f in files_under(&t.target.path, MAX_APPEARED_FILES) {
                        appeared.push(ChangeEvent {
                            paths: vec![f],
                            is_remove: false,
                        });
                    }
                }
            } else if !present && t.present {
                tracing::info!(
                    target: "mnemos_ide_sync::watcher",
                    path = ?t.target.path,
                    "watch target is gone; watching for it to come back",
                );
            }
            t.present = present;
            t.canonical = canonical_of(&t.target.path);
        }
        let inputs: Vec<PlanInput> = self
            .targets
            .iter()
            .map(|t| PlanInput {
                path: t.target.path.clone(),
                kind: t.target.kind,
                present: t.present,
                canonical: t.canonical.clone(),
                anchor: if t.present {
                    None
                } else {
                    anchor_of(&t.target.path)
                },
            })
            .collect();
        let plan = plan_watches(&inputs, &self.rules);
        self.apply(plan, retry_failed);
        appeared
    }

    fn apply(&mut self, plan: BTreeMap<PathBuf, RecursiveMode>, retry_failed: bool) {
        let desired: BTreeMap<PathBuf, (RecursiveMode, Backend)> = plan
            .into_iter()
            .map(|(p, m)| {
                let b = self.backend_for(&p);
                (p, (m, b))
            })
            .collect();
        let stale: Vec<(PathBuf, Backend)> = self
            .active
            .iter()
            .filter(|(p, cur)| desired.get(*p) != Some(*cur))
            .map(|(p, (_, b))| (p.clone(), *b))
            .collect();
        for (p, b) in stale {
            self.ops.unwatch(&p, b);
            self.active.remove(&p);
        }
        self.failed.retain(|p, _| desired.contains_key(p));
        for (p, (mode, backend)) in desired {
            if self.active.contains_key(&p) {
                continue;
            }
            if self.failed.contains_key(&p) && !retry_failed {
                continue;
            }
            match self.ops.watch(&p, mode, backend) {
                Ok(()) => {
                    if self.failed.remove(&p).is_some() {
                        tracing::info!(target: "mnemos_ide_sync::watcher", path = ?p, "watch recovered");
                    }
                    tracing::debug!(target: "mnemos_ide_sync::watcher", path = ?p, mode = ?mode, backend = ?backend, "watching");
                    self.active.insert(p, (mode, backend));
                }
                Err(e) if backend == Backend::Native && needs_poll_fallback(&e) => {
                    tracing::warn!(
                        target: "mnemos_ide_sync::watcher",
                        path = ?p,
                        "native watch refused ({e}); polling this path instead",
                    );
                    self.poll_paths.insert(p.clone());
                    match self.ops.watch(&p, mode, Backend::Poll) {
                        Ok(()) => {
                            self.failed.remove(&p);
                            self.active.insert(p, (mode, Backend::Poll));
                        }
                        Err(e) => self.note_failed(p, e),
                    }
                }
                Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => {
                    // Gone between the check and the watch: the next
                    // recheck sees it as missing and anchors it.
                    tracing::debug!(target: "mnemos_ide_sync::watcher", path = ?p, "vanished before it could be watched");
                }
                Err(e) => self.note_failed(p, e),
            }
        }
    }

    fn note_failed(&mut self, path: PathBuf, err: notify::Error) {
        let msg = err.to_string();
        if self.failed.get(&path) != Some(&msg) {
            tracing::warn!(
                target: "mnemos_ide_sync::watcher",
                path = ?path,
                "cannot watch ({msg}); other paths keep syncing, retrying later",
            );
        }
        self.failed.insert(path, msg);
    }

    /// A runtime error from a backend. Resource exhaustion moves the
    /// affected watches (all recursive native ones when the error names no
    /// path) to polling.
    fn handle_runtime_error(&mut self, err: &notify::Error) -> bool {
        tracing::warn!(target: "mnemos_ide_sync::watcher", "notify error: {err}");
        if !needs_poll_fallback(err) {
            return false;
        }
        let affected: Vec<PathBuf> = if err.paths.is_empty() {
            self.active
                .iter()
                .filter(|(_, (m, b))| *m == RecursiveMode::Recursive && *b == Backend::Native)
                .map(|(p, _)| p.clone())
                .collect()
        } else {
            self.active
                .iter()
                .filter(|(p, (_, b))| {
                    *b == Backend::Native && err.paths.iter().any(|e| e.starts_with(p))
                })
                .map(|(p, _)| p.clone())
                .collect()
        };
        let any = !affected.is_empty();
        self.poll_paths.extend(affected);
        any
    }

    /// The event in the configured spelling, if it touches any target.
    fn filter_event(&self, event: &notify::Event) -> Option<ChangeEvent> {
        let paths: Vec<PathBuf> = event
            .paths
            .iter()
            .map(|p| translate(p, &self.targets))
            .collect();
        let relevant = paths
            .iter()
            .any(|p| self.targets.iter().any(|t| t.target.covers(p)));
        relevant.then_some(ChangeEvent {
            paths,
            is_remove: matches!(event.kind, notify::EventKind::Remove(_)),
        })
    }
}

/// Watches the sync targets. Stays alive for the lifetime of the manager;
/// drop it to stop watching.
pub struct SyncWatcher {
    _inner: Arc<Mutex<Inner>>,
}

impl SyncWatcher {
    /// Watch `targets`, sending one [`ChangeEvent`] per relevant debounced
    /// event to `tx`. Fails only when neither the native nor the polling
    /// backend can start at all.
    pub fn new(
        targets: &[WatchTarget],
        tx: tokio_mpsc::UnboundedSender<ChangeEvent>,
    ) -> Result<Self, IdeError> {
        Self::with_options(targets, tx, WatcherOptions::default())
    }

    pub fn with_options(
        targets: &[WatchTarget],
        tx: tokio_mpsc::UnboundedSender<ChangeEvent>,
        options: WatcherOptions,
    ) -> Result<Self, IdeError> {
        let (raw_tx, raw_rx) = mpsc::channel::<DebounceEventResult>();
        let native = if options.force_poll {
            None
        } else {
            match new_debouncer_opt::<_, RecommendedWatcher, FileIdMap>(
                DEBOUNCE,
                None,
                raw_tx.clone(),
                FileIdMap::new(),
                notify::Config::default(),
            ) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::warn!(
                        target: "mnemos_ide_sync::watcher",
                        "native file watcher unavailable ({e}); polling every {:?}",
                        options.poll_interval,
                    );
                    None
                }
            }
        };
        let mut ops = Debouncers {
            native,
            poll: None,
            raw_tx,
            poll_interval: options.poll_interval,
        };
        if ops.native.is_none() {
            // Nothing works without a backend: fail here rather than run a
            // watcher that never fires.
            let config = notify::Config::default().with_poll_interval(options.poll_interval);
            ops.poll = Some(
                new_debouncer_opt::<_, PollWatcher, FileIdMap>(
                    DEBOUNCE,
                    None,
                    ops.raw_tx.clone(),
                    FileIdMap::new(),
                    config,
                )
                .map_err(|e| IdeError::Other(format!("file watcher init: {e}")))?,
            );
        }

        let rules = PlanRules {
            direct_file_watch: cfg!(target_os = "macos"),
            avoid: if cfg!(target_os = "macos") {
                dirs::home_dir().into_iter().collect()
            } else {
                Vec::new()
            },
        };
        let mut inner = Inner::new(Box::new(ops), targets, rules, options.force_poll);
        // Nothing has "appeared" yet: the initial push covers what exists.
        let _ = inner.reconcile(true);
        let inner = Arc::new(Mutex::new(inner));
        let weak = Arc::downgrade(&inner);
        let recheck = options.recheck_interval;
        std::thread::Builder::new()
            .name("mnemos-sync-watcher".into())
            .spawn(move || bridge(raw_rx, weak, tx, recheck))
            .map_err(|e| IdeError::Other(format!("watcher thread: {e}")))?;
        Ok(Self { _inner: inner })
    }
}

/// Forward debounced batches to the manager and keep the watches in line
/// with the filesystem. Ends when the watcher is dropped (every sender of
/// `raw_rx` lives in it) or the manager stops listening.
fn bridge(
    raw_rx: mpsc::Receiver<DebounceEventResult>,
    inner: Weak<Mutex<Inner>>,
    tx: tokio_mpsc::UnboundedSender<ChangeEvent>,
    recheck: Duration,
) {
    loop {
        let batch = match raw_rx.recv_timeout(recheck) {
            Ok(batch) => Some(batch),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let Some(strong) = inner.upgrade() else {
            return;
        };
        let mut st = strong.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = Vec::new();
        let retry = match batch {
            None => true,
            Some(Ok(events)) => {
                tracing::debug!(target: "mnemos_ide_sync::watcher", count = events.len(), "debounced batch");
                for ev in events {
                    let DebouncedEvent { event, .. } = ev;
                    if let Some(change) = st.filter_event(&event) {
                        out.push(change);
                    }
                }
                false
            }
            Some(Err(errs)) => {
                let mut moved = false;
                for e in &errs {
                    moved |= st.handle_runtime_error(e);
                }
                moved
            }
        };
        out.extend(st.reconcile(retry));
        drop(st);
        drop(strong);
        for change in out {
            if tx.send(change).is_err() {
                return; // receiver dropped: shutdown
            }
        }
    }
}

/// Helper: does `child` lie inside `parent` (or equal it)?
pub fn path_starts_with(child: &Path, parent: &Path) -> bool {
    child == parent || child.starts_with(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(path: &str, kind: TargetKind, present: bool, anchor: Option<&str>) -> PlanInput {
        PlanInput {
            path: PathBuf::from(path),
            kind,
            present,
            canonical: PathBuf::from(path),
            anchor: anchor.map(PathBuf::from),
        }
    }

    #[test]
    fn missing_targets_are_anchored_on_their_nearest_existing_ancestor() {
        let plan = plan_watches(
            &[input(
                "/h/.claude/agents",
                TargetKind::Dir,
                false,
                Some("/h/.claude"),
            )],
            &PlanRules::default(),
        );
        assert_eq!(
            plan,
            BTreeMap::from([(PathBuf::from("/h/.claude"), RecursiveMode::NonRecursive)])
        );
    }

    #[test]
    fn files_are_watched_through_their_parent_unless_direct() {
        let t = [input("/h/.claude/CLAUDE.md", TargetKind::File, true, None)];
        let parent = plan_watches(&t, &PlanRules::default());
        assert_eq!(
            parent,
            BTreeMap::from([(PathBuf::from("/h/.claude"), RecursiveMode::NonRecursive)])
        );
        let direct = plan_watches(
            &t,
            &PlanRules {
                direct_file_watch: true,
                avoid: vec![],
            },
        );
        assert!(direct.contains_key(Path::new("/h/.claude/CLAUDE.md")));
    }

    #[test]
    fn a_symlinked_file_also_watches_its_real_parent() {
        let mut t = input("/h/.claude/CLAUDE.md", TargetKind::File, true, None);
        t.canonical = PathBuf::from("/dots/CLAUDE.md");
        let plan = plan_watches(&[t], &PlanRules::default());
        assert!(plan.contains_key(Path::new("/h/.claude")));
        assert!(plan.contains_key(Path::new("/dots")));
    }

    #[test]
    fn recursive_wins_and_avoided_or_root_anchors_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        std::fs::create_dir(&dir).unwrap();
        let dir_s = dir.to_string_lossy().into_owned();
        let plan = plan_watches(
            &[
                input(&dir_s, TargetKind::Dir, true, None),
                input(
                    &format!("{dir_s}/x/missing"),
                    TargetKind::Dir,
                    false,
                    Some(&dir_s),
                ),
                input("/h/.claude.json", TargetKind::File, false, Some("/h")),
                input("/nowhere", TargetKind::Dir, false, Some("/")),
            ],
            &PlanRules {
                direct_file_watch: false,
                avoid: vec![PathBuf::from("/h")],
            },
        );
        assert_eq!(plan, BTreeMap::from([(dir, RecursiveMode::Recursive)]));
    }

    fn state(path: &str, canonical: &str, kind: TargetKind) -> TargetState {
        TargetState {
            target: WatchTarget {
                path: PathBuf::from(path),
                kind,
            },
            present: true,
            canonical: PathBuf::from(canonical),
        }
    }

    #[test]
    fn canonical_event_paths_map_back_to_the_configured_spelling() {
        let targets = [
            state("/var/t/claude", "/private/var/t/claude", TargetKind::Dir),
            state("/var/t/claude/agents", "/data/agents", TargetKind::Dir),
            state("/h/x.json", "/h/x.json", TargetKind::File),
        ];
        assert_eq!(
            translate(Path::new("/private/var/t/claude/rules/a.md"), &targets),
            PathBuf::from("/var/t/claude/rules/a.md")
        );
        assert_eq!(
            translate(Path::new("/data/agents/b.md"), &targets),
            PathBuf::from("/var/t/claude/agents/b.md")
        );
        assert_eq!(
            translate(Path::new("/private/var/t/claude"), &targets),
            PathBuf::from("/var/t/claude")
        );
        assert_eq!(
            translate(Path::new("/elsewhere/z"), &targets),
            PathBuf::from("/elsewhere/z")
        );
    }

    #[test]
    fn only_events_under_a_target_are_forwarded() {
        let inner = Inner {
            ops: Box::new(FakeOps::default()),
            targets: vec![
                state("/h/.claude/agents", "/h/.claude/agents", TargetKind::Dir),
                state(
                    "/h/.claude/CLAUDE.md",
                    "/h/.claude/CLAUDE.md",
                    TargetKind::File,
                ),
            ],
            rules: PlanRules::default(),
            active: BTreeMap::new(),
            poll_paths: HashSet::new(),
            failed: HashMap::new(),
            force_poll: false,
        };
        let ev = |p: &str| notify::Event::new(notify::EventKind::Any).add_path(PathBuf::from(p));
        assert!(inner.filter_event(&ev("/h/.claude/agents/a.md")).is_some());
        assert!(inner.filter_event(&ev("/h/.claude/CLAUDE.md")).is_some());
        assert!(inner.filter_event(&ev("/h/.claude/other.md")).is_none());
        // A rename from a temp file onto the target counts.
        let rename = notify::Event::new(notify::EventKind::Any)
            .add_path(PathBuf::from("/h/.claude/CLAUDE.md.tmp"))
            .add_path(PathBuf::from("/h/.claude/CLAUDE.md"));
        assert!(inner.filter_event(&rename).is_some());
        let remove = notify::Event::new(notify::EventKind::Remove(notify::event::RemoveKind::File))
            .add_path(PathBuf::from("/h/.claude/agents/a.md"));
        assert!(inner.filter_event(&remove).unwrap().is_remove);
    }

    /// Records watches; fails the paths it is told to.
    #[derive(Default)]
    struct FakeOps {
        fail: HashMap<PathBuf, fn() -> notify::Error>,
        log: Arc<Mutex<Vec<(PathBuf, Backend)>>>,
    }

    impl WatchOps for FakeOps {
        fn watch(
            &mut self,
            path: &Path,
            _mode: RecursiveMode,
            backend: Backend,
        ) -> Result<(), notify::Error> {
            if backend == Backend::Native {
                if let Some(f) = self.fail.get(path) {
                    return Err(f());
                }
            }
            self.log.lock().unwrap().push((path.to_path_buf(), backend));
            Ok(())
        }
        fn unwatch(&mut self, _path: &Path, _backend: Backend) {}
        fn has_native(&self) -> bool {
            true
        }
    }

    #[test]
    fn one_failing_path_degrades_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let c = tmp.path().join("c");
        for d in [&a, &b, &c] {
            std::fs::create_dir(d).unwrap();
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut fail: HashMap<PathBuf, fn() -> notify::Error> = HashMap::new();
        fail.insert(a.clone(), || {
            notify::Error::new(notify::ErrorKind::MaxFilesWatch)
        });
        fail.insert(b.clone(), || {
            notify::Error::io(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        let ops = FakeOps {
            fail,
            log: log.clone(),
        };
        let targets = [
            WatchTarget::dir(&a),
            WatchTarget::dir(&b),
            WatchTarget::dir(&c),
        ];
        let mut inner = Inner::new(Box::new(ops), &targets, PlanRules::default(), false);
        inner.reconcile(true);

        assert_eq!(
            inner.active.get(&a),
            Some(&(RecursiveMode::Recursive, Backend::Poll))
        );
        assert_eq!(
            inner.active.get(&c),
            Some(&(RecursiveMode::Recursive, Backend::Native))
        );
        assert!(!inner.active.contains_key(&b));
        assert!(inner.failed.contains_key(&b));

        // Not retried on every event, only on the periodic recheck.
        let before = log.lock().unwrap().len();
        inner.reconcile(false);
        assert_eq!(log.lock().unwrap().len(), before);
    }

    #[test]
    fn resource_errors_poll_and_others_do_not() {
        assert!(needs_poll_fallback(&notify::Error::new(
            notify::ErrorKind::MaxFilesWatch
        )));
        assert!(needs_poll_fallback(&notify::Error::io(
            std::io::Error::from(std::io::ErrorKind::Unsupported)
        )));
        #[cfg(unix)]
        assert!(needs_poll_fallback(&notify::Error::io(
            std::io::Error::from_raw_os_error(28)
        )));
        assert!(!needs_poll_fallback(&notify::Error::io(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied)
        )));
        assert!(!needs_poll_fallback(&notify::Error::path_not_found()));
    }

    #[test]
    fn a_target_that_appears_is_announced_with_its_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents");
        let ops = FakeOps::default();
        let mut inner = Inner::new(
            Box::new(ops),
            &[WatchTarget::dir(&dir)],
            PlanRules::default(),
            false,
        );
        assert!(inner.reconcile(true).is_empty());
        // Anchored on the existing parent meanwhile.
        assert!(inner.active.keys().any(|p| *p == tmp.path()));

        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.md"), b"a").unwrap();
        let events = inner.reconcile(false);
        let paths: Vec<PathBuf> = events.iter().flat_map(|e| e.paths.clone()).collect();
        assert_eq!(paths.first(), Some(&dir));
        assert!(paths.contains(&dir.join("sub/a.md")), "{paths:?}");
        assert!(inner.active.contains_key(&dir));
        assert!(inner.reconcile(false).is_empty(), "announced once");

        std::fs::remove_dir_all(&dir).unwrap();
        inner.reconcile(false);
        assert!(
            !inner.active.contains_key(&dir),
            "re-anchored after removal"
        );
    }

    async fn next_event_for(
        rx: &mut tokio_mpsc::UnboundedReceiver<ChangeEvent>,
        want: &Path,
    ) -> ChangeEvent {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let ev = rx.recv().await.expect("watcher alive");
                if ev.paths.iter().any(|p| p.starts_with(want)) {
                    return ev;
                }
            }
        })
        .await
        .expect("an event in time")
    }

    async fn rearms_late_created_dir(force_poll: bool) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("claude").join("agents");
        std::fs::create_dir_all(tmp.path().join("claude")).unwrap();
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let _w = SyncWatcher::with_options(
            &[WatchTarget::dir(&dir)],
            tx,
            WatcherOptions {
                recheck_interval: Duration::from_millis(300),
                poll_interval: Duration::from_millis(200),
                force_poll,
            },
        )
        .unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        // Paths come back in the configured spelling even where the OS
        // reports canonical ones (macOS temp dirs live under /private).
        let ev = next_event_for(&mut rx, &dir).await;
        assert!(ev.paths.iter().any(|p| p.starts_with(&dir)), "{ev:?}");

        // Re-armed: a file written inside afterwards is seen too.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let file = dir.join("a.md");
        std::fs::write(&file, b"x").unwrap();
        next_event_for(&mut rx, &file).await;
    }

    #[tokio::test]
    async fn native_watcher_rearms_a_late_created_dir() {
        rearms_late_created_dir(false).await;
    }

    #[tokio::test]
    async fn poll_watcher_rearms_a_late_created_dir() {
        rearms_late_created_dir(true).await;
    }
}
