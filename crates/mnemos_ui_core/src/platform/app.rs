use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::future::LocalBoxFuture;

use crate::modals::ModalId;
use crate::windowing::state::ApplicationStage;
use crate::windowing::{WindowCallbackDispatcher, WindowManager};
use crate::{
    keymap::Keystroke, notification, AppContext, ClosedWindowData, SingletonEntity, WindowId,
};

use super::menu::MenuItemPropertyChanges;

pub type AppInitCallbackFn =
    Box<dyn FnOnce(&mut crate::AppContext, LocalBoxFuture<'static, crate::App>)>;

pub type TerminationResult = anyhow::Result<()>;

/// A collection of callbacks which application developers can provide
/// to hook into important events that are observed by the UI framework.
#[derive(Default)]
#[allow(clippy::type_complexity)]
pub struct AppCallbacks {
    pub on_become_active: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_notification_clicked:
        Option<Box<dyn FnMut(notification::NotificationResponse, &mut AppContext)>>,
    pub on_resigned_active: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_will_terminate: Option<Box<dyn FnMut(&mut AppContext)>>,
    /// Callback on whether the app will proceed with termination.
    pub on_should_terminate_app: Option<Box<dyn FnMut(&mut AppContext) -> ApproveTerminateResult>>,
    /// Callback on whether the window will proceed with closing.
    pub on_should_close_window:
        Option<Box<dyn FnMut(WindowId, &mut AppContext) -> ApproveTerminateResult>>,
    /// Callback for when the user clicks "don't show again" on the warning modal.
    pub on_disable_warning_modal: Option<Box<dyn FnMut(&mut AppContext)>>,
    /// Callback on when the internet reachability to a specific host has changed.
    /// The host name here could be a string for an IP address or domain.
    pub on_internet_reachability_changed: Option<Box<dyn FnMut(bool, &mut AppContext)>>,
    pub on_active_window_changed: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_new_window_requested: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_window_moved: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_window_resized: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_window_will_close: Option<Box<dyn FnMut(Option<ClosedWindowData>, &mut AppContext)>>,
    /// Callback for screen parameter changes. For example, user connecting/
    /// disconnecting external monitor, changes screen arrangement, or adjusts screen
    /// resolution.
    pub on_screen_changed: Option<Box<dyn FnMut(&mut AppContext)>>,
    pub on_open_files: Option<Box<dyn FnMut(Vec<PathBuf>, &mut AppContext)>>,
    pub on_open_urls: Option<Box<dyn FnMut(Vec<String>, &mut AppContext)>>,
    pub on_os_appearance_changed: Option<Box<dyn FnMut(&mut AppContext)>>,
    /// Callback to hook into a notification for when the cpu was awakened after sleeping.
    pub on_cpu_awakened: Option<Box<dyn FnMut(&mut AppContext)>>,
    /// Callback to hook into a notification for when the cpu is about to go to sleep.
    pub on_cpu_will_sleep: Option<Box<dyn FnMut(&mut AppContext)>>,
}

/// A helper structure to simplify and standardize the act of making calls from
/// platform code into user application code.
pub struct AppCallbackDispatcher {
    callbacks: AppCallbacks,
    ui_app: crate::App,
    /// Mnemos IDE: `callbacks.on_will_terminate` lives here instead, so a platform
    /// hook outside the event loop handler (the Windows end-session window) can
    /// run the same terminate step synchronously.
    will_terminate: WillTerminateHook,
}

/// Mnemos IDE: set when the OS is ending the session (Windows shutdown, restart or
/// logoff) and the app is terminating because of it rather than because it was
/// asked to quit. Lets `on_will_terminate` skip work that must not start while the
/// OS is going down, such as launching an update installer.
static OS_SESSION_ENDING: AtomicBool = AtomicBool::new(false);

/// Mnemos IDE: whether the current termination comes from the OS ending the session.
pub fn os_session_ending() -> bool {
    OS_SESSION_ENDING.load(Ordering::SeqCst)
}

/// Mnemos IDE: marks the current termination as caused by the OS ending the session.
pub fn set_os_session_ending() {
    OS_SESSION_ENDING.store(true, Ordering::SeqCst);
}

/// Mnemos IDE: what [`WillTerminateHook::run`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WillTerminateOutcome {
    /// The terminate step ran now.
    Ran,
    /// It had already run (or was running); it never runs twice. A second run
    /// would, among other things, write an empty agent-restore snapshot after
    /// the PTYs were torn down, deleting the real one.
    AlreadyRan,
    /// The app context was borrowed (the call came from inside a nested message
    /// pump during an update), so nothing ran and nothing was marked as run.
    AppBusy,
}

type WillTerminateCallback = Box<dyn FnMut(&mut AppContext)>;

/// Mnemos IDE: the app's will-terminate step (set the `Terminating` stage, then run
/// `on_will_terminate`), shared between the event loop and platform hooks.
/// It runs at most once per process.
#[derive(Clone)]
pub struct WillTerminateHook {
    ui_app: crate::App,
    callback: Rc<RefCell<Option<WillTerminateCallback>>>,
    ran: Rc<Cell<bool>>,
}

