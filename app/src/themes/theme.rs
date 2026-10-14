use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::iter::FromIterator;
use std::path::PathBuf;
use mnemos_core::ui::color::pick_foreground_color;
use mnemos_core::ui::theme::mnemos_themes::{
    catppuccin_frappe, catppuccin_latte, catppuccin_macchiato, catppuccin_mocha, dracula,
    everforest_dark, everforest_light, fjord, github_dark, github_light, gruvbox_dark,
    gruvbox_light, kanagawa, mnemos_contrast, mnemos_day, mnemos_day_colors, mnemos_moss,
    mnemos_night, mnemos_night_colors, mountain_dawn, mountain_night, night_owl, nord, one_dark,
    rose_pine, rose_pine_dawn, rose_pine_moon, slate, solarized_dark, solarized_light, stone,
    tokyo_night, tokyo_night_day,
};
use mnemos_ui::assets::asset_cache::AssetSource;
use mnemos_ui::{
    color::ColorU,
    elements::{
        Align, Border, ConstrainedBox, Container, Element, Empty, Flex, ParentElement, Rect,
        Shrinkable, Stack, Text,
    },
    fonts::FamilyId,
};

use super::theme_creator::{pick_accent_color_from_options, top_colors_for_image};

pub use mnemos_core::ui::color::blend::Blend;
pub use mnemos_core::ui::theme::*;

const THUMBNAIL_MARGIN: f32 = 10.;

// We use the discriminant of enum variants to determine the order of theme types in the
// theme chooser view.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Serialize,
    Deserialize,
    Hash,
    Eq,
    Ord,
    PartialOrd,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "The color theme.", rename_all = "snake_case")]
pub enum ThemeKind {
    // Legacy referral themes inherited from upstream: kept only so old settings still
    // deserialize; never listed. The alias is for backwards-compatibility: originally there
    // was a single reward theme named `ReferralReward`.
    #[serde(alias = "ReferralReward")]
    #[schemars(skip)]
    SentReferralReward,
    #[schemars(skip)]
    ReceivedReferralReward,
    #[default]
    #[schemars(description = "Mnemos Night")]
    MnemosNight,
    #[schemars(description = "Mnemos Day")]
    MnemosDay,
    #[schemars(description = "Mnemos Contrast")]
    MnemosContrast,
    #[schemars(description = "Mnemos Moss")]
    MnemosMoss,
    // Mnemos IDE: the image themes, a ground colour over a bundled background image, as their
    // own group between the Mnemos themes and the community themes (in this order, not
    // alphabetically). They are new variants: like the Mnemos ones, they need every synced
    // machine on a build that has them.
    #[schemars(description = "Stone")]
    Stone,
    #[schemars(description = "Slate")]
    Slate,
    #[schemars(description = "Mountain Night")]
    MountainNight,
    #[schemars(description = "Mountain Dawn")]
    MountainDawn,
    #[schemars(description = "Fjord")]
    Fjord,
    // Mnemos IDE: the community themes, alphabetically by display name so the chooser (which
    // sorts by discriminant) lists them in that order after the image themes. Dracula,
    // Gruvbox and Solarized are upstream's legacy variants revived with Mnemos's own
    // definitions: builds that predate them already parse their values, so picking one is safe
    // across synced machines. The others are new and need every synced machine on a build that
    // has them, like the Mnemos variants. Serde names the variants, so their position here does
    // not change any saved value.
    #[schemars(description = "Catppuccin Frappé")]
    CatppuccinFrappe,
    #[schemars(description = "Catppuccin Latte")]
    CatppuccinLatte,
    #[schemars(description = "Catppuccin Macchiato")]
    CatppuccinMacchiato,
    #[schemars(description = "Catppuccin Mocha")]
    CatppuccinMocha,
    #[schemars(description = "Dracula")]
    Dracula,
    #[schemars(description = "Everforest Dark")]
    EverforestDark,
    #[schemars(description = "Everforest Light")]
    EverforestLight,
    #[schemars(description = "GitHub Dark")]
    GithubDark,
    #[schemars(description = "GitHub Light")]
    GithubLight,
    #[schemars(description = "Gruvbox Dark")]
    GruvboxDark,
    #[schemars(description = "Gruvbox Light")]
    GruvboxLight,
    #[schemars(description = "Kanagawa")]
    Kanagawa,
    #[schemars(description = "Night Owl")]
    NightOwl,
    #[schemars(description = "Nord")]
    Nord,
    #[schemars(description = "One Dark")]
    OneDark,
    #[schemars(description = "Rosé Pine")]
    RosePine,
    #[schemars(description = "Rosé Pine Dawn")]
    RosePineDawn,
    #[schemars(description = "Rosé Pine Moon")]
    RosePineMoon,
    #[schemars(description = "Solarized Dark")]
    SolarizedDark,
    #[schemars(description = "Solarized Light")]
    SolarizedLight,
    #[schemars(description = "Tokyo Night")]
    TokyoNight,
    #[schemars(description = "Tokyo Night Day")]
    TokyoNightDay,
    // Legacy built-in themes inherited from upstream: kept only so old settings still
    // deserialize, never listed. A saved value is never rewritten, only rendered as the Mnemos
    // theme that replaces it (`ThemeKind::resolved`): writing a Mnemos variant on launch would
    // reach machines on older builds through settings sync, and those cannot parse it. The
    // settings-file derive ignores serde aliases and a value that fails to parse inhibits
    // writes to that key, so these cannot simply go.
    #[schemars(skip)]
    Adeberry,
    #[schemars(skip)]
    Phenomenon,
    #[schemars(skip)]
    Dark,
    #[schemars(skip)]
    FancyDracula,
    #[schemars(skip)]
    CyberWave,
    #[schemars(skip)]
    SolarFlare,
    #[schemars(skip)]
    WillowDream,
    #[schemars(skip)]
    Light,
    #[schemars(skip)]
    DarkCity,
    #[schemars(skip)]
    RedRock,
    #[schemars(skip)]
    JellyFish,
    #[schemars(skip)]
    Leafy,
    #[schemars(skip)]
    Koi,
    #[schemars(skip)]
    Snowy,
    #[schemars(skip)]
    PinkCity,
    #[schemars(skip)]
    Marble,
    #[schemars(description = "A user-provided custom theme loaded from a file.")]
    Custom(CustomTheme),
    /// Base16 themes are a special case of custom themes with their own semantics for ANSI colors that override "bright" color variants.
    #[schemars(description = "A custom theme using the Base16 color scheme format.")]
    CustomBase16(CustomTheme),
    #[schemars(skip)]
    InMemory(InMemoryThemeOptions),
}

