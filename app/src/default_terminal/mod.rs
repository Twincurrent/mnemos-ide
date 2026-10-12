use mnemos_core::user_preferences::GetUserPreferences as _;
use mnemos_ui::{
    windowing::{StateEvent, WindowManager},
    Entity, ModelContext, SingletonEntity,
};

/// Mnemos IDE: user preferences key holding the bundle id of the default terminal Mnemos replaced,
/// so it can be restored.
const PREVIOUS_DEFAULT_TERMINAL_KEY: &str = "PreviousDefaultTerminalBundleId";

/// Mnemos IDE: what macOS opens shell scripts with when no handler was ever chosen.
const SYSTEM_DEFAULT_TERMINAL: &str = "com.apple.Terminal";

#[cfg(target_os = "macos")]
mod mac;

#[cfg(target_os = "macos")]
use mac::*;

#[allow(dead_code)]
#[cfg(not(target_os = "macos"))]
mod non_mac {
    pub fn can_become_default_terminal() -> bool {
        false
    }

    pub fn is_mnemos_default_terminal() -> bool {
        false
    }

    /// Sets Mnemos as the default terminal
    pub fn set_mnemos_as_default_terminal() -> Result<(), String> {
        Err("Not implemented".to_string())
    }

    pub fn current_default_terminal() -> Option<String> {
        None
    }

    pub fn mnemos_bundle_id() -> Option<String> {
        None
    }

    pub fn set_default_terminal(_bundle_id: &str) -> Result<(), String> {
        Err("Not implemented".to_string())
    }
}

#[allow(unused_imports)]
#[cfg(not(target_os = "macos"))]
use non_mac::*;

pub struct DefaultTerminal {
    /// Whether the OS will treat Mnemos as the default app for scripts/executables.
    is_mnemos_default: bool,
}

impl DefaultTerminal {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(
            &WindowManager::handle(ctx),
            Self::handle_window_manager_event,
        );

        // This can be slow to compute due to calling into platform APIs, so in unit
        // tests, where we shouldn't care, just pretend that we are not.
        let is_mnemos_default = if cfg!(test) {
            false
        } else {
            is_mnemos_default_terminal()
        };

        Self { is_mnemos_default }
    }

    /// This is an OS-level setting. Unlike most other settings, where Mnemos is the source-of-truth
    /// for the value of the setting, it can be changed outside of Mnemos. We monitor if it gets
    /// changed externally by checking when Mnemos is focused.
    fn handle_window_manager_event(&mut self, event: &StateEvent, ctx: &mut ModelContext<Self>) {
        match event {
            StateEvent::ValueChanged { current, previous } => {
                if current.active_window.is_some() && previous.active_window.is_none() {
                    let is_mnemos_default_now = is_mnemos_default_terminal();
                    if is_mnemos_default_now != self.is_mnemos_default {
                        self.set_is_mnemos_default(is_mnemos_default_now, ctx);
                    }
                }
            }
        }
    }

    fn set_is_mnemos_default(&mut self, value: bool, ctx: &mut ModelContext<Self>) {
        self.is_mnemos_default = value;
        ctx.emit(DefaultTerminalEvent::ValueChanged);
        ctx.notify();
    }

    pub fn can_mnemos_become_default() -> bool {
        if cfg!(test) {
            // Determining whether or not we can become the default terminal requires
            // calling into platform APIs, which can be slow, and we can't actually
            // set the default terminal in unit tests, so just say we can't.
            false
        } else {
            can_become_default_terminal()
        }
    }

    pub fn is_mnemos_default(&self) -> bool {
        self.is_mnemos_default
    }

    /// Makes Mnemos the default terminal.
    ///
    /// Mnemos IDE: the handler it replaces is saved first (Terminal when none was set), so
    /// [`Self::restore_previous_default`] can undo this.
    pub fn make_mnemos_default(&mut self, ctx: &mut ModelContext<Self>) {
        let previous = current_default_terminal()
            .filter(|handler| !handler.is_empty())
            .unwrap_or_else(|| SYSTEM_DEFAULT_TERMINAL.to_owned());
        if Some(previous.as_str()) != mnemos_bundle_id().as_deref() {
            if let Err(e) = ctx
                .private_user_preferences()
                .write_value(PREVIOUS_DEFAULT_TERMINAL_KEY, previous)
            {
                log::warn!("Could not save the previous default terminal: {e:#}");
            }
        }
        if let Err(e) = set_mnemos_as_default_terminal() {
            log::error!("Error setting Mnemos as default terminal: {e:#}");
        } else {
            self.set_is_mnemos_default(true, ctx);
        }
    }

    /// Mnemos IDE: the bundle id of the default terminal Mnemos replaced, if Mnemos replaced one.
    pub fn previous_default(&self, ctx: &mnemos_ui::AppContext) -> Option<String> {
        ctx.private_user_preferences()
            .read_value(PREVIOUS_DEFAULT_TERMINAL_KEY)
            .unwrap_or_default()
            .filter(|bundle_id| !bundle_id.is_empty())
    }

    /// Mnemos IDE: gives the default terminal role back to the app Mnemos took it from.
    pub fn restore_previous_default(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(previous) = self.previous_default(ctx) else {
            return;
        };
        match set_default_terminal(&previous) {
            Ok(()) => {
                let _ = ctx
                    .private_user_preferences()
                    .write_value(PREVIOUS_DEFAULT_TERMINAL_KEY, String::new());
                self.set_is_mnemos_default(is_mnemos_default_terminal(), ctx);
            }
            Err(e) => log::error!("Error restoring the default terminal {previous}: {e:#}"),
        }
    }
}

pub enum DefaultTerminalEvent {
    ValueChanged,
}

impl Entity for DefaultTerminal {
    type Event = DefaultTerminalEvent;
}

impl SingletonEntity for DefaultTerminal {}
