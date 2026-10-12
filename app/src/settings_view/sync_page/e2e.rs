//! Mnemos IDE: the Security part of Settings > Sync: end-to-end encryption
//! (turn on, show the recovery key once, enter it on another machine) and
//! whether `~/.claude/.env` syncs.

use mnemos_ide_sync::{E2eStatus, SyncKey};
use mnemos_ui::{
    clipboard::ClipboardContent,
    elements::{
        Align, ConstrainedBox, Container, CornerRadius, Element, Empty, Flex, MouseStateHandle,
        ParentElement, Radius, Text,
    },
    fonts::Weight,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
        switch::SwitchStateHandle,
    },
    AppContext, SingletonEntity, ViewContext, ViewHandle,
};

use super::{SyncSettingsPageAction, SyncSettingsPageView};
use crate::appearance::Appearance;
use crate::auth::auth_manager::AuthManager;
use crate::auth::ide_sync_e2e;
use crate::editor::{EditorView, SingleLineEditorOptions, TextOptions};
use crate::settings_view::settings_page::{render_body_item, render_sub_header, SettingsWidget};
use crate::settings_view::{LocalOnlyIconState, ToggleState};

const SECURITY_SUBHEADER: &str = "Security";
const ENV_LABEL: &str = "Sync ~/.claude/.env";
const ENV_TEXT: &str = "Off by default on every machine: the file usually holds API keys. \
    When off, this machine neither uploads its .env nor writes one from another machine. \
    The env and headers of MCP servers in ~/.claude.json never sync.";
const ENV_REMOVE_LABEL: &str = "Remove .env from the sync server";
const ENV_REMOVE_TEXT: &str = "Deletes the copy of ~/.claude/.env that a machine uploaded \
    before (older Mnemos builds always synced it). Nothing on this machine changes. A machine \
    that still syncs .env, such as one with an older Mnemos, uploads its copy again on its next \
    change: update or sign out that machine first.";
const OFF_LABEL: &str = "End-to-end encryption is off";
const OFF_TEXT: &str = "Your synced Claude config, projects, memory and sessions are stored on \
    the sync server as they are. Turn this on to encrypt them on this machine before they are \
    uploaded. Config you share with a team stays readable by the server either way.";
const CONFIRM_TEXT: &str = "Before you turn this on, update Mnemos on every machine that syncs \
    this account. An older build cannot read encrypted data: it stops receiving changes. What it \
    uploads unencrypted in the first 24 hours is encrypted and removed by the updated machines; \
    after that it is ignored. You get a recovery key once. Without it no other machine can read \
    your synced data, and nobody can recover it for you.";
const SHOW_KEY_LABEL: &str = "Your recovery key";
const SHOW_KEY_TEXT: &str = "Shown once. Copy it into a password manager now, then enter it in \
    Settings > Sync on each of your other machines.";
const ON_TEXT: &str = "Personal sync data is encrypted on this machine before it is uploaded. \
    Team-shared config and the live agents feed are not encrypted.";
const NEEDS_KEY_LABEL: &str = "Enter your recovery key";
const NEEDS_KEY_TEXT: &str = "End-to-end encryption is on for this account. Personal sync is \
    paused on this machine until you enter the recovery key shown when it was turned on.";
const WRONG_KEY_LABEL: &str = "The saved recovery key does not match";
const UNSUPPORTED_LABEL: &str = "Update Mnemos to sync";
const UNSUPPORTED_TEXT: &str =
    "This account was encrypted by a newer Mnemos. Personal sync is paused until you update.";
const MARKER_MISSING_LABEL: &str = "The server no longer shows encryption as on";
const MARKER_MISSING_TEXT: &str = "A recovery key for this account is saved on this machine, but \
    the sync server has no record that encryption is on. Personal sync is paused here so nothing \
    is uploaded unencrypted. Restore it to keep encrypting with the key saved here, or, if you \
    deleted your synced data, start over without encryption.";
