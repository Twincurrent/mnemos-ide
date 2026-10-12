use std::path::{Path, PathBuf};

use chrono::TimeZone;
use mnemos_ide_projects::{OpenTab, ReconcilePlan};

use super::*;

// Every store here is a temp file of its own: the app tests share one
// redirected Mnemos IDE data dir per process, and nothing may write to it.

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

fn record(store: &mut ProjectStore, id: &str, open: bool) {
    let mut p = Project::new(id, PathBuf::from(format!("/tmp/{id}")));
    p.id = id.to_string();
    p.set_open(open, now() - chrono::Duration::hours(1));
    store.add(p);
}

#[test]
fn closing_the_last_tab_closes_the_project_but_keeps_the_record() {
    let (_dir, mut store) = store();
    record(&mut store, "a", true);
    assert!(close_unless_linked(&mut store, "a", &[], now()));
    let a = store
        .get("a")
        .expect("closing a tab never removes the record");
    assert_eq!(a.is_open(), Some(false));
    // Already closed: no second stamp (it would only cause a sync push).
    let before = store.registry_value();
    assert!(!close_unless_linked(&mut store, "a", &[], now()));
    assert_eq!(store.registry_value(), before);
}

#[test]
fn a_project_still_shown_by_another_tab_stays_open() {
    let (_dir, mut store) = store();
    record(&mut store, "a", true);
    let linked = vec!["other".to_string(), "a".to_string()];
    assert!(!close_unless_linked(&mut store, "a", &linked, now()));
    assert_eq!(store.get("a").unwrap().is_open(), Some(true));
}

#[test]
fn a_tab_holding_a_folded_id_still_counts_as_showing_the_project() {
    let (_dir, mut store) = store();
    record(&mut store, "survivor", true);
    record(&mut store, "old", true);
    // "old" was folded into "survivor" (same repo registered twice).
    store.delete_project("old", now());
    let mut value = store.registry_value();
    value["deletedProjects"]["old"]["replacedBy"] = serde_json::json!("survivor");
    store.replace_registry(&value).unwrap();
    assert_eq!(store.resolve_id("old").as_deref(), Some("survivor"));

    assert!(!close_unless_linked(
        &mut store,
        "survivor",
        &["old".to_string()],
        now()
    ));
    assert_eq!(store.get("survivor").unwrap().is_open(), Some(true));
    // Closing through the old id closes the survivor.
    assert!(close_unless_linked(&mut store, "old", &[], now()));
    assert_eq!(store.get("survivor").unwrap().is_open(), Some(false));
}

#[test]
fn an_orphan_id_closes_nothing() {
    let (_dir, mut store) = store();
    record(&mut store, "a", true);
    let before = store.registry_value();
    assert!(!close_unless_linked(&mut store, "ghost", &[], now()));
    assert_eq!(store.registry_value(), before);
}

#[test]
fn switching_to_a_tab_reopens_only_projects_closed_before_this_session() {
    let (_dir, mut store) = store();
    // `record` stamps the open flag one hour before `now()`.
    record(&mut store, "stale", false);
    record(&mut store, "fresh", false);
    record(&mut store, "open", true);
    let session_start = now() - chrono::Duration::minutes(30);
    // Closed (as by the 2026-10-02 shutdown) before this session: healed.
    assert!(reopen_if_closed_before(&mut store, "stale", session_start, now()));
    assert_eq!(store.get("stale").and_then(Project::is_open), Some(true));

    // Closed on the other machine while this session runs: a decision, left alone.
    store.set_open("fresh", true, now() - chrono::Duration::minutes(10));
    store.set_open("fresh", false, now() - chrono::Duration::minutes(5));
    assert!(!reopen_if_closed_before(&mut store, "fresh", session_start, now()));
    assert_eq!(store.get("fresh").and_then(Project::is_open), Some(false));

    // Already open, or unknown: no write, so no sync push.
    assert!(!reopen_if_closed_before(&mut store, "open", session_start, now()));
    assert!(!reopen_if_closed_before(&mut store, "ghost", session_start, now()));
}

