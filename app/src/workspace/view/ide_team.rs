// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE: the workspace side of team sessions (`workspace::team_control`).
//! Every operation names its terminal by view id and acts in the tab that
//! hosts it, so a coordinator's spawned sessions always land next to it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use mnemos_ui::{AppContext, EntityId, ViewContext};

use super::Workspace;
use crate::terminal::shell::ShellType;

impl Workspace {
    /// The index of the tab whose panes include terminal `view_id`.
    pub(crate) fn ide_team_tab_of(&self, view_id: EntityId, ctx: &AppContext) -> Option<usize> {
        self.tabs.iter().position(|tab| {
            tab.pane_group
                .as_ref(ctx)
                .find_pane_id_for_terminal_view(view_id, ctx)
                .is_some()
        })
    }

    /// The terminal views in the same tab as `view_id`.
    pub(crate) fn ide_team_tab_terminals(&self, view_id: EntityId, ctx: &AppContext) -> Vec<EntityId> {
        let Some(index) = self.ide_team_tab_of(view_id, ctx) else {
            return Vec::new();
        };
        let pane_group = self.tabs[index].pane_group.as_ref(ctx);
        pane_group
            .terminal_pane_ids()
            .filter_map(|pane_id| pane_group.terminal_view_from_pane_id(pane_id, ctx))
            .map(|view| view.id())
            .collect()
    }

    /// Terminal `view_id`'s shell working directory, when it is local.
    pub(crate) fn ide_team_shell_cwd(&self, view_id: EntityId, ctx: &AppContext) -> Option<PathBuf> {
        let index = self.ide_team_tab_of(view_id, ctx)?;
        let pane_group = self.tabs[index].pane_group.as_ref(ctx);
        pane_group
            .find_pane_id_for_terminal_view(view_id, ctx)
            .and_then(|pane_id| pane_group.terminal_view_from_pane_id(pane_id, ctx))
            .and_then(|view| view.as_ref(ctx).active_session_path_if_local(ctx))
    }

    /// Whether terminal `view_id` runs PowerShell (quoting differs).
    pub(crate) fn ide_team_is_powershell(&self, view_id: EntityId, ctx: &AppContext) -> bool {
        let Some(index) = self.ide_team_tab_of(view_id, ctx) else {
            return false;
        };
        let pane_group = self.tabs[index].pane_group.as_ref(ctx);
        pane_group
            .find_pane_id_for_terminal_view(view_id, ctx)
            .and_then(|pane_id| pane_group.terminal_view_from_pane_id(pane_id, ctx))
            .and_then(|view| view.as_ref(ctx).active_session_shell_type(ctx))
            == Some(ShellType::PowerShell)
    }

    /// Splits a new terminal pane next to `caller` in its tab, titled `name`,
    /// and runs `command` there once its shell is up. Returns the new terminal.
    pub(crate) fn ide_team_split(
        &mut self,
        caller: EntityId,
        directory: PathBuf,
        env_vars: HashMap<OsString, OsString>,
        command: &str,
        name: &str,
        ctx: &mut ViewContext<Self>,
    ) -> Option<EntityId> {
        let index = self.ide_team_tab_of(caller, ctx)?;
        let pane_group = self.tabs[index].pane_group.clone();
        let view = pane_group.update(ctx, |pane_group, pctx| {
            let base = pane_group.find_pane_id_for_terminal_view(caller, pctx)?;
            pane_group.ide_split_terminal_running(base, directory, env_vars, command, pctx)
        })?;
        view.as_ref(ctx)
            .model
            .lock()
            .set_custom_title(Some(name.to_string()));
        Some(view.id())
    }

    /// Retitles terminal `view_id`. Returns whether this window hosts it.
    pub(crate) fn ide_team_set_title(&mut self, view_id: EntityId, name: &str, ctx: &mut ViewContext<Self>) -> bool {
        let Some(index) = self.ide_team_tab_of(view_id, ctx) else {
            return false;
        };
        let pane_group = self.tabs[index].pane_group.as_ref(ctx);
        let Some(view) = pane_group
            .find_pane_id_for_terminal_view(view_id, ctx)
            .and_then(|pane_id| pane_group.terminal_view_from_pane_id(pane_id, ctx))
        else {
            return false;
        };
        view.as_ref(ctx).model.lock().set_custom_title(Some(name.to_string()));
        true
    }

    /// Switches to terminal `view_id`'s tab and focuses its pane. Returns
    /// whether this window hosts it.
    pub(crate) fn ide_team_focus(&mut self, view_id: EntityId, ctx: &mut ViewContext<Self>) -> bool {
        if self.ide_team_tab_of(view_id, ctx).is_none() {
            return false;
        }
        self.activate_cli_agent_terminal(view_id.as_usize() as u64, ctx);
        true
    }

    /// Closes terminal `view_id`'s pane. Returns whether this window hosted it.
    pub(crate) fn ide_team_close(&mut self, view_id: EntityId, ctx: &mut ViewContext<Self>) -> bool {
        let Some(index) = self.ide_team_tab_of(view_id, ctx) else {
            return false;
        };
        let pane_group = self.tabs[index].pane_group.clone();
        pane_group.update(ctx, |pane_group, pctx| {
            match pane_group.find_pane_id_for_terminal_view(view_id, pctx) {
                Some(pane_id) => {
                    pane_group.close_pane(pane_id, pctx);
                    true
                }
                None => false,
            }
        })
    }
}
