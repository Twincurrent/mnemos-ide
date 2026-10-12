// SPDX-License-Identifier: AGPL-3.0-only
//! Finding a project's checkout on *this* machine.
//!
//! The registry is shared by machines with different filesystems: the Mac has
//! `/Users/alice/code/tcbv2`, the PC has `C:\code\mnemos`, and a record
//! registered on one says nothing about the other. This module finds the local
//! checkout (by git remote, then by folder name) and writes it into
//! `paths.<platform>` so the next lookup is direct.
//!
//! All filesystem access goes through [`FsEnv`] so tests can describe each
//! machine as data. Git metadata is read straight from `.git/config`: no git
//! CLI, which would mean a process spawn per scanned directory.
//!
//! Path strings are handled with `mnemos_ide_core::registry`'s platform-aware
//! helpers rather than `Path::join`, so a test running on Windows can act out
//! the Mac (and the reverse) with the other machine's separators.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

// `std::time::Instant` on native targets; the workspace lint wants the
// wasm-safe spelling.
use instant::Instant;
use mnemos_ide_core::registry::{
    self, current_platform, git_repo_key, is_fs_root, path_file_name, path_join, path_parent,
    same_path,
};

use crate::store::ProjectStore;

/// Directory names never worth descending into when looking for checkouts.
/// `Library` (macOS) and `AppData` (Windows) are huge and never hold repos.
const SKIP_DIRS: [&str; 4] = ["node_modules", "target", "Library", "AppData"];

/// The OS's own folders directly under the home directory. Discovery never
/// lists them: on macOS, reading Desktop, Documents, Downloads, Pictures,
/// Movies or Music raises a privacy prompt for every one of them ("Mnemos
/// would like to access files in your Documents folder") at app start, and
/// the rest (and Windows' profile folders, junctions and OneDrive) are large
/// and never where checkouts live. Matched case-insensitively, and only
/// under home: a repo called `music` inside `~/code` is still found.
const HOME_SKIP_DIRS: &[&str] = &[
    // macOS
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Movies",
    "Music",
    "Pictures",
    "Public",
    // Windows profile folders and the legacy junctions inside a profile.
    "AppData",
    "Application Data",
    "Contacts",
    "Cookies",
    "Favorites",
    "Links",
    "Local Settings",
    "My Documents",
    "NetHood",
    "OneDrive",
    "PrintHood",
    "Recent",
    "Saved Games",
    "Searches",
    "SendTo",
    "Start Menu",
    "Templates",
    "Videos",
    "3D Objects",
    "IntelGraphicsProfiles",
    "MicrosoftEdgeBackups",
];

/// Everything reconcile and discovery need from the filesystem.
pub trait FsEnv {
    /// The `paths` slot this machine owns. Real machines use the build's
    /// platform; tests override it to act as the other machine.
    fn platform(&self) -> &'static str {
        current_platform()
    }
    fn is_dir(&self, path: &Path) -> bool;
    fn home(&self) -> Option<PathBuf>;
    /// `origin` of the checkout at `dir`, already normalized to `owner/repo`
    /// (`registry::normalize_git_remote`). `None` when `dir` is not a git
    /// checkout or has no `origin`.
    fn git_remote(&self, dir: &Path) -> Option<String>;
    /// Nearest directory at or above `cwd` that contains `.git`, never
    /// looking at the home directory or a filesystem root.
    fn git_root(&self, cwd: &Path) -> Option<PathBuf>;
    /// Immediate subdirectories of `dir`.
    fn list_dirs(&self, dir: &Path) -> Vec<PathBuf>;
}

