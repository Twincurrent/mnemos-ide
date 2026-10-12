use std::cell::RefCell;
use std::collections::HashMap;

use mnemos_ui::{
    elements::{
        Align, Container, CornerRadius, Element, Empty, Flex, MouseStateHandle, ParentElement,
        Radius, Text,
    },
    fonts::Weight,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
        switch::SwitchStateHandle,
    },
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::{
    settings_page::{
        build_toggle_element, render_body_item, render_body_item_label,
        render_body_item_label_with_icon, render_separator, render_sub_header, MatchData, PageType,
        SettingsPageMeta, SettingsPageViewHandle, SettingsWidget,
    },
    LocalOnlyIconState, SettingsSection, ToggleState,
};
use crate::appearance::Appearance;
use crate::auth::auth_manager::{AuthManager, AuthManagerEvent};
use crate::editor::{EditorView, SingleLineEditorOptions, TextOptions};
use crate::report_if_error;
use crate::terminal::general_settings::GeneralSettings;
use settings::Setting as _;
use crate::auth::ide_sync_auth;
use crate::themes::theme::Fill;
use crate::ui_components::icons::Icon;
use mnemos_ide_sync::SyncAuthState;

// Mnemos IDE: end-to-end encryption and the .env choice.
mod e2e;

const PAGE_TITLE: &str = "Sync";
const PAGE_INTRO: &str = "Control which projects sync their workspace state across your \
    machines. New projects sync by default. Unchecking a project keeps it local-only on \
    this machine.";
const PROJECTS_SUBHEADER: &str = "Projects";
const EMPTY_STATE_TEXT: &str =
    "No projects yet - add one from the project tab bar to configure sync.";
const SIGNED_IN_LABEL: &str = "Signed in to the admin platform";
const SIGNED_IN_TEXT: &str = "Claude config, sessions and projects sync with your other machines.";
const NOT_SET_UP_LABEL: &str = "Sync is off";
const NOT_SET_UP_TEXT: &str = "Optional: Mnemos works fully offline without an account. Sign in \
                               to sync projects, Claude config and sessions across your machines.";

const GATES_SUBHEADER: &str = "Code from your other machines";
const HOOKS_SYNC_LABEL: &str = "Sync hooks and Claude Code commands";
const HOOKS_SYNC_TEXT: &str = "Off by default, per machine. When on, ~/.claude/hooks, \
    statusline.py and the commands in Claude Code's settings (hooks, statusLine, env, \
    apiKeyHelper, plugins) sync with your other machines. Every change that arrives here \
    waits for your approval below before it is written.";
const PENDING_INTRO: &str = "These changes came from another machine (or your team) and \
    can run code here. Nothing is written until you approve it. Dismiss keeps this \
    machine's copy and sends it back up.";

#[derive(Debug, Clone)]
pub enum SyncSettingsPageAction {
    ToggleProject(String),
    /// Open the admin sign-in; its redirect restarts mnemos-ide-sync.
    SignIn,
    /// Sync `~/.claude/.env` on this machine, or stop.
    ToggleClaudeEnv,
    /// Mnemos IDE: delete the account's `.env` copy from the sync server
    /// (only while `.env` sync is off here).
    RemoveServerEnv,
    /// Show the warning before turning end-to-end encryption on.
    E2eBeginTurnOn,
    /// Turn end-to-end encryption on (after the warning).
    E2eConfirmTurnOn,
    E2eCancel,
    /// Copy the recovery key shown once.
    E2eCopyKey,
    /// Hide the recovery key for good.
    E2eKeySaved,
    /// Check the typed recovery key and use it.
    E2eUnlock,
    /// Mnemos IDE: show the warning before starting over unencrypted (the
    /// server has no encryption marker but a key is saved here).
    E2eBeginStartOver,
    /// Mnemos IDE: forget the key saved here and sync unencrypted again.
    E2eStartOver,
    /// Mnemos IDE: turn hooks sync on or off on this machine.
    ToggleHooksSync,
    /// Mnemos IDE: approve the pending item with this id.
    ApprovePending(String),
    /// Mnemos IDE: dismiss the pending item with this id.
    DismissPending(String),
    /// Mnemos IDE: approve every pending item.
    ApproveAllPending,
    /// Mnemos IDE: `sync.live_feed.enabled`.
    ToggleLiveFeed,
    /// Mnemos IDE: `sync.live_feed.share_prompt_text`.
    ToggleLiveFeedShareText,
    /// Mnemos IDE: take part in this listed team on this machine.
    JoinTeam(String),
    /// Mnemos IDE: stop taking part in the team on this machine.
    LeaveTeam,
    /// Mnemos IDE: create a team named after the name field, and join it.
    CreateTeam,
    /// Mnemos IDE: ask to confirm a deletion on the sync server.
    RequestDelete(SyncDeleteKind),
    ConfirmDelete,
    CancelDelete,
}

