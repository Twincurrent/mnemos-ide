// SPDX-License-Identifier: AGPL-3.0-only
//! Which Drive team this machine takes part in, as the user chose it here.
//!
//! Being a member of a team on the server is not enough: a member's machine
//! pulls the team's `~/.claude` config (hooks included), so joining has to be
//! the user's own act on each machine. The choice lives in
//! `<Mnemos data dir>/store/team_choice.json`, local only, never synced:
//!
//! ```json
//! { "teamId": "<id>" }      // take part in this team
//! { "teamId": null }     // no team here (until the user joins one)
//! ```
//!
//! Upgrade path: a machine that synced before this file existed used to take
//! part in its first listed team automatically. The first start of this
//! build keeps exactly that team and writes it down, so existing teams keep
//! working unchanged. Such a machine is recognised by either of two signals
//! ([`legacy_install`]): a `sync_baseline.json` from an earlier run, or an
//! admin login that was already in the keyring when the app started. The
//! second covers builds older than 2026-09-28 (no baseline file) and a moved
//! data dir. It cannot pick up a fresh install by mistake: a sign-in made in
//! this process is not "saved earlier", and that first run records a choice
//! (`{"teamId": null}`), which every later start then follows.
//! Every other machine starts with no team and joins one only from Settings.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::teams::Team;

pub(crate) const TEAM_CHOICE_FILE: &str = "team_choice.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TeamChoice {
    #[serde(rename = "teamId", default)]
    pub team_id: Option<String>,
}

/// What `auto_select_team` does with the server's team list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamDecision {
    /// The team to take part in, if any.
    pub selected: Option<String>,
    /// A choice to write down (only when there was none yet).
    pub record: Option<TeamChoice>,
}

/// Whether this machine synced under a build from before team choices were
/// recorded. See the module docs.
pub(crate) fn legacy_install(has_baseline: bool, login_saved_earlier: bool) -> bool {
    has_baseline || login_saved_earlier
}

/// `choice`: what this machine recorded, if anything. `legacy_install`: the
/// machine synced before choices were recorded. `teams`: the caller's teams
/// as the server lists them, `None` when the list could not be fetched.
pub(crate) fn decide(
    choice: Option<&TeamChoice>,
    legacy_install: bool,
    teams: Option<&[Team]>,
) -> TeamDecision {
    match (choice, legacy_install, teams) {
        // A recorded choice holds while the server still lists that team.
        (Some(choice), _, teams) => TeamDecision {
            selected: choice
                .team_id
                .clone()
                .filter(|id| teams.is_none_or(|teams| teams.iter().any(|t| &t.id == id))),
            record: None,
        },
        // An upgraded machine keeps the team it had: the first one listed.
        (None, true, Some(teams)) => {
            let selected = teams.first().map(|t| t.id.clone());
            TeamDecision {
                selected: selected.clone(),
                record: Some(TeamChoice { team_id: selected }),
            }
        }
        // Upgraded but offline: decide on a later start.
        (None, true, None) => TeamDecision {
            selected: None,
            record: None,
        },
        // A new machine: no team until the user joins one.
        (None, false, _) => TeamDecision {
            selected: None,
            record: Some(TeamChoice::default()),
        },
    }
}

/// Plain `std::fs` on purpose: also called from the app's own executor
/// (Settings > Sync), where tokio's file API has no runtime. The file is a
/// few bytes.
pub(crate) fn load(store_dir: &Path) -> Option<TeamChoice> {
    let bytes = std::fs::read(store_dir.join(TEAM_CHOICE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn save(store_dir: &Path, choice: &TeamChoice) -> std::io::Result<()> {
    std::fs::create_dir_all(store_dir)?;
    let bytes = serde_json::to_vec_pretty(choice).map_err(std::io::Error::other)?;
    let path = store_dir.join(TEAM_CHOICE_FILE);
    let tmp = store_dir.join(format!("{TEAM_CHOICE_FILE}.tmp"));
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str) -> Team {
        Team {
            id: id.into(),
            name: id.to_uppercase(),
            role: "member".into(),
            member_count: 2,
        }
    }

    #[test]
    fn a_login_from_before_the_start_marks_an_upgraded_machine() {
        // A build older than the baseline file, or a moved data dir.
        assert!(legacy_install(false, true));
        assert!(legacy_install(true, false));
        // A fresh install that signed in during this run.
        assert!(!legacy_install(false, false));
        let teams = [team("a")];
        let d = decide(None, legacy_install(false, true), Some(&teams));
        assert_eq!(d.selected.as_deref(), Some("a"));
    }

    #[test]
    fn upgraded_machine_keeps_its_first_team_and_records_it() {
        let teams = [team("a"), team("b")];
        let d = decide(None, true, Some(&teams));
        assert_eq!(d.selected.as_deref(), Some("a"));
        assert_eq!(d.record, Some(TeamChoice { team_id: Some("a".into()) }));
    }

    #[test]
    fn new_machine_is_never_joined_silently() {
        let teams = [team("a")];
        let d = decide(None, false, Some(&teams));
        assert_eq!(d.selected, None);
        assert_eq!(d.record, Some(TeamChoice::default()));
        // Also when the list could not be fetched.
        assert_eq!(decide(None, false, None).selected, None);
    }

    #[test]
    fn upgraded_machine_offline_decides_later() {
        assert_eq!(
            decide(None, true, None),
            TeamDecision { selected: None, record: None }
        );
    }

    #[test]
    fn recorded_choice_wins_and_drops_a_team_that_is_gone() {
        let teams = [team("a"), team("b")];
        let b = TeamChoice { team_id: Some("b".into()) };
        assert_eq!(decide(Some(&b), true, Some(&teams)).selected.as_deref(), Some("b"));
        let gone = TeamChoice { team_id: Some("zz".into()) };
        assert_eq!(decide(Some(&gone), true, Some(&teams)).selected, None);
        // Offline: keep the recorded team.
        assert_eq!(decide(Some(&b), false, None).selected.as_deref(), Some("b"));
        // Recorded "no team": a listed team is not joined.
        assert_eq!(decide(Some(&TeamChoice::default()), true, Some(&teams)).selected, None);
    }

    #[test]
    fn choice_round_trips_through_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(load(tmp.path()), None);
        let c = TeamChoice { team_id: Some("t1".into()) };
        save(tmp.path(), &c).unwrap();
        assert_eq!(load(tmp.path()), Some(c));
    }
}