/// The real filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl FsEnv for RealFs {
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn home(&self) -> Option<PathBuf> {
        dirs::home_dir()
    }

    fn git_remote(&self, dir: &Path) -> Option<String> {
        read_origin_url(dir).and_then(|url| registry::normalize_git_remote(&url))
    }

    fn git_root(&self, cwd: &Path) -> Option<PathBuf> {
        let platform = current_platform();
        let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
        let mut cur = Some(cwd.to_path_buf());
        while let Some(dir) = cur {
            let s = dir.to_string_lossy();
            // A dotfiles repo in $HOME must not turn every tab into "home".
            if home.as_deref().is_some_and(|h| same_path(&s, h, platform))
                || dir.parent().is_none()
                || is_fs_root(&s, platform)
            {
                return None;
            }
            if dir.join(".git").exists() {
                return Some(dir);
            }
            cur = dir.parent().map(Path::to_path_buf);
        }
        None
    }

    fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    }
}

/// The raw `url` of `[remote "origin"]` for the checkout at `repo_dir`.
///
/// Handles a `.git` directory, and a `.git` *file* (`gitdir: <path>`, as in
/// worktrees and submodules) followed by that gitdir's `commondir`, which is
/// where a worktree's shared `config` lives.
pub fn read_origin_url(repo_dir: &Path) -> Option<String> {
    let dot_git = repo_dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let content = std::fs::read_to_string(&dot_git).ok()?;
        let target = content
            .lines()
            .find_map(|l| l.trim().strip_prefix("gitdir:"))?
            .trim();
        let target = PathBuf::from(target);
        if target.is_absolute() {
            target
        } else {
            repo_dir.join(target)
        }
    } else {
        return None;
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(s) => {
            let c = PathBuf::from(s.trim());
            if c.is_absolute() {
                c
            } else {
                git_dir.join(c)
            }
        }
        Err(_) => git_dir,
    };
    let config = std::fs::read_to_string(common_dir.join("config")).ok()?;
    parse_origin_url(&config)
}

/// `url` of `[remote "origin"]` in a git config file's text.
pub fn parse_origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for raw in config.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            let inner = line.trim_start_matches('[').trim_end_matches(']').trim();
            let mut parts = inner.splitn(2, char::is_whitespace);
            let section = parts.next().unwrap_or("");
            let sub = parts.next().unwrap_or("").trim().trim_matches('"');
            in_origin = section.eq_ignore_ascii_case("remote") && sub == "origin";
            continue;
        }
        if !in_origin || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("url") {
                let value = value.trim().trim_matches('"').trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The project directory a tab sitting in `cwd` belongs to: the nearest
/// enclosing git checkout, else `cwd` itself. The home directory, a
/// filesystem root, or anything above home is "no root" (`None`): a tab
/// there says nothing about which project it is. A `cwd` that does not exist
/// here is also `None`.
pub fn tab_root(env: &dyn FsEnv, cwd: &Path) -> Option<PathBuf> {
    let platform = env.platform();
    // A verbatim `\\?\C:\x` cwd would be stored as a checkout no other code
    // (nor Claude Code's project slug) spells that way.
    let plain;
    let cwd = if platform == "win32" {
        plain = PathBuf::from(registry::strip_verbatim(&path_string(cwd)));
        plain.as_path()
    } else {
        cwd
    };
    let cwd_s = path_string(cwd);
    if cwd_s.trim().is_empty() || is_fs_root(&cwd_s, platform) {
        return None;
    }
    let home = env.home().map(|h| path_string(&h));
    let is_home_or_above = |p: &str| {
        home.as_deref().is_some_and(|h| {
            let mut cur = Some(h.to_string());
            while let Some(c) = cur {
                if same_path(&c, p, platform) {
                    return true;
                }
                cur = path_parent(&c, platform);
            }
            false
        })
    };
    if is_home_or_above(&cwd_s) || !env.is_dir(cwd) {
        return None;
    }
    match env.git_root(cwd) {
        Some(root) => {
            let root_s = path_string(&root);
            if is_home_or_above(&root_s) || is_fs_root(&root_s, platform) {
                Some(cwd.to_path_buf())
            } else {
                Some(root)
            }
        }
        None => Some(cwd.to_path_buf()),
    }
}

/// One directory seen by the discovery scan.
#[derive(Debug, Clone)]
struct ScannedDir {
    path: String,
    name: String,
    remote: Option<String>,
}

/// How long one discovery scan is trusted. A repo cloned after launch is
/// found by the first reconcile or transcript pass after this, without a
/// restart; the walk (one `.git/config` read per folder) stays rare.
pub const SCAN_TTL: Duration = Duration::from_secs(5 * 60);

/// A monotonic clock. Injectable so tests can move time.
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Finds and remembers each project's checkout on this machine.
///
/// Caches the discovery scan for [`SCAN_TTL`]: walking the candidate roots
/// and reading every `.git/config` is the expensive part, and checkouts
/// rarely appear while the app runs. Call [`Self::invalidate`] to rescan at
/// once (e.g. after the user adds a project from a new location).
pub struct LocalResolver {
    /// The scan and when it was taken.
    scan: Option<(Instant, Vec<ScannedDir>)>,
    clock: Clock,
}

impl Default for LocalResolver {
    fn default() -> Self {
        Self::with_clock(Instant::now)
    }
}

impl std::fmt::Debug for LocalResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalResolver")
            .field("scanned_dirs", &self.scan.as_ref().map(|(_, dirs)| dirs.len()))
            .finish()
    }
}

