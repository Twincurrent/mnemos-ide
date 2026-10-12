// SPDX-License-Identifier: AGPL-3.0-only
//! Whether a Claude session can be resumed in place here, or has to be
//! forked so it cannot take over a process on another machine.
//!
//! Claude Code Remote Control allows one live process per session id: a
//! second `claude --resume <id>` takes the session over and the first one is
//! dropped ("another connection took over this session"). Two machines that
//! share transcripts through sync can both start one. A fork
//! (`--fork-session`) gets a new id and its own remote session, so it never
//! takes anything over.
//!
//! [`resume_mode`] is the one rule, used by agent restore at startup and by
//! the sessions list on a plain click. It is pure: callers gather the facts.

use std::time::{Duration, SystemTime};

/// How long after a pull of the other machine's turns a plain click in the
/// sessions list still forks a session nobody is known to run. Counted from
/// the pull here ([`NewestTurn::pulled_at`]), not from the turn: the other
/// machine uploads a turn only after 20 s idle (5 min at most), and this
/// machine pulls it within its poll, so the window starts when the turns can
/// first be seen here. The other machine's `claude` can keep running after
/// its `agents-live` snapshot goes stale (three minutes offline), or never
/// appear there (started outside Mnemos, no plugin, the live feed off, which
/// is the default). Ten minutes after the pull, a session the other machine
/// has stopped writing to resumes in place again; one it still runs idle can
/// be taken over then, which is the price of not forking every session ever
/// touched elsewhere.
pub const RECENT_ELSEWHERE: Duration = Duration::from_secs(10 * 60);

/// What `agents-live` says about a session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsLive {
    /// Nothing definitive: no list was read (signed out, offline, no answer
    /// yet), or the machine the session's turns came from publishes nothing
    /// (the live feed is off by default), so its absence proves nothing.
    Unknown,
    /// A list was read, the machine in question publishes, and no other
    /// machine lists this id.
    NotListed,
    /// A list was read and another machine runs this id.
    ListedElsewhere,
}

/// Where the newest turns of a session's local transcript came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewestTurn {
    /// A pull from another machine wrote them, and nothing ran here since
    /// (the file still has the length that pull left).
    pub elsewhere: bool,
    /// When that pull ran, by this machine's clock
    /// (`mnemos_ide_sync::transcripts::PulledOrigin::at`). `None` for a pull
    /// recorded by an older build, or when the turns are not from elsewhere.
    pub pulled_at: Option<SystemTime>,
    /// The transcript's mtime. After a pull that is the newest line's time
    /// by the clock of the machine that wrote it, capped at the pull: never
    /// later than the pull, often earlier. Only a stand-in for `pulled_at`.
    pub modified: SystemTime,
}

impl NewestTurn {
    /// From the local transcript's length and mtime, and what the last
    /// appending pull left (`PulledOrigin::bytes` and `at`), if a pull ever
    /// appended to it.
    pub fn from_file(
        len: u64,
        modified: SystemTime,
        pulled: Option<(u64, Option<SystemTime>)>,
    ) -> Self {
        let elsewhere = pulled.is_some_and(|(bytes, _)| bytes == len);
        Self {
            elsewhere,
            pulled_at: pulled.and_then(|(_, at)| at).filter(|_| elsewhere),
            modified,
        }
    }

