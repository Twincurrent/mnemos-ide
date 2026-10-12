// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

/// Seed the queue directly, marking the file as already consumed so the test
/// never touches the real `~/…/Mnemos/store/agent_sessions.json`.
fn seed(entries: Vec<SessionEntry>) {
    *loaded().lock().unwrap() = true;
    *queue().lock().unwrap() = entries;
}

fn entry(pane: &str, session: &str) -> SessionEntry {
    SessionEntry {
        pane_uuid_hex: pane.to_owned(),
        session_id: session.to_owned(),
        cwd: None,
        agent: "Claude".to_owned(),
        last_live_unix: None,
    }
}

#[test]
fn hex_encodes_pane_uuids_lowercase() {
    assert_eq!(pane_uuid_hex(&[0x0f, 0xa0, 0xff, 0x00]), "0fa0ff00");
    assert_eq!(pane_uuid_hex(&[]), "");
}

/// One test for the whole drain protocol: the queue is process-global, so
/// splitting this up would make the cases race each other.
#[test]
fn each_window_drains_only_its_own_panes() {
    seed(vec![
        entry("aa", "0f8b1c2d-3e4f-5061-7283-94a5b6c7d8e9"),
        entry("bb", "1a2b3c4d-5e6f-7081-9203-a4b5c6d7e8f0"),
        entry("cc", "2b3c4d5e-6f70-8192-03a4-b5c6d7e8f001"),
    ]);

    // Window one owns panes `aa` and `zz`; `zz` was never in the snapshot.
    let first = take_matching(&["aa".to_owned(), "zz".to_owned()]);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].pane_uuid_hex, "aa");

    // Window two still finds its own entry — the first window did not eat it.
    let second = take_matching(&["bb".to_owned()]);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].pane_uuid_hex, "bb");

    // Claimed entries are gone for good; a repeat restore does nothing.
    assert!(take_matching(&["aa".to_owned(), "bb".to_owned()]).is_empty());

    // `cc`'s pane never came back, so its entry simply expires unclaimed.
    assert_eq!(queue().lock().unwrap().len(), 1);
}

/// An entry taken but not run comes back in the next snapshot, after the
/// live ones, unless its session or its pane is live again; a resume from
/// the sessions list forgets it. One test: the deferred list is
/// process-global.
#[test]
fn a_deferred_entry_rides_along_until_it_runs_again() {
    let a = "0f8b1c2d-3e4f-5061-7283-94a5b6c7d8e9";
    let b = "1a2b3c4d-5e6f-7081-9203-a4b5c6d7e8f0";
    let c = "2b3c4d5e-6f70-8192-03a4-b5c6d7e8f001";
    let mut carried = entry("aa", a);
    carried.last_live_unix = Some(42);
    defer(carried);
    // Deferring again replaces, and keeps the earlier time.
    let mut again = entry("aa", a);
    again.last_live_unix = Some(42);
    defer(again);
    let out = with_deferred(vec![entry("bb", b)]);
    let ids: Vec<&str> = out.iter().map(|e| e.session_id.as_str()).collect();
    assert_eq!(ids, vec![b, a]);
    assert_eq!(out[1].last_live_unix, Some(42));
    assert_eq!(last_live_at(&out[1]), Some(UNIX_EPOCH + Duration::from_secs(42)));

    // Its pane runs another session now: that one wins.
    let out = with_deferred(vec![entry("aa", c)]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].session_id, c);
    // The session runs again in another pane: kept once, there.
    let out = with_deferred(vec![entry("dd", a)]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].pane_uuid_hex, "dd");

    forget_deferred(a);
    assert!(with_deferred(Vec::new()).is_empty());
}