#[test]
fn restoring_a_closed_tab_reopens_its_project() {
    let (_dir, mut store) = store();
    record(&mut store, "a", false);
    assert!(reopen(&mut store, "a", now()));
    assert_eq!(store.get("a").unwrap().is_open(), Some(true));
    assert!(!reopen(&mut store, "a", now()), "already open");
    assert!(!reopen(&mut store, "ghost", now()));
}

/// This machine as data: one checkout with a remote.
struct OneCheckout {
    dir: &'static str,
    remote: &'static str,
}

impl FsEnv for OneCheckout {
    fn platform(&self) -> &'static str {
        "linux"
    }
    fn is_dir(&self, path: &Path) -> bool {
        path == Path::new(self.dir)
    }
    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/home/h"))
    }
    fn git_remote(&self, dir: &Path) -> Option<String> {
        (dir == Path::new(self.dir)).then(|| self.remote.to_string())
    }
    fn git_root(&self, _cwd: &Path) -> Option<PathBuf> {
        None
    }
    fn list_dirs(&self, _dir: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
}

#[test]
fn add_claims_the_record_another_machine_registered() {
    let (_dir, mut store) = store();
    // Registered on the Mac, closed there; this machine has no checkout slot.
    let mut mac = Project::new("tcbv2", PathBuf::from("/Users/alice/code/tcbv2"));
    mac.id = "tcbv2".into();
    mac.set_git_repo("acme/TCB");
    mac.set_open(false, now() - chrono::Duration::hours(1));
    store.add(mac);

    let fs = OneCheckout {
        dir: "/home/h/code/mnemos",
        remote: "acme/TCB",
    };
    let added = add_project(&mut store, Path::new("/home/h/code/mnemos"), &fs, now());
    assert_eq!(
        added,
        AddedProject {
            id: "tcbv2".into(),
            title: "tcbv2".into()
        },
        "same repo, no checkout here yet: claimed, keeping its name"
    );
    let record = store.get("tcbv2").unwrap();
    assert_eq!(record.is_open(), Some(true));
    assert_eq!(
        record.platform_path("linux"),
        Some(PathBuf::from("/home/h/code/mnemos"))
    );
    assert_eq!(store.projects.len(), 1);

    // Picking the same folder again reuses the record.
    let again = add_project(&mut store, Path::new("/home/h/code/mnemos"), &fs, now());
    assert_eq!(again.id, "tcbv2");
    assert_eq!(store.projects.len(), 1);
}

#[test]
fn add_registers_a_new_folder_as_an_open_project() {
    let (_dir, mut store) = store();
    let fs = OneCheckout {
        dir: "/home/h/code/fresh",
        remote: "o/fresh",
    };
    let added = add_project(&mut store, Path::new("/home/h/code/fresh"), &fs, now());
    assert_eq!(added.title, "fresh");
    let record = store.get(&added.id).unwrap();
    assert_eq!(record.is_open(), Some(true));
    assert_eq!(record.git_repo().as_deref(), Some("o/fresh"));
}

#[test]
fn user_actions_jump_a_waiting_reconcile_and_reconciles_coalesce() {
    let closed = |id: &str| IdeJob::TabClosed {
        project_id: id.to_string(),
    };
    let mut queue = VecDeque::new();
    queue_job(&mut queue, IdeJob::Reconcile);
    queue_job(&mut queue, IdeJob::Reconcile);
    // Closed while a poll-triggered reconcile waits: the close goes first,
    // or the reconcile would find the project open without a tab and
    // reopen it.
    queue_job(&mut queue, closed("a"));
    queue_job(&mut queue, closed("b"));
    queue_job(&mut queue, IdeJob::Reconcile);
    assert_eq!(
        Vec::from(queue.clone()),
        vec![closed("a"), closed("b"), IdeJob::Reconcile]
    );

    queue.clear();
    queue_job(&mut queue, closed("a"));
    queue_job(&mut queue, IdeJob::Reconcile);
    queue_job(
        &mut queue,
        IdeJob::TabReopened {
            project_id: "a".to_string(),
        },
    );
    assert_eq!(
        Vec::from(queue),
        vec![
            closed("a"),
            IdeJob::TabReopened {
                project_id: "a".to_string()
            },
            IdeJob::Reconcile,
        ]
    );
}