const START_OVER_LABEL: &str = "Start over without encryption?";
const START_OVER_TEXT: &str = "This machine forgets the recovery key saved here and syncs \
    unencrypted from now on; you can turn encryption on again later with a new key. Do this \
    after you deleted your synced data. Any other machine that still has the old key pauses \
    the same way until you start over there too. If the server only lost its record, restore \
    it instead: starting over leaves the encrypted copies on the server unread.";
const UNKNOWN_LABEL: &str = "Checking end-to-end encryption";
const UNKNOWN_TEXT: &str = "Waiting for the sync server to say whether this account is encrypted.";

/// What the Security part is showing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) enum E2eStep {
    #[default]
    Idle,
    /// The warning before turning encryption on.
    ConfirmTurnOn,
    /// The new recovery key, shown once.
    ShowKey(String),
    /// Mnemos IDE: the warning before forgetting the key saved here
    /// (status `MarkerMissing`).
    ConfirmStartOver,
}

pub(super) struct E2eUiState {
    pub(super) editor: ViewHandle<EditorView>,
    pub(super) step: E2eStep,
    pub(super) busy: bool,
    /// The last outcome to show: (is an error, text).
    pub(super) message: Option<(bool, String)>,
    pub(super) env_switch: SwitchStateHandle,
}

impl E2eUiState {
    pub(super) fn new(ctx: &mut ViewContext<SyncSettingsPageView>) -> Self {
        let editor = ctx.add_typed_action_view(|ctx| {
            let options = SingleLineEditorOptions {
                text: TextOptions {
                    font_size_override: Some(13.),
                    ..Default::default()
                },
                ..Default::default()
            };
            EditorView::single_line(options, ctx)
        });
        editor.update(ctx, |editor, ctx| {
            editor.set_placeholder_text("XXXXX-XXXXX-...", ctx);
        });
        Self {
            editor,
            step: E2eStep::Idle,
            busy: false,
            message: None,
            env_switch: SwitchStateHandle::default(),
        }
    }
}

fn error(view: &mut SyncSettingsPageView, text: impl Into<String>) {
    view.e2e.message = Some((true, text.into()));
}

impl SyncSettingsPageView {
    pub(super) fn toggle_claude_env(&mut self, ctx: &mut ViewContext<Self>) {
        let manager = AuthManager::as_ref(ctx).ide_sync_manager();
        let result = match &manager {
            Some(manager) => {
                let on = !manager.sync_options().claude_env;
                manager.set_claude_env_sync(on).map_err(|e| e.to_string())
            }
            // Not signed in: saved for the first start.
            None => match mnemos_ide_sync::options::default_path() {
                Some(path) => {
                    let mut options = mnemos_ide_sync::options::load(&path);
                    options.claude_env = !options.claude_env;
                    mnemos_ide_sync::options::save(&path, &options).map_err(|e| e.to_string())
                }
                None => Err("no Mnemos data dir".to_owned()),
            },
        };
        if let Err(e) = result {
            error(self, format!("Could not save the .env choice: {e}"));
        }
        ctx.notify();
    }

