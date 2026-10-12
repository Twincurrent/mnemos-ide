use super::{
    settings_page::{
        render_body_item, MatchData, PageType, SettingsPageMeta, SettingsPageViewHandle,
        SettingsWidget,
    },
    LocalOnlyIconState, SettingsSection, ToggleState,
};
use crate::{appearance::Appearance, drive::settings::MnemosDriveSettings};
use mnemos_core::{features::FeatureFlag, report_if_error, settings::ToggleableSetting as _};
use mnemos_ui::{
    elements::Element,
    ui_components::{components::UiComponent, switch::SwitchStateHandle},
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

#[derive(Debug, Clone)]
pub enum MnemosDriveSettingsPageAction {
    ToggleShowMnemosDrive,
}

pub struct MnemosDriveSettingsPageView {
    page: PageType<Self>,
}

impl MnemosDriveSettingsPageView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            page: PageType::new_uncategorized(
                // Mnemos IDE: no "create an account" header. The Drive's
                // Mnemos sections (session picker, sync status) work signed out.
                vec![Box::new(MnemosDriveToggleWidget::default())],
                None,
            ),
        }
    }
}

impl Entity for MnemosDriveSettingsPageView {
    type Event = ();
}

impl TypedActionView for MnemosDriveSettingsPageView {
    type Action = MnemosDriveSettingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            MnemosDriveSettingsPageAction::ToggleShowMnemosDrive => {
                MnemosDriveSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.enable_mnemos_drive.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
        }
    }
}

impl View for MnemosDriveSettingsPageView {
    fn ui_name() -> &'static str {
        "MnemosDrivePage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl SettingsPageMeta for MnemosDriveSettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::MnemosDrive
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::OpenMnemosNewSettingsModes.is_enabled()
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

impl From<ViewHandle<MnemosDriveSettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<MnemosDriveSettingsPageView>) -> Self {
        SettingsPageViewHandle::MnemosDrive(view_handle)
    }
}

#[derive(Default)]
struct MnemosDriveToggleWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for MnemosDriveToggleWidget {
    type View = MnemosDriveSettingsPageView;

    fn search_terms(&self) -> &str {
        "mnemos drive tools panel command palette search workflows prompts notebooks environment variables"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let settings = MnemosDriveSettings::as_ref(app);

        // Mnemos IDE: the toggle works signed out; the Drive's Mnemos sections
        // need no account.
        render_body_item::<MnemosDriveSettingsPageAction>(
            "Drive".into(),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*settings.enable_mnemos_drive)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(MnemosDriveSettingsPageAction::ToggleShowMnemosDrive);
                })
                .finish(),
            Some("The Drive lists your Claude Code sessions for the current folder (click to resume) and, once you sign in to sync, what syncs across your machines. No account needed.".into()),
        )
    }
}