#[test]
fn plan_keys_map_back_to_tabs_and_closes_follow_relinks() {
    let w1 = WindowId::from_usize(1);
    let w2 = WindowId::from_usize(2);
    let tab = |window_id, pane_group: usize, id: Option<&str>| IdeTabRef {
        window_id,
        pane_group_id: EntityId::from_usize(pane_group),
        project_id: id.map(str::to_string),
    };
    let refs = vec![
        tab(w1, 10, Some("stale")),
        tab(w2, 20, Some("b")),
        tab(w2, 21, None),
    ];
    let plan = ReconcilePlan {
        relink: vec![(0, Some("folded".into())), (2, Some("c".into()))],
        open: vec![OpenTab {
            project_id: "d".into(),
            cwd: Some(PathBuf::from("/home/h/code/d")),
            title: "d".into(),
        }],
        close: vec![0, 1],
        kept: Vec::new(),
        store_changed: true,
    };
    let update = update_from_plan(&plan, &refs, Some(w1), Vec::new());
    assert_eq!(
        update.relink,
        vec![
            (refs[0].clone(), Some("folded".to_string())),
            (refs[2].clone(), Some("c".to_string())),
        ]
    );
    // Tab 0 is relinked before it is closed, so the close expects the new id.
    assert_eq!(
        update.close,
        vec![tab(w1, 10, Some("folded")), refs[1].clone()]
    );
    assert_eq!(
        update.open,
        vec![IdeTabOpen {
            window_id: w1,
            project_id: "d".into(),
            title: "d".into(),
            cwd: Some(PathBuf::from("/home/h/code/d")),
            focus: false,
        }]
    );

    let no_window = update_from_plan(&plan, &refs, None, Vec::new());
    assert!(no_window.open.is_empty(), "no window to open tabs in");
}

#[test]
fn a_tab_kept_busy_is_spared_until_its_project_reopens() {
    let w1 = WindowId::from_usize(1);
    let tab = |pane_group: usize, id: &str| IdeTabRef {
        window_id: w1,
        pane_group_id: EntityId::from_usize(pane_group),
        project_id: Some(id.to_string()),
    };
    let refs = vec![tab(10, "old-p"), tab(11, "q")];
    let plan = ReconcilePlan {
        relink: vec![(0, Some("p".into()))],
        kept: vec![0, 1],
        ..Default::default()
    };
    let mut registry = WorkspaceRegistry::new();
    registry.ide_note_spared(&plan, &refs, &[]);
    assert_eq!(
        registry
            .ide
            .spared
            .get(&EntityId::from_usize(10))
            .map(String::as_str),
        Some("p"),
        "remembered under the id it carries after the relink"
    );
    assert_eq!(
        registry
            .ide
            .spared
            .get(&EntityId::from_usize(11))
            .map(String::as_str),
        Some("q")
    );

    // `q` was opened again somewhere: its tab is no longer spared, so a later
    // close of `q` applies to it.
    let mut q = Project::new("q", PathBuf::from("/tmp/q"));
    q.id = "q".into();
    registry.ide_note_spared(&ReconcilePlan::default(), &refs, &[q]);
    assert!(registry.ide.spared.contains_key(&EntityId::from_usize(10)));
    assert!(!registry.ide.spared.contains_key(&EntityId::from_usize(11)));
}

#[test]
fn the_active_project_is_recorded_locally_and_only_the_latest_waits() {
    let (_dir, mut store) = store();
    record(&mut store, "survivor", true);
    let before = store.registry_value();
    assert!(set_active_project(&mut store, "survivor"));
    assert_eq!(
        store
            .tab_state
            .as_ref()
            .and_then(|t| t.active_project_id.as_deref()),
        Some("survivor")
    );
    assert!(!set_active_project(&mut store, "survivor"), "unchanged");
    assert!(!set_active_project(&mut store, "ghost"));
    assert_eq!(
        store.registry_value(),
        before,
        "the synced registry is untouched"
    );

    let active = |id: &str| IdeJob::Activated {
        project_id: id.to_string(),
    };
    let mut queue = VecDeque::new();
    queue_job(&mut queue, IdeJob::Reconcile);
    queue_job(&mut queue, active("a"));
    queue_job(&mut queue, active("b"));
    assert_eq!(Vec::from(queue), vec![active("b"), IdeJob::Reconcile]);
}
