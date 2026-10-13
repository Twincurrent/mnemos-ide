use std::time::SystemTime;

use chrono::Utc;
use mnemos_ide_sync::live_agents::LiveAgent;

use super::*;

fn tab(project_id: Option<&str>, cwds: &[&str]) -> TabFacts {
    TabFacts {
        project_id: project_id.map(Into::into),
        cwds: cwds.iter().map(PathBuf::from).collect(),
    }
}

#[test]
fn resume_finds_the_projects_tab_by_id_preferring_the_active_one() {
    let tabs = vec![
        tab(Some("a"), &["/code/a"]),
        tab(Some("b"), &["/code/b"]),
        tab(Some("b"), &["/code/b/sub"]),
    ];
    let root = Path::new("/code/b");
    assert_eq!(
        resume_tab(&tabs, 0, Some("b"), root, MatchBy::ProjectId),
        Some(1)
    );
    assert_eq!(
        resume_tab(&tabs, 2, Some("b"), root, MatchBy::ProjectId),
        Some(2)
    );
    assert_eq!(
        resume_tab(&tabs, 0, Some("c"), root, MatchBy::ProjectId),
        None
    );
    // A session outside every project never matches by id.
    assert_eq!(resume_tab(&tabs, 0, None, root, MatchBy::ProjectId), None);
}

#[test]
fn resume_falls_back_to_a_tab_whose_terminal_is_under_the_root() {
    let tabs = vec![
        tab(None, &["/code/other"]),
        tab(None, &["/home", "/code/foo/app"]),
        tab(None, &["/code/foobar"]),
    ];
    let root = Path::new("/code/foo");
    assert_eq!(
        resume_tab(&tabs, 0, Some("foo"), root, MatchBy::Cwd),
        Some(1)
    );
    // `/code/foobar` is not under `/code/foo`.
    assert_eq!(
        resume_tab(&tabs[2..], 0, Some("foo"), root, MatchBy::Cwd),
        None
    );
    assert_eq!(resume_tab(&[], 0, Some("foo"), root, MatchBy::Cwd), None);
}

/// A nested project's tab has every terminal under the outer root, and
/// another project's tab can have a terminal cd'd into it: neither is the
/// outer project's tab.
#[test]
fn resume_by_cwd_never_picks_another_projects_tab() {
    let tabs = vec![
        tab(Some("b"), &["/code/a/b"]),
        tab(Some("c"), &["/code/c", "/code/a"]),
    ];
    let root = Path::new("/code/a");
    assert_eq!(resume_tab(&tabs, 0, Some("a"), root, MatchBy::Cwd), None);
    // An unlinked tab, or the project's own, still matches.
    let tabs = vec![
        tab(Some("b"), &["/code/a/b"]),
        tab(None, &["/code/a/x"]),
        tab(Some("a"), &["/code/a"]),
    ];
    assert_eq!(resume_tab(&tabs, 0, Some("a"), root, MatchBy::Cwd), Some(1));
    assert_eq!(resume_tab(&tabs, 2, Some("a"), root, MatchBy::Cwd), Some(2));
}

fn machine(label: &str, agents: Vec<LiveAgent>) -> MachineAgents {
    MachineAgents {
        machine: label.to_lowercase(),
        label: label.into(),
        platform: "win32".into(),
        updated_at: Utc::now(),
        agents,
        machine_id: None,
    }
}

fn agent(id: Option<&str>, url: Option<&str>) -> LiveAgent {
    LiveAgent {
        id: id.map(Into::into),
        project: "p".into(),
        agent: "claude".into(),
        status: "running".into(),
        query: None,
        response: None,
        tool: None,
        last_activity: None,
        remote_url: url.map(Into::into),
        background_tasks: 0,
    }
}

