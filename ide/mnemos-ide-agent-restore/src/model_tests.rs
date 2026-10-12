// SPDX-License-Identifier: AGPL-3.0-only
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use super::*;

/// `CLAUDE_CONFIG_DIR` is process-global, so the tests that point it at a
/// temp dir have to take turns.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const UUID_A: &str = "0f8b1c2d-3e4f-5061-7283-94a5b6c7d8e9";
const UUID_B: &str = "1a2b3c4d-5e6f-7081-9203-a4b5c6d7e8f0";
const UUID_C: &str = "2b3c4d5e-6f70-8192-03a4-b5c6d7e8f001";

fn entry(pane: &str, session: &str, cwd: Option<PathBuf>) -> SessionEntry {
    SessionEntry {
        pane_uuid_hex: pane.to_owned(),
        session_id: session.to_owned(),
        cwd,
        agent: "Claude".to_owned(),
        last_live_unix: None,
    }
}

/// Runs `f` with `CLAUDE_CONFIG_DIR` pointed at a fresh temp dir and hands it
/// that dir's `projects/`. Every test that calls [`RestoreSnapshot::resumable`]
/// goes through here, so none of them can read the real `~/.claude`. The
/// variable is removed again before the result comes back to be asserted on.
fn with_claude_home<R>(f: impl FnOnce(&Path) -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("CLAUDE_CONFIG_DIR", home.path());
    let out = f(&home.path().join("projects"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    out
}

/// Lays down `projects/<slug>/<session>.jsonl` the way Claude Code starts one:
/// the first user record carries the directory the session was launched in.
fn write_transcript(projects: &Path, slug: &str, session: &str, launch_dir: &Path) -> PathBuf {
    let record = serde_json::json!({
        "type": "user",
        "cwd": launch_dir,
        "message": { "content": "hi" },
    });
    write_transcript_line(projects, slug, session, &record)
}

fn write_transcript_line(
    projects: &Path,
    slug: &str,
    session: &str,
    record: &serde_json::Value,
) -> PathBuf {
    let dir = projects.join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, format!("{record}\n")).unwrap();
    path
}

fn set_mtime(path: &Path, mtime: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

fn panes(resumable: Vec<&SessionEntry>) -> Vec<String> {
    resumable
        .into_iter()
        .map(|e| e.pane_uuid_hex.clone())
        .collect()
}

#[test]
fn round_trips_through_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");

    let saved = RestoreSnapshot::from_entries([
        entry("aa01", UUID_A, Some(PathBuf::from("/tmp/one"))),
        entry("bb02", UUID_B, None),
    ]);
    saved.save_blocking(&path).unwrap();
    assert!(path.is_file());

    let loaded = RestoreSnapshot::take_blocking(&path).expect("snapshot should load");
    assert_eq!(loaded.entries, saved.entries);
    assert_eq!(loaded.version, SNAPSHOT_VERSION);
}

#[test]
fn reading_consumes_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");

    RestoreSnapshot::from_entries([entry("aa01", UUID_A, None)])
        .save_blocking(&path)
        .unwrap();

    assert!(RestoreSnapshot::take_blocking(&path).is_some());
    // The whole anti-restore-loop guarantee: a second launch finds nothing.
    assert!(!path.exists());
    assert!(RestoreSnapshot::take_blocking(&path).is_none());
}