impl LocalResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// A resolver that reads the time from `clock` rather than
    /// `Instant::now` (tests).
    pub fn with_clock(clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self {
            scan: None,
            clock: Arc::new(clock),
        }
    }

    /// Drop the cached scan; the next discovery walks the disk again.
    pub fn invalidate(&mut self) {
        self.scan = None;
    }

    /// The checkout of record `id` (following `replacedBy`) on this machine.
    ///
    /// The record's own path for this platform wins when it exists on disk.
    /// Otherwise discovery scans the usual code folders for a checkout of the
    /// record's `gitRepo`, then for a folder with the record's name, never
    /// taking a folder another record already calls its own here. On success
    /// the answer is stored as `paths.<platform>` and a missing `gitRepo` is
    /// filled from the folder's remote.
    pub fn resolve_local_path(
        &mut self,
        store: &mut ProjectStore,
        id: &str,
        env: &dyn FsEnv,
    ) -> Option<PathBuf> {
        let platform = env.platform();
        let live = store.resolve_id(id)?;
        let record = store.get(&live)?;
        if let Some(p) = record.platform_path(platform) {
            if env.is_dir(&p) {
                let remote = if record.git_repo().is_none() {
                    env.git_remote(&p)
                } else {
                    None
                };
                persist(store, &live, platform, &path_string(&p), remote.as_deref());
                return Some(p);
            }
        }
        let found = self.discover(store, &live, env)?;
        persist(store, &live, platform, &found.path, found.remote.as_deref());
        Some(PathBuf::from(found.path))
    }

    fn discover(&mut self, store: &ProjectStore, id: &str, env: &dyn FsEnv) -> Option<ScannedDir> {
        let platform = env.platform();
        let record = store.get(id)?;
        let name = record.name.clone();
        let repo_key = record.git_repo().map(|g| git_repo_key(&g));
        let claimed: Vec<String> = store
            .projects
            .iter()
            .filter(|p| p.id != record.id)
            .filter_map(|p| p.platform_path(platform))
            .map(|p| path_string(&p))
            .collect();
        // Other records of the same repo with no checkout here yet, by name.
        // Two records can share a repo (TwinCurrent-Build and TCB are both
        // `bob/TCB`, with different Mac checkouts); records resolve
        // one at a time in registry order, so without this the first one
        // would take the folder named after the second.
        let siblings: Vec<String> = match &repo_key {
            Some(key) => store
                .projects
                .iter()
                .filter(|p| p.id != record.id && p.platform_path(platform).is_none())
                .filter(|p| p.git_repo().is_some_and(|g| git_repo_key(&g) == *key))
                .map(|p| p.name.clone())
                .collect(),
            None => Vec::new(),
        };
        let scan = self.ensure_scan(store, env);
        let free = |d: &&ScannedDir| !claimed.iter().any(|c| same_path(c, &d.path, platform));
        let name_matches = |d: &ScannedDir| d.name.eq_ignore_ascii_case(&name);
        let sibling_named = |d: &&ScannedDir| {
            !name_matches(d) && siblings.iter().any(|s| d.name.eq_ignore_ascii_case(s))
        };

        if let Some(key) = &repo_key {
            let mut by_repo: Vec<&ScannedDir> = scan
                .iter()
                .filter(|d| d.remote.as_deref().map(git_repo_key).as_ref() == Some(key))
                .filter(free)
                .filter(|d| !sibling_named(d))
                .collect();
            by_repo.sort_by(|a, b| {
                (!name_matches(a), a.path.len(), &a.path).cmp(&(!name_matches(b), b.path.len(), &b.path))
            });
            if let Some(d) = by_repo.first() {
                return Some((*d).clone());
            }
        }
        let mut by_name: Vec<&ScannedDir> = scan
            .iter()
            .filter(|d| name_matches(d) && (repo_key.is_none() || d.remote.is_none()))
            .filter(free)
            .collect();
        by_name.sort_by(|a, b| (a.path.len(), &a.path).cmp(&(b.path.len(), &b.path)));
        by_name.first().map(|d| (*d).clone())
    }

    fn ensure_scan(&mut self, store: &ProjectStore, env: &dyn FsEnv) -> &[ScannedDir] {
        let now = (self.clock)();
        let fresh = self
            .scan
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) < SCAN_TTL);
        if !fresh {
            self.scan = Some((now, scan_candidates(store, env)));
        }
        self.scan.as_ref().map_or(&[], |(_, dirs)| dirs.as_slice())
    }
}

