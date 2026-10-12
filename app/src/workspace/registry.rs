use std::collections::HashMap;

use mnemos_ui::{AppContext, Entity, SingletonEntity, WeakViewHandle, WindowId};

use super::ide_sync::{IdeProjectSync, WorkspaceRegistryEvent};
use super::Workspace;

/// A registry that tracks all workspace views by their window ID.
///
/// This provides O(1) lookup of workspaces instead of the O(n) linear scan
/// that `views_of_type::<Workspace>` performs.
///
/// It also hosts the Mnemos IDE project-tab queue (`ide_sync`): that work spans
/// every window, and this is the one model every workspace already depends
/// on.
pub struct WorkspaceRegistry {
    workspaces: HashMap<WindowId, WeakViewHandle<Workspace>>,
    pub(super) ide: IdeProjectSync,
    pub(super) live_agents: super::live_agents::LiveAgentsSync,
}

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceRegistry {
    pub fn new() -> Self {
        Self {
            workspaces: HashMap::new(),
            ide: IdeProjectSync::default(),
            live_agents: Default::default(),
        }
    }

    /// Registers a workspace for the given window.
    pub fn register(&mut self, window_id: WindowId, workspace: WeakViewHandle<Workspace>) {
        self.workspaces.insert(window_id, workspace);
    }

    /// Unregisters the workspace for the given window.
    pub fn unregister(&mut self, window_id: WindowId) {
        self.workspaces.remove(&window_id);
    }

    /// Returns the workspace for the given window, if it is still alive.
    pub fn get(
        &self,
        window_id: WindowId,
        app: &AppContext,
    ) -> Option<mnemos_ui::ViewHandle<Workspace>> {
        self.workspaces.get(&window_id)?.upgrade(app)
    }

    /// Returns all registered workspaces that are still alive.
    /// The returned vector contains tuples of (WindowId, ViewHandle<Workspace>).
    ///
    /// A workspace that is being updated right now cannot be upgraded, so it
    /// is missing from the result when this is called from inside one.
    pub fn all_workspaces(
        &self,
        app: &AppContext,
    ) -> Vec<(WindowId, mnemos_ui::ViewHandle<Workspace>)> {
        self.workspaces
            .iter()
            .filter_map(|(window_id, weak_handle)| {
                weak_handle.upgrade(app).map(|handle| (*window_id, handle))
            })
            .collect()
    }

    /// Windows with a registered workspace, including one that is being
    /// updated right now (which [`Self::all_workspaces`] leaves out).
    pub fn window_ids(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.workspaces.keys().copied()
    }
}

impl Entity for WorkspaceRegistry {
    type Event = WorkspaceRegistryEvent;
}

impl SingletonEntity for WorkspaceRegistry {}
