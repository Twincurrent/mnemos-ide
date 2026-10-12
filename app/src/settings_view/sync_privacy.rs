//! Mnemos IDE: the privacy parts of Settings > Sync: the live agents feed,
//! the Drive team this machine takes part in, and deleting synced data or
//! the account on the sync server.

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
    AppContext, SingletonEntity,
};

use super::settings_page::{render_body_item, render_separator, render_sub_header, SettingsWidget};
use super::sync_page::{SyncDeleteKind, SyncSettingsPageAction, SyncSettingsPageView};
use super::{LocalOnlyIconState, ToggleState};
use crate::appearance::Appearance;
use crate::auth::auth_manager::{AuthManager, SyncDeletionStatus};
use crate::auth::ide_sync_auth::{DeletionOffer, STAFF_ACCOUNT_DELETION_REFUSED};
use crate::terminal::general_settings::GeneralSettings;

const LIVE_FEED_SUBHEADER: &str = "Live agents feed";
const LIVE_FEED_LABEL: &str = "Share running agents with your other machines";
const LIVE_FEED_TEXT: &str = "Publishes this machine's running agents (project, agent, status, \
    tool, session id) to your sync account every 15 seconds, so your other signed-in machines \
    can list them. Removed from the server when you turn this off or sign out, and expired \
    there after 24 hours without an update.";
const SHARE_TEXT_LABEL: &str = "Include prompt and reply text";
const SHARE_TEXT_TEXT: &str = "Adds the first 200 characters of each agent's last prompt and \
    reply. Secret-looking text (keys, tokens, passwords) is redacted either way.";

const TEAM_SUBHEADER: &str = "Team";
const TEAM_JOIN_TEXT: &str = "Joining pulls the team's shared Claude config (CLAUDE.md, agents, \
    commands, skills, rules and hooks) onto this machine. Team data is stored on the sync \
    server without end-to-end encryption.";
const TEAM_LEAVE_TEXT: &str = "Leaving stops this machine pulling from or pushing to the team. \
    Your membership on the server stays, so your other machines are not affected.";
const TEAM_CREATE_LABEL: &str = "Create a team";
const TEAM_CREATE_TEXT: &str = "Name the new team, then add members from the admin platform.";

const DELETE_SUBHEADER: &str = "Your data on the sync server";
const DELETE_DATA_LABEL: &str = "Delete synced data";
const DELETE_DATA_TEXT: &str = "Deletes everything this account synced from the server: \
    projects, settings, Claude config, memory, session transcripts and the live agents feed. \
    Your account and the files on this machine stay.";
const DELETE_ACCOUNT_LABEL: &str = "Delete account";
const DELETE_ACCOUNT_TEXT: &str = "Deletes the synced data and the account. Teams you share with \
    others are kept for them; you are removed from them.";
const CONFIRM_TEXT: &str = "Sign out of sync on your other machines first, or they upload their \
    copy again. This machine signs out of sync before the deletion starts, so it cannot upload \
    its copy again. This cannot be undone.";

fn button(
    appearance: &Appearance,
    variant: ButtonVariant,
    mouse: MouseStateHandle,
    label: &str,
    action: SyncSettingsPageAction,
) -> Box<dyn Element> {
    appearance
        .ui_builder()
        .button(variant, mouse)
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
        .finish()
}

fn note(appearance: &Appearance, text: impl Into<String>) -> Box<dyn Element> {
    Container::new(
        Align::new(
            Text::new(text.into(), appearance.ui_font_family(), 12.)
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .finish(),
        )
        .left()
        .finish(),
    )
    .with_padding_bottom(12.)
    .finish()
}

// ── Live agents feed ────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct LiveFeedWidget {
    enabled_switch: SwitchStateHandle,
    share_text_switch: SwitchStateHandle,
}

impl SettingsWidget for LiveFeedWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "live feed agents running other machines share prompt text privacy redact"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let settings = GeneralSettings::as_ref(app);
        let enabled = settings.live_feed_enabled();
        let share_text = settings.live_feed_share_text_enabled();
        let enabled_toggle = appearance
            .ui_builder()
            .switch(self.enabled_switch.clone())
            .check(enabled)
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(SyncSettingsPageAction::ToggleLiveFeed))
            .finish();
        let mut column = Flex::column()
            .with_child(render_sub_header(appearance, LIVE_FEED_SUBHEADER, None))
            .with_child(render_body_item::<SyncSettingsPageAction>(
                LIVE_FEED_LABEL.to_owned(),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
                enabled_toggle,
                Some(LIVE_FEED_TEXT.to_owned()),
            ));
        if enabled {
            let share_toggle = appearance
                .ui_builder()
                .switch(self.share_text_switch.clone())
                .check(share_text)
                .build()
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(SyncSettingsPageAction::ToggleLiveFeedShareText)
                })
                .finish();
            column.add_child(render_body_item::<SyncSettingsPageAction>(
                SHARE_TEXT_LABEL.to_owned(),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
                share_toggle,
                Some(SHARE_TEXT_TEXT.to_owned()),
            ));
        }
        Container::new(column.finish())
            .with_padding_bottom(20.)
            .finish()
    }
}