/// What a deletion on the sync server removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDeleteKind {
    /// Every state key and blob the account synced.
    Data,
    /// The synced data and the account.
    Account,
}

pub struct SyncSettingsPageView {
    page: PageType<Self>,
    switch_states: RefCell<HashMap<String, SwitchStateHandle>>,
    e2e: e2e::E2eUiState,
    hooks_switch: SwitchStateHandle,
    approve_all_button: MouseStateHandle,
    /// Approve and dismiss buttons per pending id.
    approval_buttons: RefCell<HashMap<String, (MouseStateHandle, MouseStateHandle)>>,
    /// The name field for a new team.
    pub(super) team_name_editor: ViewHandle<EditorView>,
    /// The outcome of the last team action, shown under the team rows.
    pub(super) team_message: Option<String>,
    /// A deletion waiting for its confirmation click.
    pub(super) pending_delete: Option<SyncDeleteKind>,
}

impl SyncSettingsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        // The login row follows mnemos-ide-sync's state live: a session the server
        // ends while this page is open, and the sign-in that fixes it.
        ctx.subscribe_to_model(&AuthManager::handle(ctx), |_, _, event, ctx| {
            if matches!(
                event,
                AuthManagerEvent::IdeSyncAuthChanged | AuthManagerEvent::AuthComplete
            ) {
                ctx.notify();
            }
        });
        let e2e = e2e::E2eUiState::new(ctx);
        let team_name_editor = ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions::ui_font_size(Appearance::as_ref(ctx)),
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        });
        Self {
            page: PageType::new_uncategorized(
                vec![
                    Box::new(SyncAuthWidget::default()),
                    Box::new(e2e::SyncSecurityWidget::default()),
                    Box::new(SyncIntroWidget::default()),
                    Box::new(SyncGatesWidget),
                    Box::new(SyncProjectListWidget::default()),
                    Box::new(super::sync_privacy::LiveFeedWidget::default()),
                    Box::new(super::sync_privacy::TeamWidget::default()),
                    Box::new(super::sync_privacy::DeleteDataWidget::default()),
                ],
                Some(PAGE_TITLE),
            ),
            switch_states: RefCell::new(HashMap::new()),
            e2e,
            hooks_switch: SwitchStateHandle::default(),
            approve_all_button: MouseStateHandle::default(),
            approval_buttons: RefCell::new(HashMap::new()),
            team_name_editor,
            team_message: None,
            pending_delete: None,
        }
    }

    fn approval_buttons_for(&self, id: &str) -> (MouseStateHandle, MouseStateHandle) {
        self.approval_buttons
            .borrow_mut()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    fn switch_handle_for(&self, project_id: &str) -> SwitchStateHandle {
        let mut map = self.switch_states.borrow_mut();
        map.entry(project_id.to_string())
            .or_insert_with(SwitchStateHandle::default)
            .clone()
    }
}

impl Entity for SyncSettingsPageView {
    type Event = ();
}