#[test]
fn an_empty_snapshot_removes_a_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");

    RestoreSnapshot::from_entries([entry("aa01", UUID_A, None)])
        .save_blocking(&path)
        .unwrap();
    assert!(path.is_file());

    // Next shutdown has no live agents: the old file must not survive it.
    RestoreSnapshot::from_entries([]).save_blocking(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn saving_empty_over_nothing_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");
    RestoreSnapshot::from_entries([]).save_blocking(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn dedupes_the_parent_session_id_shared_by_subagents() {
    // Two panes reporting the same session id — what a subagent's hooks
    // produce, since Claude Code files subagent transcripts under the
    // parent's uuid. Resuming twice would fork one conversation into two.
    let snapshot = RestoreSnapshot::from_entries([
        entry("parent", UUID_A, Some(PathBuf::from("/tmp/one"))),
        entry("subagent", UUID_A, Some(PathBuf::from("/tmp/one"))),
        entry("other", UUID_B, None),
    ]);

    assert_eq!(snapshot.entries.len(), 2);
    assert_eq!(snapshot.entries[0].pane_uuid_hex, "parent");
    assert_eq!(snapshot.entries[1].session_id, UUID_B);
}

#[test]
fn rejects_session_ids_that_could_reach_a_shell() {
    let snapshot = RestoreSnapshot::from_entries([
        entry("bad", "; rm -rf ~", None),
        entry("also-bad", "$(whoami)", None),
        entry("spaces", "abc def", None),
        entry("empty", "", None),
        entry("good", UUID_A, None),
    ]);

    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(snapshot.entries[0].session_id, UUID_A);
}

#[test]
fn caps_the_number_of_entries() {
    let work = tempfile::tempdir().unwrap();
    let ids: Vec<String> = (0..(MAX_RESTORED_SESSIONS + 10))
        .map(|i| format!("{UUID_A}{i:x}"))
        .collect();
    let entries: Vec<SessionEntry> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| entry(&format!("pane{i}"), id, None))
        .collect();

    let snapshot = RestoreSnapshot::from_entries(entries);
    let resumable = with_claude_home(|projects| {
        for id in &ids {
            write_transcript(projects, &project_slug(work.path()), id, work.path());
        }
        snapshot.resumable().len()
    });

    assert_eq!(snapshot.entries.len(), MAX_RESTORED_SESSIONS);
    assert_eq!(resumable, MAX_RESTORED_SESSIONS);
}

#[test]
fn discards_a_snapshot_from_another_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");

    let mut snapshot = RestoreSnapshot::from_entries([entry("aa01", UUID_A, None)]);
    snapshot.version = SNAPSHOT_VERSION + 1;
    snapshot.save_blocking(&path).unwrap();

    assert!(RestoreSnapshot::take_blocking(&path).is_none());
    assert!(!path.exists());
}

#[test]
fn discards_a_stale_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");

    let mut snapshot = RestoreSnapshot::from_entries([entry("aa01", UUID_A, None)]);
    snapshot.saved_at_unix = snapshot.saved_at_unix.saturating_sub(MAX_SNAPSHOT_AGE_SECS + 60);
    snapshot.save_blocking(&path).unwrap();

    assert!(RestoreSnapshot::take_blocking(&path).is_none());
}

#[test]
fn discards_unparseable_json_and_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");
    std::fs::write(&path, b"{ not json at all").unwrap();

    assert!(RestoreSnapshot::take_blocking(&path).is_none());
    assert!(!path.exists());
}

/// (d) The transcript is found, but the directory it was launched in is gone:
/// `claude --resume` would have nowhere to run, so the entry is dropped.
#[test]
fn skips_entries_whose_launch_dir_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live");
    std::fs::create_dir(&live).unwrap();
    let deleted = dir.path().join("deleted-worktree");

    let snapshot = RestoreSnapshot::from_entries([
        // Its last hook cwd is live, but it was launched in the dead worktree.
        entry("gone", UUID_A, Some(live.clone())),
        // No cwd recorded: found by scanning, launched somewhere that exists.
        entry("nocwd", UUID_B, None),
    ]);

    let resumable = with_claude_home(|projects| {
        write_transcript(projects, &project_slug(&deleted), UUID_A, &deleted);
        write_transcript(projects, &project_slug(&live), UUID_B, &live);
        panes(snapshot.resumable())
    });

    assert_eq!(resumable, ["nocwd"]);
}

#[test]
fn skips_entries_whose_transcript_is_gone() {
    let work = tempfile::tempdir().unwrap();

    let snapshot = RestoreSnapshot::from_entries([
        entry("kept", UUID_A, Some(work.path().to_path_buf())),
        entry("cleared", UUID_B, Some(work.path().to_path_buf())),
    ]);

    let resumable = with_claude_home(|projects| {
        // A transcript for UUID_A only, in the slug Claude Code would use.
        write_transcript(projects, &project_slug(work.path()), UUID_A, work.path());
        panes(snapshot.resumable())
    });

    assert_eq!(resumable, ["kept"]);
}