#[test]
fn live_elsewhere_keys_by_session_id_and_keeps_only_remote_control_urls() {
    let ok = "https://claude.ai/code/session_01Test";
    let map = live_elsewhere(&[machine(
        "ALICE-PC",
        vec![
            agent(Some("s1"), Some(ok)),
            agent(Some("s2"), Some("https://evil.example/x")),
            agent(None, Some(ok)),
            agent(Some(""), None),
        ],
    )]);
    assert_eq!(map.len(), 2);
    assert_eq!(map["s1"], ("ALICE-PC".to_string(), Some(ok.to_string())));
    assert_eq!(map["s2"], ("ALICE-PC".to_string(), None));
}

fn meta(id: &str, size_bytes: u64) -> SessionMeta {
    SessionMeta {
        id: id.into(),
        path: PathBuf::from(format!("/x/{id}.jsonl")),
        cwd: Some(PathBuf::from("/code/p")),
        git_branch: Some("main".into()),
        title: format!("title {id}"),
        title_is_ai: true,
        modified: SystemTime::UNIX_EPOCH,
        size_bytes,
    }
}

fn scan_of(sessions: Vec<(SessionMeta, bool)>) -> SessionScan {
    SessionScan {
        groups: vec![ScannedGroup {
            root: ScanRoot {
                project_id: Some("p".into()),
                name: "P".into(),
                root: PathBuf::from("/code/p"),
            },
            sessions: sessions
                .into_iter()
                .map(|(meta, cwd_exists)| ScannedSession {
                    cwd: meta.cwd.clone().unwrap(),
                    meta,
                    cwd_exists,
                })
                .collect(),
        }],
        origins: HashMap::new(),
    }
}

#[test]
fn a_session_live_elsewhere_is_never_offered_in_place() {
    let scan = scan_of(vec![
        (meta("here", 1), true),
        (meta("there", 1), true),
        (meta("gone", 1), false),
        (meta("idle", 1), true),
        // Running here wins over everything else.
        (meta("both", 1), false),
    ]);
    let here = HashMap::from([("here".to_string(), 7u64), ("both".to_string(), 9u64)]);
    let elsewhere = HashMap::from([
        ("there".to_string(), ("ALICE-PC".to_string(), None)),
        ("both".to_string(), ("ALICE-PC".to_string(), None)),
    ]);
    let groups = session_groups(&scan, &here, &elsewhere, SystemTime::now());
    let states: Vec<&SessionState> = groups[0].rows.iter().map(|r| &r.state).collect();
    assert_eq!(
        states,
        vec![
            &SessionState::LiveHere {
                terminal_view_id_bits: 7
            },
            &SessionState::LiveElsewhere {
                machine: "ALICE-PC".into(),
                remote_url: None
            },
            &SessionState::FolderMissing,
            &SessionState::Resumable,
            &SessionState::LiveHere {
                terminal_view_id_bits: 9
            },
        ]
    );
    assert_eq!(groups[0].project_id.as_deref(), Some("p"));
    assert_eq!(groups[0].rows[0].branch.as_deref(), Some("main"));
}

#[test]
fn from_another_machine_only_while_nothing_ran_here_since() {
    let mut scan = scan_of(vec![
        (meta("pulled", 100), true),
        (meta("grown", 150), true),
    ]);
    for sid in ["pulled", "grown"] {
        scan.origins.insert(
            sid.to_string(),
            PulledOrigin {
                platform: "win32".into(),
                bytes: 100,
                at: None,
            },
        );
    }
    let groups = session_groups(&scan, &HashMap::new(), &HashMap::new(), SystemTime::now());
    assert_eq!(groups[0].rows[0].origin.as_deref(), Some("Windows"));
    assert_eq!(groups[0].rows[1].origin, None);
}