    /// When those turns arrived here: the pull time, else the mtime (which
    /// is never later than the pull).
    fn arrived(&self) -> SystemTime {
        self.pulled_at.unwrap_or(self.modified)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMode {
    /// `claude --resume <id>`: continue the same session.
    ResumeInPlace,
    /// `claude --resume <id> --fork-session`: a copy with a new id.
    Fork,
}

/// Resume in place or fork. `newest` is `None` when nothing is known about
/// the transcript (treated as written here). `since` is the point after
/// which turns pulled from another machine mean that machine may still be
/// running it:
///
/// - agent restore: the time this machine last had the session open, which
///   is the snapshot's write time (the entry's own time for one carried
///   over). Transcript sync never appends to a session a `claude` here has
///   open, so turns pulled at or before then were there before this
///   machine's own run, which superseded the other machine's. Turns pulled
///   after it are the other machine's, run while this one was down.
/// - the sessions list: now minus [`RECENT_ELSEWHERE`].
///
/// | agents-live | newest turn | result |
/// | --- | --- | --- |
/// | lists the id elsewhere | any | fork |
/// | any other | pulled from elsewhere after `since` | fork |
/// | any other | pulled at or before `since`, or written here | in place |
///
/// A missing `agents-live` answer (`Unknown`) is not a reason to fork on
/// its own: what it cannot show (a `claude` started elsewhere that has
/// written nothing that reached here yet) no rule here can see either.
/// Callers wait, bounded, for the first answer and the first transcript
/// pull before they decide.
pub fn resume_mode(newest: Option<NewestTurn>, since: SystemTime, live: AgentsLive) -> ResumeMode {
    let fork = match live {
        AgentsLive::ListedElsewhere => true,
        AgentsLive::Unknown | AgentsLive::NotListed => newest
            .filter(|n| n.elsewhere)
            .is_some_and(|n| n.arrived() > since),
    };
    if fork {
        ResumeMode::Fork
    } else {
        ResumeMode::ResumeInPlace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn here(at: u64) -> Option<NewestTurn> {
        Some(NewestTurn {
            elsewhere: false,
            pulled_at: None,
            modified: t(at),
        })
    }

    /// Pulled at `pulled`, its newest line written at `modified`.
    fn elsewhere(pulled: Option<u64>, modified: u64) -> Option<NewestTurn> {
        Some(NewestTurn {
            elsewhere: true,
            pulled_at: pulled.map(t),
            modified: t(modified),
        })
    }

    use AgentsLive::*;
    use ResumeMode::*;

    #[test]
    fn listed_elsewhere_always_forks() {
        for newest in [
            None,
            here(50),
            here(150),
            elsewhere(Some(50), 40),
            elsewhere(Some(150), 140),
        ] {
            assert_eq!(resume_mode(newest, t(100), ListedElsewhere), Fork, "{newest:?}");
        }
    }

    #[test]
    fn turns_pulled_after_since_fork_whatever_agents_live_knows() {
        for live in [Unknown, NotListed, ListedElsewhere] {
            assert_eq!(resume_mode(elsewhere(Some(101), 101), t(100), live), Fork, "{live:?}");
        }
    }

    /// The other machine's clock runs behind: its newest line is dated
    /// before `since`, but the pull that brought it ran after. The pull time
    /// decides.
    #[test]
    fn the_pull_time_decides_not_the_other_machines_clock() {
        for live in [Unknown, NotListed] {
            assert_eq!(resume_mode(elsewhere(Some(150), 50), t(100), live), Fork, "{live:?}");
        }
    }

    /// An idle session this machine resumed after pulling the other
    /// machine's turns, restored with agents-live unknown (offline): it was
    /// open here when the snapshot was written, so it resumes in place.
    #[test]
    fn turns_pulled_before_since_resume_in_place_even_unknown() {
        for live in [Unknown, NotListed] {
            assert_eq!(
                resume_mode(elsewhere(Some(100), 90), t(100), live),
                ResumeInPlace,
                "{live:?}"
            );
            assert_eq!(resume_mode(elsewhere(Some(50), 40), t(100), live), ResumeInPlace);
            assert_eq!(resume_mode(here(150), t(100), live), ResumeInPlace);
            assert_eq!(resume_mode(None, t(100), live), ResumeInPlace);
        }
    }

    /// A pull recorded by an older build has no time: the mtime stands in.
    #[test]
    fn without_a_pull_time_the_mtime_stands_in() {
        assert_eq!(resume_mode(elsewhere(None, 150), t(100), Unknown), Fork);
        assert_eq!(resume_mode(elsewhere(None, 50), t(100), NotListed), ResumeInPlace);
    }

    #[test]
    fn newest_turn_is_elsewhere_only_while_the_file_has_the_pulled_length() {
        let n = NewestTurn::from_file(100, t(1), Some((100, Some(t(5)))));
        assert!(n.elsewhere);
        assert_eq!(n.pulled_at, Some(t(5)));
        // Turns ran here after the pull: no pull time either.
        let n = NewestTurn::from_file(150, t(1), Some((100, Some(t(5)))));
        assert!(!n.elsewhere);
        assert_eq!(n.pulled_at, None);
        // Longer than the file (truncated here since): not from elsewhere.
        assert!(!NewestTurn::from_file(100, t(1), Some((150, Some(t(5))))).elsewhere);
        // Never appended to by a pull.
        assert!(!NewestTurn::from_file(100, t(1), None).elsewhere);
        assert_eq!(NewestTurn::from_file(100, t(7), None).modified, t(7));
    }
}