impl TypedActionView for SyncSettingsPageView {
    type Action = SyncSettingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            SyncSettingsPageAction::ToggleProject(project_id) => {
                // Through the locked update: a plain load + save here would
                // write back a stale registry over whatever mnemos-ide-sync or the
                // tab reconcile saved in between.
                let result = mnemos_ide_projects::ProjectStore::update_blocking(|store| {
                    let currently_disabled = store.is_sync_disabled(project_id);
                    store.set_sync_disabled(project_id, !currently_disabled);
                });
                if let Err(e) = result {
                    log::warn!(
                        target: "mnemos_ide",
                        "Sync settings: toggling sync failed for {project_id}: {e}"
                    );
                }
                ctx.notify();
            }
            SyncSettingsPageAction::SignIn => {
                AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                    auth_manager.open_admin_sign_in(ctx);
                });
            }
            SyncSettingsPageAction::ToggleClaudeEnv => self.toggle_claude_env(ctx),
            SyncSettingsPageAction::RemoveServerEnv => self.remove_server_env(ctx),
            SyncSettingsPageAction::E2eBeginTurnOn => self.e2e_begin_turn_on(ctx),
            SyncSettingsPageAction::E2eConfirmTurnOn => self.e2e_confirm_turn_on(ctx),
            SyncSettingsPageAction::E2eCancel => self.e2e_cancel(ctx),
            SyncSettingsPageAction::E2eCopyKey => self.e2e_copy_key(ctx),
            SyncSettingsPageAction::E2eKeySaved => self.e2e_key_saved(ctx),
            SyncSettingsPageAction::E2eUnlock => self.e2e_unlock(ctx),
            SyncSettingsPageAction::E2eBeginStartOver => self.e2e_begin_start_over(ctx),
            SyncSettingsPageAction::E2eStartOver => self.e2e_start_over(ctx),
            SyncSettingsPageAction::ToggleHooksSync => {
                if let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() {
                    manager.set_hooks_sync(!manager.hooks_sync_enabled());
                }
                ctx.notify();
            }
            SyncSettingsPageAction::ApprovePending(id) => {
                if let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() {
                    manager.approve_pending(std::slice::from_ref(id));
                }
                ctx.notify();
            }
            SyncSettingsPageAction::DismissPending(id) => {
                if let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() {
                    manager.dismiss_pending(std::slice::from_ref(id));
                }
                ctx.notify();
            }
            SyncSettingsPageAction::ApproveAllPending => {
                if let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() {
                    let ids: Vec<String> = manager
                        .status()
                        .borrow()
                        .pending_approvals
                        .iter()
                        .map(|p| p.id.clone())
                        .collect();
                    manager.approve_pending(&ids);
                }
                ctx.notify();
            }
            SyncSettingsPageAction::ToggleLiveFeed => {
                GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
                    let on = settings.live_feed_enabled();
                    report_if_error!(settings.live_feed_enabled.set_value(!on, ctx));
                });
                ctx.notify();
            }
            SyncSettingsPageAction::ToggleLiveFeedShareText => {
                GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
                    let on = settings.live_feed_share_text_enabled();
                    report_if_error!(settings.live_feed_share_text.set_value(!on, ctx));
                });
                ctx.notify();
            }
            SyncSettingsPageAction::JoinTeam(team_id) => {
                let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
                    return;
                };
                let team = manager
                    .status()
                    .borrow()
                    .teams
                    .iter()
                    .find(|t| &t.id == team_id)
                    .cloned();
                let Some(team) = team else {
                    return;
                };
                ctx.spawn(
                    async move { manager.join_team(team).await },
                    |me, result, ctx| {
                        me.team_message = result.err().map(|e| format!("Could not join: {e}"));
                        ctx.notify();
                    },
                );
            }
            SyncSettingsPageAction::LeaveTeam => {
                let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
                    return;
                };
                ctx.spawn(
                    async move { manager.leave_team_here().await },
                    |me, result, ctx| {
                        me.team_message = result.err().map(|e| format!("Could not leave: {e}"));
                        ctx.notify();
                    },
                );
            }
            SyncSettingsPageAction::CreateTeam => {
                let name = self.team_name_editor.as_ref(ctx).buffer_text(ctx);
                let name = name.trim().to_owned();
                if name.is_empty() {
                    self.team_message = Some("Type a name for the team first.".to_owned());
                    ctx.notify();
                    return;
                }
                let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
                    return;
                };
                ctx.spawn(
                    async move { manager.create_and_join_team(&name).await },
                    |me, result, ctx| {
                        match result {
                            Ok(()) => {
                                me.team_message = None;
                                me.team_name_editor.update(ctx, |editor, ctx| {
                                    editor.clear_buffer_and_reset_undo_stack(ctx);
                                });
                            }
                            Err(e) => me.team_message = Some(format!("Could not create the team: {e}")),
                        }
                        ctx.notify();
                    },
                );
            }
            SyncSettingsPageAction::RequestDelete(kind) => {
                self.pending_delete = Some(*kind);
                ctx.notify();
            }
            SyncSettingsPageAction::CancelDelete => {
                self.pending_delete = None;
                ctx.notify();
            }
            SyncSettingsPageAction::ConfirmDelete => {
                let Some(kind) = self.pending_delete.take() else {
                    return;
                };
                AuthManager::handle(ctx).update(ctx, |auth_manager, ctx| {
                    auth_manager.delete_synced_data(kind == SyncDeleteKind::Account, ctx);
                });
                ctx.notify();
            }
        }
    }
}

impl View for SyncSettingsPageView {
    fn ui_name() -> &'static str {
        "SyncPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for SyncSettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Sync
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<SyncSettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<SyncSettingsPageView>) -> Self {
        SettingsPageViewHandle::Sync(view_handle)
    }
}