    pub(super) fn remove_server_env(&mut self, ctx: &mut ViewContext<Self>) {
        if self.e2e.busy {
            return;
        }
        let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
            error(self, "Sign in to sync first.");
            ctx.notify();
            return;
        };
        self.e2e.busy = true;
        self.e2e.message = None;
        ctx.notify();
        ctx.spawn(
            async move { manager.remove_server_env().await.map_err(|e| e.to_string()) },
            move |me, result, ctx| {
                me.e2e.busy = false;
                match result {
                    Ok(()) => {
                        me.e2e.message =
                            Some((false, "The .env copy was removed from the sync server.".to_owned()))
                    }
                    Err(reason) => error(me, format!("Not removed: {reason}")),
                }
                ctx.notify();
            },
        );
    }

    pub(super) fn e2e_begin_turn_on(&mut self, ctx: &mut ViewContext<Self>) {
        let off = AuthManager::as_ref(ctx)
            .ide_sync_manager()
            .is_some_and(|m| m.e2e_report().status == E2eStatus::Off);
        if !off {
            return;
        }
        self.e2e.step = E2eStep::ConfirmTurnOn;
        self.e2e.message = None;
        ctx.notify();
    }

    pub(super) fn e2e_cancel(&mut self, ctx: &mut ViewContext<Self>) {
        self.e2e.step = E2eStep::Idle;
        ctx.notify();
    }

    pub(super) fn e2e_confirm_turn_on(&mut self, ctx: &mut ViewContext<Self>) {
        if self.e2e.busy {
            return;
        }
        let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
            error(self, "Sign in to sync first.");
            ctx.notify();
            return;
        };
        let Some(profile_id) = manager.profile_id_now() else {
            error(self, "Sync is still starting. Try again in a moment.");
            ctx.notify();
            return;
        };
        match manager.e2e_report().status {
            E2eStatus::Off => {}
            E2eStatus::MarkerMissing => {
                self.e2e_restore_marker(manager, &profile_id, ctx);
                return;
            }
            // Unknown (offline, halted, not asked yet) or already on: a new
            // key here could replace the account's real one.
            _ => {
                self.e2e.step = E2eStep::Idle;
                error(
                    self,
                    "Not turned on: this account's encryption status is not known as off. Try \
                     again once sync is connected.",
                );
                ctx.notify();
                return;
            }
        }
        // Never replace a key saved here: it may be the account's.
        match ide_sync_e2e::get(ctx, &profile_id) {
            Ok(None) => {}
            Ok(Some(_)) => {
                self.e2e.step = E2eStep::Idle;
                error(
                    self,
                    "Not turned on: a recovery key for this account is already saved on this \
                     machine. Restart Mnemos so sync picks it up.",
                );
                ctx.notify();
                return;
            }
            Err(e) => {
                error(
                    self,
                    format!(
                        "Could not read the keys saved on this machine ({e}). Nothing was changed."
                    ),
                );
                ctx.notify();
                return;
            }
        }
        let key = SyncKey::generate();
        let text = key.recovery_text();
        // Saved before the server hears of it: a key that is on the server
        // and nowhere else would lock every machine out.
        if let Err(e) = ide_sync_e2e::save_new(ctx, &profile_id, &text) {
            error(
                self,
                format!("Could not save the key on this machine ({e}). Nothing was changed."),
            );
            ctx.notify();
            return;
        }
        self.e2e.busy = true;
        self.e2e.message = None;
        ctx.notify();
        ctx.spawn(
            async move {
                manager
                    .enable_e2e(key)
                    .await
                    .map_err(|e| (mnemos_ide_sync::e2e::is_locked_error(&e), e.to_string()))
            },
            move |me, result, ctx| {
                me.e2e.busy = false;
                match result {
                    Ok(()) => {
                        me.e2e.step = E2eStep::ShowKey(text);
                        AuthManager::handle(ctx)
                            .update(ctx, |auth, ctx| auth.restart_ide_sync(ctx));
                    }
                    // Refused before anything was written (already on): the
                    // key was never used. Only the key saved above goes.
                    Err((true, reason)) => {
                        ide_sync_e2e::remove_if(ctx, &profile_id, &text);
                        me.e2e.step = E2eStep::Idle;
                        error(me, format!("Not turned on: {reason}"));
                    }
                    // No answer: the server may have taken it. Keep the key
                    // and show it, so it is not lost if it is in use.
                    Err((false, reason)) => {
                        me.e2e.step = E2eStep::ShowKey(text);
                        error(
                            me,
                            format!(
                                "The server did not confirm ({reason}). Save this key anyway: if \
                                 encryption shows as on after a restart, it is the account's key."
                            ),
                        );
                        AuthManager::handle(ctx)
                            .update(ctx, |auth, ctx| auth.restart_ide_sync(ctx));
                    }
                }
                ctx.notify();
            },
        );
    }

    /// Put the account's encryption marker back with the key saved here
    /// (status `MarkerMissing`). Nothing saved is changed either way.
    fn e2e_restore_marker(
        &mut self,
        manager: std::sync::Arc<mnemos_ide_sync::SyncManager>,
        profile_id: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let key = match ide_sync_e2e::get(ctx, profile_id) {
            Ok(Some(text)) => match SyncKey::parse_recovery(&text) {
                Ok(key) => key,
                Err(e) => {
                    error(
                        self,
                        format!("The recovery key saved here is not usable: {e}"),
                    );
                    ctx.notify();
                    return;
                }
            },
            Ok(None) => {
                error(
                    self,
                    "No recovery key is saved here any more. Restart Mnemos to check again.",
                );
                ctx.notify();
                return;
            }
            Err(e) => {
                error(
                    self,
                    format!("Could not read the keys saved on this machine ({e})."),
                );
                ctx.notify();
                return;
            }
        };
        self.e2e.busy = true;
        self.e2e.message = None;
        ctx.notify();
        ctx.spawn(
            async move { manager.enable_e2e(key).await.map_err(|e| e.to_string()) },
            move |me, result, ctx| {
                me.e2e.busy = false;
                me.e2e.step = E2eStep::Idle;
                match result {
                    Ok(()) => {
                        me.e2e.message = Some((
                            false,
                            "Restored. Sync is starting over with encryption.".to_owned(),
                        ));
                        AuthManager::handle(ctx)
                            .update(ctx, |auth, ctx| auth.restart_ide_sync(ctx));
                    }
                    Err(reason) => error(me, format!("Not restored: {reason}")),
                }
                ctx.notify();
            },
        );
    }

    pub(super) fn e2e_begin_start_over(&mut self, ctx: &mut ViewContext<Self>) {
        let missing = AuthManager::as_ref(ctx)
            .ide_sync_manager()
            .is_some_and(|m| m.e2e_report().status == E2eStatus::MarkerMissing);
        if missing {
            self.e2e.step = E2eStep::ConfirmStartOver;
            self.e2e.message = None;
        }
        ctx.notify();
    }

    /// Forget the recovery key saved here and restart sync, which then finds
    /// no marker and no key: unencrypted, like a machine that never had one.
    /// Only while the server has no marker; with one, the key is needed.
    pub(super) fn e2e_start_over(&mut self, ctx: &mut ViewContext<Self>) {
        self.e2e.step = E2eStep::Idle;
        let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
            error(self, "Sign in to sync first.");
            ctx.notify();
            return;
        };
        if manager.e2e_report().status != E2eStatus::MarkerMissing {
            error(self, "Not changed: the server shows encryption as on again.");
            ctx.notify();
            return;
        }
        let Some(profile_id) = manager.profile_id_now() else {
            error(self, "Sync is still starting. Try again in a moment.");
            ctx.notify();
            return;
        };
        if let Err(e) = ide_sync_e2e::remove(ctx, &profile_id) {
            error(
                self,
                format!("Could not update the keys saved on this machine ({e}). Nothing was changed."),
            );
            ctx.notify();
            return;
        }
        manager.forget_e2e_key(&profile_id);
        self.e2e.message = Some((
            false,
            "The recovery key was forgotten here. Sync is starting over without encryption."
                .to_owned(),
        ));
        AuthManager::handle(ctx).update(ctx, |auth, ctx| auth.restart_ide_sync(ctx));
        ctx.notify();
    }

    pub(super) fn e2e_copy_key(&mut self, ctx: &mut ViewContext<Self>) {
        if let E2eStep::ShowKey(text) = &self.e2e.step {
            ctx.clipboard()
                .write(ClipboardContent::plain_text(text.clone()));
            self.e2e.message = Some((false, "Recovery key copied.".to_owned()));
            ctx.notify();
        }
    }

    pub(super) fn e2e_key_saved(&mut self, ctx: &mut ViewContext<Self>) {
        self.e2e.step = E2eStep::Idle;
        self.e2e.message = None;
        ctx.notify();
    }

    pub(super) fn e2e_unlock(&mut self, ctx: &mut ViewContext<Self>) {
        if self.e2e.busy {
            return;
        }
        let text = self.e2e.editor.as_ref(ctx).buffer_text(ctx);
        let key = match SyncKey::parse_recovery(&text) {
            Ok(key) => key,
            Err(e) => {
                error(self, e.to_string());
                ctx.notify();
                return;
            }
        };
        let Some(manager) = AuthManager::as_ref(ctx).ide_sync_manager() else {
            error(self, "Sign in to sync first.");
            ctx.notify();
            return;
        };
        let Some(profile_id) = manager.profile_id_now() else {
            error(self, "Sync is still starting. Try again in a moment.");
            ctx.notify();
            return;
        };
        let canonical = key.recovery_text();
        self.e2e.busy = true;
        self.e2e.message = None;
        ctx.notify();
        ctx.spawn(
            async move { manager.unlock_e2e(key).await.map_err(|e| e.to_string()) },
            move |me, result, ctx| {
                me.e2e.busy = false;
                match result {
                    Ok(()) => {
                        me.e2e.editor.update(ctx, |editor, ctx| {
                            editor.clear_buffer_and_reset_undo_stack(ctx);
                        });
                        match ide_sync_e2e::save(ctx, &profile_id, &canonical) {
                            Ok(()) => {
                                me.e2e.message = Some((
                                    false,
                                    "Unlocked. Sync is starting over with encryption.".to_owned(),
                                ));
                                AuthManager::handle(ctx)
                                    .update(ctx, |auth, ctx| auth.restart_ide_sync(ctx));
                            }
                            // A restart would rebuild sync from the saved
                            // keys, which lack this one, and lock it again
                            // at once. The running sync keeps the key.
                            Err(e) => {
                                me.e2e.message = Some((
                                    true,
                                    format!(
                                        "Unlocked until sync restarts (sign-in, sign-out or a \
                                         Mnemos restart): the key could not be saved on this \
                                         machine ({e}). Enter it again after that."
                                    ),
                                ));
                            }
                        }
                    }
                    Err(reason) => error(me, reason),
                }
                ctx.notify();
            },
        );
    }
}

