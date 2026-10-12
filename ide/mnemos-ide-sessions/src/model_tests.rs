// SPDX-License-Identifier: AGPL-3.0-only
//! Unit tests for session metadata extraction.
//!
//! The pure functions are tested against fixtures written to a temp dir; the
//! last test additionally runs the real parser over the developer's actual
//! `~/.claude/projects` tree when one exists, since that is the only thing
//! that catches a record-shape change upstream. It skips silently on CI.

use super::*;
use std::io::Write;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mnemos-ide-sessions-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_session(dir: &Path, id: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(format!("{id}.jsonl"));
    let mut file = File::create(&path).expect("create fixture");
    for line in lines {
        writeln!(file, "{line}").expect("write fixture");
    }
    path
}

#[test]
fn slug_matches_claude_code_layout() {
    assert_eq!(
        project_slug(Path::new("/Users/alice/code/tcbv2")),
        "-Users-alice-code-tcbv2"
    );
    // A dot becomes a dash, which is why `.claude` shows up as `--claude`.
    assert_eq!(
        project_slug(Path::new("/Users/alice/code/Admin/.claude/apps/TwinCurrent-Build")),
        "-Users-alice-code-Admin--claude-apps-TwinCurrent-Build"
    );
    // Non-ASCII collapses to a dash too (`træning` -> `tr-ning`).
    assert_eq!(
        project_slug(Path::new("/Users/alice/code/træning")),
        "-Users-alice-code-tr-ning"
    );
    assert_eq!(
        project_slug(Path::new(r"C:\Users\alice\code")),
        "C--Users-alice-code"
    );
    // A trailing separator names the same folder.
    assert_eq!(
        project_slug(Path::new("/Users/alice/code/tcbv2/")),
        "-Users-alice-code-tcbv2"
    );
}

/// The slug must be the one the transcript sync files pulled sessions
/// under, or a synced session would land in a folder this list never reads.
/// A character outside the Basic Multilingual Plane is two UTF-16 units in
/// Claude Code's JavaScript, hence two dashes.
#[test]
fn slug_matches_the_sync_rule_for_non_bmp_characters() {
    for path in [
        "/Users/alice/code/emoji-\u{1F600}-repo",
        "/Users/alice/code/tr\u{e6}ning",
        "/Users/alice/code/plain",
    ] {
        let path = Path::new(path);
        assert_eq!(
            Some(project_slug(path)),
            mnemos_ide_core::registry::claude_project_slug(path),
            "{}",
            path.display()
        );
    }
    assert_eq!(
        project_slug(Path::new("/x/\u{1F600}")),
        "-x---",
        "one emoji is two dashes"
    );
}