impl From<CustomTheme> for ThemeKind {
    fn from(custom_theme: CustomTheme) -> ThemeKind {
        if custom_theme.name.as_str().starts_with("Base16") {
            ThemeKind::CustomBase16(custom_theme)
        } else {
            ThemeKind::Custom(custom_theme)
        }
    }
}

impl std::fmt::Display for ThemeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match &self {
            ThemeKind::MnemosNight => "Mnemos Night",
            ThemeKind::MnemosDay => "Mnemos Day",
            ThemeKind::MnemosContrast => "Mnemos Contrast",
            ThemeKind::MnemosMoss => "Mnemos Moss",
            ThemeKind::Stone => "Stone",
            ThemeKind::Slate => "Slate",
            ThemeKind::MountainNight => "Mountain Night",
            ThemeKind::MountainDawn => "Mountain Dawn",
            ThemeKind::Fjord => "Fjord",
            ThemeKind::CatppuccinFrappe => "Catppuccin Frappé",
            ThemeKind::CatppuccinLatte => "Catppuccin Latte",
            ThemeKind::CatppuccinMacchiato => "Catppuccin Macchiato",
            ThemeKind::CatppuccinMocha => "Catppuccin Mocha",
            ThemeKind::Dracula => "Dracula",
            ThemeKind::EverforestDark => "Everforest Dark",
            ThemeKind::EverforestLight => "Everforest Light",
            ThemeKind::GithubDark => "GitHub Dark",
            ThemeKind::GithubLight => "GitHub Light",
            ThemeKind::GruvboxDark => "Gruvbox Dark",
            ThemeKind::GruvboxLight => "Gruvbox Light",
            ThemeKind::Kanagawa => "Kanagawa",
            ThemeKind::NightOwl => "Night Owl",
            ThemeKind::Nord => "Nord",
            ThemeKind::OneDark => "One Dark",
            ThemeKind::RosePine => "Rosé Pine",
            ThemeKind::RosePineDawn => "Rosé Pine Dawn",
            ThemeKind::RosePineMoon => "Rosé Pine Moon",
            ThemeKind::SolarizedDark => "Solarized Dark",
            ThemeKind::SolarizedLight => "Solarized Light",
            ThemeKind::TokyoNight => "Tokyo Night",
            ThemeKind::TokyoNightDay => "Tokyo Night Day",
            ThemeKind::Light => "Light",
            ThemeKind::Dark => "Dark",
            ThemeKind::JellyFish => "Jellyfish",
            ThemeKind::Koi => "Koi",
            ThemeKind::Leafy => "Leafy",
            ThemeKind::Marble => "Marble",
            ThemeKind::PinkCity => "Pink City",
            ThemeKind::Snowy => "Snowy",
            ThemeKind::DarkCity => "Dark City",
            ThemeKind::RedRock => "Red Rock",
            ThemeKind::CyberWave => "Cyber Wave",
            ThemeKind::WillowDream => "Willow Dream",
            ThemeKind::FancyDracula => "Fancy Dracula",
            ThemeKind::Phenomenon => "Phenomenon",
            ThemeKind::SolarFlare => "Solar Flare",
            ThemeKind::Adeberry => "Adeberry",
            ThemeKind::SentReferralReward => "Mnemos Referral",
            ThemeKind::ReceivedReferralReward => "Referred to Mnemos",
            ThemeKind::Custom(custom_theme) => custom_theme.name.as_str(),
            ThemeKind::CustomBase16(custom_theme) => custom_theme.name.as_str(),
            ThemeKind::InMemory(in_memory_theme) => in_memory_theme.name.as_str(),
        };
        write!(f, "{value}")
    }
}