// ── Team ────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct TeamWidget {
    join_buttons: RefCell<HashMap<String, MouseStateHandle>>,
    leave_button: MouseStateHandle,
    create_button: MouseStateHandle,
}

impl SettingsWidget for TeamWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "team join leave create shared claude config hooks"
    }

    /// Only for an account that is in a team: the server lists no teams for
    /// anyone else, and then nothing about teams is shown (see
    /// [`shows_team_section`]).
    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let Some(manager) = AuthManager::as_ref(app).ide_sync_manager() else {
            return Empty::new().finish();
        };
        let snapshot = manager.status().borrow().clone();
        if !shows_team_section(&snapshot) {
            return Empty::new().finish();
        }
        let mut column = Flex::column().with_child(render_sub_header(appearance, TEAM_SUBHEADER, None));
        match (&snapshot.team_id, &snapshot.team_name) {
            (Some(_), name) => {
                let name = name.clone().unwrap_or_else(|| "team".to_owned());
                let role = if snapshot.team_is_owner {
                    "owner, shares your Claude config"
                } else {
                    "member, pull-only"
                };
                column.add_child(render_body_item::<SyncSettingsPageAction>(
                    format!("{name} ({role})"),
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                    button(
                        appearance,
                        ButtonVariant::Secondary,
                        self.leave_button.clone(),
                        "Leave on this machine",
                        SyncSettingsPageAction::LeaveTeam,
                    ),
                    Some(TEAM_LEAVE_TEXT.to_owned()),
                ));
            }
            (None, _) => {
                for team in &snapshot.teams {
                    let mouse = self
                        .join_buttons
                        .borrow_mut()
                        .entry(team.id.clone())
                        .or_default()
                        .clone();
                    column.add_child(render_body_item::<SyncSettingsPageAction>(
                        format!("{} ({} members)", team.name, team.member_count),
                        None,
                        LocalOnlyIconState::Hidden,
                        ToggleState::Enabled,
                        appearance,
                        button(
                            appearance,
                            ButtonVariant::Accent,
                            mouse,
                            "Join",
                            SyncSettingsPageAction::JoinTeam(team.id.clone()),
                        ),
                        Some(TEAM_JOIN_TEXT.to_owned()),
                    ));
                }
                column.add_child(render_separator(appearance));
                column.add_child(render_body_item::<SyncSettingsPageAction>(
                    TEAM_CREATE_LABEL.to_owned(),
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                    button(
                        appearance,
                        ButtonVariant::Secondary,
                        self.create_button.clone(),
                        "Create",
                        SyncSettingsPageAction::CreateTeam,
                    ),
                    Some(TEAM_CREATE_TEXT.to_owned()),
                ));
                column.add_child(
                    Container::new(
                        appearance
                            .ui_builder()
                            .text_input(view.team_name_editor.clone())
                            .with_style(UiComponentStyles::default())
                            .build()
                            .finish(),
                    )
                    .with_padding_bottom(8.)
                    .finish(),
                );
            }
        }
        if let Some(message) = &view.team_message {
            column.add_child(note(appearance, message.clone()));
        }
        Container::new(column.finish())
            .with_padding_bottom(20.)
            .finish()
    }
}

/// Mnemos IDE: teams are kept for their existing members only (owner
/// decision). `/api/tcb/teams` lists just the teams the caller belongs to, so
/// an account in no team gets an empty list, and then the Team section, its
/// Join rows and "Create a team" are not shown at all. A member who left on
/// this machine still sees their teams, to join again.
pub(super) fn shows_team_section(snapshot: &mnemos_ide_sync::SyncStatusSnapshot) -> bool {
    snapshot.team_id.is_some() || !snapshot.teams.is_empty()
}

// ── Deletion ────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(super) struct DeleteDataWidget {
    delete_data_button: MouseStateHandle,
    delete_account_button: MouseStateHandle,
    confirm_button: MouseStateHandle,
    cancel_button: MouseStateHandle,
}

impl SettingsWidget for DeleteDataWidget {
    type View = SyncSettingsPageView;

