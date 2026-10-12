// SPDX-License-Identifier: AGPL-3.0-only
//! Project store — load/save of `projects.json` (synced registry) + local
//! `workspace_state.json` (per-machine tab state). v1-compatible.
//!
//! # On-disk schema (v1 compatibility — must round-trip lossless)
//!
//! Source: `apps/frontend/src/main/sync-manager.ts` (TCB v1 Electron),
//! `ProjectsData` interface around line 64. The admin platform sync key is
//! `projects`; the file is read/written by both v1 (Electron) and v2 (this
//! crate) during the migration window, so the JSON shape MUST stay compatible.
//!
//! ## Two-file split (v2)
//!
//! `projects.json` (synced via mnemos-ide-sync — shared across machines):
//!
//! ```jsonc
//! {
//!   "projects":  [ /* registry — shared on all machines */ ],
//!   "machineId": "string",
//!   /* additional top-level keys preserved verbatim via ProjectsData.extra */
//! }
//! ```
//!
//! `workspace_state.json` (local-only, per-machine):
//!
//! ```jsonc
//! {
//!   "openProjectIds":  ["id1", "id2"],
//!   "activeProjectId": "id1" | null,
//!   "tabOrder":        ["id1", "id2"]
//! }
//! ```
//!
//! ## Migrations performed on load
//!
//! 1. **v1-seed**: if `projects.json` doesn't exist and the old TCB v1
//!    Electron file at `ide_v1_projects_file()` does, copy v1 → v2 verbatim
//!    so the user's project list shows up on first launch.
//! 2. **v1-tabState split**: if a legacy `projects.json` carries a `tabState`
//!    block and no `workspace_state.json` exists yet, peel the `tabState`
//!    out and persist it as the new local file. The next save drops
//!    `tabState` from `projects.json`.
//!
//! Default file locations come from `mnemos_ide_core::paths`.
//!
//! ## Cross-machine registry
//!
//! `projects.json` is merged, not overwritten, across machines (see
//! `mnemos_ide_core::registry`). Records carry `paths.<platform>`, `gitRepo`, and a
//! synced `open` flag; deletes are tombstones under `deletedProjects`. Every
//! save runs `registry::normalize`, and read-modify-write callers go through
//! [`ProjectStore::update_blocking`] / [`ProjectStore::update`], which hold a
//! process-wide lock so the app and mnemos-ide-sync never interleave their edits.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use mnemos_ide_core::paths::{
    ide_legacy_v2_projects_file, ide_projects_file, ide_v1_projects_file,
    ide_workspace_state_file,
};
use mnemos_ide_core::registry;
use mnemos_ide_core::types::Project;
use mnemos_ide_core::IdeError;
use tokio::fs;

/// Serializes every load -> mutate -> save cycle in this process. The app
/// (tab reconcile, add/close project) and mnemos-ide-sync (pull apply, path
/// learning) all edit `projects.json`; without the lock one side's save
/// silently discards the other's edit.
static STORE_LOCK: Mutex<()> = Mutex::new(());

fn store_lock() -> MutexGuard<'static, ()> {
    // A panic inside an update closure poisons the lock but leaves no torn
    // state behind (the file is written atomically or not at all).
    STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TabState {
    #[serde(rename = "openProjectIds", default)]
    pub open_project_ids: Vec<String>,
    #[serde(rename = "activeProjectId", default)]
    pub active_project_id: Option<String>,
    #[serde(rename = "tabOrder", default)]
    pub tab_order: Vec<String>,
    /// Per-machine opt-out: project ids whose state should NOT be pushed
    /// to or pulled from the sync server. Default-empty (opt-out, sync-on).
    /// Lives in `workspace_state.json` so the override stays device-local.
    #[serde(rename = "syncDisabledProjectIds", default)]
    pub sync_disabled_project_ids: Vec<String>,
}

/// Wire-format mirror of v1's `ProjectsData`. Used for serde — the
/// public `ProjectStore` is the consumer-facing API.
///
/// `tab_state` is kept here only for backwards-compatible *reads* of legacy
/// files; new writes never include it (tab state lives in
/// `workspace_state.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectsData {
    #[serde(default)]
    projects: Vec<Project>,
    #[serde(default, rename = "tabState", skip_serializing_if = "Option::is_none")]
    tab_state: Option<TabState>,
    #[serde(default, rename = "machineId", skip_serializing_if = "Option::is_none")]
    machine_id: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ProjectStore {
    /// Synced registry file — `projects.json`.
    pub path: PathBuf,
    /// Local-only workspace state file — `workspace_state.json`.
    pub workspace_state_path: PathBuf,
    pub projects: Vec<Project>,
    pub tab_state: Option<TabState>,
    pub machine_id: Option<String>,
    /// Unknown top-level JSON keys preserved verbatim for v1 round-trip.
    extra: serde_json::Map<String, serde_json::Value>,
}

impl ProjectStore {
    /// Load from the default Mnemos paths.
    /// Tolerates missing files (returns empty store).
    /// On first run, attempts a one-time seed from legacy locations
    /// (`twincurrent-build-ui` v2, then v1 Electron `TwinCurrent Build`).
    pub async fn load() -> Result<Self, IdeError> {
        let projects_path = ide_projects_file()?;
        let workspace_state_path = ide_workspace_state_file()?;
        maybe_seed_chained(&projects_path);
        Self::load_from(projects_path, workspace_state_path).await
    }

    /// Load from explicit paths. Tolerates missing files. Does NOT trigger
    /// the legacy-seed migration — that only fires from [`load`].
    pub async fn load_from(
        projects_path: impl AsRef<Path>,
        workspace_state_path: impl AsRef<Path>,
    ) -> Result<Self, IdeError> {
        let projects_path = projects_path.as_ref().to_path_buf();
        let workspace_state_path = workspace_state_path.as_ref().to_path_buf();

        let projects_bytes = match fs::read(&projects_path).await {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(?projects_path, "projects.json missing — starting empty");
                None
            }
            Err(e) => return Err(e.into()),
        };
        let workspace_bytes = match fs::read(&workspace_state_path).await {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        Self::from_bytes(
            projects_path,
            workspace_state_path,
            projects_bytes.as_deref(),
            workspace_bytes.as_deref(),
        )
    }

    /// Synchronous variant of [`load`] — useful from non-async callers like
    /// mnemos_ui View constructors. Same tolerance for missing file and same
    /// one-time legacy-seed migration. Uses `std::fs` so it does not require
    /// a tokio runtime.
    pub fn load_blocking() -> Result<Self, IdeError> {
        let projects_path = ide_projects_file()?;
        let workspace_state_path = ide_workspace_state_file()?;
        maybe_seed_chained(&projects_path);
        Self::load_blocking_from(projects_path, workspace_state_path)
    }