impl ThemeKind {
    pub fn matches(&self, query: &str) -> bool {
        // Fold the accent of "Rosé Pine" so typing "rose" finds it.
        let fold = |text: &str| text.to_lowercase().replace('é', "e");
        fold(&self.to_string()).contains(&fold(query))
    }

    /// The Mnemos theme that replaces a legacy built-in (or referral) theme, or `None` when
    /// this kind is a built-in (Mnemos, image or community), custom or in-memory theme. Light legacy
    /// themes map to Mnemos Day, every other legacy theme to Mnemos Night.
    pub fn migrated(&self) -> Option<ThemeKind> {
        match self {
            ThemeKind::Light | ThemeKind::Snowy | ThemeKind::PinkCity | ThemeKind::Marble => {
                Some(ThemeKind::MnemosDay)
            }
            ThemeKind::SentReferralReward
            | ThemeKind::ReceivedReferralReward
            | ThemeKind::Adeberry
            | ThemeKind::Phenomenon
            | ThemeKind::Dark
            | ThemeKind::FancyDracula
            | ThemeKind::CyberWave
            | ThemeKind::SolarFlare
            | ThemeKind::WillowDream
            | ThemeKind::DarkCity
            | ThemeKind::RedRock
            | ThemeKind::JellyFish
            | ThemeKind::Leafy
            | ThemeKind::Koi => Some(ThemeKind::MnemosNight),
            ThemeKind::MnemosNight
            | ThemeKind::MnemosDay
            | ThemeKind::MnemosContrast
            | ThemeKind::MnemosMoss
            | ThemeKind::Stone
            | ThemeKind::Slate
            | ThemeKind::MountainNight
            | ThemeKind::MountainDawn
            | ThemeKind::Fjord
            | ThemeKind::CatppuccinFrappe
            | ThemeKind::CatppuccinLatte
            | ThemeKind::CatppuccinMacchiato
            | ThemeKind::CatppuccinMocha
            | ThemeKind::Dracula
            | ThemeKind::EverforestDark
            | ThemeKind::EverforestLight
            | ThemeKind::GithubDark
            | ThemeKind::GithubLight
            | ThemeKind::GruvboxDark
            | ThemeKind::GruvboxLight
            | ThemeKind::Kanagawa
            | ThemeKind::NightOwl
            | ThemeKind::Nord
            | ThemeKind::OneDark
            | ThemeKind::RosePine
            | ThemeKind::RosePineDawn
            | ThemeKind::RosePineMoon
            | ThemeKind::SolarizedDark
            | ThemeKind::SolarizedLight
            | ThemeKind::TokyoNight
            | ThemeKind::TokyoNightDay
            | ThemeKind::Custom(_)
            | ThemeKind::CustomBase16(_)
            | ThemeKind::InMemory(_) => None,
        }
    }

