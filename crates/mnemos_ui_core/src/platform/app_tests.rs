//! Mnemos IDE: tests for the shared will-terminate step.

use std::cell::Cell;
use std::rc::Rc;

use super::{AppCallbackDispatcher, AppCallbacks, WillTerminateOutcome};
use crate::windowing::state::ApplicationStage;
use crate::App;

fn dispatcher_counting_runs(app: &App) -> (AppCallbackDispatcher, Rc<Cell<u32>>) {
    let runs = Rc::new(Cell::new(0));
    let runs_in_callback = runs.clone();
    let callbacks = AppCallbacks {
        on_will_terminate: Some(Box::new(move |ctx| {
            // The stage is already Terminating when the app callback runs, which is
            // what keeps shell exits during shutdown from closing projects.
            assert_eq!(ctx.windows().stage(), ApplicationStage::Terminating);
            runs_in_callback.set(runs_in_callback.get() + 1);
        })),
        ..Default::default()
    };
    (AppCallbackDispatcher::new(callbacks, app.clone()), runs)
}

#[test]
fn will_terminate_runs_once_across_the_hook_and_the_event_loop() {
    App::test((), |app| async move {
        let (mut dispatcher, runs) = dispatcher_counting_runs(&app);
        let mut hook = dispatcher.will_terminate_hook();

        assert_eq!(hook.run(), WillTerminateOutcome::Ran);
        assert_eq!(runs.get(), 1);

        // The event loop's own exit path afterwards must not run it again.
        dispatcher.app_will_terminate();
        assert_eq!(runs.get(), 1);
        assert_eq!(hook.run(), WillTerminateOutcome::AlreadyRan);
        assert_eq!(runs.get(), 1);
    });
}

#[test]
fn event_loop_exit_still_runs_will_terminate() {
    App::test((), |app| async move {
        let (mut dispatcher, runs) = dispatcher_counting_runs(&app);
        dispatcher.app_will_terminate();
        assert_eq!(runs.get(), 1);
        assert_eq!(
            dispatcher.will_terminate_hook().run(),
            WillTerminateOutcome::AlreadyRan
        );
    });
}

#[test]
fn will_terminate_does_nothing_while_the_app_is_borrowed() {
    App::test((), |mut app| async move {
        let (dispatcher, runs) = dispatcher_counting_runs(&app);
        let mut hook = dispatcher.will_terminate_hook();

        // A sent message that arrives inside an update (a nested message pump)
        // must not panic on the RefCell, and must not use up the single run.
        app.update(|_ctx| {
            assert_eq!(hook.run(), WillTerminateOutcome::AppBusy);
        });
        assert_eq!(runs.get(), 0);

        assert_eq!(hook.run(), WillTerminateOutcome::Ran);
        assert_eq!(runs.get(), 1);
    });
}