/// Folder names directly under home that usually hold checkouts.
const HOME_CODE_DIRS: [&str; 8] = [
    "code",
    "Code",
    "Development",
    "development",
    "dev",
    "projects",
    "src",
    "repos",
];

/// Drive-root code folders on Windows.
const WIN32_CODE_ROOTS: [&str; 5] = [
    "C:\\code",
    "C:\\development",
    "C:\\dev",
    "C:\\src",
    "C:\\repos",
];

/// The usual code folders that exist on this machine: on Windows the
/// drive-root ones first, then the [`HOME_CODE_DIRS`] under home. Never home
/// itself. Folders that differ only in case (`code` and `Code` on a
/// case-insensitive disk) appear once.
pub fn code_roots(env: &dyn FsEnv) -> Vec<String> {
    let platform = env.platform();
    let mut roots: Vec<String> = Vec::new();
    if platform == "win32" {
        roots.extend(WIN32_CODE_ROOTS.iter().map(|r| r.to_string()));
    }
    if let Some(home) = env.home() {
        let home = path_string(&home);
        roots.extend(HOME_CODE_DIRS.iter().map(|sub| path_join(&home, sub, platform)));
    }
    let mut out: Vec<String> = Vec::new();
    for r in roots {
        if out.iter().any(|o| same_path(o, &r, platform)) {
            continue;
        }
        if env.is_dir(Path::new(&r)) {
            out.push(r);
        }
    }
    out
}

/// Where checkouts usually live: next to every checkout this machine already
/// knows about, the usual code folders ([`code_roots`]), and home itself.
fn candidate_roots(store: &ProjectStore, env: &dyn FsEnv) -> Vec<String> {
    let platform = env.platform();
    let mut roots: Vec<String> = Vec::new();
    for p in &store.projects {
        if let Some(local) = p.platform_path(platform) {
            if env.is_dir(&local) {
                if let Some(parent) = path_parent(&path_string(&local), platform) {
                    roots.push(parent);
                }
            }
        }
    }
    roots.extend(code_roots(env));
    if let Some(home) = env.home() {
        roots.push(path_string(&home));
    }
    let mut out: Vec<String> = Vec::new();
    for r in roots {
        if is_fs_root(&r, platform) || out.iter().any(|o| same_path(o, &r, platform)) {
            continue;
        }
        if env.is_dir(Path::new(&r)) {
            out.push(r);
        }
    }
    out
}

