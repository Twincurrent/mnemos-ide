// SPDX-License-Identifier: AGPL-3.0-only
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::registry;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
    /// Optional in v1 — v1 stores `updatedAt` instead, which lands in `extra`.
    #[serde(default)]
    pub last_opened_at: Option<DateTime<Utc>>,

    /// Catch-all for v1 fields the Rust runtime doesn't model strictly
    /// (e.g. `gitRepo`, `autoBuildPath`, `settings`, `updatedAt`). Lets v1
    /// sync round-trip lossless against mnemos-ide-core.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Project {
    /// Build a fresh `Project` from a folder pick — generates a v1-style id
    /// (`project-<unix-millis>`) and stamps `created_at` / `last_opened_at`
    /// to now. `extra` is empty; the v1 frontend can fill in `gitRepo` etc.
    /// on its next sync.
    pub fn new(name: impl Into<String>, path: PathBuf) -> Self {
        let now = Utc::now();
        let id = format!("project-{}", now.timestamp_millis());
        Self {
            id,
            name: name.into(),
            path,
            created_at: now,
            last_opened_at: Some(now),
            extra: serde_json::Map::new(),
        }
    }

    // Cross-machine fields. They live in `extra` rather than as struct fields
    // so the serde shape of `Project` (and every v1 file it round-trips) stays
    // exactly as it was; see `registry` for the rules that read them.

    /// The `paths` map (`platform -> checkout path`), empty slots skipped.
    pub fn paths(&self) -> BTreeMap<String, String> {
        self.extra
            .get("paths")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| {
                        v.as_str()
                            .filter(|s| !s.is_empty())
                            .map(|s| (k.clone(), s.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// This project's checkout on `platform`, without touching the disk:
    /// `paths[platform]`, else the legacy `path` when its shape belongs to
    /// `platform`. Same rule as [`registry::record_local_path`].
    pub fn platform_path(&self, platform: &str) -> Option<PathBuf> {
        if let Some(p) = self.paths().remove(platform) {
            return Some(PathBuf::from(p));
        }
        let bare = self.path.to_string_lossy();
        (registry::platform_of_path(&bare) == Some(platform)).then(|| self.path.clone())
    }

    /// Record `path` as this project's checkout on `platform`.
    pub fn set_platform_path(&mut self, platform: &str, path: &Path) {
        let slot = self
            .extra
            .entry("paths".to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if !slot.is_object() {
            *slot = Value::Object(serde_json::Map::new());
        }
        if let Some(map) = slot.as_object_mut() {
            map.insert(
                platform.to_string(),
                Value::String(path.to_string_lossy().into_owned()),
            );
        }
    }

    /// [`Self::platform_path`] for the platform this build runs on. Use this,
    /// not `path`, anywhere a directory on this machine is needed: `path` is
    /// whatever machine registered the project first.
    pub fn local_path(&self) -> Option<PathBuf> {
        self.platform_path(registry::current_platform())
    }

    /// `owner/repo`, when known and non-empty.
    pub fn git_repo(&self) -> Option<String> {
        self.extra
            .get("gitRepo")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    pub fn set_git_repo(&mut self, repo: &str) {
        self.extra
            .insert("gitRepo".to_string(), Value::String(repo.to_string()));
    }

    /// `Some(true)` when the project should show as a tab on every machine,
    /// `Some(false)` when it was closed, `None` for records from before the
    /// flag existed.
    pub fn is_open(&self) -> Option<bool> {
        self.extra.get("open").and_then(Value::as_bool)
    }

    /// Set the synced open flag and stamp its clock (`openChangedAt`) plus
    /// `updatedAt`, so the newer choice wins when machines merge.
    ///
    /// Each clock moves strictly forward from the value it replaces, even
    /// when `now` is behind it: the stamp may have come from a machine whose
    /// clock runs ahead, and this change is made after seeing that state, so
    /// it must outrank it or the merge would silently undo it.
    pub fn set_open(&mut self, open: bool, now: DateTime<Utc>) {
        let opened = advance(self.open_changed_at(), now);
        let updated = advance(self.updated_at(), now);
        self.extra.insert("open".to_string(), Value::Bool(open));
        self.extra.insert(
            "openChangedAt".to_string(),
            Value::String(registry::format_timestamp(opened)),
        );
        self.extra.insert(
            "updatedAt".to_string(),
            Value::String(registry::format_timestamp(updated)),
        );
    }

    /// Stamp `updatedAt`, which picks the winning copy of the record's
    /// ordinary fields on merge. Moves strictly forward, like
    /// [`Self::set_open`].
    pub fn touch(&mut self, now: DateTime<Utc>) {
        let updated = advance(self.updated_at(), now);
        self.extra.insert(
            "updatedAt".to_string(),
            Value::String(registry::format_timestamp(updated)),
        );
    }

    pub fn updated_at(&self) -> Option<DateTime<Utc>> {
        self.extra
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(registry::parse_timestamp)
    }

    pub fn open_changed_at(&self) -> Option<DateTime<Utc>> {
        self.extra
            .get("openChangedAt")
            .and_then(Value::as_str)
            .and_then(registry::parse_timestamp)
    }
}

/// `now`, or 1 ms past `prev` when `prev` is not older than `now`.
fn advance(prev: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    match prev {
        Some(p) if p >= now => p + chrono::Duration::milliseconds(1),
        _ => now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_project() -> Project {
        Project {
            id: "abc123".to_string(),
            name: "TwinCurrent Build".to_string(),
            path: PathBuf::from("/Users/alice/code/tcbv2"),
            created_at: Utc.with_ymd_and_hms(2026, 4, 29, 10, 30, 0).unwrap(),
            last_opened_at: Some(Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap()),
            extra: serde_json::Map::new(),
        }
    }

    #[test]
    fn project_round_trips_through_json() {
        let project = sample_project();
        let json = serde_json::to_string(&project).expect("serialize");
        let back: Project = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(project, back);
    }




    #[test]
    fn project_preserves_unknown_v1_fields() {
        let raw = r#"{
            "id": "abc123",
            "name": "Admin",
            "path": "/Users/alice/code/admin",
            "createdAt": "2026-04-29T10:30:00Z",
            "lastOpenedAt": null,
            "gitRepo": "acme/Admin",
            "autoBuildPath": "/Users/alice/code/admin/.twincurrent-build",
            "settings": { "model": "claude-opus-4-7", "memoryBackend": "graphiti" }
        }"#;
        let project: Project = serde_json::from_str(raw).unwrap();
        assert_eq!(project.id, "abc123");
        assert_eq!(project.name, "Admin");
        assert!(project.extra.contains_key("gitRepo"));
        assert!(project.extra.contains_key("autoBuildPath"));
        assert!(project.extra.contains_key("settings"));

        // Round-trip preserves the unknown keys.
        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(project, back);
        assert_eq!(
            back.extra.get("gitRepo").and_then(|v| v.as_str()),
            Some("acme/Admin")
        );
    }

    #[test]
    fn cross_machine_helpers_live_in_extra() {
        let mut p = sample_project();
        assert_eq!(p.platform_path("darwin"), Some(PathBuf::from("/Users/alice/code/tcbv2")));
        assert_eq!(p.platform_path("win32"), None);
        p.set_platform_path("win32", Path::new(r"C:\code\mnemos"));
        assert_eq!(p.platform_path("win32"), Some(PathBuf::from(r"C:\code\mnemos")));
        assert_eq!(p.paths().len(), 1);

        assert_eq!(p.git_repo(), None);
        p.set_git_repo("acme/TCB");
        assert_eq!(p.git_repo().as_deref(), Some("acme/TCB"));

        assert_eq!(p.is_open(), None);
        let now = Utc.with_ymd_and_hms(2026, 9, 24, 8, 0, 0).unwrap();
        p.set_open(true, now);
        assert_eq!(p.is_open(), Some(true));
        assert_eq!(p.open_changed_at(), Some(now));
        assert_eq!(p.updated_at(), Some(now));
        assert_eq!(p.extra["openChangedAt"], "2026-09-24T08:00:00Z");

        // Serde shape unchanged: the new fields ride along in `extra`.
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["paths"]["win32"], r"C:\code\mnemos");
        assert_eq!(json["open"], true);
        let back: Project = serde_json::from_value(json).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn open_clock_outranks_a_skewed_stamp() {
        // The other machine's clock ran an hour ahead when it opened the
        // project; closing it here "earlier" must still win the merge.
        let mut p = sample_project();
        let now = Utc.with_ymd_and_hms(2026, 9, 24, 8, 0, 0).unwrap();
        let ahead = now + chrono::Duration::hours(1);
        p.set_open(true, ahead);
        p.set_open(false, now);
        assert_eq!(p.is_open(), Some(false));
        let tick = ahead + chrono::Duration::milliseconds(1);
        assert_eq!(p.open_changed_at(), Some(tick));
        assert_eq!(p.updated_at(), Some(tick));
        p.touch(now);
        assert_eq!(p.updated_at(), Some(tick + chrono::Duration::milliseconds(1)));
    }
}