    /// This kind with a legacy built-in theme replaced by its Mnemos theme. Every other kind
    /// is returned unchanged.
    pub fn resolved(&self) -> ThemeKind {
        self.migrated().unwrap_or_else(|| self.clone())
    }

    /// Whether this is a legacy built-in theme that is no longer offered.
    pub fn is_legacy_builtin(&self) -> bool {
        self.migrated().is_some()
    }
}

#[derive(
    Debug,
    Clone,
    Hash,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "A user-provided custom theme.")]
pub struct CustomTheme {
    #[schemars(description = "The display name of the custom theme.")]
    name: String,
    #[schemars(description = "The file path to the custom theme definition.")]
    path: PathBuf,
}

impl CustomTheme {
    pub fn new(s: String, p: PathBuf) -> Self {
        CustomTheme { name: s, path: p }
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
}

#[derive(
    Debug,
    Clone,
    Hash,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    PartialOrd,
    Ord,
    settings_value::SettingsValue,
)]
pub struct InMemoryThemeOptions {
    name: String,
    path: PathBuf,
    #[serde(skip)]
    possible_bg_colors: Vec<ColorU>,
    #[serde(skip)]
    chosen_bg_color_index: usize,
}

impl InMemoryThemeOptions {
    pub async fn new(name: String, path: PathBuf) -> Result<Self> {
        top_colors_for_image(path.clone()).map(|top_colors| InMemoryThemeOptions {
            name,
            path,
            possible_bg_colors: top_colors,
            chosen_bg_color_index: 0,
        })
    }

    pub fn chosen_bg_color(&self) -> ColorU {
        self.possible_bg_colors[self.chosen_bg_color_index]
    }

    pub fn possible_bg_colors(&self) -> Vec<ColorU> {
        self.possible_bg_colors.clone()
    }

    pub fn chosen_bg_color_index(&self) -> usize {
        self.chosen_bg_color_index
    }

    pub fn set_chosen_bg_color_index(&mut self, index: usize) {
        self.chosen_bg_color_index = index;
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = path;
    }

    pub fn theme(&self) -> MnemosTheme {
        let bg_color = self.chosen_bg_color();
        let fg_color = pick_foreground_color(bg_color);
        let possible_accent_colors: Vec<ColorU> = self
            .possible_bg_colors
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != self.chosen_bg_color_index)
            .map(|(_, color)| *color)
            .collect();
        let accent_color =
            pick_accent_color_from_options(&[bg_color, fg_color], &possible_accent_colors[..]);

        let (details, terminal_colors) = if fg_color.eq(&ColorU::white()) {
            (Details::Darker, mnemos_night_colors())
        } else {
            (Details::Lighter, mnemos_day_colors())
        };

        MnemosTheme::new(
            bg_color.into(),
            fg_color,
            accent_color.into(),
            None,
            Some(details),
            terminal_colors,
            Some(Image {
                // Note that, as an invariant, in-memory themes come from local files.
                source: AssetSource::LocalFile {
                    path: self.path().to_str().unwrap_or_default().to_owned(),
                },
                opacity: 30,
            }),
            Some(self.name()),
        )
    }
}

#[derive(Debug, Clone)]
pub struct MnemosThemeConfig {
    theme_map: HashMap<ThemeKind, MnemosTheme>,
}