    /// Synchronous variant of [`load_from`]. Does NOT trigger the
    /// legacy-seed migration — that only fires from [`load_blocking`].
    pub fn load_blocking_from(
        projects_path: impl AsRef<Path>,
        workspace_state_path: impl AsRef<Path>,
    ) -> Result<Self, IdeError> {
        let projects_path = projects_path.as_ref().to_path_buf();
        let workspace_state_path = workspace_state_path.as_ref().to_path_buf();

        let projects_bytes = match std::fs::read(&projects_path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(?projects_path, "projects.json missing — starting empty");
                None
            }
            Err(e) => return Err(e.into()),
        };
        let workspace_bytes = match std::fs::read(&workspace_state_path) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        Self::from_bytes(
            projects_path,
            workspace_state_path,
            projects_bytes.as_deref(),
            workspace_bytes.as_deref(),
        )
    }

    fn from_bytes(
        projects_path: PathBuf,
        workspace_state_path: PathBuf,
        projects_bytes: Option<&[u8]>,
        workspace_bytes: Option<&[u8]>,
    ) -> Result<Self, IdeError> {
        let projects_data: ProjectsData = match projects_bytes {
            Some(bytes) => serde_json::from_slice(bytes)?,
            None => ProjectsData::default(),
        };

        // Workspace state precedence:
        // 1. workspace_state.json if present.
        // 2. Else legacy tabState from projects.json (one-time split migration).
        // 3. Else None.
        let tab_state = match workspace_bytes {
            Some(bytes) => {
                let state: TabState = serde_json::from_slice(bytes)?;
                Some(state)
            }
            None => projects_data.tab_state.clone(),
        };

        Ok(Self {
            path: projects_path,
            workspace_state_path,
            projects: projects_data.projects,
            tab_state,
            machine_id: projects_data.machine_id,
            extra: projects_data.extra,
        })
    }

    /// Save atomically: write to `<path>.tmp`, then rename. Writes both
    /// `projects.json` (synced) and `workspace_state.json` (local).
    pub async fn save(&self) -> Result<(), IdeError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let bytes = self.serialize_synced()?;
        let tmp = tmp_sibling(&self.path);
        fs::write(&tmp, &bytes).await?;
        fs::rename(&tmp, &self.path).await?;

        if let Some(state) = &self.tab_state {
            if let Some(parent) = self.workspace_state_path.parent() {
                fs::create_dir_all(parent).await?;
            }
            let bytes = serde_json::to_vec_pretty(state)?;
            let tmp = tmp_sibling(&self.workspace_state_path);
            fs::write(&tmp, &bytes).await?;
            fs::rename(&tmp, &self.workspace_state_path).await?;
        }
        // No explicit notify here: mnemos-ide-sync's watcher already watches
        // `projects.json` (manager.rs `collect_watch_paths`) and pushes on
        // change. `workspace_state.json` is local-only and not watched.
        Ok(())
    }

    /// Synchronous variant of [`save`] — usable from non-async callers like
    /// mnemos_ui View event handlers.
    pub fn save_blocking(&self) -> Result<(), IdeError> {
        self.save_blocking_parts(true, true)
    }

    /// Write `projects.json` and/or `workspace_state.json`. Rewriting an
    /// unchanged `projects.json` is not free: its new mtime wakes the app's
    /// change poll (a reconcile) and mnemos-ide-sync's watcher.
    fn save_blocking_parts(&self, registry: bool, tabs: bool) -> Result<(), IdeError> {
        if registry {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = self.serialize_synced()?;
            let tmp = tmp_sibling(&self.path);
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &self.path)?;
        }

        if let Some(state) = self.tab_state.as_ref().filter(|_| tabs) {
            if let Some(parent) = self.workspace_state_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec_pretty(state)?;
            let tmp = tmp_sibling(&self.workspace_state_path);
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &self.workspace_state_path)?;
        }
        Ok(())
    }

    /// Serialize only the synced subset (registry + machineId + extra).
    /// `tabState` is intentionally omitted — it lives in `workspace_state.json`.
    /// Normalized first, so no writer can put a duplicate registration or a
    /// tombstoned record back on disk.
    fn serialize_synced(&self) -> Result<Vec<u8>, IdeError> {
        let normalized =
            registry::normalize_keeping_apart(&self.registry_value(), &self.sync_disabled_set());
        Ok(serde_json::to_vec_pretty(&normalized)?)
    }

    /// Projects whose sync is disabled on this machine. Normalizing never
    /// folds them: the server does not know them, so a fold would push a
    /// `replacedBy` to an id no other machine can resolve.
    pub fn sync_disabled_set(&self) -> HashSet<String> {
        self.sync_disabled_ids().iter().cloned().collect()
    }

    // ── Locked read-modify-write ────────────────────────────────────────────

    /// Load the default store, apply `f`, normalize, and save, all under the
    /// process-wide store lock. The write is skipped when nothing changed, so
    /// a no-op update never wakes the sync watcher.
    pub fn update_blocking<R>(f: impl FnOnce(&mut ProjectStore) -> R) -> Result<R, IdeError> {
        let _guard = store_lock();
        let projects_path = ide_projects_file()?;
        let workspace_state_path = ide_workspace_state_file()?;
        maybe_seed_chained(&projects_path);
        Self::update_locked(&projects_path, &workspace_state_path, f)
    }

    /// [`Self::update_blocking`] against explicit paths (tests, tools). Same
    /// lock; no legacy seed.
    pub fn update_blocking_at<R>(
        projects_path: impl AsRef<Path>,
        workspace_state_path: impl AsRef<Path>,
        f: impl FnOnce(&mut ProjectStore) -> R,
    ) -> Result<R, IdeError> {
        let _guard = store_lock();
        Self::update_locked(projects_path.as_ref(), workspace_state_path.as_ref(), f)
    }

    /// Async twin of [`Self::update_blocking`]. Runs on the blocking pool so
    /// the lock is never held across an `.await`.
    pub async fn update<R, F>(f: F) -> Result<R, IdeError>
    where
        F: FnOnce(&mut ProjectStore) -> R + Send + 'static,
        R: Send + 'static,
    {
        tokio::task::spawn_blocking(move || Self::update_blocking(f))
            .await
            .map_err(|e| IdeError::Io(std::io::Error::other(e)))?
    }

    fn update_locked<R>(
        projects_path: &Path,
        workspace_state_path: &Path,
        f: impl FnOnce(&mut ProjectStore) -> R,
    ) -> Result<R, IdeError> {
        let mut store = Self::load_blocking_from(projects_path, workspace_state_path)?;
        let before_registry = store.registry_value();
        let before_tabs = store.tab_state.clone();
        let out = f(&mut store);
        store.normalize();
        let registry_changed = store.registry_value() != before_registry;
        let tabs_changed = store.tab_state != before_tabs;
        if registry_changed || tabs_changed {
            store.save_blocking_parts(registry_changed, tabs_changed)?;
        }
        Ok(out)
    }

    // ── Registry as JSON ────────────────────────────────────────────────────

    /// The synced registry (`projects`, `machineId`, `deletedProjects` and any
    /// other top-level keys) as a JSON value, for `mnemos_ide_core::registry`.
    pub fn registry_value(&self) -> Value {
        // Built by hand rather than `to_value(ProjectsData)`: that fails as a
        // whole on one unserializable record (a non-UTF-8 `path`), and an
        // empty fallback would make `normalize`/`update_*` save an empty
        // registry. Same key layout as `ProjectsData`'s serde output.
        let mut out = serde_json::Map::new();
        let projects = self.projects.iter().map(project_value).collect();
        out.insert(registry::PROJECTS_KEY.to_string(), Value::Array(projects));
        if let Some(m) = &self.machine_id {
            out.insert("machineId".to_string(), Value::String(m.clone()));
        }
        for (k, v) in &self.extra {
            out.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(out)
    }

    /// Replace the synced registry with `value` (e.g. the result of
    /// `registry::merge`). Local tab state is untouched.
    pub fn replace_registry(&mut self, value: &Value) -> Result<(), IdeError> {
        let data: ProjectsData = serde_json::from_value(value.clone())?;
        self.projects = data.projects;
        self.machine_id = data.machine_id;
        self.extra = data.extra;
        Ok(())
    }

    /// Apply `registry::normalize` in memory: tombstones, and folding two
    /// registrations of one repo into the older id (never a project whose
    /// sync is disabled here; see [`Self::sync_disabled_set`]).
    pub fn normalize(&mut self) {
        let normalized =
            registry::normalize_keeping_apart(&self.registry_value(), &self.sync_disabled_set());
        if let Err(e) = self.replace_registry(&normalized) {
            tracing::warn!(error = %e, "normalized registry did not parse back; keeping as is");
        }
    }

    // ── Ids ─────────────────────────────────────────────────────────────────

    fn tombstones(&self) -> Option<&serde_json::Map<String, Value>> {
        self.extra.get(registry::DELETED_KEY).and_then(Value::as_object)
    }

    fn tombstones_mut(&mut self) -> &mut serde_json::Map<String, Value> {
        let slot = self
            .extra
            .entry(registry::DELETED_KEY.to_string())
            .or_insert_with(|| Value::Object(Default::default()));
        if !slot.is_object() {
            *slot = Value::Object(Default::default());
        }
        slot.as_object_mut().expect("just made an object")
    }

    /// Whether `id` is a live record or a tombstone. New records must not
    /// reuse such an id, or a stale tab could resurrect a deleted project.
    pub fn is_known_id(&self, id: &str) -> bool {
        self.projects.iter().any(|p| p.id == id)
            || self.tombstones().is_some_and(|t| t.contains_key(id))
    }

    /// The live record `id` refers to, following `replacedBy` tombstones.
    pub fn resolve_id(&self, id: &str) -> Option<String> {
        registry::resolve_id_with(
            |candidate| self.projects.iter().any(|p| p.id == candidate),
            self.tombstones(),
            id,
        )
    }

    /// The live record for `id`, following `replacedBy` tombstones, so ids
    /// held from before a merge still find their project.
    pub fn get(&self, id: &str) -> Option<&Project> {
        let live = self.resolve_id(id)?;
        self.projects.iter().find(|p| p.id == live)
    }

    pub(crate) fn get_mut(&mut self, id: &str) -> Option<&mut Project> {
        let live = self.resolve_id(id)?;
        self.projects.iter_mut().find(|p| p.id == live)
    }

    /// A fresh `project-<millis>` id that is neither live, tombstoned, nor in
    /// `taken`. Counts up from `now` so several records minted in one pass
    /// never collide.
    pub(crate) fn mint_project_id(&self, now: DateTime<Utc>, taken: &HashSet<String>) -> String {
        let mut millis = now.timestamp_millis();
        loop {
            let id = format!("project-{millis}");
            if !self.is_known_id(&id) && !taken.contains(&id) {
                return id;
            }
            millis += 1;
        }
    }

    // ── Open set ────────────────────────────────────────────────────────────

    /// Set the synced open flag on `id` (following `replacedBy`). Returns
    /// `false` when no live record matches.
    pub fn set_open(&mut self, id: &str, open: bool, now: DateTime<Utc>) -> bool {
        match self.get_mut(id) {
            Some(p) => {
                p.set_open(open, now);
                true
            }
            None => false,
        }
    }

    /// Ids of records whose synced `open` flag is true, in registry order.
    /// This is the set every machine shows as project tabs.
    pub fn open_project_ids(&self) -> Vec<String> {
        self.projects
            .iter()
            .filter(|p| p.is_open() == Some(true))
            .map(|p| p.id.clone())
            .collect()
    }

    /// The "Add project" picker: reuse the record already checked out at
    /// `root` on this machine, else claim a record of the same `git_repo`
    /// that has no checkout here yet, else register a new one. The record
    /// ends up open, with `paths.<current platform>` set. Returns its id.
    pub fn register_or_claim(
        &mut self,
        root: &Path,
        name: &str,
        git_repo: Option<&str>,
        now: DateTime<Utc>,
    ) -> String {
        self.register_or_claim_on(registry::current_platform(), root, name, git_repo, now)
    }

    /// [`Self::register_or_claim`] for an explicit platform slot, so tests
    /// can act as another machine.
    pub fn register_or_claim_on(
        &mut self,
        platform: &str,
        root: &Path,
        name: &str,
        git_repo: Option<&str>,
        now: DateTime<Utc>,
    ) -> String {
        let root_s = root.to_string_lossy().into_owned();
        let git_repo = git_repo.map(str::trim).filter(|g| !g.is_empty());
        let by_path = self.projects.iter().position(|p| {
            p.platform_path(platform)
                .is_some_and(|lp| registry::same_path(&lp.to_string_lossy(), &root_s, platform))
        });
        let by_repo = || {
            let key = registry::git_repo_key(git_repo?);
            let mut candidates: Vec<usize> = self
                .projects
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p.platform_path(platform).is_none()
                        && p.git_repo().is_some_and(|g| registry::git_repo_key(&g) == key)
                })
                .map(|(i, _)| i)
                .collect();
            candidates.sort_by_key(|&i| {
                let p = &self.projects[i];
                (!p.name.eq_ignore_ascii_case(name), p.created_at, p.id.clone())
            });
            candidates.first().copied()
        };
        let idx = match by_path.or_else(by_repo) {
            Some(i) => i,
            None => {
                let id = self.mint_project_id(now, &HashSet::new());
                self.projects.push(Project {
                    id,
                    name: name.to_string(),
                    path: root.to_path_buf(),
                    created_at: now,
                    last_opened_at: Some(now),
                    extra: serde_json::Map::new(),
                });
                self.projects.len() - 1
            }
        };
        let project = &mut self.projects[idx];
        if !project.paths().contains_key(platform) {
            project.set_platform_path(platform, root);
        }
        if project.git_repo().is_none() {
            if let Some(g) = git_repo {
                project.set_git_repo(g);
            }
        }
        project.last_opened_at = Some(now);
        project.set_open(true, now);
        project.id.clone()
    }

    /// Return projects in the order they should render as tabs.
    ///
    /// Once any record carries the synced `open` flag, the open set is the
    /// registry's (`open == true`), ordered by this machine's `tab_order`
    /// where it has one, then registry order. Before that (a registry no
    /// build with the flag has touched yet) the legacy per-machine
    /// `open_project_ids` apply: `tab_order` first (intersected with
    /// `open_project_ids`), then any open ids not yet in `tab_order`, dead
    /// ids filtered, empty when there is no tab state.
    pub fn open_projects_in_order(&self) -> Vec<Project> {
        if self.projects.iter().any(|p| p.is_open().is_some()) {
            let open: Vec<&Project> = self
                .projects
                .iter()
                .filter(|p| p.is_open() == Some(true))
                .collect();
            let mut seen: HashSet<String> = HashSet::new();
            let mut out = Vec::new();
            if let Some(state) = self.tab_state.as_ref() {
                for id in &state.tab_order {
                    let Some(live) = self.resolve_id(id) else { continue };
                    if let Some(p) = open.iter().find(|p| p.id == live) {
                        if seen.insert(live) {
                            out.push((*p).clone());
                        }
                    }
                }
            }
            for p in open {
                if seen.insert(p.id.clone()) {
                    out.push(p.clone());
                }
            }
            return out;
        }

        let Some(state) = self.tab_state.as_ref() else {
            return Vec::new();
        };
        let open: HashSet<&str> = state.open_project_ids.iter().map(String::as_str).collect();

        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        for id in &state.tab_order {
            if open.contains(id.as_str()) {
                if let Some(p) = self.projects.iter().find(|p| &p.id == id) {
                    out.push(p.clone());
                    seen.insert(id.clone());
                }
            }
        }
        for id in &state.open_project_ids {
            if !seen.contains(id) {
                if let Some(p) = self.projects.iter().find(|p| &p.id == id) {
                    out.push(p.clone());
                }
            }
        }
        out
    }

    /// Insert or replace by id. Adding a record is an explicit "this project
    /// exists", so any tombstone for its id is lifted.
    ///
    /// Lifting it here is not enough on its own: every other machine still
    /// holds the tombstone and merges it back. So when the record is not
    /// newer than a plain delete, its `updatedAt` is moved just past
    /// `deletedAt`, which is what makes the re-add outrank the delete in
    /// every merge. (A `replacedBy` tombstone always wins a merge; re-adding
    /// a folded id only lasts until the next pull, which maps it onto the
    /// record it was folded into.)
    pub fn add(&mut self, mut project: Project) {
        let lifted = self
            .extra
            .get_mut(registry::DELETED_KEY)
            .and_then(Value::as_object_mut)
            .and_then(|t| t.remove(&project.id));
        let deleted_at = lifted
            .as_ref()
            .filter(|t| t.get("replacedBy").and_then(Value::as_str).is_none())
            .and_then(|t| t.get("deletedAt"))
            .and_then(Value::as_str)
            .and_then(registry::parse_timestamp);
        if let Some(deleted) = deleted_at {
            let newest = [
                Some(project.created_at),
                project.updated_at(),
                project.open_changed_at(),
            ]
            .into_iter()
            .flatten()
            .max();
            if newest.is_none_or(|t| t <= deleted) {
                project.touch(deleted + Duration::milliseconds(1));
            }
        }
        if let Some(slot) = self.projects.iter_mut().find(|p| p.id == project.id) {
            *slot = project;
        } else {
            self.projects.push(project);
        }
    }

    /// Delete a project on every machine: removes the record and leaves a
    /// tombstone, which is what makes the delete survive a merge with a
    /// machine that still has the record. Closing a tab is NOT a delete; use
    /// [`Self::set_open`] for that.
    pub fn remove(&mut self, id: &str) -> Option<Project> {
        self.delete_project(id, Utc::now())
    }

    /// [`Self::remove`] with an explicit clock. The tombstone is stamped at
    /// least 1 ms after the record's newest clock so it always outranks the
    /// version being deleted, even with a skewed local clock.
    pub fn delete_project(&mut self, id: &str, now: DateTime<Utc>) -> Option<Project> {
        let idx = self.projects.iter().position(|p| p.id == id)?;
        let removed = self.projects.remove(idx);
        let newest = [
            Some(removed.created_at),
            removed.updated_at(),
            removed.open_changed_at(),
        ]
        .into_iter()
        .flatten()
        .max();
        let deleted_at = match newest {
            Some(t) if t >= now => t + Duration::milliseconds(1),
            _ => now,
        };
        let tombs = self.tombstones_mut();
        let replaced_by = tombs
            .get(id)
            .and_then(|t| t.get("replacedBy"))
            .cloned();
        let mut entry = serde_json::Map::new();
        entry.insert(
            "deletedAt".to_string(),
            Value::String(registry::format_timestamp(deleted_at)),
        );
        if let Some(r) = replaced_by {
            entry.insert("replacedBy".to_string(), r);
        }
        tombs.insert(id.to_string(), Value::Object(entry));
        Some(removed)
    }

    /// Whether the given project id has sync disabled on this machine. A
    /// `None` tab state implies sync is on for every project (default).
    pub fn is_sync_disabled(&self, project_id: &str) -> bool {
        self.tab_state
            .as_ref()
            .map(|s| s.sync_disabled_project_ids.iter().any(|id| id == project_id))
            .unwrap_or(false)
    }

    /// View of the disabled-id list. Empty slice when no tab state exists.
    pub fn sync_disabled_ids(&self) -> &[String] {
        match &self.tab_state {
            Some(s) => &s.sync_disabled_project_ids,
            None => &[],
        }
    }

    /// Toggle sync-disabled for a given project id. Idempotent — adding an
    /// id twice has no effect, removing one that isn't there is a no-op.
    /// Lazily creates a default `tab_state` when enabling the flag and one
    /// doesn't exist yet.
    pub fn set_sync_disabled(&mut self, project_id: &str, disabled: bool) {
        if disabled {
            let state = self.tab_state.get_or_insert_with(TabState::default);
            if !state.sync_disabled_project_ids.iter().any(|id| id == project_id) {
                state.sync_disabled_project_ids.push(project_id.to_string());
            }
        } else if let Some(state) = self.tab_state.as_mut() {
            state.sync_disabled_project_ids.retain(|id| id != project_id);
        }
    }

    /// Move the project at index `from` to index `to`. Returns
    /// `IdeError::Io(InvalidInput)` if either index is out of range.
    pub fn reorder(&mut self, from: usize, to: usize) -> Result<(), IdeError> {
        let len = self.projects.len();
        if from >= len || to >= len {
            return Err(IdeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid reorder: from={from} to={to} len={len}"),
            )));
        }
        if from == to {
            return Ok(());
        }
        let item = self.projects.remove(from);
        self.projects.insert(to, item);
        Ok(())
    }
}