/// A pull from another machine minutes ago with nothing run here since: no
/// machine lists the session, but that one may still run it, so it is not
/// offered in place. The window counts from the pull here, not from the
/// file's mtime (the newest line's time on the other machine). Older pulls,
/// and sessions that ran here after the pull, are resumable again.
#[test]
fn a_session_pulled_minutes_ago_is_not_offered_in_place() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
    let ago = |secs: u64| now - Duration::from_secs(secs);
    let window = RECENT_ELSEWHERE.as_secs();
    let at = |secs_ago: u64, mut m: SessionMeta| {
        m.modified = ago(secs_ago);
        m
    };
    let mut scan = scan_of(vec![
        // Written 20 minutes ago over there, uploaded and pulled a minute ago.
        (at(20 * 60, meta("fresh", 100)), true),
        (at(window + 120, meta("old", 100)), true),
        (at(window, meta("edge", 100)), true),
        (at(window - 1, meta("inside", 100)), true),
        (at(60, meta("grown", 150)), true),
        (at(60, meta("shrunk", 50)), true),
        (at(60, meta("legacy", 100)), true),
        (at(60, meta("local", 100)), true),
        (at(60, meta("running", 100)), true),
    ]);
    let pulls: [(&str, u64, Option<u64>); 7] = [
        ("fresh", 100, Some(60)),
        ("old", 100, Some(window + 60)),
        ("edge", 100, Some(window)),
        ("inside", 100, Some(window - 1)),
        ("grown", 100, Some(60)),
        // A pull left more than the file holds now: it was cut back here.
        ("shrunk", 100, Some(60)),
        // Recorded by an older build: no pull time, the mtime stands in.
        ("legacy", 100, None),
    ];
    for (sid, bytes, secs_ago) in pulls.into_iter().chain([("running", 100, Some(60))]) {
        scan.origins.insert(
            sid.to_string(),
            PulledOrigin {
                platform: "win32".into(),
                bytes,
                at: secs_ago.map(ago),
            },
        );
    }
    let elsewhere = HashMap::from([("running".to_string(), ("ALICE-PC".to_string(), None))]);
    let groups = session_groups(&scan, &HashMap::new(), &elsewhere, now);
    let states: Vec<(&str, &SessionState)> = groups[0]
        .rows
        .iter()
        .map(|r| (r.id.as_str(), &r.state))
        .collect();
    assert_eq!(
        states,
        vec![
            ("fresh", &SessionState::PulledRecently),
            ("old", &SessionState::Resumable),
            // Exactly the window ago: no longer recent.
            ("edge", &SessionState::Resumable),
            ("inside", &SessionState::PulledRecently),
            ("grown", &SessionState::Resumable),
            ("shrunk", &SessionState::Resumable),
            ("legacy", &SessionState::PulledRecently),
            ("local", &SessionState::Resumable),
            (
                "running",
                &SessionState::LiveElsewhere {
                    machine: "ALICE-PC".into(),
                    remote_url: None
                }
            ),
        ]
    );
    // The "other machine" note still shows on the recent one.
    assert_eq!(groups[0].rows[0].origin.as_deref(), Some("Windows"));
}

fn publisher(platform: &str, ids: &[&str]) -> MachineAgents {
    MachineAgents {
        machine: platform.into(),
        label: platform.into(),
        platform: platform.into(),
        updated_at: Utc::now(),
        agents: ids
            .iter()
            .map(|id| LiveAgent {
                id: Some((*id).into()),
                project: "p".into(),
                agent: "claude".into(),
                status: "idle".into(),
                query: None,
                response: None,
                tool: None,
                last_activity: None,
                remote_url: None,
                background_tasks: 0,
            })
            .collect(),
        machine_id: None,
    }
}

/// A list is definitive only for a machine that publishes one: the live
/// feed is off by default, so a missing machine proves nothing.
#[test]
fn agents_live_is_definitive_only_for_machines_that_publish() {
    let mac = [publisher("darwin", &["m1"])];
    assert_eq!(agents_live_for(None, "s", Some("win32")), AgentsLive::Unknown);
    assert_eq!(agents_live_for(Some(&[]), "s", None), AgentsLive::Unknown);
    // The turns came from Windows, which publishes nothing.
    assert_eq!(agents_live_for(Some(&mac), "s", Some("win32")), AgentsLive::Unknown);
    assert_eq!(agents_live_for(Some(&mac), "s", Some("darwin")), AgentsLive::NotListed);
    assert_eq!(agents_live_for(Some(&mac), "s", None), AgentsLive::NotListed);
    assert_eq!(
        agents_live_for(Some(&mac), "m1", Some("win32")),
        AgentsLive::ListedElsewhere
    );
}

