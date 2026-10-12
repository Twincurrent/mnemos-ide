//! Mnemos IDE: the platform-independent part of Windows end-session handling.
//!
//! When Windows shuts down, restarts or logs off, it sends `WM_QUERYENDSESSION` and then
//! `WM_ENDSESSION` to every top-level window. The Win32 side lives in
//! `winit/windows/end_session.rs`; this module only decides what a message means, so the
//! decision is testable on every platform.

/// `WM_QUERYENDSESSION` from `WinUser.h`.
pub(crate) const WM_QUERYENDSESSION: u32 = 0x0011;
/// `WM_ENDSESSION` from `WinUser.h`.
pub(crate) const WM_ENDSESSION: u32 = 0x0016;

/// Default shutdown level of a Windows process (`SetProcessShutdownParameters`).
#[cfg(test)]
pub(crate) const DEFAULT_SHUTDOWN_LEVEL: u32 = 0x280;
/// The shutdown level Mnemos asks for: the bottom of the "application first" range
/// (0x300-0x3FF). Windows ends higher levels first, so this puts Mnemos ahead of its own
/// shells and agents (which keep the default 0x280). Without it Windows may end the
/// ConPTY shells, and the `claude` sessions inside them, before Mnemos hears about the
/// shutdown, and there is nothing left to snapshot.
pub(crate) const MNEMOS_SHUTDOWN_LEVEL: u32 = 0x300;

/// What an end-session message asks of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EndSessionMessage {
    /// `WM_QUERYENDSESSION`: may the session end? Always yes; Mnemos never blocks it.
    Query,
    /// `WM_ENDSESSION` with `wParam = TRUE`: the session is ending. The process can be
    /// killed as soon as the handler returns, so terminate inside the handler.
    Ending,
    /// `WM_ENDSESSION` with `wParam = FALSE`: another app cancelled the shutdown.
    /// Nothing was done at query time, so there is nothing to undo.
    Cancelled,
}

/// Classifies a window message; `None` for anything that is not an end-session message.
pub(crate) fn classify(msg: u32, wparam: usize) -> Option<EndSessionMessage> {
    match msg {
        WM_QUERYENDSESSION => Some(EndSessionMessage::Query),
        WM_ENDSESSION if wparam != 0 => Some(EndSessionMessage::Ending),
        WM_ENDSESSION => Some(EndSessionMessage::Cancelled),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_answered_without_terminating() {
        assert_eq!(
            classify(WM_QUERYENDSESSION, 0),
            Some(EndSessionMessage::Query)
        );
        // lParam flags (ENDSESSION_LOGOFF, ENDSESSION_CLOSEAPP) never reach classify,
        // and wParam is unused for the query.
        assert_eq!(
            classify(WM_QUERYENDSESSION, 1),
            Some(EndSessionMessage::Query)
        );
    }

    #[test]
    fn end_session_terminates_only_when_the_session_really_ends() {
        assert_eq!(classify(WM_ENDSESSION, 1), Some(EndSessionMessage::Ending));
        assert_eq!(
            classify(WM_ENDSESSION, 0),
            Some(EndSessionMessage::Cancelled)
        );
    }

    #[test]
    fn other_messages_are_ignored() {
        // WM_CLOSE, WM_DESTROY, WM_QUIT.
        for msg in [0x0010, 0x0002, 0x0012] {
            assert_eq!(classify(msg, 1), None);
        }
    }

    #[test]
    fn mnemos_shuts_down_before_its_child_processes() {
        // Documented application range is 0x100-0x3FF; 0x300 and up is "first".
        const { assert!(MNEMOS_SHUTDOWN_LEVEL >= 0x300 && MNEMOS_SHUTDOWN_LEVEL <= 0x3FF) };
        const { assert!(MNEMOS_SHUTDOWN_LEVEL > DEFAULT_SHUTDOWN_LEVEL) };
    }
}