/// One record as JSON. A `path` that is not valid Unicode is stored lossily
/// instead of failing the whole registry.
fn project_value(p: &Project) -> Value {
    serde_json::to_value(p).unwrap_or_else(|_| {
        let mut lossy = p.clone();
        lossy.path = PathBuf::from(p.path.to_string_lossy().into_owned());
        serde_json::to_value(&lossy).unwrap_or(Value::Null)
    })
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Layered one-time seed: if `target` (Mnemos `projects.json`) doesn't exist,
/// try a chain of legacy source files in priority order, first hit wins:
///
/// 1. `ide_legacy_v2_projects_file()` — pre-rename v2 dir
///    (`twincurrent-build-ui/store/projects.json`).
/// 2. `ide_v1_projects_file()` — TCB v1 Electron
///    (`TwinCurrent Build/store/projects.json`).
///
/// Safe to call on every load — once `target` exists this is a single
/// `exists()` stat call.
fn maybe_seed_chained(target: &Path) {
    let sources: [(&str, Option<PathBuf>); 2] = [
        ("legacy-v2", ide_legacy_v2_projects_file()),
        ("v1", ide_v1_projects_file()),
    ];
    maybe_seed_from_sources(target, &sources);
}

/// Generic chained-seed implementation. Iterates `sources` in order, picks the
/// first one whose file exists and parses as valid `ProjectsData`, and copies
/// it byte-for-byte to `target`. Corrupt sources are warned and skipped so the
/// chain falls through to the next candidate.
///
/// Failures are logged but never propagated: we'd rather start with an empty
/// store than crash the app over a bad legacy file.
fn maybe_seed_from_sources(target: &Path, sources: &[(&str, Option<PathBuf>)]) {
    if target.exists() {
        return;
    }
    for (label, path) in sources {
        let Some(src) = path else {
            continue;
        };
        if !src.exists() {
            continue;
        }
        let bytes = match std::fs::read(src) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(source = %label, ?src, error = %e, "seed read failed");
                continue;
            }
        };
        // Validate shape — fall through on corrupt source.
        if let Err(e) = serde_json::from_slice::<ProjectsData>(&bytes) {
            tracing::warn!(source = %label, ?src, error = %e, "seed parse failed; trying next source");
            continue;
        }
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(?parent, error = %e, "seed mkdir failed");
                return;
            }
        }
        let tmp = tmp_sibling(target);
        if let Err(e) = std::fs::write(&tmp, &bytes) {
            tracing::warn!(?tmp, error = %e, "seed write failed");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, target) {
            tracing::warn!(?tmp, ?target, error = %e, "seed rename failed");
            return;
        }
        tracing::info!(source = %label, ?src, ?target, "seeded projects.json");
        return;
    }
}