/// (a) The agent `cd`'d into a subdirectory, so the last hook cwd is
/// `work/sub`, while Claude filed the transcript under the launch dir `work`.
#[test]
fn keeps_a_session_that_moved_into_a_subdirectory() {
    let work = tempfile::tempdir().unwrap();
    let sub = work.path().join("sub");
    std::fs::create_dir(&sub).unwrap();

    let snapshot = RestoreSnapshot::from_entries([entry("cd", UUID_A, Some(sub))]);

    let resumable = with_claude_home(|projects| {
        write_transcript(projects, &project_slug(work.path()), UUID_A, work.path());
        panes(snapshot.resumable())
    });

    assert_eq!(resumable, ["cd"]);
}

/// (b) The last hook cwd was a worktree that has since been deleted, but the
/// session was launched in a directory that still exists.
#[test]
fn keeps_a_session_whose_last_cwd_was_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let removed_worktree = dir.path().join("wt-feature");

    let snapshot =
        RestoreSnapshot::from_entries([entry("drifted", UUID_A, Some(removed_worktree))]);

    let resumable = with_claude_home(|projects| {
        write_transcript(projects, &project_slug(&work), UUID_A, &work);
        panes(snapshot.resumable())
    });

    assert_eq!(resumable, ["drifted"]);
}

/// (c) Other sessions have transcripts in several slug dirs, but these two
/// have none anywhere: one with a cwd hint, one without. The ids differ so
/// `from_entries` keeps both, and a regression back to "no cwd recorded ->
/// keep" fails here.
#[test]
fn skips_a_session_with_no_transcript_in_any_project_dir() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();

    let snapshot = RestoreSnapshot::from_entries([
        entry("hinted", UUID_B, Some(one.path().to_path_buf())),
        entry("unhinted", UUID_C, None),
    ]);
    assert_eq!(snapshot.entries.len(), 2);

    let resumable = with_claude_home(|projects| {
        write_transcript(projects, &project_slug(one.path()), UUID_A, one.path());
        write_transcript(projects, &project_slug(two.path()), UUID_A, two.path());
        snapshot.resumable().len()
    });

    assert_eq!(resumable, 0);
}

/// A head with no `cwd` means the launch dir is unknown, not gone.
#[test]
fn keeps_a_session_whose_transcript_records_no_cwd() {
    let work = tempfile::tempdir().unwrap();

    let snapshot = RestoreSnapshot::from_entries([entry("bare", UUID_A, None)]);

    let resumable = with_claude_home(|projects| {
        let record = serde_json::json!({ "type": "summary", "summary": "no cwd here" });
        write_transcript_line(projects, &project_slug(work.path()), UUID_A, &record);
        panes(snapshot.resumable())
    });

    assert_eq!(resumable, ["bare"]);
}

#[test]
fn find_transcript_prefers_the_newest_copy() {
    let projects = tempfile::tempdir().unwrap();
    let projects = projects.path();
    let now = SystemTime::now();

    let old = write_transcript(projects, "-old-home", UUID_A, Path::new("/old"));
    let new = write_transcript(projects, "-new-home", UUID_A, Path::new("/new"));
    set_mtime(&old, now - Duration::from_secs(3600));
    set_mtime(&new, now);

    // Neither the hint nor directory order beats the newer copy.
    assert_eq!(find_transcript(projects, UUID_A, None), Some(new.clone()));
    assert_eq!(
        find_transcript(projects, UUID_A, Some(Path::new("/old/home"))),
        Some(new.clone())
    );

    set_mtime(&old, now + Duration::from_secs(3600));
    assert_eq!(
        find_transcript(projects, UUID_A, Some(Path::new("/new/home"))),
        Some(old)
    );
}

