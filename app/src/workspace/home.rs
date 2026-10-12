//! Mnemos Home
//!
//! This is the landing page for new tabs if session creation isn't supported (e.g. on the web).
//! It's barebones at the moment, but may grow into a more full-featured admin experience.

use mnemos_ui::ViewContext;

use super::view::Workspace;
use crate::pane_group::{AnyPaneContent, FilePane};

const MNEMOS_HOME_TITLE: &str = "Welcome to Mnemos on Web";
const MNEMOS_HOME_CONTENT: &str = r#"
Welcome to Mnemos on Web - your browser-based home for Mnemos! 
Use Mnemos on Web to:
* Join Shared Sessions
* Create, View, and Edit Mnemos Drive Objects
* Manage your Mnemos Settings

Mnemos on Web can also be used by your teammates and peers who don't have Mnemos downloaded yet to view your shared sessions, notebooks, and workflows."#;

/// Create a static "home page" pane.
pub fn create_home_pane(ctx: &mut ViewContext<Workspace>) -> Box<dyn AnyPaneContent> {
    let pane = FilePane::new(
        None,
        None,
        #[cfg(feature = "local_fs")]
        None,
        ctx,
    );
    pane.file_view(ctx).update(ctx, |pane, ctx| {
        pane.open_static(MNEMOS_HOME_TITLE, MNEMOS_HOME_CONTENT, ctx);
    });
    Box::new(pane)
}