fn titled(title: &str, cwd: Option<&str>) -> String {
    match cwd {
        Some(cwd) => format!(
            r#"{{"type":"ai-title","aiTitle":"{title}"}}
{{"type":"user","cwd":"{cwd}","message":{{"content":"hi"}}}}"#
        ),
        None => format!(r#"{{"type":"ai-title","aiTitle":"{title}"}}"#),
    }
}

fn set_mtime(path: &Path, secs_ago: u64) {
    let file = File::options().write(true).open(path).expect("open");
    file.set_modified(SystemTime::now() - std::time::Duration::from_secs(secs_ago))
        .expect("set mtime");
}

#[test]
fn root_listing_includes_sessions_started_in_subfolders() {
    let projects = temp_dir("root-subdirs");
    let root = Path::new("/tmp/code/foo");
    let at_root = projects.join(project_slug(root));
    let in_app = projects.join(project_slug(&root.join("app")));
    std::fs::create_dir_all(&at_root).unwrap();
    std::fs::create_dir_all(&in_app).unwrap();

    let a = write_session(&at_root, "aaaaaaaa-0000-0000-0000-000000000000", &[&titled("root", Some("/tmp/code/foo"))]);
    let b = write_session(&in_app, "bbbbbbbb-0000-0000-0000-000000000000", &[&titled("sub", Some("/tmp/code/foo/app"))]);
    // No cwd recorded: trusted only in the root's own folder.
    let c = write_session(&at_root, "cccccccc-0000-0000-0000-000000000000", &[&titled("bare root", None)]);
    let d = write_session(&in_app, "dddddddd-0000-0000-0000-000000000000", &[&titled("bare sub", None)]);
    set_mtime(&a, 30);
    set_mtime(&b, 10);
    set_mtime(&c, 20);
    set_mtime(&d, 5);

    let mut store = SessionStore::with_projects_dir(projects);
    let titles: Vec<String> = store
        .list_for_root(root, 10)
        .unwrap()
        .into_iter()
        .map(|s| s.title)
        .collect();
    assert_eq!(titles, vec!["sub", "bare root", "root"]);
}

#[test]
fn root_listing_excludes_a_sibling_whose_slug_shares_the_prefix() {
    let projects = temp_dir("root-sibling");
    let root = Path::new("/tmp/code/foo");
    // `/tmp/code/foo-bar` and `/tmp/code/foo.bar` both slug to `...-foo-bar`,
    // which starts with `slug(foo) + "-"`; `foobar` does not even match the
    // prefix. None of them is under `foo`.
    let sibling = projects.join(project_slug(Path::new("/tmp/code/foo-bar")));
    let other = projects.join(project_slug(Path::new("/tmp/code/foobar")));
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    write_session(&sibling, "aaaaaaaa-0000-0000-0000-000000000001", &[&titled("dash", Some("/tmp/code/foo-bar"))]);
    write_session(&sibling, "aaaaaaaa-0000-0000-0000-000000000002", &[&titled("dot", Some("/tmp/code/foo.bar"))]);
    write_session(&other, "aaaaaaaa-0000-0000-0000-000000000003", &[&titled("joined", Some("/tmp/code/foobar"))]);

    let mut store = SessionStore::with_projects_dir(projects);
    assert!(store.list_for_root(root, 10).unwrap().is_empty());
    assert_eq!(store.list_for_root(Path::new("/tmp/code/foobar"), 10).unwrap().len(), 1);
}

#[test]
fn root_listing_caps_and_dedupes_by_id_keeping_the_newest_copy() {
    let projects = temp_dir("root-dedupe");
    let root = Path::new("/tmp/code/foo");
    let at_root = projects.join(project_slug(root));
    let in_sub = projects.join(project_slug(&root.join("sub")));
    std::fs::create_dir_all(&at_root).unwrap();
    std::fs::create_dir_all(&in_sub).unwrap();
    let sid = "eeeeeeee-0000-0000-0000-000000000000";
    let old = write_session(&at_root, sid, &[&titled("old copy", Some("/tmp/code/foo"))]);
    let new = write_session(&in_sub, sid, &[&titled("new copy", Some("/tmp/code/foo/sub"))]);
    set_mtime(&old, 100);
    set_mtime(&new, 1);
    for i in 0..5 {
        let p = write_session(
            &at_root,
            &format!("ffffffff-0000-0000-0000-00000000000{i}"),
            &[&titled(&format!("n{i}"), Some("/tmp/code/foo"))],
        );
        set_mtime(&p, 10 + i);
    }

    let mut store = SessionStore::with_projects_dir(projects);
    let all = store.list_for_root(root, 40).unwrap();
    assert_eq!(all.len(), 6, "one row per session id");
    assert_eq!(all[0].title, "new copy");
    assert_eq!(all.iter().filter(|s| s.id == sid).count(), 1);

    let capped = store.list_for_root(root, 3).unwrap();
    let titles: Vec<&str> = capped.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["new copy", "n0", "n1"]);
}

#[test]
fn root_listing_without_any_folder_is_empty() {
    let projects = temp_dir("root-none");
    let mut store = SessionStore::with_projects_dir(projects.join("absent"));
    assert!(store.list_for_root(Path::new("/tmp/nothing"), 5).unwrap().is_empty());
}

#[test]
fn within_compares_whole_components() {
    assert!(is_within(Path::new("/code/foo"), Path::new("/code/foo")));
    assert!(is_within(Path::new("/code/foo/app"), Path::new("/code/foo")));
    assert!(!is_within(Path::new("/code/foobar"), Path::new("/code/foo")));
    assert!(!is_within(Path::new("/code"), Path::new("/code/foo")));
}