/// Restore waits only while signed in, only before the deadline, and only
/// until both the live lists and the first transcript pull are in.
#[test]
fn restore_waits_only_signed_in_until_both_answers_or_the_deadline() {
    // Signed out: never, whatever is pending.
    assert!(!restore_should_wait(false, true, true, true));
    assert!(restore_should_wait(true, true, false, true));
    assert!(restore_should_wait(true, false, true, true));
    assert!(!restore_should_wait(true, false, false, true));
    // The deadline passed.
    assert!(!restore_should_wait(true, true, true, false));
}

/// Restore types into a pane only once its shell is up, and only while the
/// pane is untouched: a command parked during startup would take whatever
/// the user types before the shell is ready along with it.
#[test]
fn restore_waits_for_the_shell_and_never_types_into_a_used_pane() {
    use RestorePaneStep::*;
    assert_eq!(restore_pane_step(true, true, true), Run);
    assert_eq!(restore_pane_step(true, true, false), Run);
    // Still bootstrapping: wait, then give up.
    assert_eq!(restore_pane_step(true, false, true), Wait);
    assert_eq!(restore_pane_step(true, false, false), Defer);
    // Typed into (during startup too): never.
    assert_eq!(restore_pane_step(false, true, true), Defer);
    assert_eq!(restore_pane_step(false, false, true), Defer);
}

/// A restore entry carried over to the next launch is dropped only when the
/// list resumed that very session in place: not for a fork of it (the
/// original stays the pane's session) and not when nothing launched.
#[test]
fn only_a_launched_in_place_resume_forgets_the_carried_over_restore() {
    let sid = "5e55e55e-0000-4000-8000-00000000c1c1";
    let kept = || {
        mnemos_ide_agent_restore::with_deferred(Vec::new())
            .iter()
            .any(|e| e.session_id == sid)
    };
    mnemos_ide_agent_restore::defer(mnemos_ide_agent_restore::SessionEntry {
        pane_uuid_hex: "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1".into(),
        session_id: sid.into(),
        cwd: None,
        agent: "Claude".into(),
        last_live_unix: Some(1),
        skip_permissions: false,
    });
    assert!(kept());
    note_resumed_from_list(sid, true, true);
    assert!(kept(), "a fork leaves the original's restore alone");
    note_resumed_from_list(sid, false, false);
    assert!(kept(), "a launch that never happened resumed nothing");
    note_resumed_from_list(sid, false, true);
    assert!(!kept());
}

/// A click on a row the last scan called resumable looks again: a pull
/// since that scan forks it.
#[test]
fn a_click_rechecks_the_transcript_it_resumes() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
    let ago = |secs: u64| now - Duration::from_secs(secs);
    // The file grew by a pull 5 s ago; the scan saw it before.
    assert_eq!(
        click_mode(Some((200, ago(3600))), Some((200, Some(ago(5)))), AgentsLive::Unknown, now),
        ResumeMode::Fork
    );
    // Turns ran here after that pull.
    assert_eq!(
        click_mode(Some((250, ago(1))), Some((200, Some(ago(5)))), AgentsLive::Unknown, now),
        ResumeMode::ResumeInPlace
    );
    // Listed elsewhere by now.
    assert_eq!(
        click_mode(Some((250, ago(1))), None, AgentsLive::ListedElsewhere, now),
        ResumeMode::Fork
    );
    // No transcript found: nothing says it came from elsewhere.
    assert_eq!(click_mode(None, None, AgentsLive::Unknown, now), ResumeMode::ResumeInPlace);
}