/// Mnemos IDE: the Security part of the Sync page.
#[derive(Default)]
pub(super) struct SyncSecurityWidget {
    turn_on_button: MouseStateHandle,
    confirm_button: MouseStateHandle,
    cancel_button: MouseStateHandle,
    copy_button: MouseStateHandle,
    saved_button: MouseStateHandle,
    unlock_button: MouseStateHandle,
    start_over_button: MouseStateHandle,
    remove_env_button: MouseStateHandle,
}

impl SettingsWidget for SyncSecurityWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "sync security encryption end-to-end e2e recovery key env secrets mcp"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let manager = AuthManager::as_ref(app).ide_sync_manager();
        let mut column =
            Flex::column().with_child(render_sub_header(appearance, SECURITY_SUBHEADER, None));

        // ~/.claude/.env
        let env_on = match &manager {
            Some(m) => m.sync_options().claude_env,
            None => mnemos_ide_sync::options::default_path()
                .map(|p| mnemos_ide_sync::options::load(&p).claude_env)
                .unwrap_or(false),
        };
        let env_switch = appearance
            .ui_builder()
            .switch(view.e2e.env_switch.clone())
            .check(env_on)
            .build()
            .on_click(|ctx, _, _| {
                ctx.dispatch_typed_action(SyncSettingsPageAction::ToggleClaudeEnv)
            })
            .finish();
        column.add_child(render_body_item::<SyncSettingsPageAction>(
            ENV_LABEL.to_owned(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            env_switch,
            Some(ENV_TEXT.to_owned()),
        ));
        // Mnemos IDE: a copy uploaded before stays on the server until removed.
        if !env_on && manager.is_some() {
            column.add_child(render_body_item::<SyncSettingsPageAction>(
                ENV_REMOVE_LABEL.to_owned(),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
                button(
                    appearance,
                    &self.remove_env_button,
                    "Remove",
                    ButtonVariant::Secondary,
                    SyncSettingsPageAction::RemoveServerEnv,
                ),
                Some(ENV_REMOVE_TEXT.to_owned()),
            ));
        }

        // End-to-end encryption, only with a sync run to ask.
        let Some(manager) = manager else {
            return column.finish();
        };
        let report = manager.e2e_report();
        let row = |label: String, text: String, child: Box<dyn Element>| {
            render_body_item::<SyncSettingsPageAction>(
                label,
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
                child,
                Some(text),
            )
        };
        match (&view.e2e.step, &report.status) {
            (E2eStep::ShowKey(key), _) => {
                column.add_child(row(
                    SHOW_KEY_LABEL.to_owned(),
                    SHOW_KEY_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
                column.add_child(
                    Container::new(
                        Align::new(
                            Text::new(key.clone(), appearance.monospace_font_family(), 14.)
                                .with_color(appearance.theme().active_ui_text_color().into())
                                .finish(),
                        )
                        .left()
                        .finish(),
                    )
                    .with_padding_bottom(8.)
                    .finish(),
                );
                column.add_child(buttons(vec![
                    button(
                        appearance,
                        &self.copy_button,
                        "Copy",
                        ButtonVariant::Accent,
                        SyncSettingsPageAction::E2eCopyKey,
                    ),
                    button(
                        appearance,
                        &self.saved_button,
                        "I saved it",
                        ButtonVariant::Secondary,
                        SyncSettingsPageAction::E2eKeySaved,
                    ),
                ]));
            }
            (_, E2eStatus::On { key_id }) => {
                let mut text = format!("{ON_TEXT} Key id {key_id}.");
                if report.absorbed > 0 {
                    text.push_str(&format!(
                        " This session encrypted {} item(s) that were still unencrypted on the \
                         server and removed the unencrypted copies. If that keeps happening, a \
                         machine with an older Mnemos is still syncing: update it.",
                        report.absorbed
                    ));
                }
                if !report.ignored.is_empty() {
                    text.push_str(&format!(
                        " Ignored {} unencrypted item(s) on the server, uploaded after the first \
                         24 hours: a machine with an older Mnemos is probably still syncing. \
                         Update it.",
                        report.ignored.len()
                    ));
                }
                if !report.unreadable.is_empty() {
                    text.push_str(&format!(
                        " Not applied (damaged, another key, or an older copy than one already \
                         read here): {}.",
                        report.unreadable.join(", ")
                    ));
                }
                column.add_child(row(
                    "End-to-end encryption is on".to_owned(),
                    text,
                    Empty::new().finish(),
                ));
            }
            (_, E2eStatus::NeedsKey) | (_, E2eStatus::WrongKey) => {
                let label = if report.status == E2eStatus::WrongKey {
                    WRONG_KEY_LABEL
                } else {
                    NEEDS_KEY_LABEL
                };
                column.add_child(row(
                    label.to_owned(),
                    NEEDS_KEY_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
                column.add_child(
                    Flex::row()
                        .with_child(
                            ConstrainedBox::new(
                                appearance
                                    .ui_builder()
                                    .text_input(view.e2e.editor.clone())
                                    .with_style(UiComponentStyles {
                                        background: Some(appearance.theme().background().into()),
                                        ..Default::default()
                                    })
                                    .build()
                                    .finish(),
                            )
                            .with_width(440.)
                            .finish(),
                        )
                        .with_child(button(
                            appearance,
                            &self.unlock_button,
                            "Unlock",
                            ButtonVariant::Accent,
                            SyncSettingsPageAction::E2eUnlock,
                        ))
                        .finish(),
                );
            }
            (_, E2eStatus::Unsupported) => {
                column.add_child(row(
                    UNSUPPORTED_LABEL.to_owned(),
                    UNSUPPORTED_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
            }
            (E2eStep::ConfirmStartOver, E2eStatus::MarkerMissing) => {
                column.add_child(row(
                    START_OVER_LABEL.to_owned(),
                    START_OVER_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
                column.add_child(buttons(vec![
                    button(
                        appearance,
                        &self.start_over_button,
                        "Forget the key, start over",
                        ButtonVariant::Warn,
                        SyncSettingsPageAction::E2eStartOver,
                    ),
                    button(
                        appearance,
                        &self.cancel_button,
                        "Cancel",
                        ButtonVariant::Secondary,
                        SyncSettingsPageAction::E2eCancel,
                    ),
                ]));
            }
            (_, E2eStatus::MarkerMissing) => {
                column.add_child(row(
                    MARKER_MISSING_LABEL.to_owned(),
                    MARKER_MISSING_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
                column.add_child(buttons(vec![
                    button(
                        appearance,
                        &self.confirm_button,
                        "Restore",
                        ButtonVariant::Secondary,
                        SyncSettingsPageAction::E2eConfirmTurnOn,
                    ),
                    button(
                        appearance,
                        &self.start_over_button,
                        "Start over...",
                        ButtonVariant::Secondary,
                        SyncSettingsPageAction::E2eBeginStartOver,
                    ),
                ]));
            }
            // Turn on only while the account is known to be off: with the
            // status unknown, the account may be encrypted already.
            (_, E2eStatus::Unknown) => {
                column.add_child(row(
                    UNKNOWN_LABEL.to_owned(),
                    UNKNOWN_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
            }
            (E2eStep::ConfirmTurnOn, E2eStatus::Off) => {
                column.add_child(row(
                    "Turn on end-to-end encryption?".to_owned(),
                    CONFIRM_TEXT.to_owned(),
                    Empty::new().finish(),
                ));
                column.add_child(buttons(vec![
                    button(
                        appearance,
                        &self.confirm_button,
                        "I updated every machine, turn it on",
                        ButtonVariant::Warn,
                        SyncSettingsPageAction::E2eConfirmTurnOn,
                    ),
                    button(
                        appearance,
                        &self.cancel_button,
                        "Cancel",
                        ButtonVariant::Secondary,
                        SyncSettingsPageAction::E2eCancel,
                    ),
                ]));
            }
            // Idle, or a start-over warning left from a status that moved on.
            (_, E2eStatus::Off) => {
                column.add_child(row(
                    OFF_LABEL.to_owned(),
                    OFF_TEXT.to_owned(),
                    button(
                        appearance,
                        &self.turn_on_button,
                        "Turn on",
                        ButtonVariant::Secondary,
                        SyncSettingsPageAction::E2eBeginTurnOn,
                    ),
                ));
            }
        }

        let status_line = if view.e2e.busy {
            Some((false, "Working...".to_owned()))
        } else {
            view.e2e.message.clone()
        };
        if let Some((is_error, text)) = status_line {
            let color = if is_error {
                appearance.theme().ui_error_color()
            } else {
                appearance.theme().nonactive_ui_text_color().into()
            };
            column.add_child(
                Container::new(
                    Align::new(
                        Text::new(text, appearance.ui_font_family(), 12.)
                            .with_color(color)
                            .finish(),
                    )
                    .left()
                    .finish(),
                )
                .with_padding_top(8.)
                .finish(),
            );
        }
        Container::new(column.finish())
            .with_padding_bottom(20.)
            .finish()
    }
}

fn button(
    appearance: &Appearance,
    handle: &MouseStateHandle,
    label: &str,
    variant: ButtonVariant,
    action: SyncSettingsPageAction,
) -> Box<dyn Element> {
    Container::new(
        appearance
            .ui_builder()
            .button(variant, handle.clone())
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
            .with_text_label(label.to_owned())
            .build()
            .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
            .finish(),
    )
    .with_padding_left(8.)
    .finish()
}

fn buttons(items: Vec<Box<dyn Element>>) -> Box<dyn Element> {
    let mut row = Flex::row();
    for item in items {
        row.add_child(item);
    }
    Container::new(row.finish()).with_padding_top(4.).finish()
}