impl MnemosThemeConfig {
    pub fn new() -> Self {
        // Preload with the built-in themes: the Mnemos, image and community themes.
        // Legacy kinds are deliberately absent, so the theme chooser never lists them;
        // `theme()` resolves them to their Mnemos theme.
        let theme_map: HashMap<ThemeKind, MnemosTheme> = HashMap::from_iter([
            (ThemeKind::MnemosNight, mnemos_night()),
            (ThemeKind::MnemosDay, mnemos_day()),
            (ThemeKind::MnemosContrast, mnemos_contrast()),
            (ThemeKind::MnemosMoss, mnemos_moss()),
            (ThemeKind::Stone, stone()),
            (ThemeKind::Slate, slate()),
            (ThemeKind::MountainNight, mountain_night()),
            (ThemeKind::MountainDawn, mountain_dawn()),
            (ThemeKind::Fjord, fjord()),
            (ThemeKind::CatppuccinFrappe, catppuccin_frappe()),
            (ThemeKind::CatppuccinLatte, catppuccin_latte()),
            (ThemeKind::CatppuccinMacchiato, catppuccin_macchiato()),
            (ThemeKind::CatppuccinMocha, catppuccin_mocha()),
            (ThemeKind::Dracula, dracula()),
            (ThemeKind::EverforestDark, everforest_dark()),
            (ThemeKind::EverforestLight, everforest_light()),
            (ThemeKind::GithubDark, github_dark()),
            (ThemeKind::GithubLight, github_light()),
            (ThemeKind::GruvboxDark, gruvbox_dark()),
            (ThemeKind::GruvboxLight, gruvbox_light()),
            (ThemeKind::Kanagawa, kanagawa()),
            (ThemeKind::NightOwl, night_owl()),
            (ThemeKind::Nord, nord()),
            (ThemeKind::OneDark, one_dark()),
            (ThemeKind::RosePine, rose_pine()),
            (ThemeKind::RosePineDawn, rose_pine_dawn()),
            (ThemeKind::RosePineMoon, rose_pine_moon()),
            (ThemeKind::SolarizedDark, solarized_dark()),
            (ThemeKind::SolarizedLight, solarized_light()),
            (ThemeKind::TokyoNight, tokyo_night()),
            (ThemeKind::TokyoNightDay, tokyo_night_day()),
        ]);
        MnemosThemeConfig { theme_map }
    }

    pub fn add_new_theme(&mut self, theme_name: ThemeKind, theme: MnemosTheme) {
        self.theme_map.insert(theme_name, theme);
    }

    pub fn file_to_theme(name: String, path: PathBuf) -> ThemeKind {
        CustomTheme::new(name, path).into()
    }

    pub fn theme_items(&self) -> impl Iterator<Item = (&ThemeKind, &MnemosTheme)> {
        self.theme_map.iter()
    }

    pub fn theme(&self, name: &ThemeKind) -> MnemosTheme {
        self.theme_map
            .get(name)
            .or_else(|| self.theme_map.get(&name.resolved()))
            .cloned()
            .unwrap_or_else(mnemos_night)
    }
}

impl Default for MnemosThemeConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RespectSystemTheme {
    #[default]
    Off,
    On(SelectedSystemThemes),
}

#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(description = "Themes to use when following the system light/dark mode.")]
pub struct SelectedSystemThemes {
    #[schemars(description = "The theme to use in light mode.")]
    pub light: ThemeKind,
    #[schemars(description = "The theme to use in dark mode.")]
    pub dark: ThemeKind,
}

impl RespectSystemTheme {
    pub fn selected_system_themes(&self) -> Option<&SelectedSystemThemes> {
        match self {
            RespectSystemTheme::Off => None,
            RespectSystemTheme::On(selected) => Some(selected),
        }
    }
}

