#![allow(dead_code)]

use ai::LLMId;
use anyhow::Result;
use onboarding::slides::OnboardingModelInfo;
use onboarding::{
    AgentOnboardingEvent, AgentOnboardingView, MockTelemetryContextProvider, SelectedSettings,
};
use rust_embed::RustEmbed;
use std::borrow::Cow;
use mnemos_core::ui::icons::Icon;
use mnemos_core::ui::theme::mnemos_themes;
use mnemos_core::ui::{appearance::Appearance, theme::MnemosTheme};
use mnemos_ui::platform;
use mnemos_ui::{
    elements::{
        Container, CrossAxisAlignment, Flex, MainAxisAlignment, MainAxisSize, ParentElement,
    },
    fonts::{Cache, FamilyId, Weight},
    presenter::ChildView,
    ui_components::components::{UiComponent as _, UiComponentStyles},
    AddWindowOptions, AppContext, AssetProvider, Element, Entity, SingletonEntity as _,
    TypedActionView, View, ViewContext, ViewHandle,
};

#[derive(Clone, Copy, RustEmbed)]
#[folder = "../../app/assets"]
pub struct Assets;

pub static ASSETS: Assets = Assets;

impl AssetProvider for Assets {
    fn get(&self, path: &str) -> Result<Cow<'_, [u8]>> {
        <Assets as RustEmbed>::get(path)
            .map(|f| f.data)
            .ok_or_else(|| anyhow::anyhow!(format!("no asset exists at path {path}")))
    }
}

fn main() -> Result<()> {
    // Initialize logging for the onboarding binary.
    mnemos_logging::init(mnemos_logging::LogConfig {
        is_cli: false,
        log_destination: None,
    })?;

    let app_builder =
        platform::AppBuilder::new(platform::AppCallbacks::default(), Box::new(ASSETS), None);
    let _ = app_builder.run(move |ctx| {
        // Register Appearance singleton so views can access Appearance::handle(ctx).
        ctx.add_singleton_model(|ctx| build_appearance(mnemos_themes::mnemos_night(), ctx));

        // Register telemetry context provider for logging telemetry events.
        ctx.add_singleton_model(MockTelemetryContextProvider::new_context_provider);

        ctx.add_window(AddWindowOptions::default(), |ctx| {
            OnboardingMainView::new(ctx)
        });

        onboarding::init(ctx);
    });

    Ok(())
}

#[derive(Clone, Debug)]
enum OnboardingMainState {
    Onboarding(ViewHandle<AgentOnboardingView>),
    Finished(ViewHandle<FinishedOnboardingView>),
}

struct OnboardingMainView {
    state: OnboardingMainState,
}

impl OnboardingMainView {
    fn new(ctx: &mut ViewContext<Self>) -> Self {
        let themes = mnemos_themes::mnemos();
        let default_model_id = LLMId::from("auto");
        let models = vec![
            OnboardingModelInfo {
                id: LLMId::from("auto"),
                title: "Auto".to_string(),
                icon: Icon::Oz,
                requires_upgrade: false,
                is_default: true,
            },
            OnboardingModelInfo {
                id: LLMId::from("claude-sonnet"),
                title: "Claude Sonnet".to_string(),
                icon: Icon::ClaudeLogo,
                requires_upgrade: false,
                is_default: false,
            },
            OnboardingModelInfo {
                id: LLMId::from("gpt-4o"),
                title: "GPT-4o".to_string(),
                icon: Icon::OpenAILogo,
                requires_upgrade: true,
                is_default: false,
            },
        ];
        let onboarding_view = ctx.add_typed_action_view(move |ctx| {
            // agent_modality_enabled is false for demo purposes
            AgentOnboardingView::new(
                themes.clone(),
                true,
                models.clone(),
                default_model_id.clone(),
                false,
                false,
                onboarding::OnboardingAuthState::LoggedOut,
                ctx,
            )
        });
        onboarding_view.update(ctx, |view, ctx| {
            view.start_onboarding(ctx);
        });
        ctx.subscribe_to_view(&onboarding_view, |me, _view, event, ctx| {
            me.handle_onboarding_event(event, ctx);
        });

        Self {
            state: OnboardingMainState::Onboarding(onboarding_view),
        }
    }