/// Back-compat wrapper used by existing tests. Equivalent to a chained seed
/// with a single `v1` source.
#[cfg(test)]
fn maybe_seed_from_v1(target: &Path, v1_path: Option<PathBuf>) {
    maybe_seed_from_sources(target, &[("v1", v1_path)]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::path::PathBuf;
    use mnemos_ide_core::types::Project;

    fn sample_project(id: &str) -> Project {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "gitRepo".into(),
            serde_json::Value::String(format!("git@github.com:tc/{}.git", id)),
        );
        Project {
            id: id.to_string(),
            name: format!("Project {}", id),
            path: PathBuf::from(format!("/tmp/{}", id)),
            created_at: Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0).unwrap(),
            last_opened_at: Some(Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap()),
            extra,
        }
    }

    fn paths(dir: &std::path::Path) -> (PathBuf, PathBuf) {
        (dir.join("projects.json"), dir.join("workspace_state.json"))
    }

    #[tokio::test]
    async fn roundtrip_save_load_equality() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());

        // Start empty
        let mut store = ProjectStore::load_from(&pp, &wp).await.unwrap();
        assert!(store.projects.is_empty());
        assert!(store.tab_state.is_none());

        // Mutate
        store.add(sample_project("a"));
        store.add(sample_project("b"));
        store.add(sample_project("c"));
        store.tab_state = Some(TabState {
            open_project_ids: vec!["a".into(), "b".into(), "c".into()],
            active_project_id: Some("b".into()),
            tab_order: vec!["a".into(), "b".into(), "c".into()],
            sync_disabled_project_ids: Vec::new(),
        });
        store.machine_id = Some("alice-laptop-darwin".into());

        // Persist
        store.save().await.unwrap();

        // Reload
        let reloaded = ProjectStore::load_from(&pp, &wp).await.unwrap();
        assert_eq!(reloaded.projects, store.projects);
        assert_eq!(reloaded.tab_state, store.tab_state);
        assert_eq!(reloaded.machine_id, store.machine_id);
    }

    #[test]
    fn load_blocking_from_matches_async_load() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let v1_json = serde_json::json!({
            "projects": [
                {
                    "id": "blk",
                    "name": "Blocking",
                    "path": "/tmp/blk",
                    "createdAt": "2026-04-30T08:00:00Z",
                    "lastOpenedAt": null
                }
            ],
            "tabState": {
                "openProjectIds": ["blk"],
                "activeProjectId": "blk",
                "tabOrder": ["blk"]
            }
        });
        std::fs::write(&pp, serde_json::to_vec_pretty(&v1_json).unwrap()).unwrap();

        let store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert_eq!(store.projects.len(), 1);
        assert_eq!(store.projects[0].id, "blk");
        assert_eq!(
            store.tab_state.as_ref().unwrap().active_project_id.as_deref(),
            Some("blk")
        );
    }

    #[test]
    fn load_blocking_from_missing_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert!(store.projects.is_empty());
        assert!(store.tab_state.is_none());
        assert!(store.machine_id.is_none());
    }

    #[tokio::test]
    async fn missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let store = ProjectStore::load_from(&pp, &wp).await.unwrap();
        assert!(store.projects.is_empty());
        assert!(store.tab_state.is_none());
        assert!(store.machine_id.is_none());
    }

    #[tokio::test]
    async fn add_remove_reorder() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_from(&pp, &wp).await.unwrap();

        store.add(sample_project("a"));
        store.add(sample_project("b"));
        store.add(sample_project("c"));
        assert_eq!(store.projects.len(), 3);

        // add() with same id should replace (idempotent upsert)
        let mut updated = sample_project("b");
        updated.name = "renamed".to_string();
        store.add(updated);
        assert_eq!(store.projects.len(), 3);
        assert_eq!(store.projects[1].name, "renamed");

        // reorder a -> end
        store.reorder(0, 2).unwrap();
        assert_eq!(store.projects[0].id, "b");
        assert_eq!(store.projects[1].id, "c");
        assert_eq!(store.projects[2].id, "a");

        // remove
        let removed = store.remove("c").unwrap();
        assert_eq!(removed.id, "c");
        assert_eq!(store.projects.len(), 2);
        assert!(store.remove("nonexistent").is_none());

        // reorder out-of-bounds -> err
        assert!(store.reorder(0, 99).is_err());
    }

    #[tokio::test]
    async fn v1_schema_unknown_keys_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());

        // v1-shaped projects.json with unknown keys both at the top level and
        // inside a project entry. These must round-trip lossless via
        // serde(flatten) on Project.extra and ProjectsData.extra.
        let v1_json = serde_json::json!({
            "projects": [
                {
                    "id": "abc",
                    "name": "Foo",
                    "path": "/Users/alice/code/foo",
                    "createdAt": "2026-04-29T10:00:00Z",
                    "lastOpenedAt": "2026-04-29T12:00:00Z",
                    "gitRepo": "git@github.com:tc/foo.git",
                    "lastOpened": "2026-04-29T10:00:00Z",
                    "color": "copper-500"
                }
            ],
            "tabState": {
                "openProjectIds": ["abc"],
                "activeProjectId": "abc",
                "tabOrder": ["abc"]
            },
            "machineId": "test-machine",
            "schemaVersion": 1
        });
        tokio::fs::write(&pp, serde_json::to_vec_pretty(&v1_json).unwrap())
            .await
            .unwrap();

        let store = ProjectStore::load_from(&pp, &wp).await.unwrap();
        assert_eq!(store.projects.len(), 1);
        assert_eq!(store.projects[0].id, "abc");
        assert_eq!(store.projects[0].name, "Foo");
        assert_eq!(
            store.projects[0].extra.get("gitRepo").and_then(|v| v.as_str()),
            Some("git@github.com:tc/foo.git")
        );
        assert_eq!(
            store.projects[0].extra.get("color").and_then(|v| v.as_str()),
            Some("copper-500")
        );
        assert_eq!(
            store.projects[0]
                .extra
                .get("lastOpened")
                .and_then(|v| v.as_str()),
            Some("2026-04-29T10:00:00Z")
        );
        assert_eq!(
            store.extra.get("schemaVersion").and_then(|v| v.as_i64()),
            Some(1)
        );

        store.save().await.unwrap();
        let reloaded = ProjectStore::load_from(&pp, &wp).await.unwrap();
        assert_eq!(reloaded.projects, store.projects);
        assert_eq!(
            reloaded.extra.get("schemaVersion"),
            store.extra.get("schemaVersion")
        );
    }

    // ---- New tests for the two-file split + migrations ----

    #[tokio::test]
    async fn legacy_tabstate_splits_into_workspace_state_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());

        // Legacy projects.json carrying tabState (the v1 shape).
        let legacy = serde_json::json!({
            "projects": [
                {
                    "id": "a",
                    "name": "A",
                    "path": "/tmp/a",
                    "createdAt": "2026-04-29T10:00:00Z",
                    "lastOpenedAt": null
                }
            ],
            "tabState": {
                "openProjectIds": ["a"],
                "activeProjectId": "a",
                "tabOrder": ["a"]
            },
            "machineId": "machine-1"
        });
        tokio::fs::write(&pp, serde_json::to_vec_pretty(&legacy).unwrap())
            .await
            .unwrap();
        assert!(!wp.exists(), "workspace_state.json should not exist yet");

        let store = ProjectStore::load_from(&pp, &wp).await.unwrap();
        // tabState was picked up from the legacy file.
        assert_eq!(
            store.tab_state.as_ref().unwrap().active_project_id.as_deref(),
            Some("a")
        );

        store.save().await.unwrap();

        // After save: projects.json no longer carries tabState.
        let synced: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&pp).await.unwrap()).unwrap();
        assert!(
            synced.get("tabState").is_none(),
            "tabState must be stripped from synced file after migration"
        );
        assert_eq!(synced["machineId"], "machine-1");

        // workspace_state.json now exists and carries the tab state.
        assert!(wp.exists(), "workspace_state.json should be written");
        let local: TabState =
            serde_json::from_slice(&tokio::fs::read(&wp).await.unwrap()).unwrap();
        assert_eq!(local.active_project_id.as_deref(), Some("a"));
        assert_eq!(local.open_project_ids, vec!["a".to_string()]);

        // Reloading reads from the new file, not the legacy field.
        let reloaded = ProjectStore::load_from(&pp, &wp).await.unwrap();
        assert_eq!(reloaded.tab_state, store.tab_state);
    }

    #[test]
    fn workspace_state_file_wins_over_legacy_tabstate() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());

        // Both files exist. workspace_state.json must take precedence.
        let projects_json = serde_json::json!({
            "projects": [],
            "tabState": {
                "openProjectIds": ["legacy"],
                "activeProjectId": "legacy",
                "tabOrder": ["legacy"]
            }
        });
        std::fs::write(&pp, serde_json::to_vec_pretty(&projects_json).unwrap()).unwrap();
        let local_state = serde_json::json!({
            "openProjectIds": ["fresh"],
            "activeProjectId": "fresh",
            "tabOrder": ["fresh"]
        });
        std::fs::write(&wp, serde_json::to_vec_pretty(&local_state).unwrap()).unwrap();

        let store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        let state = store.tab_state.unwrap();
        assert_eq!(state.active_project_id.as_deref(), Some("fresh"));
        assert_eq!(state.open_project_ids, vec!["fresh".to_string()]);
    }

    #[test]
    fn open_projects_in_order_respects_tab_order_and_filters_dead_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        store.add(sample_project("b"));
        store.add(sample_project("c"));
        store.tab_state = Some(TabState {
            // "ghost" is dead — not in registry. "b" not in open set.
            open_project_ids: vec!["c".into(), "a".into(), "ghost".into()],
            active_project_id: Some("c".into()),
            tab_order: vec!["a".into(), "ghost".into(), "c".into(), "b".into()],
            sync_disabled_project_ids: Vec::new(),
        });

        let rendered = store.open_projects_in_order();
        let ids: Vec<&str> = rendered.iter().map(|p| p.id.as_str()).collect();
        // Order follows tab_order, intersected with open_project_ids,
        // dead ids filtered. "b" is in tab_order but not open → skipped.
        assert_eq!(ids, vec!["a", "c"]);
    }

    #[test]
    fn open_projects_in_order_empty_when_no_tab_state() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        assert!(store.open_projects_in_order().is_empty());
    }

    #[test]
    fn open_projects_in_order_falls_back_for_ids_missing_from_tab_order() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        store.add(sample_project("b"));
        store.tab_state = Some(TabState {
            open_project_ids: vec!["a".into(), "b".into()],
            active_project_id: Some("a".into()),
            // b is open but not in tab_order — defensive fallback path.
            tab_order: vec!["a".into()],
            sync_disabled_project_ids: Vec::new(),
        });
        let ids: Vec<String> = store
            .open_projects_in_order()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn save_writes_two_files_synced_has_no_tabstate() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        store.tab_state = Some(TabState {
            open_project_ids: vec!["a".into()],
            active_project_id: Some("a".into()),
            tab_order: vec!["a".into()],
            sync_disabled_project_ids: Vec::new(),
        });
        store.save_blocking().unwrap();

        let synced: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pp).unwrap()).unwrap();
        assert!(synced["projects"].is_array());
        assert!(synced.get("tabState").is_none());

        let local: TabState =
            serde_json::from_slice(&std::fs::read(&wp).unwrap()).unwrap();
        assert_eq!(local.active_project_id.as_deref(), Some("a"));
    }

    #[test]
    fn save_skips_workspace_file_when_no_tab_state() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        store.save_blocking().unwrap();
        assert!(pp.exists());
        assert!(!wp.exists(), "workspace_state.json must not be created without tab state");
    }

    // ---- v1 (Electron) seed migration ----

    #[test]
    fn v1_seed_copies_when_v2_missing() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = dir.path().join("v1-projects.json");
        let target = dir.path().join("v2-projects.json");

        let v1_payload = serde_json::json!({
            "projects": [{
                "id": "seeded",
                "name": "Seeded",
                "path": "/tmp/seeded",
                "createdAt": "2026-04-29T10:00:00Z",
                "lastOpenedAt": null
            }],
            "machineId": "seeded-machine"
        });
        let v1_bytes = serde_json::to_vec_pretty(&v1_payload).unwrap();
        std::fs::write(&v1, &v1_bytes).unwrap();

        assert!(!target.exists());
        maybe_seed_from_v1(&target, Some(v1.clone()));
        assert!(target.exists(), "v2 file should be created by the seed");

        let copied = std::fs::read(&target).unwrap();
        assert_eq!(copied, v1_bytes, "seeded file must be byte-identical to v1");
    }

    #[test]
    fn v1_seed_is_no_op_when_v2_exists() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = dir.path().join("v1.json");
        let target = dir.path().join("v2.json");

        std::fs::write(&v1, b"v1-bytes").unwrap();
        std::fs::write(&target, b"v2-bytes").unwrap();

        maybe_seed_from_v1(&target, Some(v1));
        assert_eq!(std::fs::read(&target).unwrap(), b"v2-bytes");
    }

    #[test]
    fn v1_seed_is_no_op_when_v1_missing() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = dir.path().join("nope.json");
        let target = dir.path().join("v2.json");

        maybe_seed_from_v1(&target, Some(v1));
        assert!(!target.exists());
    }

    #[test]
    fn v1_seed_skips_corrupt_v1_file() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = dir.path().join("corrupt.json");
        let target = dir.path().join("v2.json");

        std::fs::write(&v1, b"{not json").unwrap();
        maybe_seed_from_v1(&target, Some(v1));
        assert!(!target.exists(), "corrupt v1 must not produce a v2 file");
    }

    // ---- Chained migration (legacy-v2 → v1) ----

    fn projects_payload(id: &str) -> Vec<u8> {
        let json = serde_json::json!({
            "projects": [{
                "id": id,
                "name": id,
                "path": format!("/tmp/{id}"),
                "createdAt": "2026-04-29T10:00:00Z",
                "lastOpenedAt": null
            }],
            "machineId": format!("{id}-machine"),
        });
        serde_json::to_vec_pretty(&json).unwrap()
    }

    #[test]
    fn chain_prefers_legacy_v2_when_both_present() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mnemos.json");
        let legacy_v2 = dir.path().join("legacy-v2.json");
        let v1 = dir.path().join("v1.json");

        std::fs::write(&legacy_v2, projects_payload("from-legacy-v2")).unwrap();
        std::fs::write(&v1, projects_payload("from-v1")).unwrap();

        maybe_seed_from_sources(
            &target,
            &[
                ("legacy-v2", Some(legacy_v2.clone())),
                ("v1", Some(v1.clone())),
            ],
        );

        assert!(target.exists());
        let copied = std::fs::read(&target).unwrap();
        assert_eq!(
            copied,
            std::fs::read(&legacy_v2).unwrap(),
            "legacy-v2 must win when both sources exist"
        );
    }

    #[test]
    fn chain_falls_through_to_v1_when_legacy_v2_missing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mnemos.json");
        let legacy_v2_missing = dir.path().join("not-here.json");
        let v1 = dir.path().join("v1.json");

        std::fs::write(&v1, projects_payload("from-v1")).unwrap();

        maybe_seed_from_sources(
            &target,
            &[
                ("legacy-v2", Some(legacy_v2_missing)),
                ("v1", Some(v1.clone())),
            ],
        );

        assert!(target.exists(), "should have seeded from v1 fallback");
        assert_eq!(std::fs::read(&target).unwrap(), std::fs::read(&v1).unwrap());
    }

    #[test]
    fn chain_no_op_when_target_exists() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mnemos.json");
        let legacy_v2 = dir.path().join("legacy-v2.json");

        std::fs::write(&target, b"existing-bytes").unwrap();
        std::fs::write(&legacy_v2, projects_payload("ignored")).unwrap();

        maybe_seed_from_sources(&target, &[("legacy-v2", Some(legacy_v2))]);

        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"existing-bytes",
            "existing target must not be overwritten"
        );
    }

    // ---- Per-project sync opt-out (TabState.sync_disabled_project_ids) ----

    #[test]
    fn sync_disabled_defaults_to_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert!(store.sync_disabled_ids().is_empty());
        assert!(!store.is_sync_disabled("anything"));
    }

    #[test]
    fn set_sync_disabled_is_idempotent_when_enabling() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.set_sync_disabled("a", true);
        store.set_sync_disabled("a", true);
        store.set_sync_disabled("a", true);
        assert_eq!(store.sync_disabled_ids(), &["a".to_string()]);
        assert!(store.is_sync_disabled("a"));
    }

    #[test]
    fn set_sync_disabled_false_removes_id() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.set_sync_disabled("a", true);
        store.set_sync_disabled("b", true);
        store.set_sync_disabled("a", false);
        assert_eq!(store.sync_disabled_ids(), &["b".to_string()]);
        assert!(!store.is_sync_disabled("a"));
        // Removing one that isn't there is a no-op.
        store.set_sync_disabled("ghost", false);
        assert_eq!(store.sync_disabled_ids(), &["b".to_string()]);
    }

    #[test]
    fn sync_disabled_round_trips_through_json() {
        let json = serde_json::json!({
            "openProjectIds": ["a"],
            "activeProjectId": "a",
            "tabOrder": ["a"],
            "syncDisabledProjectIds": ["a", "b"]
        });
        let state: TabState = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(state.sync_disabled_project_ids, vec!["a", "b"]);
        let serialized = serde_json::to_value(&state).unwrap();
        assert_eq!(serialized["syncDisabledProjectIds"][0], "a");
        assert_eq!(serialized["syncDisabledProjectIds"][1], "b");
    }

    #[test]
    fn sync_disabled_round_trips_through_workspace_state_file() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());

        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        store.add(sample_project("b"));
        store.tab_state = Some(TabState {
            open_project_ids: vec!["a".into(), "b".into()],
            active_project_id: Some("a".into()),
            tab_order: vec!["a".into(), "b".into()],
            sync_disabled_project_ids: vec!["b".into()],
        });
        store.save_blocking().unwrap();

        // Workspace state file carries the flag.
        let local: TabState =
            serde_json::from_slice(&std::fs::read(&wp).unwrap()).unwrap();
        assert_eq!(local.sync_disabled_project_ids, vec!["b".to_string()]);

        // Reload preserves it through the file-based path too.
        let reloaded = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert!(reloaded.is_sync_disabled("b"));
        assert!(!reloaded.is_sync_disabled("a"));
    }

    // ---- Cross-machine registry ----

    fn at(h: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, h, 0, 0).unwrap()
    }

    fn record(id: &str, path: &str, git: Option<&str>) -> Project {
        let mut p = Project {
            id: id.to_string(),
            name: id.to_string(),
            path: PathBuf::from(path),
            created_at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            last_opened_at: None,
            extra: serde_json::Map::new(),
        };
        if let Some(g) = git {
            p.set_git_repo(g);
        }
        p
    }

    #[test]
    fn register_or_claim_reuses_by_path_then_repo_then_creates() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(record("admin", "/Users/alice/code/Admin", Some("acme/Admin")));

        // Same checkout on the Mac: the existing record, now open.
        let id = store.register_or_claim_on(
            "darwin",
            Path::new("/Users/alice/code/Admin/"),
            "Admin",
            None,
            at(8),
        );
        assert_eq!(id, "admin");
        assert_eq!(store.get("admin").unwrap().is_open(), Some(true));

        // Windows checkout of the same repo claims the record's win32 slot.
        let id = store.register_or_claim_on(
            "win32",
            Path::new("C:\\code\\Admin"),
            "Admin",
            Some("acme/admin"),
            at(9),
        );
        assert_eq!(id, "admin");
        assert_eq!(store.projects.len(), 1);
        let admin = store.get("admin").unwrap();
        assert_eq!(admin.platform_path("win32"), Some(PathBuf::from("C:\\code\\Admin")));
        assert_eq!(admin.platform_path("darwin"), Some(PathBuf::from("/Users/alice/code/Admin")));

        // Unknown folder: a new open record.
        let id = store.register_or_claim_on("win32", Path::new("C:\\code\\new"), "new", None, at(10));
        assert_ne!(id, "admin");
        assert_eq!(store.open_project_ids(), vec!["admin".to_string(), id.clone()]);
        assert_eq!(store.get(&id).unwrap().platform_path("win32"), Some(PathBuf::from("C:\\code\\new")));
    }

    #[test]
    fn open_projects_in_order_uses_registry_flag_once_present() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("a"));
        store.add(sample_project("b"));
        store.add(sample_project("c"));
        // Legacy per-machine open set says a + b...
        store.tab_state = Some(TabState {
            open_project_ids: vec!["a".into(), "b".into()],
            active_project_id: None,
            tab_order: vec!["c".into(), "b".into()],
            sync_disabled_project_ids: Vec::new(),
        });
        // ...but once the registry carries flags, they decide.
        assert!(store.set_open("c", true, at(8)));
        assert!(store.set_open("b", true, at(8)));
        assert!(store.set_open("a", false, at(8)));
        assert!(!store.set_open("ghost", true, at(8)));
        let ids: Vec<String> = store.open_projects_in_order().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec!["c".to_string(), "b".to_string()]);
        assert_eq!(store.open_project_ids(), vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn delete_project_leaves_tombstone_and_add_lifts_it() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        let mut a = sample_project("a");
        a.touch(at(10));
        store.add(a.clone());
        // Deleting "before" the record's own clock still outranks it.
        assert!(store.delete_project("a", at(9)).is_some());
        assert!(store.get("a").is_none());
        assert!(store.is_known_id("a"));
        let tomb = &store.registry_value()["deletedProjects"]["a"];
        assert_eq!(tomb["deletedAt"], "2026-09-24T10:00:00.001Z");

        // A stale copy of the record coming back through a merge stays gone.
        let stale = serde_json::json!({ "projects": [a.clone()] });
        let merged = registry::merge(&store.registry_value(), &stale, "darwin");
        store.replace_registry(&merged).unwrap();
        assert!(store.get("a").is_none());

        // Re-adding is an explicit "this exists".
        let server = store.registry_value();
        store.add(a);
        assert!(store.get("a").is_some());
        store.save_blocking().unwrap();
        let reloaded = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert!(reloaded.get("a").is_some());
        // ...and it outlasts the tombstone every other machine still holds.
        assert_eq!(
            reloaded.get("a").unwrap().extra["updatedAt"],
            "2026-09-24T10:00:00.002Z"
        );
        for merged in [
            registry::merge(&reloaded.registry_value(), &server, "darwin"),
            registry::merge(&server, &reloaded.registry_value(), "win32"),
        ] {
            assert_eq!(merged["projects"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn registry_value_survives_a_non_unicode_path() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(sample_project("ok"));
        let mut bad = sample_project("bad");
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            bad.path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/bad\xff"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            bad.path = PathBuf::from(std::ffi::OsString::from_wide(&[0x43, 0x3a, 0x5c, 0xd800]));
        }
        store.add(bad);
        store.normalize();
        assert_eq!(store.projects.len(), 2, "one bad path must not empty the registry");
        store.save_blocking().unwrap();
        let reloaded = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert_eq!(reloaded.projects.len(), 2);
    }

    #[test]
    fn save_normalizes_and_ids_follow_replaced_by() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(record("old", "/Users/alice/code/tcbv2", Some("acme/TCB")));
        let mut newer = record("new", "C:\\code\\mnemos", Some("acme/TCB"));
        newer.created_at = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        store.add(newer);
        store.save_blocking().unwrap();

        let reloaded = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert_eq!(reloaded.projects.len(), 1);
        assert_eq!(reloaded.resolve_id("new").as_deref(), Some("old"));
        assert_eq!(reloaded.get("new").map(|p| p.id.as_str()), Some("old"));
        assert_eq!(
            reloaded.get("old").unwrap().platform_path("win32"),
            Some(PathBuf::from("C:\\code\\mnemos"))
        );
    }

    #[test]
    fn update_blocking_at_skips_noop_and_serializes_writers() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        // Nothing changed -> nothing written.
        ProjectStore::update_blocking_at(&pp, &wp, |_| ()).unwrap();
        assert!(!pp.exists());

        // Eight threads each add a record through the locked update; with a
        // plain load/save some of them would be lost.
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (pp, wp) = (pp.clone(), wp.clone());
                std::thread::spawn(move || {
                    ProjectStore::update_blocking_at(&pp, &wp, |s| {
                        s.add(sample_project(&format!("t{i}")));
                    })
                    .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert_eq!(store.projects.len(), 8);
    }

    #[test]
    fn a_tab_state_only_update_leaves_projects_json_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        ProjectStore::update_blocking_at(&pp, &wp, |s| s.add(sample_project("a"))).unwrap();
        let before = std::fs::read(&pp).unwrap();
        let mtime = std::fs::metadata(&pp).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        ProjectStore::update_blocking_at(&pp, &wp, |s| {
            s.tab_state.get_or_insert_with(TabState::default).active_project_id =
                Some("a".to_string());
        })
        .unwrap();
        // A new mtime on `projects.json` would wake the app's poll and the
        // sync watcher for nothing.
        assert_eq!(std::fs::metadata(&pp).unwrap().modified().unwrap(), mtime);
        assert_eq!(std::fs::read(&pp).unwrap(), before);
        let reloaded = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        assert_eq!(
            reloaded.tab_state.and_then(|t| t.active_project_id).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn normalize_never_folds_a_sync_disabled_record() {
        let dir = tempfile::tempdir().unwrap();
        let (pp, wp) = paths(dir.path());
        let mut store = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        store.add(record("win-d", "C:\\code\\mnemos", Some("acme/TCB")));
        let mut mac = record("mac-r", "/Users/alice/code/tcbv2", Some("acme/TCB"));
        mac.created_at = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        store.add(mac);
        store.set_sync_disabled("win-d", true);
        store.normalize();
        store.save_blocking().unwrap();
        let reloaded = ProjectStore::load_blocking_from(&pp, &wp).unwrap();
        let ids: Vec<&str> = reloaded.projects.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["win-d", "mac-r"]);
        assert!(!reloaded.is_known_id("nothing"));
        assert_eq!(reloaded.resolve_id("mac-r").as_deref(), Some("mac-r"));
    }

    #[test]
    fn chain_skips_corrupt_legacy_v2() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mnemos.json");
        let legacy_v2_corrupt = dir.path().join("legacy-v2.json");
        let v1 = dir.path().join("v1.json");

        std::fs::write(&legacy_v2_corrupt, b"{not json").unwrap();
        std::fs::write(&v1, projects_payload("from-v1")).unwrap();

        maybe_seed_from_sources(
            &target,
            &[
                ("legacy-v2", Some(legacy_v2_corrupt)),
                ("v1", Some(v1.clone())),
            ],
        );

        assert!(
            target.exists(),
            "corrupt legacy-v2 must fall through to v1, not abort"
        );
        assert_eq!(std::fs::read(&target).unwrap(), std::fs::read(&v1).unwrap());
    }
}