/// Mnemos IDE: the admin login behind sync, at the top of the page: signed in, or
/// why sync stopped with a button to sign in again. Same state as the
/// workspace header chip.
#[derive(Default)]
struct SyncAuthWidget {
    sign_in_button: MouseStateHandle,
}

impl SettingsWidget for SyncAuthWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "sync sign in signed out login account admin keyring"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let state = AuthManager::as_ref(app).ide_sync_auth();
        // Never signed in here: not a warning, just the way in.
        if state == SyncAuthState::NeverConfigured {
            return Container::new(build_toggle_element(
                render_body_item_label::<SyncSettingsPageAction>(
                    NOT_SET_UP_LABEL.to_owned(),
                    None,
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                ),
                self.render_sign_in_button(appearance),
                appearance,
                Some(NOT_SET_UP_TEXT.to_owned()),
            ))
            .with_padding_bottom(12.)
            .finish();
        }
        let Some(notice) = ide_sync_auth::notice(&state) else {
            return Container::new(build_toggle_element(
                render_body_item_label::<SyncSettingsPageAction>(
                    SIGNED_IN_LABEL.to_owned(),
                    None,
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                ),
                Empty::new().finish(),
                appearance,
                Some(SIGNED_IN_TEXT.to_owned()),
            ))
            .with_padding_bottom(12.)
            .finish();
        };

        let label = render_body_item_label_with_icon::<SyncSettingsPageAction>(
            notice.label.to_owned(),
            Icon::AlertTriangle,
            Some(Fill::Solid(appearance.theme().ui_warning_color())),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
        );
        // Mnemos IDE: signing in again cannot fix a refused sync protocol;
        // the chip in the tab bar offers the update check instead.
        let button = match notice.action {
            ide_sync_auth::SyncAuthNoticeAction::SignIn => self.render_sign_in_button(appearance),
            ide_sync_auth::SyncAuthNoticeAction::CheckForUpdate => Empty::new().finish(),
        };
        Container::new(build_toggle_element(
            label,
            button,
            appearance,
            Some(notice.detail),
        ))
        .with_padding_bottom(12.)
        .finish()
    }
}

impl SyncAuthWidget {
    fn render_sign_in_button(&self, appearance: &Appearance) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .button(ButtonVariant::Accent, self.sign_in_button.clone())
            .with_style(UiComponentStyles {
                font_size: Some(13.),
                font_weight: Some(Weight::Semibold),
                border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
                padding: Some(Coords {
                    top: 6.,
                    bottom: 6.,
                    left: 16.,
                    right: 16.,
                }),
                ..Default::default()
            })
            .with_text_label("Sign in".to_owned())
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(SyncSettingsPageAction::SignIn))
            .finish()
    }
}

#[derive(Default)]
struct SyncIntroWidget;

impl SettingsWidget for SyncIntroWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "sync projects workspace state machines local-only"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        // Page title is rendered by PageType (Some(PAGE_TITLE) below); this
        // widget only renders the intro paragraph in the subdued ui text color.
        Container::new(
            Align::new(
                Text::new(PAGE_INTRO, appearance.ui_font_family(), 13.)
                    .with_color(appearance.theme().nonactive_ui_text_color().into())
                    .finish(),
            )
            .left()
            .finish(),
        )
        .with_padding_bottom(20.)
        .finish()
    }
}

/// Mnemos IDE: the hooks opt-in and the incoming hooks, Claude Code commands
/// and MCP servers waiting for this machine's approval (mnemos_ide_sync::gates).
struct SyncGatesWidget;

fn small_button(
    appearance: &Appearance,
    variant: ButtonVariant,
    handle: MouseStateHandle,
    label: &str,
    action: SyncSettingsPageAction,
) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .button(variant, handle)
        .with_style(UiComponentStyles {
            font_size: Some(12.),
            border_radius: Some(CornerRadius::with_all(Radius::Pixels(4.))),
            padding: Some(Coords {
                top: 4.,
                bottom: 4.,
                left: 10.,
                right: 10.,
            }),
            ..Default::default()
        })
        .with_text_label(label.to_owned())
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .finish()
}