impl WillTerminateHook {
    fn new(ui_app: crate::App, callback: Option<WillTerminateCallback>) -> Self {
        Self {
            ui_app,
            callback: Rc::new(RefCell::new(callback)),
            ran: Rc::new(Cell::new(false)),
        }
    }

    /// Runs the terminate step if it has not run yet and the app context is free.
    pub fn run(&mut self) -> WillTerminateOutcome {
        if self.ran.get() {
            return WillTerminateOutcome::AlreadyRan;
        }
        if !self.ui_app.can_borrow_mut() {
            return WillTerminateOutcome::AppBusy;
        }
        self.ran.set(true);

        log::info!("application will terminate");
        self.ui_app.update(|ctx| {
            WindowManager::handle(ctx).update(ctx, |state, ctx| {
                state.set_stage(ApplicationStage::Terminating, ctx);
            });
        });

        // Taken out of the cell before it runs, so nothing is borrowed while it does.
        let callback = self.callback.borrow_mut().take();
        if let Some(mut callback) = callback {
            self.ui_app.update(|ctx| callback(ctx));
        }
        WillTerminateOutcome::Ran
    }
}

pub enum ApproveTerminateResult {
    /// The window or app should be closed.
    Terminate,
    /// Do not close the window or app.
    Cancel,
}

impl AppCallbackDispatcher {
    pub fn new(mut callbacks: AppCallbacks, ui_app: crate::App) -> Self {
        // Mnemos IDE: see `will_terminate`.
        let will_terminate =
            WillTerminateHook::new(ui_app.clone(), callbacks.on_will_terminate.take());
        Self {
            callbacks,
            ui_app,
            will_terminate,
        }
    }

    /// Mnemos IDE: a handle that runs the same terminate step as
    /// [`Self::app_will_terminate`], for platform code that has to run it from
    /// outside the event loop handler (the Windows end-session window).
    pub fn will_terminate_hook(&self) -> WillTerminateHook {
        self.will_terminate.clone()
    }

    pub fn initialize_app(&mut self, init_fn: AppInitCallbackFn) {
        let app_clone = self.ui_app.clone();
        self.ui_app.update(|ctx| {
            use futures_util::FutureExt;

            // Provide the init function with access to the UI app,
            // but only from a future that is running on the main
            // thread (to prevent double-borrow issues).
            init_fn(ctx, async move { app_clone }.boxed_local());

            // Validate all of the registered bindings now that the app is initialized.
            ctx.validate_bindings();
        });
    }

    pub fn app_became_active(&mut self) {
        log::info!("application did become active");
        self.ui_app.update(|ctx| {
            WindowManager::handle(ctx).update(ctx, |windowing_state, ctx| {
                windowing_state.set_stage(ApplicationStage::Active, ctx);
            });
        });

        if let Some(callback) = &mut self.callbacks.on_become_active {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    // This is not called on Linux or wasm, as there isn't any generic way to
    // click on/interact with a notification.
    // TODO(CORE-2322): implement desktop notifications on Windows
    #[cfg_attr(
        any(target_os = "linux", target_os = "windows", target_family = "wasm"),
        allow(dead_code)
    )]
    pub fn notification_clicked(&mut self, response: notification::NotificationResponse) {
        if let Some(callback) = &mut self.callbacks.on_notification_clicked {
            self.ui_app.update(|ctx| callback(response, ctx));
        }
    }