fn skip_dir(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('$') || SKIP_DIRS.contains(&name)
}

/// One of the OS folders under home that discovery must not open (see
/// [`HOME_SKIP_DIRS`]). `OneDrive` also covers `OneDrive - <Org>`.
fn skip_home_child(name: &str) -> bool {
    HOME_SKIP_DIRS.iter().any(|s| s.eq_ignore_ascii_case(name))
        || name.to_ascii_lowercase().starts_with("onedrive")
}

/// Children and grandchildren of every candidate root, with their remotes.
fn scan_candidates(store: &ProjectStore, env: &dyn FsEnv) -> Vec<ScannedDir> {
    let platform = env.platform();
    let home = env.home().map(|h| path_string(&h));
    let mut out: Vec<ScannedDir> = Vec::new();
    // `under_home`: `dir` sits directly in the home directory, where the OS
    // folders live. Checked before anything reads `dir` (its `.git`, its
    // children), since touching a protected folder is what raises the prompt.
    let push = |out: &mut Vec<ScannedDir>, dir: &Path, under_home: bool| -> bool {
        let path = path_string(dir);
        let Some(name) = path_file_name(&path, platform) else {
            return false;
        };
        if skip_dir(&name) || (under_home && skip_home_child(&name)) {
            return false;
        }
        if !out.iter().any(|d| same_path(&d.path, &path, platform)) {
            let remote = env.git_remote(dir);
            out.push(ScannedDir { path, name, remote });
        }
        true
    };
    for root in candidate_roots(store, env) {
        let root_is_home = home.as_deref().is_some_and(|h| same_path(h, &root, platform));
        for child in env.list_dirs(Path::new(&root)) {
            if !push(&mut out, &child, root_is_home) {
                continue;
            }
            for grandchild in env.list_dirs(&child) {
                push(&mut out, &grandchild, false);
            }
        }
    }
    out
}

