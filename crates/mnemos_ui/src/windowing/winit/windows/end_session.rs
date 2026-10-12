//! Mnemos IDE: run the app's normal terminate step when Windows ends the session.
//!
//! A shutdown, restart or logoff sends `WM_QUERYENDSESSION` and `WM_ENDSESSION` to every
//! top-level window. winit has no event for either (its window procedure hands them to
//! `DefWindowProcW`), and its message hook only sees posted messages, while these two are
//! sent. So Mnemos creates one hidden top-level window of its own on the UI thread and
//! answers them there:
//!
//! * `WM_QUERYENDSESSION`: return TRUE. Mnemos never blocks a shutdown.
//! * `WM_ENDSESSION` with `wParam = TRUE`: run the same will-terminate step a normal quit
//!   runs (`AppCallbackDispatcher::app_will_terminate`: the `Terminating` stage, then the
//!   app's `on_will_terminate`, which writes the agent-restore snapshot and flushes the
//!   SQLite writer), then exit. Windows may kill the process as soon as this message
//!   returns, so all of it happens inside the handler.
//!
//! A hidden window is used rather than subclassing winit's windows: it needs no
//! comctl32 v6, never interferes with winit's own window procedure, and exists even
//! when every Mnemos window is closed.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::SetProcessShutdownParameters;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassExW, WINDOW_EX_STYLE, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::platform::app::{set_os_session_ending, WillTerminateHook, WillTerminateOutcome};
use crate::windowing::end_session::{classify, EndSessionMessage, MNEMOS_SHUTDOWN_LEVEL};

thread_local! {
    /// The terminate step, set once on the UI thread by [`install`] and read by the
    /// window procedure, which Windows calls on that same thread.
    static WILL_TERMINATE: RefCell<Option<WillTerminateHook>> = const { RefCell::new(None) };
}

/// Creates the hidden end-session window and raises the process shutdown level.
///
/// Must be called on the UI thread (the thread running the winit event loop), because
/// sent messages are delivered to a window on the thread that created it, while that
/// thread pumps messages.
pub fn install(hook: WillTerminateHook) -> anyhow::Result<()> {
    WILL_TERMINATE.with(|cell| *cell.borrow_mut() = Some(hook));

    // Ask to be shut down before the default level, so the shells and `claude`
    // sessions are still alive when the snapshot is taken. A failure only loses that
    // ordering, so it is logged and the window is created anyway.
    if let Err(e) = unsafe { SetProcessShutdownParameters(MNEMOS_SHUTDOWN_LEVEL, 0) } {
        log::warn!("could not raise the process shutdown level: {e}");
    }

    let class_name = w!("MnemosEndSessionWindow");
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(end_session_wnd_proc),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(windows::core::Error::from_thread().into());
    }

    // Top-level (no parent, not HWND_MESSAGE: message-only windows get no
    // end-session messages) and never shown.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Mnemos"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    }?;
    Ok(())
}

unsafe extern "system" fn end_session_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match classify(msg, wparam.0) {
        Some(EndSessionMessage::Query) => LRESULT(1),
        Some(EndSessionMessage::Cancelled) => LRESULT(0),
        Some(EndSessionMessage::Ending) => {
            log::info!("Windows is ending the session (lParam {:#x})", lparam.0);
            set_os_session_ending();
            // Cloned out so the thread-local is not borrowed while the app runs.
            let hook = WILL_TERMINATE.with(|cell| cell.borrow().clone());
            let Some(mut hook) = hook else {
                return LRESULT(0);
            };
            match hook.run() {
                WillTerminateOutcome::Ran => {
                    log::info!("terminated for the end of the Windows session");
                    // Same as the event loop's own exit: the app is torn down now,
                    // and Windows may kill the process at any moment anyway.
                    std::process::exit(0);
                }
                outcome => {
                    // AppBusy: the message arrived inside an app update (a nested
                    // message pump), so the app context cannot be entered. AlreadyRan:
                    // a quit is already in progress. Either way, let Windows proceed.
                    log::warn!("could not terminate for the end of the session: {outcome:?}");
                    LRESULT(0)
                }
            }
        }
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