    fn handle_onboarding_event(
        &mut self,
        event: &AgentOnboardingEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        match event {
            AgentOnboardingEvent::ThemeSelected { theme_name } => {
                let Some(theme) = mnemos_themes::mnemos()
                    .into_iter()
                    .find(|theme| theme.name().as_deref() == Some(theme_name.as_str()))
                else {
                    return;
                };

                Appearance::handle(ctx).update(ctx, |appearance, ctx| {
                    appearance.set_theme(theme, ctx);
                });
            }
            AgentOnboardingEvent::OnboardingCompleted(selected_settings) => {
                let finished_view = ctx.add_typed_action_view(|_| {
                    FinishedOnboardingView::new(Some(selected_settings.clone()))
                });
                self.state = OnboardingMainState::Finished(finished_view);
                ctx.notify();
            }
            AgentOnboardingEvent::OnboardingSkipped => {
                let finished_view =
                    ctx.add_typed_action_view(|_| FinishedOnboardingView::new(None));
                self.state = OnboardingMainState::Finished(finished_view);
                ctx.notify();
            }
            AgentOnboardingEvent::SyncWithOsToggled { .. }
            | AgentOnboardingEvent::UpgradeRequested
            | AgentOnboardingEvent::UpgradeCopyUrlRequested
            | AgentOnboardingEvent::UpgradePasteTokenFromClipboardRequested
            | AgentOnboardingEvent::LoginFromWelcomeRequested
            | AgentOnboardingEvent::PrivacySettingsFromTerminalThemeSlideRequested
            | AgentOnboardingEvent::AppBecameActive => {
                // No-op in the standalone demo binary
            }
        }
    }
}

struct FinishedOnboardingView {
    selected_settings: Option<SelectedSettings>,
}

impl FinishedOnboardingView {
    fn new(selected_settings: Option<SelectedSettings>) -> Self {
        Self { selected_settings }
    }
}

impl Entity for FinishedOnboardingView {
    type Event = ();
}

impl View for FinishedOnboardingView {
    fn ui_name() -> &'static str {
        "FinishedOnboardingView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);

        let header_text = if self.selected_settings.is_some() {
            "Finished Onboarding"
        } else {
            "Skipped Onboarding"
        };

        let header = appearance
            .ui_builder()
            .paragraph(header_text)
            .with_style(UiComponentStyles {
                font_size: Some(28.),
                font_weight: Some(Weight::Medium),
                ..Default::default()
            })
            .build()
            .finish();

        let details_text = match &self.selected_settings {
            Some(selected_settings) => format!("SelectedSettings: {selected_settings:?}"),
            None => "SelectedSettings: (none)".to_string(),
        };

        let details = appearance
            .ui_builder()
            .paragraph(details_text)
            .with_style(UiComponentStyles {
                font_size: Some(14.),
                font_weight: Some(Weight::Normal),
                ..Default::default()
            })
            .build()
            .finish();

        let theme = appearance.theme();

        Container::new(
            Flex::column()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::Center)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(header)
                .with_child(Container::new(details).with_margin_top(12.).finish())
                .finish(),
        )
        .with_background(theme.background())
        .with_uniform_padding(64.)
        .finish()
    }
}

impl TypedActionView for FinishedOnboardingView {
    type Action = ();
}

impl Entity for OnboardingMainView {
    type Event = ();
}

impl View for OnboardingMainView {
    fn ui_name() -> &'static str {
        "OnboardingMainView"
    }

    fn render(&self, _app: &AppContext) -> Box<dyn Element> {
        match &self.state {
            OnboardingMainState::Onboarding(view) => ChildView::new(view).finish(),
            OnboardingMainState::Finished(view) => ChildView::new(view).finish(),
        }
    }

    fn on_focus(&mut self, focus_ctx: &mnemos_ui::FocusContext, ctx: &mut ViewContext<Self>) {
        if let OnboardingMainState::Onboarding(view) = &self.state {
            if focus_ctx.is_self_focused() {
                ctx.focus(view);
            }
        }
    }
}

impl TypedActionView for OnboardingMainView {
    type Action = ();
}

fn build_appearance(theme: MnemosTheme, ctx: &mut AppContext) -> Appearance {
    let ui_font_family =
        load_default_ui_font_family(ctx).expect("unable to load default ui font family");

    Appearance::new(
        theme,
        ui_font_family,
        13.0,
        Weight::Normal,
        ui_font_family,
        1.2,
        ui_font_family,
        ui_font_family,
    )
}

fn load_default_ui_font_family(ctx: &mut AppContext) -> anyhow::Result<FamilyId> {
    Cache::handle(ctx).update(ctx, |font_cache, _| {
        // On Windows, default to use Segoe UI as the UI font.
        #[cfg(windows)]
        if let Ok(font_family_id) = font_cache.load_system_font("Segoe UI") {
            return Ok(font_family_id);
        }

        font_cache.load_family_from_bytes(
            "Roboto",
            vec![
                ASSETS
                    .get("bundled/fonts/roboto/Roboto-Italic.ttf")?
                    .to_vec(),
                ASSETS.get("bundled/fonts/roboto/Roboto-Bold.ttf")?.to_vec(),
                ASSETS
                    .get("bundled/fonts/roboto/Roboto-Regular.ttf")?
                    .to_vec(),
                ASSETS
                    .get("bundled/fonts/roboto/Roboto-Medium.ttf")?
                    .to_vec(),
                ASSETS
                    .get("bundled/fonts/roboto/RobotoFlex-Semibold.ttf")?
                    .to_vec(),
                ASSETS
                    .get("bundled/fonts/roboto/Roboto-BoldItalic.ttf")?
                    .to_vec(),
            ],
        )
    })
}