    pub fn app_resigned_active(&mut self) {
        self.ui_app.update(|ctx| {
            WindowManager::handle(ctx).update(ctx, |state, ctx| {
                state.set_stage(ApplicationStage::Inactive, ctx);
            });
        });
        if let Some(callback) = &mut self.callbacks.on_resigned_active {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn app_will_terminate(&mut self) {
        // Mnemos IDE: shared with the Windows end-session hook, and runs once.
        let outcome = self.will_terminate.run();
        if outcome != WillTerminateOutcome::Ran {
            log::info!("application will terminate: {outcome:?}");
        }
    }

    pub fn should_terminate_app(&mut self) -> ApproveTerminateResult {
        if let Some(callback) = &mut self.callbacks.on_should_terminate_app {
            self.ui_app.update(|ctx| callback(ctx))
        } else {
            ApproveTerminateResult::Terminate
        }
    }

    pub fn should_close_window(&mut self, window_id: WindowId) -> ApproveTerminateResult {
        if let Some(callback) = &mut self.callbacks.on_should_close_window {
            self.ui_app.update(|ctx| callback(window_id, ctx))
        } else {
            ApproveTerminateResult::Terminate
        }
    }

    // Dead code is allowed on wasm as when we register the network connection
    // listener on wasm, we don't yet have access to an `AppCallbackDispatcher`,
    // so we directly check the `Callbacks` object instead.
    // TODO(CORE-2683): implement events for internet reachability changes
    #[cfg_attr(any(target_family = "wasm", target_os = "windows"), allow(dead_code))]
    pub fn has_internet_reachability_changed_callback(&self) -> bool {
        self.callbacks.on_internet_reachability_changed.is_some()
    }

    pub fn internet_reachability_changed(&mut self, is_reachable: bool) {
        if is_reachable {
            log::info!("application can reach internet");
        } else {
            log::info!("application can not reach internet");
        }
        if let Some(callback) = &mut self.callbacks.on_internet_reachability_changed {
            self.ui_app.update(|ctx| callback(is_reachable, ctx));
        }
    }

    pub fn active_window_changed(&mut self, active_window_id: Option<WindowId>) {
        log::info!("active window changed: {active_window_id:?}");
        self.ui_app.update(|ctx| {
            WindowManager::handle(ctx).update(ctx, |state, ctx| {
                state.set_active_window(active_window_id, ctx);
            });
        });

        if let Some(callback) = &mut self.callbacks.on_active_window_changed {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn open_new_window(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_new_window_requested {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn window_moved(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_window_moved {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn window_resized(&mut self) {
        log::info!("window resized");
        if let Some(callback) = &mut self.callbacks.on_window_resized {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn window_will_close(&mut self, window_id: WindowId) {
        log::info!("{window_id:?} will close");
        if let Some(callback) = &mut self.callbacks.on_window_will_close {
            self.ui_app.update(|ctx| {
                let closed_window_data = ctx.handle_window_closed(window_id);
                callback(closed_window_data, ctx);
            });
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn screen_changed(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_screen_changed {
            self.ui_app.update(|ctx| callback(ctx));
        }

        // Update the window fullscreen state, which in turn triggers a re-render of the workspace view.
        self.ui_app.update(|ctx| {
            WindowManager::handle(ctx).update(ctx, |state, model_ctx| {
                state.update_is_active_window_fullscreen(model_ctx);
            });
        });
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn open_files(&mut self, file_paths: Vec<PathBuf>) {
        if let Some(callback) = &mut self.callbacks.on_open_files {
            self.ui_app.update(|ctx| callback(file_paths, ctx));
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn open_urls(&mut self, urls: Vec<String>) {
        if let Some(callback) = &mut self.callbacks.on_open_urls {
            self.ui_app.update(|ctx| callback(urls, ctx));
        }
    }

    pub fn os_appearance_changed(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_os_appearance_changed {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn cpu_awakened(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_cpu_awakened {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn cpu_will_sleep(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_cpu_will_sleep {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }

    pub fn global_shortcut_triggered(&mut self, shortcut: Keystroke) {
        self.ui_app
            .update(|ctx| ctx.on_global_shortcut_triggered(shortcut))
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused))]
    pub fn can_borrow_mut(&self) -> bool {
        self.ui_app.can_borrow_mut()
    }

    pub fn with_mutable_app_context<T>(
        &mut self,
        callback: impl FnOnce(&mut AppContext) -> T,
    ) -> T {
        self.ui_app.update(|ctx| callback(ctx))
    }

    pub fn for_window<'a>(
        &'a mut self,
        window: &'a dyn super::Window,
    ) -> WindowCallbackDispatcher<'a> {
        WindowCallbackDispatcher::new(window.callbacks(), self.ui_app.as_mut())
    }
}

// Functions in AppCallbackDispatcher that relate to application menus.
//
// This is marked as `allow(dead_code)` on Linux, as it doesn't support
// application menus, so these never get called.
// TODO(CORE-2691): implement native Windows OS app menus
#[cfg_attr(
    any(target_os = "linux", target_os = "windows", target_family = "wasm"),
    allow(dead_code)
)]
impl AppCallbackDispatcher {
    pub fn menu_item_triggered(&mut self, callback: impl FnOnce(&mut AppContext)) {
        self.ui_app.update(callback);
    }

    pub fn update_menu_item(
        &mut self,
        callback: impl FnOnce(&mut AppContext) -> MenuItemPropertyChanges,
    ) -> MenuItemPropertyChanges {
        self.ui_app.update(callback)
    }
}

// Functions in AppCallbackDispatcher that relate to native platform modals.
//
// This is marked as `allow(dead_code)` on Linux and WASM, as we do not support
// native platform modals on these platforms, so these never get called.
// TODO(CORE-2323): implement native Windows OS modal
#[cfg_attr(
    any(target_os = "linux", target_os = "windows", target_family = "wasm"),
    allow(dead_code)
)]
impl AppCallbackDispatcher {
    pub fn process_platform_modal_response(
        &mut self,
        modal_id: ModalId,
        response_button_index: usize,
        disable_modal: bool,
    ) {
        self.ui_app.update(|ctx| {
            ctx.process_platform_modal_response(modal_id, response_button_index, disable_modal)
        });
    }

    pub fn warning_modal_disabled(&mut self) {
        if let Some(callback) = &mut self.callbacks.on_disable_warning_modal {
            self.ui_app.update(|ctx| callback(ctx));
        }
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
