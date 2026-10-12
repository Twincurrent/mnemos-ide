use std::sync::OnceLock;

use super::{
    settings_page::{
        MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
        SettingsWidget,
    },
    SettingsSection,
};
use crate::{
    appearance::Appearance, channel::ChannelState, themes::theme::ColorScheme,
    workspace::WorkspaceAction,
};
use mnemos_ui::{
    assets::asset_cache::AssetSource,
    elements::{
        Align, CacheOption, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex, Image,
        MainAxisAlignment, MouseStateHandle, ParentElement, Wrap,
    },
    ui_components::components::UiComponent,
    AppContext, Entity, View, ViewContext, ViewHandle,
};

pub struct AboutPageView {
    page: PageType<Self>,
}

impl AboutPageView {
    pub fn new(_ctx: &mut ViewContext<AboutPageView>) -> Self {
        AboutPageView {
            page: PageType::new_monolith(AboutPageWidget::default(), None, false),
        }
    }
}

impl Entity for AboutPageView {
    type Event = SettingsPageEvent;
}

impl View for AboutPageView {
    fn ui_name() -> &'static str {
        "AboutPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Default)]
struct AboutPageWidget {
    copy_version_button_mouse_state: MouseStateHandle,
    source_link_mouse_state: MouseStateHandle,
    license_link_mouse_state: MouseStateHandle,
    notice_link_mouse_state: MouseStateHandle,
    third_party_link_mouse_state: MouseStateHandle,
}

/// Mnemos IDE: AGPL-3.0 section 5(d) and 6. The legal notices the About page shows. Where the
/// source is published and the commit this binary was built from come from `app/build.rs`
/// (`embed_source_info`); a dev build has no commit.
const SOURCE_URL: &str = env!("MNEMOS_SOURCE_URL");
const GIT_COMMIT: &str = env!("MNEMOS_GIT_COMMIT");

const LEGAL_NOTICE_LINES: [&str; 3] = [
    "Mnemos IDE. Copyright (C) 2026 TwinCurrent ApS.",
    "Based on Warp, Copyright (C) 2020-2026 Denver Technologies, Inc.",
    "Free software under the GNU Affero General Public License, version 3 only (the UI \
     framework crates under the MIT license). It comes with ABSOLUTELY NO WARRANTY.",
];

/// The source tree this binary was built from: the commit when known, else the repository.
fn source_tree_url() -> String {
    if GIT_COMMIT.is_empty() {
        SOURCE_URL.to_string()
    } else {
        format!("{SOURCE_URL}/tree/{GIT_COMMIT}")
    }
}

/// Where the About page's "License", "NOTICE" and "Third-party licenses" links point.
struct NoticeLinks {
    license: Option<String>,
    notice: Option<String>,
    third_party: Option<String>,
}

/// Computed once: the links stat the bundled files, and the page renders often.
fn notice_links() -> &'static NoticeLinks {
    static LINKS: OnceLock<NoticeLinks> = OnceLock::new();
    LINKS.get_or_init(|| NoticeLinks {
        license: notice_file_url("LICENSE-AGPL", true),
        notice: notice_file_url("NOTICE", true),
        third_party: notice_file_url("THIRD_PARTY_LICENSES.txt", false),
    })
}

/// A link to `file`. A file that is in git links to its copy at this build's commit when the
/// source URL is known: `NOTICE` and `LICENSE-AGPL` have no extension, and opening an
/// extensionless `file://` URL shows an "open with" picker on Windows and an error on macOS.
/// Otherwise the copy installed beside the app by `prepare_bundled_resources`, as a `file://`
/// URL.
fn notice_file_url(file: &str, in_git: bool) -> Option<String> {
    if in_git && !SOURCE_URL.is_empty() {
        let rev = if GIT_COMMIT.is_empty() { "HEAD" } else { GIT_COMMIT };
        return Some(format!("{SOURCE_URL}/blob/{rev}/{file}"));
    }
    mnemos_core::paths::bundled_resources_dir()
        .map(|dir| dir.join(file))
        .filter(|path| path.is_file())
        .and_then(|local| url::Url::from_file_path(local).ok())
        .map(String::from)
}

impl SettingsWidget for AboutPageWidget {
    type View = AboutPageView;

    fn search_terms(&self) -> &str {
        "about mnemos ide version license source copyright notice"
    }

    fn render(
        &self,
        _view: &AboutPageView,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let ui_builder = appearance.ui_builder();

        let image_path = if theme.inferred_color_scheme() == ColorScheme::LightOnDark {
            "bundled/svg/mnemos-logo-with-light-title.svg"
        } else {
            "bundled/svg/mnemos-logo-with-dark-title.svg"
        };

        let version = ChannelState::app_version().unwrap_or("v#.##.###");

        let version_text = ui_builder
            .span(version.to_string())
            .with_soft_wrap()
            .build()
            .with_margin_top(16.)
            .finish();

        let copy_version_icon = appearance
            .ui_builder()
            .copy_button(16., self.copy_version_button_mouse_state.clone())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::CopyVersion(version));
            })
            .finish();

        let version_row = Wrap::row()
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_children([
                version_text,
                Container::new(copy_version_icon)
                    .with_margin_top(16.)
                    .with_padding_left(6.)
                    .finish(),
            ]);

        let links = notice_links();
        let links = [
            (
                "Source code",
                (!SOURCE_URL.is_empty()).then(source_tree_url),
                &self.source_link_mouse_state,
            ),
            (
                "License (AGPL-3.0)",
                links.license.clone(),
                &self.license_link_mouse_state,
            ),
            (
                "NOTICE",
                links.notice.clone(),
                &self.notice_link_mouse_state,
            ),
            (
                "Third-party licenses",
                links.third_party.clone(),
                &self.third_party_link_mouse_state,
            ),
        ]
        .into_iter()
        .filter_map(|(label, url, mouse_state)| {
            let url = url?;
            Some(
                Container::new(
                    ui_builder
                        .link(label.to_string(), Some(url), None, mouse_state.clone())
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .with_margin_top(12.)
                .with_padding_left(8.)
                .with_padding_right(8.)
                .finish(),
            )
        })
        .collect::<Vec<_>>();

        Align::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        Image::new(
                            AssetSource::Bundled { path: image_path },
                            CacheOption::BySize,
                        )
                        .finish(),
                    )
                    .with_max_height(100.)
                    .with_max_width(350.)
                    .finish(),
                )
                .with_child(version_row.finish())
                .with_children(LEGAL_NOTICE_LINES.iter().enumerate().map(|(i, line)| {
                    ui_builder
                        .span(line.to_string())
                        .with_soft_wrap()
                        .build()
                        .with_margin_top(if i == 0 { 16. } else { 4. })
                        .finish()
                }))
                .with_child(
                    ui_builder
                        .span(if GIT_COMMIT.is_empty() {
                            "Development build (no commit recorded).".to_string()
                        } else {
                            format!("Built from commit {GIT_COMMIT}.")
                        })
                        .with_soft_wrap()
                        .build()
                        .with_margin_top(4.)
                        .finish(),
                )
                .with_child(
                    Wrap::row()
                        .with_main_axis_alignment(MainAxisAlignment::Center)
                        .with_children(links)
                        .finish(),
                )
                .finish(),
        )
        .finish()
    }
}

impl SettingsPageMeta for AboutPageView {
    fn section() -> SettingsSection {
        SettingsSection::About
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

impl From<ViewHandle<AboutPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<AboutPageView>) -> Self {
        SettingsPageViewHandle::About(view_handle)
    }
}