impl SettingsWidget for SyncGatesWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "sync hooks approve approval mcp servers commands statusline security"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let Some(manager) = AuthManager::as_ref(app).ide_sync_manager() else {
            return Empty::new().finish();
        };
        let status = manager.status().borrow().clone();

        let mut column =
            Flex::column().with_child(render_sub_header(appearance, GATES_SUBHEADER, None));
        let toggle = appearance
            .ui_builder()
            .switch(view.hooks_switch.clone())
            .check(status.hooks_sync)
            .build()
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(SyncSettingsPageAction::ToggleHooksSync)
            })
            .finish();
        column.add_child(render_body_item::<SyncSettingsPageAction>(
            HOOKS_SYNC_LABEL.to_owned(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            toggle,
            Some(HOOKS_SYNC_TEXT.to_owned()),
        ));

        if !status.pending_approvals.is_empty() {
            column.add_child(render_separator(appearance));
            let label = render_body_item_label_with_icon::<SyncSettingsPageAction>(
                format!(
                    "Waiting for your approval ({})",
                    status.pending_approvals.len()
                ),
                Icon::AlertTriangle,
                Some(Fill::Solid(appearance.theme().ui_warning_color())),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
            );
            column.add_child(build_toggle_element(
                label,
                small_button(
                    appearance,
                    ButtonVariant::Accent,
                    view.approve_all_button.clone(),
                    "Approve all",
                    SyncSettingsPageAction::ApproveAllPending,
                ),
                appearance,
                Some(PENDING_INTRO.to_owned()),
            ));
            for pending in &status.pending_approvals {
                let (approve, dismiss) = view.approval_buttons_for(&pending.id);
                let buttons = Flex::row()
                    .with_child(small_button(
                        appearance,
                        ButtonVariant::Accent,
                        approve,
                        "Approve",
                        SyncSettingsPageAction::ApprovePending(pending.id.clone()),
                    ))
                    .with_child(
                        Container::new(small_button(
                            appearance,
                            ButtonVariant::Secondary,
                            dismiss,
                            "Dismiss",
                            SyncSettingsPageAction::DismissPending(pending.id.clone()),
                        ))
                        .with_padding_left(8.)
                        .finish(),
                    )
                    .finish();
                let kind = if pending.new_item { "new" } else { "changed" };
                column.add_child(render_separator(appearance));
                column.add_child(render_body_item::<SyncSettingsPageAction>(
                    format!("{} ({kind})", pending.title),
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                    buttons,
                    None,
                ));
                column.add_child(
                    Container::new(
                        Text::new(
                            pending.diff.clone(),
                            appearance.monospace_font_family(),
                            11.,
                        )
                        .with_color(appearance.theme().nonactive_ui_text_color().into())
                        .finish(),
                    )
                    .with_padding_bottom(12.)
                    .finish(),
                );
            }
        }

        Container::new(column.finish())
            .with_padding_bottom(20.)
            .finish()
    }
}

#[derive(Default)]
struct SyncProjectListWidget;

impl SettingsWidget for SyncProjectListWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "sync projects toggle disable enable"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let store = mnemos_ide_projects::ProjectStore::load_blocking().ok();

        let mut column =
            Flex::column().with_child(render_sub_header(appearance, PROJECTS_SUBHEADER, None));

        let projects = store
            .as_ref()
            .map(|s| {
                let mut p = s.projects.clone();
                p.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                p
            })
            .unwrap_or_default();

        if projects.is_empty() {
            column.add_child(
                Container::new(
                    Text::new_inline(
                        EMPTY_STATE_TEXT.to_string(),
                        appearance.ui_font_family(),
                        12.,
                    )
                    .with_color(appearance.theme().disabled_ui_text_color().into())
                    .finish(),
                )
                .with_padding_top(16.)
                .with_padding_bottom(16.)
                .finish(),
            );
            return column.finish();
        }

        for (idx, project) in projects.iter().enumerate() {
            let id = project.id.clone();
            let disabled = store
                .as_ref()
                .map(|s| s.is_sync_disabled(&id))
                .unwrap_or(false);
            let switch_handle = view.switch_handle_for(&id);
            // This machine's checkout; `path` is whichever machine registered
            // the project first, so show it only as where the project lives
            // elsewhere.
            let path_text = match project.local_path() {
                Some(local) => local.display().to_string(),
                None => format!("Not on this machine ({})", project.path.display()),
            };

            let toggle_id = id.clone();
            let toggle = appearance
                .ui_builder()
                .switch(switch_handle)
                .check(!disabled)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(SyncSettingsPageAction::ToggleProject(
                        toggle_id.clone(),
                    ));
                })
                .finish();

            column.add_child(render_body_item::<SyncSettingsPageAction>(
                project.name.clone(),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
                toggle,
                Some(path_text),
            ));

            if idx + 1 < projects.len() {
                column.add_child(render_separator(appearance));
            }
        }

        column.finish()
    }
}