impl Default for SelectedSystemThemes {
    fn default() -> Self {
        Self {
            light: ThemeKind::MnemosDay,
            dark: ThemeKind::MnemosNight,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct PromptColors {
    pub input_prompt_conversation_management: ColorU,
    pub input_prompt_pwd: ColorU,
    pub input_prompt_git: ColorU,
    pub input_prompt_branch: ColorU,
    pub input_prompt_agent_mode_hint: ColorU,
    pub input_prompt_agent_mode_tasks: ColorU,
    pub input_prompt_dirty_color: ColorU,
    pub input_prompt_virtual_env: ColorU,
    pub input_prompt_user_and_host: ColorU,
    pub input_prompt_date: ColorU,
    pub input_prompt_time: ColorU,
    pub input_prompt_kubernetes: ColorU,
    pub input_prompt_svn: ColorU,
    pub input_prompt_separator: ColorU,
    pub input_prompt_subshell: ColorU,
    pub input_prompt_ssh: ColorU,
}

impl From<MnemosTheme> for PromptColors {
    fn from(theme: MnemosTheme) -> Self {
        PromptColors {
            input_prompt_conversation_management: theme.terminal_colors().normal.white.into(),
            input_prompt_pwd: theme.terminal_colors().normal.magenta.into(),
            input_prompt_git: theme.terminal_colors().normal.green.into(),
            input_prompt_agent_mode_hint: theme.terminal_colors().normal.yellow.into(),
            input_prompt_agent_mode_tasks: theme.terminal_colors().normal.yellow.into(),
            input_prompt_branch: theme.terminal_colors().normal.yellow.into(),
            input_prompt_dirty_color: theme.terminal_colors().normal.green.into(),
            input_prompt_virtual_env: theme.terminal_colors().normal.yellow.into(),
            input_prompt_user_and_host: theme.terminal_colors().normal.green.into(),
            input_prompt_date: theme.terminal_colors().normal.cyan.into(),
            input_prompt_time: theme.terminal_colors().normal.red.into(),
            input_prompt_kubernetes: theme.terminal_colors().normal.cyan.into(),
            input_prompt_ssh: theme.terminal_colors().normal.blue.into(),
            input_prompt_subshell: theme.terminal_colors().normal.blue.into(),
            input_prompt_svn: theme.terminal_colors().normal.blue.into(),
            input_prompt_separator: theme.terminal_colors().normal.magenta.into(),
        }
    }
}

pub fn render_preview(
    theme: &MnemosTheme,
    font_family: FamilyId,
    form_factor: Option<f32>,
) -> Box<dyn Element> {
    let text_size = 8. * form_factor.unwrap_or(1.);
    let margin = THUMBNAIL_MARGIN * form_factor.unwrap_or(1.);
    let padding = 5. * form_factor.unwrap_or(1.);
    let text_line_1 = Container::new(
        Text::new_inline("ls", font_family, text_size)
            .with_color(theme.foreground().into_solid())
            .finish(),
    )
    .with_margin_left(margin)
    .with_margin_right(margin)
    .finish();

    let text_line_2 = Container::new(
        Flex::row()
            .with_child(
                Text::new_inline("dir   ", font_family, text_size)
                    .with_color(theme.terminal_colors().normal.blue.into())
                    .finish(),
            )
            .with_child(
                Text::new_inline("executable   ", font_family, text_size)
                    .with_color(theme.terminal_colors().normal.red.into())
                    .finish(),
            )
            .with_child(
                Text::new_inline("file", font_family, text_size)
                    .with_color(theme.foreground().into_solid())
                    .finish(),
            )
            .finish(),
    )
    .with_margin_left(margin)
    .with_margin_right(margin)
    .finish();

    let input_box = Shrinkable::new(
        1.,
        Align::new(
            Flex::column()
                // The border above the input box.
                .with_child(
                    Container::new(Empty::new().finish())
                        .with_padding_bottom(padding)
                        .with_border(
                            Border::top(1.2 * form_factor.unwrap_or(1.))
                                .with_border_color(theme.outline().into_solid()),
                        )
                        .finish(),
                )
                // The fake cursor within the input box.
                .with_child(
                    Container::new(
                        ConstrainedBox::new(
                            Rect::new()
                                .with_background_color(theme.accent().into_solid())
                                .finish(),
                        )
                        .with_height(12. * form_factor.unwrap_or(1.))
                        .with_width(2. * form_factor.unwrap_or(1.))
                        .finish(),
                    )
                    .with_margin_left(margin)
                    .with_margin_right(margin)
                    .finish(),
                )
                .finish(),
        )
        .bottom_left()
        .finish(),
    )
    .finish();

    let mut thumbnail = Stack::new();
    let mut background_opacity = 100;
    if let Some(background_image) = theme.background_image() {
        thumbnail.add_child(
            Shrinkable::new(
                1.,
                mnemos_ui::elements::Image::new(
                    background_image.source(),
                    mnemos_ui::elements::CacheOption::BySize,
                )
                .cover()
                .finish(),
            )
            .finish(),
        );
        background_opacity -= background_image.opacity;
    }

    thumbnail.add_child(
        Container::new(
            Container::new(
                Flex::column()
                    .with_child(text_line_1)
                    .with_child(
                        Container::new(text_line_2)
                            .with_padding_top(padding)
                            .finish(),
                    )
                    .with_child(input_box)
                    .finish(),
            )
            .with_margin_top(margin)
            .with_margin_bottom(margin)
            .finish(),
        )
        .with_background(theme.background().with_opacity(background_opacity))
        .finish(),
    );

    Align::new(
        Container::new(
            ConstrainedBox::new(thumbnail.finish())
                .with_height(100. * form_factor.unwrap_or(1.))
                .with_width(190. * form_factor.unwrap_or(1.))
                .finish(),
        )
        .finish(),
    )
    .finish()
}

#[cfg(test)]
#[path = "theme_test.rs"]
mod tests;