/// Store a resolved checkout: `paths.<platform>` when it differs (an existing
/// slot that only differs in case or a trailing separator is left alone), and
/// `gitRepo` when the record has none.
fn persist(store: &mut ProjectStore, id: &str, platform: &str, path: &str, remote: Option<&str>) {
    let Some(project) = store.get_mut(id) else {
        return;
    };
    let current = project.paths().remove(platform);
    if !current.is_some_and(|c| same_path(&c, path, platform)) {
        project.set_platform_path(platform, Path::new(path));
    }
    if project.git_repo().is_none() {
        if let Some(r) = remote.filter(|r| !r.trim().is_empty()) {
            project.set_git_repo(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_origin_from_git_config() {
        let config = r#"
[core]
	repositoryformatversion = 0
[remote "upstream"]
	url = https://github.com/warpdotdev/warp.git
[remote "origin"]
	url = git@github.com:acme/TCB.git
	fetch = +refs/heads/*:refs/remotes/origin/*
[branch "main"]
	remote = origin
"#;
        assert_eq!(
            parse_origin_url(config).as_deref(),
            Some("git@github.com:acme/TCB.git")
        );
        assert_eq!(parse_origin_url("[core]\n\tbare = false\n"), None);
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn real_fs_reads_git_dir_and_worktree_file() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        write(
            &main.join(".git").join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/acme/Admin.git\n",
        );
        assert_eq!(RealFs.git_remote(&main).as_deref(), Some("acme/Admin"));

        // Worktree: `.git` is a file pointing into main's worktrees dir, and
        // that dir's `commondir` points back at main's `.git`.
        let wt_git = main.join(".git").join("worktrees").join("wt");
        write(&wt_git.join("commondir"), "../..\n");
        let wt = tmp.path().join("wt");
        write(&wt.join(".git"), &format!("gitdir: {}\n", wt_git.display()));
        assert_eq!(RealFs.git_remote(&wt).as_deref(), Some("acme/Admin"));

        // Relative gitdir.
        let rel = tmp.path().join("rel");
        write(&rel.join(".git"), "gitdir: ../main/.git\n");
        assert_eq!(RealFs.git_remote(&rel).as_deref(), Some("acme/Admin"));

        let plain = tmp.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(RealFs.git_remote(&plain), None);
    }

    /// A Mac home described as data that records every directory discovery
    /// lists or reads a `.git` from.
    struct RecordingFs {
        /// `(path, origin remote)`; ancestors exist implicitly.
        dirs: Vec<(&'static str, Option<&'static str>)>,
        touched: std::cell::RefCell<Vec<String>>,
    }

    impl RecordingFs {
        fn exists(&self, path: &str) -> bool {
            self.dirs.iter().any(|(d, _)| {
                let mut cur = Some(d.to_string());
                while let Some(c) = cur {
                    if same_path(&c, path, "darwin") {
                        return true;
                    }
                    cur = path_parent(&c, "darwin");
                }
                false
            })
        }
    }

    impl FsEnv for RecordingFs {
        fn platform(&self) -> &'static str {
            "darwin"
        }
        fn is_dir(&self, path: &Path) -> bool {
            self.exists(&path.to_string_lossy())
        }
        fn home(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/Users/h"))
        }
        fn git_remote(&self, dir: &Path) -> Option<String> {
            let s = dir.to_string_lossy().into_owned();
            self.touched.borrow_mut().push(s.clone());
            self.dirs
                .iter()
                .find(|(d, _)| same_path(d, &s, "darwin"))
                .and_then(|(_, r)| *r)
                .and_then(registry::normalize_git_remote)
        }
        fn git_root(&self, _cwd: &Path) -> Option<PathBuf> {
            None
        }
        fn list_dirs(&self, dir: &Path) -> Vec<PathBuf> {
            let s = dir.to_string_lossy().into_owned();
            self.touched.borrow_mut().push(s.clone());
            let mut out: Vec<String> = Vec::new();
            for (d, _) in &self.dirs {
                let mut cur = Some(d.to_string());
                while let Some(c) = cur {
                    let parent = path_parent(&c, "darwin");
                    if parent.as_deref().is_some_and(|p| same_path(p, &s, "darwin"))
                        && !out.iter().any(|o| same_path(o, &c, "darwin"))
                    {
                        out.push(c.clone());
                    }
                    cur = parent;
                }
            }
            out.into_iter().map(PathBuf::from).collect()
        }
    }

    #[test]
    fn discovery_never_opens_the_os_folders_under_home() {
        let fs = RecordingFs {
            dirs: vec![
                ("/Users/h/Documents/decoy", Some("git@github.com:o/app.git")),
                ("/Users/h/Downloads/decoy", Some("git@github.com:o/app.git")),
                ("/Users/h/Desktop/app", None),
                ("/Users/h/Library/app", None),
                ("/Users/h/Pictures/app", None),
                ("/Users/h/Movies/x", None),
                ("/Users/h/Music/x", None),
                ("/Users/h/Public/x", None),
                ("/Users/h/Applications/x", None),
                ("/Users/h/.cache/app", None),
                ("/Users/h/OneDrive - Org/app", None),
                // A repo named like an OS folder, inside a code folder, is
                // still a candidate.
                ("/Users/h/code/music", Some("git@github.com:o/music.git")),
                ("/Users/h/work/app", Some("git@github.com:o/app.git")),
            ],
            touched: Default::default(),
        };
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProjectStore::load_blocking_from(
            dir.path().join("projects.json"),
            dir.path().join("workspace_state.json"),
        )
        .unwrap();
        let mut app = mnemos_ide_core::types::Project::new("app", PathBuf::from("C:\\code\\app"));
        app.id = "app".into();
        app.set_git_repo("o/app");
        store.add(app);
        let mut music = mnemos_ide_core::types::Project::new("music", PathBuf::from("C:\\code\\music"));
        music.id = "music".into();
        music.set_git_repo("o/music");
        store.add(music);

        let mut resolver = LocalResolver::new();
        assert_eq!(
            resolver.resolve_local_path(&mut store, "app", &fs),
            Some(PathBuf::from("/Users/h/work/app"))
        );
        assert_eq!(
            resolver.resolve_local_path(&mut store, "music", &fs),
            Some(PathBuf::from("/Users/h/code/music"))
        );
        let touched = fs.touched.borrow();
        for folder in [
            "Documents",
            "Downloads",
            "Desktop",
            "Library",
            "Pictures",
            "Movies",
            "Music",
            "Public",
            "Applications",
            ".cache",
            "OneDrive - Org",
        ] {
            let prefix = format!("/Users/h/{folder}");
            assert!(
                !touched.iter().any(|t| t.starts_with(&prefix)),
                "discovery opened {prefix}: {touched:?}"
            );
        }
        assert!(touched.iter().any(|t| t == "/Users/h/code/music"));
    }

    #[test]
    fn code_roots_are_the_usual_folders_that_exist_never_home() {
        let mac = RecordingFs {
            dirs: vec![
                ("/Users/h/code/app", None),
                ("/Users/h/Desktop/scratch", None),
                ("/Users/h/repos", None),
            ],
            touched: Default::default(),
        };
        // `Code` is the same folder as `code` on a case-insensitive disk.
        assert_eq!(code_roots(&mac), ["/Users/h/code", "/Users/h/repos"]);

        struct Pc;
        impl FsEnv for Pc {
            fn platform(&self) -> &'static str {
                "win32"
            }
            fn is_dir(&self, path: &Path) -> bool {
                let s = path.to_string_lossy();
                ["C:\\code", "C:\\Users\\h\\code", "C:\\Users\\h"]
                    .iter()
                    .any(|d| same_path(d, &s, "win32"))
            }
            fn home(&self) -> Option<PathBuf> {
                Some(PathBuf::from("C:\\Users\\h"))
            }
            fn git_remote(&self, _: &Path) -> Option<String> {
                None
            }
            fn git_root(&self, _: &Path) -> Option<PathBuf> {
                None
            }
            fn list_dirs(&self, _: &Path) -> Vec<PathBuf> {
                Vec::new()
            }
        }
        assert_eq!(code_roots(&Pc), ["C:\\code", "C:\\Users\\h\\code"]);
    }

    #[test]
    fn a_folder_named_after_another_record_of_the_repo_is_left_to_it() {
        // TwinCurrent-Build and TCB are both bob/TCB (two Mac
        // checkouts). Here the only checkout is `~/code/TCB`, and the
        // registry lists TwinCurrent-Build first.
        let fs = RecordingFs {
            dirs: vec![("/Users/h/code/TCB", Some("git@github.com:bob/TCB.git"))],
            touched: Default::default(),
        };
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProjectStore::load_blocking_from(
            dir.path().join("projects.json"),
            dir.path().join("workspace_state.json"),
        )
        .unwrap();
        for (id, name) in [("ed5f6c66", "TwinCurrent-Build"), ("421a6cc0", "TCB")] {
            let mut p = mnemos_ide_core::types::Project::new(name, PathBuf::from("C:\\x"));
            p.id = id.into();
            p.set_git_repo("bob/TCB");
            store.add(p);
        }
        let mut resolver = LocalResolver::new();
        assert_eq!(resolver.resolve_local_path(&mut store, "ed5f6c66", &fs), None);
        assert_eq!(
            resolver.resolve_local_path(&mut store, "421a6cc0", &fs),
            Some(PathBuf::from("/Users/h/code/TCB"))
        );
        // Once TCB has it, the folder is claimed and still not handed over.
        assert_eq!(resolver.resolve_local_path(&mut store, "ed5f6c66", &fs), None);
    }

    #[test]
    fn a_repo_cloned_after_the_scan_is_found_once_the_scan_expires() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ProjectStore::load_blocking_from(
            dir.path().join("projects.json"),
            dir.path().join("workspace_state.json"),
        )
        .unwrap();
        let mut app = mnemos_ide_core::types::Project::new("app", PathBuf::from("C:\\code\\app"));
        app.id = "app".into();
        app.set_git_repo("o/app");
        store.add(app);

        let start = Instant::now();
        let now = Arc::new(std::sync::Mutex::new(start));
        let mut resolver = LocalResolver::with_clock({
            let now = now.clone();
            move || *now.lock().unwrap()
        });
        let set = |d: Duration| *now.lock().unwrap() = start + d;

        // Nothing checked out yet: the scan is taken, and cached.
        let before = RecordingFs {
            dirs: vec![("/Users/h/code/other", None)],
            touched: Default::default(),
        };
        assert_eq!(resolver.resolve_local_path(&mut store, "app", &before), None);

        // The repo is cloned. Inside the TTL the cached scan answers and the
        // disk is not walked again.
        let cloned = || RecordingFs {
            dirs: vec![
                ("/Users/h/code/other", None),
                ("/Users/h/code/app", Some("git@github.com:o/app.git")),
            ],
            touched: Default::default(),
        };
        let after = cloned();
        set(SCAN_TTL - Duration::from_secs(1));
        assert_eq!(resolver.resolve_local_path(&mut store, "app", &after), None);
        assert!(after.touched.borrow().is_empty(), "{:?}", after.touched.borrow());

        // Past the TTL the next pass rescans and finds it.
        set(SCAN_TTL + Duration::from_secs(1));
        assert_eq!(
            resolver.resolve_local_path(&mut store, "app", &after),
            Some(PathBuf::from("/Users/h/code/app"))
        );

        // `invalidate` still forces a rescan at once, whatever the clock.
        let mut fresh = LocalResolver::with_clock({
            let now = now.clone();
            move || *now.lock().unwrap()
        });
        let mut other = ProjectStore::load_blocking_from(
            dir.path().join("other.json"),
            dir.path().join("other_state.json"),
        )
        .unwrap();
        let mut again = mnemos_ide_core::types::Project::new("app", PathBuf::from("C:\\code\\app"));
        again.id = "app".into();
        again.set_git_repo("o/app");
        other.add(again);
        assert_eq!(fresh.resolve_local_path(&mut other, "app", &before), None);
        fresh.invalidate();
        let after = cloned();
        assert_eq!(
            fresh.resolve_local_path(&mut other, "app", &after),
            Some(PathBuf::from("/Users/h/code/app"))
        );
    }

    #[test]
    fn real_fs_tab_root_walks_up_to_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let deep = repo.join("src").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(tab_root(&RealFs, &deep), Some(repo.clone()));
        let loose = tmp.path().join("loose");
        std::fs::create_dir_all(&loose).unwrap();
        assert_eq!(tab_root(&RealFs, &loose), Some(loose.clone()));
        if let Some(home) = dirs::home_dir() {
            assert_eq!(tab_root(&RealFs, &home), None);
        }
        assert_eq!(tab_root(&RealFs, &tmp.path().join("missing")), None);

        // `canonicalize` on Windows yields `\\?\C:\...`; the root must come
        // back in the plain form every other path in the registry uses.
        #[cfg(windows)]
        {
            let verbatim = std::fs::canonicalize(&deep).unwrap();
            assert!(verbatim.to_string_lossy().starts_with(r"\\?\"));
            let root = tab_root(&RealFs, &verbatim).unwrap();
            let want = registry::strip_verbatim(
                &std::fs::canonicalize(&repo).unwrap().to_string_lossy(),
            );
            assert_eq!(root, PathBuf::from(want));
        }
    }
}