#[test]
fn find_transcript_breaks_an_mtime_tie_toward_the_hint() {
    let projects = tempfile::tempdir().unwrap();
    let projects = projects.path();
    let now = SystemTime::now();

    let a = write_transcript(projects, "-a", UUID_A, Path::new("/a"));
    let b = write_transcript(projects, "-b", UUID_A, Path::new("/b"));
    set_mtime(&a, now);
    set_mtime(&b, now);

    assert_eq!(
        find_transcript(projects, UUID_A, Some(Path::new("/a"))),
        Some(a)
    );
    assert_eq!(
        find_transcript(projects, UUID_A, Some(Path::new("/b"))),
        Some(b)
    );
}

#[test]
fn find_transcript_only_matches_top_level_transcripts() {
    let projects = tempfile::tempdir().unwrap();
    let projects = projects.path();

    // A session's own directory with subagent transcripts underneath it.
    let subagents = projects.join("-work").join(UUID_A).join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(subagents.join(format!("{UUID_A}.jsonl")), b"\n").unwrap();
    // One level too deep, and a directory that merely has the right name.
    write_transcript(
        &projects.join("-work"),
        "nested",
        UUID_A,
        Path::new("/work"),
    );
    std::fs::create_dir_all(projects.join("-other").join(format!("{UUID_A}.jsonl"))).unwrap();
    // Loose at the top of `projects/`, outside any slug dir.
    std::fs::write(projects.join(format!("{UUID_A}.jsonl")), b"\n").unwrap();

    assert_eq!(find_transcript(projects, UUID_A, None), None);
    assert_eq!(
        find_transcript(projects, UUID_A, Some(Path::new("/work"))),
        None
    );
}

/// The whole shutdown-to-launch path in one test: capture what the workspace
/// would hand us (including a subagent duplicate, a session that `cd`'d away
/// from where it started, and one launched in a worktree that is now gone),
/// write it, read it back on the "next launch", and check that exactly the
/// sessions worth resuming survive with the right commands.
#[test]
fn full_round_trip_from_shutdown_to_resume() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent_sessions.json");
    let live = dir.path().join("repo");
    let live_sub = live.join("crates");
    std::fs::create_dir_all(&live_sub).unwrap();
    let deleted = dir.path().join("deleted");

    // What `ide_agent_session_entries` would produce at shutdown. The cwds
    // are the last ones the hooks reported.
    let captured = vec![
        entry("pane-a", UUID_A, Some(live_sub.clone())),
        // Same session id: a subagent's hooks reporting the parent's id.
        entry("pane-a-sub", UUID_A, Some(live_sub.clone())),
        entry("pane-b", UUID_B, Some(deleted.clone())),
    ];
    RestoreSnapshot::from_entries(captured)
        .save_blocking(&path)
        .unwrap();

    // Next launch.
    let loaded = RestoreSnapshot::take_blocking(&path).expect("snapshot should load");
    // The subagent duplicate was already dropped at write time.
    assert_eq!(loaded.entries.len(), 2);

    let resumable = with_claude_home(|projects| {
        // pane-a was launched in the repo root and filed there.
        write_transcript(projects, &project_slug(&live), UUID_A, &live);
        // pane-b was launched in a worktree that has since been removed.
        write_transcript(projects, &project_slug(&deleted), UUID_B, &deleted);
        loaded.resumable()
    });

    // pane-b's worktree is gone, so only pane-a is replayed.
    assert_eq!(resumable.len(), 1);
    assert_eq!(resumable[0].pane_uuid_hex, "pane-a");
    assert_eq!(
        resume_command(&resumable[0].session_id),
        format!("claude --resume {UUID_A}")
    );
    assert!(!path.exists());
}

#[test]
fn resume_command_continues_rather_than_forks() {
    let command = resume_command(UUID_A);
    assert_eq!(command, format!("claude --resume {UUID_A}"));
    assert!(!command.contains("--fork-session"));
}

#[test]
fn fork_command_forks_the_same_session() {
    assert_eq!(
        fork_command(UUID_A),
        format!("claude --resume {UUID_A} --fork-session")
    );
}