    fn search_terms(&self) -> &str {
        "delete synced data account remove server privacy gdpr erase"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let auth = AuthManager::as_ref(app);
        let status = auth.sync_deletion().cloned();
        // Mnemos IDE: also true after a failed deletion, which stays
        // retryable here although this machine is already signed out of sync.
        let signed_in = auth.can_delete_synced_data();
        // Mnemos IDE: the server never deletes a staff account's data on
        // request; explain that rather than offer a button that would sign
        // this machine out of sync only to be refused.
        if auth.sync_deletion_offer() == DeletionOffer::RefusedForAccount {
            return Flex::column()
                .with_child(render_sub_header(appearance, DELETE_SUBHEADER, None))
                .with_child(note(appearance, STAFF_ACCOUNT_DELETION_REFUSED))
                .finish();
        }
        if !signed_in && status.is_none() {
            return Empty::new().finish();
        }
        let mut column =
            Flex::column().with_child(render_sub_header(appearance, DELETE_SUBHEADER, None));
        match (&status, view.pending_delete) {
            (Some(SyncDeletionStatus::InProgress { account }), _) => {
                column.add_child(note(
                    appearance,
                    if *account {
                        "Deleting the account..."
                    } else {
                        "Deleting synced data..."
                    },
                ));
            }
            (_, Some(kind)) if signed_in => {
                let (label, text) = match kind {
                    SyncDeleteKind::Data => (DELETE_DATA_LABEL, DELETE_DATA_TEXT),
                    SyncDeleteKind::Account => (DELETE_ACCOUNT_LABEL, DELETE_ACCOUNT_TEXT),
                };
                column.add_child(note(appearance, format!("{label}? {text} {CONFIRM_TEXT}")));
                column.add_child(
                    Flex::row()
                        .with_child(button(
                            appearance,
                            ButtonVariant::Error,
                            self.confirm_button.clone(),
                            label,
                            SyncSettingsPageAction::ConfirmDelete,
                        ))
                        .with_child(
                            Container::new(button(
                                appearance,
                                ButtonVariant::Secondary,
                                self.cancel_button.clone(),
                                "Cancel",
                                SyncSettingsPageAction::CancelDelete,
                            ))
                            .with_margin_left(8.)
                            .finish(),
                        )
                        .finish(),
                );
            }
            _ if signed_in => {
                column.add_child(render_body_item::<SyncSettingsPageAction>(
                    DELETE_DATA_LABEL.to_owned(),
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                    button(
                        appearance,
                        ButtonVariant::Warn,
                        self.delete_data_button.clone(),
                        "Delete...",
                        SyncSettingsPageAction::RequestDelete(SyncDeleteKind::Data),
                    ),
                    Some(DELETE_DATA_TEXT.to_owned()),
                ));
                column.add_child(render_body_item::<SyncSettingsPageAction>(
                    DELETE_ACCOUNT_LABEL.to_owned(),
                    None,
                    LocalOnlyIconState::Hidden,
                    ToggleState::Enabled,
                    appearance,
                    button(
                        appearance,
                        ButtonVariant::Error,
                        self.delete_account_button.clone(),
                        "Delete...",
                        SyncSettingsPageAction::RequestDelete(SyncDeleteKind::Account),
                    ),
                    Some(DELETE_ACCOUNT_TEXT.to_owned()),
                ));
            }
            _ => {}
        }
        match &status {
            Some(SyncDeletionStatus::Done { account: true }) => column.add_child(note(
                appearance,
                "The account and its synced data were deleted. This machine is signed out of sync.",
            )),
            Some(SyncDeletionStatus::Done { account: false }) => column.add_child(note(
                appearance,
                "Synced data was deleted. This machine is signed out of sync; sign in again to start over.",
            )),
            Some(SyncDeletionStatus::Failed(reason))
                if signed_in && auth.ide_sync_manager().is_none() =>
            {
                column.add_child(note(
                    appearance,
                    format!(
                        "Deletion failed: {reason}. This machine stays signed out of sync so it \
                         does not upload its copy again. Try again above; restarting Mnemos \
                         drops the retry, and signing in again starts syncing again."
                    ),
                ))
            }
            Some(SyncDeletionStatus::Failed(reason)) => {
                column.add_child(note(appearance, format!("Deletion failed: {reason}")))
            }
            _ => {}
        }
        Container::new(column.finish())
            .with_padding_bottom(20.)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::shows_team_section;
    use mnemos_ide_sync::{SyncStatusSnapshot, Team};

    #[test]
    fn no_team_section_for_an_account_in_no_team() {
        assert!(!shows_team_section(&SyncStatusSnapshot::default()));
    }

    #[test]
    fn team_section_for_existing_members() {
        let listed = SyncStatusSnapshot {
            teams: vec![Team {
                id: "t1".into(),
                name: "Team".into(),
                role: "member".into(),
                member_count: 2,
            }],
            ..Default::default()
        };
        assert!(shows_team_section(&listed));

        let joined = SyncStatusSnapshot {
            team_id: Some("t1".into()),
            ..Default::default()
        };
        assert!(shows_team_section(&joined));
    }
}