/// Sessions from the other machine are listed under this machine's
/// checkout: the pull filed them under the slug of the mapped cwd, and the
/// scan reads that root (and its subfolders) like any other.
#[test]
fn a_synced_session_lists_under_the_local_checkout() {
    let dir = std::env::temp_dir().join(format!("mnemos-ide-sessions-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let checkout = dir.join("code").join("proj");
    let sub = checkout.join("app");
    std::fs::create_dir_all(&sub).unwrap();
    let projects = dir.join("claude-projects");
    // What a pull from the Mac leaves on this machine: a transcript in the
    // slug folder of the mapped cwd, with that cwd on every line.
    let slug_dir = projects.join(mnemos_ide_sessions::project_slug(&sub));
    std::fs::create_dir_all(&slug_dir).unwrap();
    let sid = "0a0a0a0a-0000-0000-0000-000000000000";
    let line = serde_json::json!({
        "type": "user",
        "cwd": sub.to_string_lossy(),
        "message": { "content": "from the mac" },
    });
    std::fs::write(slug_dir.join(format!("{sid}.jsonl")), format!("{line}\n")).unwrap();

    let mut store = SessionStore::with_projects_dir(projects);
    let listed = store.list_for_root(&checkout, PER_PROJECT).unwrap();
    assert_eq!(listed.len(), 1);
    let session = scanned(listed.into_iter().next().unwrap(), &checkout);
    assert_eq!(session.meta.id, sid);
    assert_eq!(session.cwd, sub);
    assert!(session.cwd_exists);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_resume_command_forks_only_when_asked() {
    let id = "c259d1fd-10fd-4517-b0c3-82a23ad52f64";
    assert_eq!(resume_command(id, false), format!("claude --resume {id}"));
    assert_eq!(
        resume_command(id, true),
        format!("claude --resume {id} --fork-session")
    );
}

/// A nested project with more than [`PER_PROJECT`] newer sessions does not
/// crowd the outer project's own sessions out of its group.
#[test]
fn a_busy_nested_project_leaves_the_outer_ones_sessions_listed() {
    let dir =
        std::env::temp_dir().join(format!("mnemos-ide-sessions-nested-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let outer = dir.join("code").join("admin");
    let inner = outer.join("apps").join("x");
    std::fs::create_dir_all(&inner).unwrap();
    let projects = dir.join("claude-projects");
    let write = |cwd: &Path, n: usize| {
        let slug_dir = projects.join(mnemos_ide_sessions::project_slug(cwd));
        std::fs::create_dir_all(&slug_dir).unwrap();
        let sid = format!("{n:08x}-0000-0000-0000-000000000000");
        let line = serde_json::json!({
            "type": "user",
            "cwd": cwd.to_string_lossy(),
            "message": { "content": format!("session {n}") },
        });
        let path = slug_dir.join(format!("{sid}.jsonl"));
        std::fs::write(&path, format!("{line}\n")).unwrap();
        // Outer sessions oldest, inner ones newer.
        let at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + n as u64);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(at)
            .unwrap();
        sid
    };
    let outer_ids: Vec<String> = (0..3).map(|n| write(&outer, n)).collect();
    for n in 100..100 + PER_PROJECT + 5 {
        write(&inner, n);
    }

    let mut store = SessionStore::with_projects_dir(projects);
    let groups = scan_with(
        &mut store,
        vec![
            ScanRoot {
                project_id: Some("outer".into()),
                name: "Admin".into(),
                root: outer.clone(),
            },
            ScanRoot {
                project_id: Some("inner".into()),
                name: "X".into(),
                root: inner.clone(),
            },
        ],
        None,
    );
    let ids = |pid: &str| -> Vec<String> {
        groups
            .iter()
            .find(|g| g.root.project_id.as_deref() == Some(pid))
            .map(|g| g.sessions.iter().map(|s| s.meta.id.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(ids("inner").len(), PER_PROJECT);
    let mut outer_listed = ids("outer");
    outer_listed.sort();
    assert_eq!(outer_listed, outer_ids);
    let _ = std::fs::remove_dir_all(&dir);
}