#[test]
fn json_str_field_handles_escapes_and_absent_keys() {
    let line = r#"{"type":"user","cwd":"/Users/alice/co\"de","gitBranch":"main"}"#;
    assert_eq!(json_str_field(line, "type").as_deref(), Some("user"));
    assert_eq!(json_str_field(line, "cwd").as_deref(), Some(r#"/Users/alice/co"de"#));
    assert_eq!(json_str_field(line, "gitBranch").as_deref(), Some("main"));
    assert_eq!(json_str_field(line, "aiTitle"), None);
    // Non-string values are skipped rather than mis-decoded.
    assert_eq!(json_str_field(r#"{"isMeta":true}"#, "isMeta"), None);
}

#[test]
fn ai_title_beats_the_first_prompt_and_the_first_title_wins() {
    let dir = temp_dir("ai-title");
    let path = write_session(
        &dir,
        "aaaaaaaa-0000-0000-0000-000000000000",
        &[
            r#"{"type":"mode","mode":"normal"}"#,
            r#"{"type":"user","cwd":"/tmp/proj","gitBranch":"main","message":{"content":"first thing I said"}}"#,
            r#"{"type":"ai-title","aiTitle":"What it was opened to do"}"#,
            r#"{"type":"ai-title","aiTitle":"Where it drifted to"}"#,
        ],
    );
    let meta = read_session_head(&path, SystemTime::UNIX_EPOCH, 0).expect("parse");
    // The opening title identifies the session; a later one reflects drift,
    // and chasing it would mean reading every transcript to its budget.
    assert_eq!(meta.title, "What it was opened to do");
    assert!(meta.title_is_ai);
    assert_eq!(meta.cwd.as_deref(), Some(Path::new("/tmp/proj")));
    assert_eq!(meta.git_branch.as_deref(), Some("main"));
    assert_eq!(meta.id, "aaaaaaaa-0000-0000-0000-000000000000");
    assert_eq!(meta.short_id(), "aaaaaaaa");
}

#[test]
fn falls_back_to_the_first_real_prompt() {
    let dir = temp_dir("prompt");
    let path = write_session(
        &dir,
        "bbbbbbbb-0000-0000-0000-000000000000",
        &[
            r#"{"type":"user","isMeta":true,"message":{"content":"ignore me"}}"#,
            r#"{"type":"user","cwd":"/tmp/proj","message":{"content":[{"type":"text","text":"yo   build\nthe thing"}]}}"#,
        ],
    );
    let meta = read_session_head(&path, SystemTime::UNIX_EPOCH, 0).expect("parse");
    assert_eq!(meta.title, "yo build the thing");
    assert!(!meta.title_is_ai);
}

#[test]
fn slash_commands_are_labelled_by_their_arguments() {
    // `/prime` on its own says nothing; the args are the actual intent.
    assert_eq!(
        prompt_label("<command-message>prime</command-message>\n<command-name>/prime</command-name>\n<command-args>build the mac release</command-args>").as_deref(),
        Some("/prime build the mac release")
    );
    assert_eq!(
        prompt_label("<command-name>/model</command-name><command-args></command-args>").as_deref(),
        Some("/model")
    );
    // Local-command scaffolding is not user intent.
    assert_eq!(prompt_label("<local-command-caveat>Caveat: …</local-command-caveat>"), None);
    assert_eq!(prompt_label("   "), None);
}

#[test]
fn unparseable_session_still_yields_a_row() {
    let dir = temp_dir("garbage");
    let path = write_session(
        &dir,
        "cccccccc-0000-0000-0000-000000000000",
        &["not json at all", "{\"type\":\"mode\"}"],
    );
    let meta = read_session_head(&path, SystemTime::UNIX_EPOCH, 12).expect("parse");
    assert_eq!(meta.title, "session cccccccc");
    assert_eq!(meta.cwd, None);
}

#[test]
fn listing_sorts_newest_first_and_ignores_non_transcripts() {
    let root = temp_dir("listing");
    let cwd = Path::new("/tmp/some/project");
    let dir = root.join(project_slug(cwd));
    std::fs::create_dir_all(&dir).expect("create slug dir");

    write_session(&dir, "11111111-0000-0000-0000-000000000000", &[r#"{"type":"ai-title","aiTitle":"older"}"#]);
    // Subdirectories (Claude Code keeps per-session scratch dirs beside the
    // transcripts) and stray files must not become rows.
    std::fs::create_dir_all(dir.join("22222222-0000-0000-0000-000000000000")).expect("subdir");
    std::fs::write(dir.join("notes.md"), b"hello").expect("stray file");

    // Force a distinguishable mtime ordering without sleeping.
    let newer = write_session(&dir, "33333333-0000-0000-0000-000000000000", &[r#"{"type":"ai-title","aiTitle":"newer"}"#]);
    let now = SystemTime::now();
    let ft = std::fs::File::options().write(true).open(&newer).expect("open newer");
    ft.set_modified(now).expect("set mtime");

    let mut store = SessionStore::with_projects_dir(root);
    let sessions = store.list_for_cwd(cwd).expect("list");
    assert_eq!(sessions.len(), 2, "only .jsonl files become rows");
    assert_eq!(sessions[0].title, "newer");
    assert_eq!(sessions[1].title, "older");

    // Second call is served from cache and must be identical.
    let again = store.list_for_cwd(cwd).expect("list again");
    assert_eq!(sessions, again);
}

#[test]
fn ancestor_lookup_finds_the_repo_root_from_a_subdirectory() {
    let root = temp_dir("ancestor");
    let repo = Path::new("/tmp/some/repo");
    let dir = root.join(project_slug(repo));
    std::fs::create_dir_all(&dir).expect("create slug dir");
    write_session(&dir, "44444444-0000-0000-0000-000000000000", &[r#"{"type":"ai-title","aiTitle":"at the root"}"#]);

    let mut store = SessionStore::with_projects_dir(root);
    // `claude` was launched at the repo root; the shell is three levels down.
    let (matched, sessions) = store
        .list_for_cwd_or_ancestor(Path::new("/tmp/some/repo/crates/foo/src"))
        .expect("walk up to the root");
    assert_eq!(matched, repo);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "at the root");
}

#[test]
fn only_uuid_shaped_ids_reach_the_shell() {
    assert!(is_safe_session_id("c259d1fd-10fd-4517-b0c3-82a23ad52f64"));
    assert!(!is_safe_session_id(""));
    assert!(!is_safe_session_id("../../etc/passwd"));
    assert!(!is_safe_session_id("abc; rm -rf ~"));
    assert!(!is_safe_session_id("$(whoami)"));
}

#[test]
fn missing_project_dir_is_a_named_error_not_a_panic() {
    let root = temp_dir("missing");
    let mut store = SessionStore::with_projects_dir(root);
    let err = store
        .list_for_cwd(Path::new("/nope/not/here"))
        .expect_err("should not exist");
    assert!(matches!(err, SessionError::NoProjectDir(_)));
}

/// Env var that opts in to [`parses_the_real_claude_transcripts_when_present`].
const REAL_CLAUDE_TEST_ENV: &str = "MNEMOS_TEST_REAL_CLAUDE";

/// Runs the real parser over this developer's real transcripts. Opt-in
/// (`MNEMOS_TEST_REAL_CLAUDE=1`), so a default run never reads user data; it
/// also skips when they aren't there (a fresh machine).
#[test]
fn parses_the_real_claude_transcripts_when_present() {
    if std::env::var_os(REAL_CLAUDE_TEST_ENV).is_none_or(|v| v != "1") {
        return;
    }
    let Ok(projects) = claude_projects_dir() else {
        return;
    };
    if !projects.is_dir() {
        return;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf();

    let mut store = SessionStore::new();
    let sessions = match store.list_for_cwd(&repo) {
        Ok(s) => s,
        Err(SessionError::NoProjectDir(_)) => return,
        Err(e) => panic!("unexpected error listing real sessions: {e}"),
    };
    assert!(
        !sessions.is_empty(),
        "this repo has transcripts on disk; the parser returned none"
    );
    for s in &sessions {
        assert!(!s.title.trim().is_empty(), "every session needs a label");
        assert_eq!(s.id.len(), 36, "file stem should be a UUID: {}", s.id);
        // The transcripts record the cwd they ran in; it must round-trip back
        // to the slug we looked them up under.
        if let Some(cwd) = &s.cwd {
            assert_eq!(
                project_slug(cwd),
                project_slug(&repo),
                "session {} reports a cwd that slugs differently",
                s.id
            );
        }
    }
    assert!(
        sessions.windows(2).all(|w| w[0].modified >= w[1].modified),
        "sessions must come back newest-first"
    );
}
