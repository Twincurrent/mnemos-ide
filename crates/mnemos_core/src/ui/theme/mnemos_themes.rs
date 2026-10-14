//! Mnemos's built-in themes.
//!
//! The four Mnemos themes derive from the app icon: an off-white (#F4F1EA) over
//! lime (#C6F35A) disc on a dark violet plate (#1B1033). These replace the
//! built-in themes inherited from upstream. They live here, not in the app
//! crate, so the onboarding dev binary can use the same definitions.
//!
//! The image themes come next: Stone, Slate, Mountain Night, Mountain Dawn and
//! Fjord, each a solid ground colour over a background image bundled with the app
//! (`app/assets/bundled/jpg/themes/`, credited in NOTICE and in the ATTRIBUTION.md
//! next to the images). The image shows through at the theme's image opacity.
//!
//! The community themes after them take background, foreground and the 16 ANSI
//! colours from each project's own published palette and terminal port (credited
//! in NOTICE); the accent is the colour the project uses for its main highlight.
//! They are not copied from upstream Warp's theme files. Where a palette is lower
//! in contrast than the Mnemos themes, only the derived selection highlights are
//! retuned (`with_selection_opacities`); the published colours stay as they are.
//!
//! Each ANSI set is listed in the order black, red, green, yellow, blue,
//! magenta, cyan, white. The contrast floors are pinned by
//! `mnemos_themes_tests.rs`.

use mnemos_ui::assets::asset_cache::AssetSource;
use mnemos_ui::color::ColorU;

use super::{AnsiColor, AnsiColors, Details, Fill, Image, MnemosTheme, Opacity, TerminalColors};

pub const MNEMOS_NIGHT_NAME: &str = "Mnemos Night";
pub const MNEMOS_DAY_NAME: &str = "Mnemos Day";
pub const MNEMOS_CONTRAST_NAME: &str = "Mnemos Contrast";
pub const MNEMOS_MOSS_NAME: &str = "Mnemos Moss";
pub const STONE_NAME: &str = "Stone";
pub const SLATE_NAME: &str = "Slate";
pub const MOUNTAIN_NIGHT_NAME: &str = "Mountain Night";
pub const MOUNTAIN_DAWN_NAME: &str = "Mountain Dawn";
pub const FJORD_NAME: &str = "Fjord";
pub const CATPPUCCIN_LATTE_NAME: &str = "Catppuccin Latte";
pub const CATPPUCCIN_MOCHA_NAME: &str = "Catppuccin Mocha";
pub const DRACULA_NAME: &str = "Dracula";
pub const GRUVBOX_DARK_NAME: &str = "Gruvbox Dark";
pub const GRUVBOX_LIGHT_NAME: &str = "Gruvbox Light";
pub const KANAGAWA_NAME: &str = "Kanagawa";
pub const NORD_NAME: &str = "Nord";
pub const ONE_DARK_NAME: &str = "One Dark";
pub const ROSE_PINE_NAME: &str = "Rosé Pine";
pub const SOLARIZED_DARK_NAME: &str = "Solarized Dark";
pub const SOLARIZED_LIGHT_NAME: &str = "Solarized Light";
pub const TOKYO_NIGHT_NAME: &str = "Tokyo Night";
pub const CATPPUCCIN_FRAPPE_NAME: &str = "Catppuccin Frappé";
pub const CATPPUCCIN_MACCHIATO_NAME: &str = "Catppuccin Macchiato";
pub const EVERFOREST_DARK_NAME: &str = "Everforest Dark";
pub const EVERFOREST_LIGHT_NAME: &str = "Everforest Light";
pub const GITHUB_DARK_NAME: &str = "GitHub Dark";
pub const GITHUB_LIGHT_NAME: &str = "GitHub Light";
pub const NIGHT_OWL_NAME: &str = "Night Owl";
pub const ROSE_PINE_DAWN_NAME: &str = "Rosé Pine Dawn";
pub const ROSE_PINE_MOON_NAME: &str = "Rosé Pine Moon";
pub const TOKYO_NIGHT_DAY_NAME: &str = "Tokyo Night Day";

/// The bundled background images of the image themes, as paths in the app's embedded assets
/// (`app/assets/bundled/...`, embedded by `Assets` in `app/src/lib.rs`). Bundled, never fetched:
/// the images ship inside the binary on every platform.
pub const STONE_IMAGE: &str = "bundled/jpg/themes/stone.jpg";
pub const SLATE_IMAGE: &str = "bundled/jpg/themes/slate.jpg";
pub const MOUNTAIN_NIGHT_IMAGE: &str = "bundled/jpg/themes/mountain_night.jpg";
pub const MOUNTAIN_DAWN_IMAGE: &str = "bundled/jpg/themes/mountain_dawn.jpg";
pub const FJORD_IMAGE: &str = "bundled/jpg/themes/fjord.jpg";

// Mnemos Night: the icon's violet plate as the ground, lime accent.
const MNEMOS_NIGHT_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x433565FF),
    AnsiColor::from_u32(0xFF6F7DFF),
    AnsiColor::from_u32(0x8EDB6AFF),
    AnsiColor::from_u32(0xF3CB6BFF),
    AnsiColor::from_u32(0x82A9FFFF),
    AnsiColor::from_u32(0xDD8CFFFF),
    AnsiColor::from_u32(0x62D9D0FF),
    AnsiColor::from_u32(0xDDD7CBFF),
);
const MNEMOS_NIGHT_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x8878ACFF),
    AnsiColor::from_u32(0xFFA3ACFF),
    AnsiColor::from_u32(0xC6F35AFF),
    AnsiColor::from_u32(0xFFE6A3FF),
    AnsiColor::from_u32(0xB3CBFFFF),
    AnsiColor::from_u32(0xEEBDFFFF),
    AnsiColor::from_u32(0xA0F0E8FF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Mnemos Day: off-white paper, violet ink, deep olive-lime accent.
const MNEMOS_DAY_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x1B1033FF),
    AnsiColor::from_u32(0xB42332FF),
    AnsiColor::from_u32(0x2D6A1EFF),
    AnsiColor::from_u32(0x7A4F00FF),
    AnsiColor::from_u32(0x2549B5FF),
    AnsiColor::from_u32(0x8A2BA6FF),
    AnsiColor::from_u32(0x00686EFF),
    AnsiColor::from_u32(0xD6CFC2FF),
);
const MNEMOS_DAY_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x5D5275FF),
    AnsiColor::from_u32(0xC2185BFF),
    AnsiColor::from_u32(0x3C7A08FF),
    AnsiColor::from_u32(0x7A6A00FF),
    AnsiColor::from_u32(0x4A3FD1FF),
    AnsiColor::from_u32(0xB0279AFF),
    AnsiColor::from_u32(0x007A63FF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Mnemos Contrast: near-black violet, white text, electric lime.
const MNEMOS_CONTRAST_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x4D4266FF),
    AnsiColor::from_u32(0xFF7A86FF),
    AnsiColor::from_u32(0x7FF08AFF),
    AnsiColor::from_u32(0xFFD84DFF),
    AnsiColor::from_u32(0x8CB4FFFF),
    AnsiColor::from_u32(0xF09BFFFF),
    AnsiColor::from_u32(0x5CF0E6FF),
    AnsiColor::from_u32(0xE8E4DCFF),
);
const MNEMOS_CONTRAST_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x9A8FB8FF),
    AnsiColor::from_u32(0xFFB3BAFF),
    AnsiColor::from_u32(0xD4FF5CFF),
    AnsiColor::from_u32(0xFFF0A8FF),
    AnsiColor::from_u32(0xC2D6FFFF),
    AnsiColor::from_u32(0xF8CCFFFF),
    AnsiColor::from_u32(0xB0FFF8FF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Mnemos Moss: Night with the roles swapped, a dark moss ground and a lavender accent.
const MNEMOS_MOSS_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x33402AFF),
    AnsiColor::from_u32(0xF57E73FF),
    AnsiColor::from_u32(0xA8D65CFF),
    AnsiColor::from_u32(0xE8C25EFF),
    AnsiColor::from_u32(0x8FAEF0FF),
    AnsiColor::from_u32(0xC79BFFFF),
    AnsiColor::from_u32(0x6FCFB6FF),
    AnsiColor::from_u32(0xD9DACBFF),
);
const MNEMOS_MOSS_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x76856AFF),
    AnsiColor::from_u32(0xFFAFA6FF),
    AnsiColor::from_u32(0xD2F59AFF),
    AnsiColor::from_u32(0xF8DD94FF),
    AnsiColor::from_u32(0xBCCEFAFF),
    AnsiColor::from_u32(0xDDC4FFFF),
    AnsiColor::from_u32(0xA6EAD6FF),
    AnsiColor::from_u32(0xFAFBF0FF),
);

// Dracula: the ANSI palette of the Dracula specification (draculatheme.com/spec).
const DRACULA_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x21222CFF),
    AnsiColor::from_u32(0xFF5555FF),
    AnsiColor::from_u32(0x50FA7BFF),
    AnsiColor::from_u32(0xF1FA8CFF),
    AnsiColor::from_u32(0xBD93F9FF),
    AnsiColor::from_u32(0xFF79C6FF),
    AnsiColor::from_u32(0x8BE9FDFF),
    AnsiColor::from_u32(0xF8F8F2FF),
);
const DRACULA_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6272A4FF),
    AnsiColor::from_u32(0xFF6E6EFF),
    AnsiColor::from_u32(0x69FF94FF),
    AnsiColor::from_u32(0xFFFFA5FF),
    AnsiColor::from_u32(0xD6ACFFFF),
    AnsiColor::from_u32(0xFF92DFFF),
    AnsiColor::from_u32(0xA4FFFFFF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Solarized (Ethan Schoonover): one ANSI set for both variants, mapped like the official
// terminal ports: base02, the accents and base2, then base03, orange, base01, base00, base0,
// violet, base1 and base3.
const SOLARIZED_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x073642FF),
    AnsiColor::from_u32(0xDC322FFF),
    AnsiColor::from_u32(0x859900FF),
    AnsiColor::from_u32(0xB58900FF),
    AnsiColor::from_u32(0x268BD2FF),
    AnsiColor::from_u32(0xD33682FF),
    AnsiColor::from_u32(0x2AA198FF),
    AnsiColor::from_u32(0xEEE8D5FF),
);
const SOLARIZED_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x002B36FF),
    AnsiColor::from_u32(0xCB4B16FF),
    AnsiColor::from_u32(0x586E75FF),
    AnsiColor::from_u32(0x657B83FF),
    AnsiColor::from_u32(0x839496FF),
    AnsiColor::from_u32(0x6C71C4FF),
    AnsiColor::from_u32(0x93A1A1FF),
    AnsiColor::from_u32(0xFDF6E3FF),
);

// Gruvbox (morhetz) dark: the neutral accents and fg4, then gray and the bright accents and
// fg1, as gruvbox.vim sets its terminal colours. One change: gruvbox.vim's black is bg0, the
// background itself, so black text vanished; black is bg1 here, one step up.
const GRUVBOX_DARK_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x3C3836FF),
    AnsiColor::from_u32(0xCC241DFF),
    AnsiColor::from_u32(0x98971AFF),
    AnsiColor::from_u32(0xD79921FF),
    AnsiColor::from_u32(0x458588FF),
    AnsiColor::from_u32(0xB16286FF),
    AnsiColor::from_u32(0x689D6AFF),
    AnsiColor::from_u32(0xA89984FF),
);
const GRUVBOX_DARK_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x928374FF),
    AnsiColor::from_u32(0xFB4934FF),
    AnsiColor::from_u32(0xB8BB26FF),
    AnsiColor::from_u32(0xFABD2FFF),
    AnsiColor::from_u32(0x83A598FF),
    AnsiColor::from_u32(0xD3869BFF),
    AnsiColor::from_u32(0x8EC07CFF),
    AnsiColor::from_u32(0xEBDBB2FF),
);

// Gruvbox light: the neutral and faded accents as gruvbox.vim sets them, but with black and
// white un-inverted. gruvbox.vim's light black is bg0 (the background itself, so black text
// vanished) and its whites are the dark fg4 and fg1; here black is fg1, white bg1 and bright
// white bg0_h, the way Solarized Light maps its neutrals.
const GRUVBOX_LIGHT_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x3C3836FF),
    AnsiColor::from_u32(0xCC241DFF),
    AnsiColor::from_u32(0x98971AFF),
    AnsiColor::from_u32(0xD79921FF),
    AnsiColor::from_u32(0x458588FF),
    AnsiColor::from_u32(0xB16286FF),
    AnsiColor::from_u32(0x689D6AFF),
    AnsiColor::from_u32(0xEBDBB2FF),
);
const GRUVBOX_LIGHT_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x928374FF),
    AnsiColor::from_u32(0x9D0006FF),
    AnsiColor::from_u32(0x79740EFF),
    AnsiColor::from_u32(0xB57614FF),
    AnsiColor::from_u32(0x076678FF),
    AnsiColor::from_u32(0x8F3F71FF),
    AnsiColor::from_u32(0x427B58FF),
    AnsiColor::from_u32(0xF9F5D7FF),
);

// Tokyo Night, night variant (folke/tokyonight.nvim terminal extras).
const TOKYO_NIGHT_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x15161EFF),
    AnsiColor::from_u32(0xF7768EFF),
    AnsiColor::from_u32(0x9ECE6AFF),
    AnsiColor::from_u32(0xE0AF68FF),
    AnsiColor::from_u32(0x7AA2F7FF),
    AnsiColor::from_u32(0xBB9AF7FF),
    AnsiColor::from_u32(0x7DCFFFFF),
    AnsiColor::from_u32(0xA9B1D6FF),
);
const TOKYO_NIGHT_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x414868FF),
    AnsiColor::from_u32(0xFF899DFF),
    AnsiColor::from_u32(0x9FE044FF),
    AnsiColor::from_u32(0xFABA4AFF),
    AnsiColor::from_u32(0x8DB0FFFF),
    AnsiColor::from_u32(0xC7A9FFFF),
    AnsiColor::from_u32(0xA4DAFFFF),
    AnsiColor::from_u32(0xC0CAF5FF),
);

// Catppuccin Mocha (catppuccin terminal ports).
const CATPPUCCIN_MOCHA_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x45475AFF),
    AnsiColor::from_u32(0xF38BA8FF),
    AnsiColor::from_u32(0xA6E3A1FF),
    AnsiColor::from_u32(0xF9E2AFFF),
    AnsiColor::from_u32(0x89B4FAFF),
    AnsiColor::from_u32(0xF5C2E7FF),
    AnsiColor::from_u32(0x94E2D5FF),
    AnsiColor::from_u32(0xBAC2DEFF),
);
const CATPPUCCIN_MOCHA_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x585B70FF),
    AnsiColor::from_u32(0xF38BA8FF),
    AnsiColor::from_u32(0xA6E3A1FF),
    AnsiColor::from_u32(0xF9E2AFFF),
    AnsiColor::from_u32(0x89B4FAFF),
    AnsiColor::from_u32(0xF5C2E7FF),
    AnsiColor::from_u32(0x94E2D5FF),
    AnsiColor::from_u32(0xA6ADC8FF),
);

// Catppuccin Latte (catppuccin terminal ports).
const CATPPUCCIN_LATTE_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x5C5F77FF),
    AnsiColor::from_u32(0xD20F39FF),
    AnsiColor::from_u32(0x40A02BFF),
    AnsiColor::from_u32(0xDF8E1DFF),
    AnsiColor::from_u32(0x1E66F5FF),
    AnsiColor::from_u32(0xEA76CBFF),
    AnsiColor::from_u32(0x179299FF),
    AnsiColor::from_u32(0xACB0BEFF),
);
const CATPPUCCIN_LATTE_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6C6F85FF),
    AnsiColor::from_u32(0xD20F39FF),
    AnsiColor::from_u32(0x40A02BFF),
    AnsiColor::from_u32(0xDF8E1DFF),
    AnsiColor::from_u32(0x1E66F5FF),
    AnsiColor::from_u32(0xEA76CBFF),
    AnsiColor::from_u32(0x179299FF),
    AnsiColor::from_u32(0xBCC0CCFF),
);

// Rosé Pine, main variant (rose-pine terminal ports).
const ROSE_PINE_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x26233AFF),
    AnsiColor::from_u32(0xEB6F92FF),
    AnsiColor::from_u32(0x31748FFF),
    AnsiColor::from_u32(0xF6C177FF),
    AnsiColor::from_u32(0x9CCFD8FF),
    AnsiColor::from_u32(0xC4A7E7FF),
    AnsiColor::from_u32(0xEBBCBAFF),
    AnsiColor::from_u32(0xE0DEF4FF),
);
const ROSE_PINE_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6E6A86FF),
    AnsiColor::from_u32(0xEB6F92FF),
    AnsiColor::from_u32(0x31748FFF),
    AnsiColor::from_u32(0xF6C177FF),
    AnsiColor::from_u32(0x9CCFD8FF),
    AnsiColor::from_u32(0xC4A7E7FF),
    AnsiColor::from_u32(0xEBBCBAFF),
    AnsiColor::from_u32(0xE0DEF4FF),
);

// Nord (nordtheme terminal ports).
const NORD_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x3B4252FF),
    AnsiColor::from_u32(0xBF616AFF),
    AnsiColor::from_u32(0xA3BE8CFF),
    AnsiColor::from_u32(0xEBCB8BFF),
    AnsiColor::from_u32(0x81A1C1FF),
    AnsiColor::from_u32(0xB48EADFF),
    AnsiColor::from_u32(0x88C0D0FF),
    AnsiColor::from_u32(0xE5E9F0FF),
);
const NORD_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x4C566AFF),
    AnsiColor::from_u32(0xBF616AFF),
    AnsiColor::from_u32(0xA3BE8CFF),
    AnsiColor::from_u32(0xEBCB8BFF),
    AnsiColor::from_u32(0x81A1C1FF),
    AnsiColor::from_u32(0xB48EADFF),
    AnsiColor::from_u32(0x8FBCBBFF),
    AnsiColor::from_u32(0xECEFF4FF),
);

// One Dark: Atom's one-dark-syntax palette, mapped like joshdick/onedark.vim's terminal colours
// with two changes. Black is the visual grey, not onedark.vim's #282C34, which is the
// background itself and made black text vanish. Bright black is the comment grey and bright
// white the default text, so bright white is never darker than white.
const ONE_DARK_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x3E4452FF),
    AnsiColor::from_u32(0xE06C75FF),
    AnsiColor::from_u32(0x98C379FF),
    AnsiColor::from_u32(0xE5C07BFF),
    AnsiColor::from_u32(0x61AFEFFF),
    AnsiColor::from_u32(0xC678DDFF),
    AnsiColor::from_u32(0x56B6C2FF),
    AnsiColor::from_u32(0xABB2BFFF),
);
const ONE_DARK_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x5C6370FF),
    AnsiColor::from_u32(0xBE5046FF),
    AnsiColor::from_u32(0x98C379FF),
    AnsiColor::from_u32(0xD19A66FF),
    AnsiColor::from_u32(0x61AFEFFF),
    AnsiColor::from_u32(0xC678DDFF),
    AnsiColor::from_u32(0x56B6C2FF),
    AnsiColor::from_u32(0xABB2BFFF),
);

// Kanagawa, wave variant (rebelot/kanagawa.nvim terminal extras).
const KANAGAWA_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x16161DFF),
    AnsiColor::from_u32(0xC34043FF),
    AnsiColor::from_u32(0x76946AFF),
    AnsiColor::from_u32(0xC0A36EFF),
    AnsiColor::from_u32(0x7E9CD8FF),
    AnsiColor::from_u32(0x957FB8FF),
    AnsiColor::from_u32(0x6A9589FF),
    AnsiColor::from_u32(0xC8C093FF),
);
const KANAGAWA_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x727169FF),
    AnsiColor::from_u32(0xE82424FF),
    AnsiColor::from_u32(0x98BB6CFF),
    AnsiColor::from_u32(0xE6C384FF),
    AnsiColor::from_u32(0x7FB4CAFF),
    AnsiColor::from_u32(0x938AA9FF),
    AnsiColor::from_u32(0x7AA89FFF),
    AnsiColor::from_u32(0xDCD7BAFF),
);

// Catppuccin Frappé (catppuccin terminal ports).
const CATPPUCCIN_FRAPPE_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x51576DFF),
    AnsiColor::from_u32(0xE78284FF),
    AnsiColor::from_u32(0xA6D189FF),
    AnsiColor::from_u32(0xE5C890FF),
    AnsiColor::from_u32(0x8CAAEEFF),
    AnsiColor::from_u32(0xF4B8E4FF),
    AnsiColor::from_u32(0x81C8BEFF),
    AnsiColor::from_u32(0xB5BFE2FF),
);
const CATPPUCCIN_FRAPPE_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x626880FF),
    AnsiColor::from_u32(0xE78284FF),
    AnsiColor::from_u32(0xA6D189FF),
    AnsiColor::from_u32(0xE5C890FF),
    AnsiColor::from_u32(0x8CAAEEFF),
    AnsiColor::from_u32(0xF4B8E4FF),
    AnsiColor::from_u32(0x81C8BEFF),
    AnsiColor::from_u32(0xA5ADCEFF),
);

// Catppuccin Macchiato (catppuccin terminal ports).
const CATPPUCCIN_MACCHIATO_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x494D64FF),
    AnsiColor::from_u32(0xED8796FF),
    AnsiColor::from_u32(0xA6DA95FF),
    AnsiColor::from_u32(0xEED49FFF),
    AnsiColor::from_u32(0x8AADF4FF),
    AnsiColor::from_u32(0xF5BDE6FF),
    AnsiColor::from_u32(0x8BD5CAFF),
    AnsiColor::from_u32(0xB8C0E0FF),
);
const CATPPUCCIN_MACCHIATO_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x5B6078FF),
    AnsiColor::from_u32(0xED8796FF),
    AnsiColor::from_u32(0xA6DA95FF),
    AnsiColor::from_u32(0xEED49FFF),
    AnsiColor::from_u32(0x8AADF4FF),
    AnsiColor::from_u32(0xF5BDE6FF),
    AnsiColor::from_u32(0x8BD5CAFF),
    AnsiColor::from_u32(0xA5ADCBFF),
);

// Everforest dark, medium contrast (sainnhe/everforest): its accents, with black bg3 and white
// fg as everforest.vim sets its terminal colours; the bright set repeats the normal one there.
const EVERFOREST_DARK_ANSI: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x475258FF),
    AnsiColor::from_u32(0xE67E80FF),
    AnsiColor::from_u32(0xA7C080FF),
    AnsiColor::from_u32(0xDBBC7FFF),
    AnsiColor::from_u32(0x7FBBB3FF),
    AnsiColor::from_u32(0xD699B6FF),
    AnsiColor::from_u32(0x83C092FF),
    AnsiColor::from_u32(0xD3C6AAFF),
);

// Everforest light, medium contrast: the light accents, with black fg and white bg3, as
// everforest.vim sets them; the bright set repeats the normal one.
const EVERFOREST_LIGHT_ANSI: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x5C6A72FF),
    AnsiColor::from_u32(0xF85552FF),
    AnsiColor::from_u32(0x8DA101FF),
    AnsiColor::from_u32(0xDFA000FF),
    AnsiColor::from_u32(0x3A94C5FF),
    AnsiColor::from_u32(0xDF69BAFF),
    AnsiColor::from_u32(0x35A77CFF),
    AnsiColor::from_u32(0xE6E2CCFF),
);

// GitHub Dark (primer/github-vscode-theme, Dark Default terminal colours).
const GITHUB_DARK_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x484F58FF),
    AnsiColor::from_u32(0xFF7B72FF),
    AnsiColor::from_u32(0x3FB950FF),
    AnsiColor::from_u32(0xD29922FF),
    AnsiColor::from_u32(0x58A6FFFF),
    AnsiColor::from_u32(0xBC8CFFFF),
    AnsiColor::from_u32(0x39C5CFFF),
    AnsiColor::from_u32(0xB1BAC4FF),
);
const GITHUB_DARK_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6E7681FF),
    AnsiColor::from_u32(0xFFA198FF),
    AnsiColor::from_u32(0x56D364FF),
    AnsiColor::from_u32(0xE3B341FF),
    AnsiColor::from_u32(0x79C0FFFF),
    AnsiColor::from_u32(0xD2A8FFFF),
    AnsiColor::from_u32(0x56D4DDFF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// GitHub Light (primer/github-vscode-theme, Light Default terminal colours).
const GITHUB_LIGHT_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x24292FFF),
    AnsiColor::from_u32(0xCF222EFF),
    AnsiColor::from_u32(0x116329FF),
    AnsiColor::from_u32(0x4D2D00FF),
    AnsiColor::from_u32(0x0969DAFF),
    AnsiColor::from_u32(0x8250DFFF),
    AnsiColor::from_u32(0x1B7C83FF),
    AnsiColor::from_u32(0x6E7781FF),
);
const GITHUB_LIGHT_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x57606AFF),
    AnsiColor::from_u32(0xA40E26FF),
    AnsiColor::from_u32(0x1A7F37FF),
    AnsiColor::from_u32(0x633C01FF),
    AnsiColor::from_u32(0x218BFFFF),
    AnsiColor::from_u32(0xA475F9FF),
    AnsiColor::from_u32(0x3192AAFF),
    AnsiColor::from_u32(0x8C959FFF),
);

// Night Owl (sdras/night-owl-vscode-theme terminal colours), with one change: the theme's black
// is #011627, the background itself, so black text vanished; black is the theme's selection
// blue #1D3B53 here, one step up.
const NIGHT_OWL_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x1D3B53FF),
    AnsiColor::from_u32(0xEF5350FF),
    AnsiColor::from_u32(0x22DA6EFF),
    AnsiColor::from_u32(0xC5E478FF),
    AnsiColor::from_u32(0x82AAFFFF),
    AnsiColor::from_u32(0xC792EAFF),
    AnsiColor::from_u32(0x21C7A8FF),
    AnsiColor::from_u32(0xFFFFFFFF),
);
const NIGHT_OWL_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x575656FF),
    AnsiColor::from_u32(0xEF5350FF),
    AnsiColor::from_u32(0x22DA6EFF),
    AnsiColor::from_u32(0xFFEB95FF),
    AnsiColor::from_u32(0x82AAFFFF),
    AnsiColor::from_u32(0xC792EAFF),
    AnsiColor::from_u32(0x7FDBCAFF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Rosé Pine Moon (rose-pine terminal ports).
const ROSE_PINE_MOON_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x393552FF),
    AnsiColor::from_u32(0xEB6F92FF),
    AnsiColor::from_u32(0x3E8FB0FF),
    AnsiColor::from_u32(0xF6C177FF),
    AnsiColor::from_u32(0x9CCFD8FF),
    AnsiColor::from_u32(0xC4A7E7FF),
    AnsiColor::from_u32(0xEA9A97FF),
    AnsiColor::from_u32(0xE0DEF4FF),
);
const ROSE_PINE_MOON_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6E6A86FF),
    AnsiColor::from_u32(0xEB6F92FF),
    AnsiColor::from_u32(0x3E8FB0FF),
    AnsiColor::from_u32(0xF6C177FF),
    AnsiColor::from_u32(0x9CCFD8FF),
    AnsiColor::from_u32(0xC4A7E7FF),
    AnsiColor::from_u32(0xEA9A97FF),
    AnsiColor::from_u32(0xE0DEF4FF),
);

// Rosé Pine Dawn (rose-pine terminal ports).
const ROSE_PINE_DAWN_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0xF2E9E1FF),
    AnsiColor::from_u32(0xB4637AFF),
    AnsiColor::from_u32(0x286983FF),
    AnsiColor::from_u32(0xEA9D34FF),
    AnsiColor::from_u32(0x56949FFF),
    AnsiColor::from_u32(0x907AA9FF),
    AnsiColor::from_u32(0xD7827EFF),
    AnsiColor::from_u32(0x575279FF),
);
const ROSE_PINE_DAWN_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x9893A5FF),
    AnsiColor::from_u32(0xB4637AFF),
    AnsiColor::from_u32(0x286983FF),
    AnsiColor::from_u32(0xEA9D34FF),
    AnsiColor::from_u32(0x56949FFF),
    AnsiColor::from_u32(0x907AA9FF),
    AnsiColor::from_u32(0xD7827EFF),
    AnsiColor::from_u32(0x575279FF),
);

// Tokyo Night, day variant (folke/tokyonight.nvim terminal extras).
const TOKYO_NIGHT_DAY_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0xB4B5B9FF),
    AnsiColor::from_u32(0xF52A65FF),
    AnsiColor::from_u32(0x587539FF),
    AnsiColor::from_u32(0x8C6C3EFF),
    AnsiColor::from_u32(0x2E7DE9FF),
    AnsiColor::from_u32(0x9854F1FF),
    AnsiColor::from_u32(0x007197FF),
    AnsiColor::from_u32(0x6172B0FF),
);
const TOKYO_NIGHT_DAY_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0xA1A6C5FF),
    AnsiColor::from_u32(0xFF4774FF),
    AnsiColor::from_u32(0x5C8524FF),
    AnsiColor::from_u32(0xA27629FF),
    AnsiColor::from_u32(0x358AFFFF),
    AnsiColor::from_u32(0xA463FFFF),
    AnsiColor::from_u32(0x007EA8FF),
    AnsiColor::from_u32(0x3760BFFF),
);

// The image themes: a background image drawn under the theme's ground colour (`image_theme`). Their ANSI
// colours start from the Mnemos Night (dark) or Mnemos Day (light) tables, are mixed 15% toward
// the theme's accent and are then pushed toward the foreground until each chromatic colour is at
// least 4.6:1 on the ground colour. The normal chromatic colours are then nudged (or the image
// regraded) until they also keep 4.5:1 over the composited image where text sits, checked by
// `image_themes_are_embedded_and_readable_over_their_image` in the app crate. Black and white come
// from the theme's own ground and text.

// Stone: Mnemos Day's ANSI colours tinted 15% toward the rust accent, darkened where needed to
// keep 4.5:1 on the pale sandstone ground. Black is the ink colour, white a step below the ground.
const STONE_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x221E1AFF),
    AnsiColor::from_u32(0xA1262DFF),
    AnsiColor::from_u32(0x385F1CFF),
    AnsiColor::from_u32(0x784B03FF),
    AnsiColor::from_u32(0x32479DFF),
    AnsiColor::from_u32(0x872E8FFF),
    AnsiColor::from_u32(0x135F5DFF),
    AnsiColor::from_u32(0xD2CDC3FF),
);
const STONE_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x706B65FF),
    AnsiColor::from_u32(0xB71E50FF),
    AnsiColor::from_u32(0x45710AFF),
    AnsiColor::from_u32(0x7A6303FF),
    AnsiColor::from_u32(0x513FB5FF),
    AnsiColor::from_u32(0xA82A86FF),
    AnsiColor::from_u32(0x127157FF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Slate: Mnemos Night's ANSI colours tinted 15% toward the ice-blue accent. Black and white are
// the ground and text greys.
const SLATE_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x373C41FF),
    AnsiColor::from_u32(0xEC8696FF),
    AnsiColor::from_u32(0x8ED87DFF),
    AnsiColor::from_u32(0xE4CA7DFF),
    AnsiColor::from_u32(0x84ADFBFF),
    AnsiColor::from_u32(0xD194FBFF),
    AnsiColor::from_u32(0x68D6D3FF),
    AnsiColor::from_u32(0xD0D6DDFF),
);
const SLATE_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6B7076FF),
    AnsiColor::from_u32(0xEEA8B5FF),
    AnsiColor::from_u32(0xBDEC6FFF),
    AnsiColor::from_u32(0xEEE1ADFF),
    AnsiColor::from_u32(0xADCAFBFF),
    AnsiColor::from_u32(0xDFBEFBFF),
    AnsiColor::from_u32(0x9DE9E8FF),
    AnsiColor::from_u32(0xF1F4F7FF),
);

// Mountain Night: Mnemos Night's ANSI colours tinted 15% toward the warm horizon accent.
const MOUNTAIN_NIGHT_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x343841FF),
    AnsiColor::from_u32(0xFB7A7DFF),
    AnsiColor::from_u32(0x9BD66DFF),
    AnsiColor::from_u32(0xF0C86EFF),
    AnsiColor::from_u32(0x90ABECFF),
    AnsiColor::from_u32(0xDE93ECFF),
    AnsiColor::from_u32(0x75D4C4FF),
    AnsiColor::from_u32(0xCDD2DBFF),
);
const MOUNTAIN_NIGHT_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x686C75FF),
    AnsiColor::from_u32(0xFBA6A5FF),
    AnsiColor::from_u32(0xCAEA60FF),
    AnsiColor::from_u32(0xFBDF9EFF),
    AnsiColor::from_u32(0xBAC8ECFF),
    AnsiColor::from_u32(0xECBCECFF),
    AnsiColor::from_u32(0xAAE8D8FF),
    AnsiColor::from_u32(0xF0F2F6FF),
);

// Mountain Dawn: Mnemos Day's ANSI colours tinted 15% toward the plum accent, darkened where
// needed to keep 4.5:1 on the pale rose ground.
const MOUNTAIN_DAWN_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x262A36FF),
    AnsiColor::from_u32(0xA92535FF),
    AnsiColor::from_u32(0x3B6124FF),
    AnsiColor::from_u32(0x7C4A0BFF),
    AnsiColor::from_u32(0x3445A5FF),
    AnsiColor::from_u32(0x8A2C98FF),
    AnsiColor::from_u32(0x155F68FF),
    AnsiColor::from_u32(0xD7D1CFFF),
);
const MOUNTAIN_DAWN_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x6B6C73FF),
    AnsiColor::from_u32(0xBA1B58FF),
    AnsiColor::from_u32(0x486F12FF),
    AnsiColor::from_u32(0x7C610BFF),
    AnsiColor::from_u32(0x543DBCFF),
    AnsiColor::from_u32(0xAA288EFF),
    AnsiColor::from_u32(0x156F5FFF),
    AnsiColor::from_u32(0xFFFFFFFF),
);

// Fjord: Mnemos Night's ANSI colours tinted 15% toward the mint accent.
const FJORD_NORMAL: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x343D3EFF),
    AnsiColor::from_u32(0xEE878DFF),
    AnsiColor::from_u32(0x8FDB76FF),
    AnsiColor::from_u32(0xE5CD77FF),
    AnsiColor::from_u32(0x85B0F4FF),
    AnsiColor::from_u32(0xD297F4FF),
    AnsiColor::from_u32(0x6AD9CCFF),
    AnsiColor::from_u32(0xD0DAD7FF),
);
const FJORD_BRIGHT: AnsiColors = AnsiColors::new(
    AnsiColor::from_u32(0x697272FF),
    AnsiColor::from_u32(0xEFABAEFF),
    AnsiColor::from_u32(0xBEEF68FF),
    AnsiColor::from_u32(0xEFE4A6FF),
    AnsiColor::from_u32(0xAECDF4FF),
    AnsiColor::from_u32(0xE0C1F4FF),
    AnsiColor::from_u32(0x9EECE1FF),
    AnsiColor::from_u32(0xF1F5F4FF),
);

/// Terminal colours of Mnemos Night. Also used for themes generated from a
/// dark background image.
pub fn mnemos_night_colors() -> TerminalColors {
    TerminalColors::new(MNEMOS_NIGHT_NORMAL, MNEMOS_NIGHT_BRIGHT)
}

/// Terminal colours of Mnemos Day. Also used for themes generated from a
/// light background image.
pub fn mnemos_day_colors() -> TerminalColors {
    TerminalColors::new(MNEMOS_DAY_NORMAL, MNEMOS_DAY_BRIGHT)
}

fn solid_theme(
    name: &str,
    background: u32,
    foreground: u32,
    accent: u32,
    details: Details,
    terminal_colors: TerminalColors,
) -> MnemosTheme {
    MnemosTheme::new(
        Fill::Solid(ColorU::from_u32(background)),
        ColorU::from_u32(foreground),
        Fill::Solid(ColorU::from_u32(accent)),
        None,
        Some(details),
        terminal_colors,
        None,
        Some(name.to_string()),
    )
}

/// A theme with a bundled background image. The image is drawn first and the ground colour over
/// it at `100 - image_opacity` percent, so `image_opacity` is how much of the image shows.
#[allow(clippy::too_many_arguments)]
fn image_theme(
    name: &str,
    background: u32,
    foreground: u32,
    accent: u32,
    details: Details,
    terminal_colors: TerminalColors,
    image: &'static str,
    image_opacity: Opacity,
) -> MnemosTheme {
    MnemosTheme::new(
        Fill::Solid(ColorU::from_u32(background)),
        ColorU::from_u32(foreground),
        Fill::Solid(ColorU::from_u32(accent)),
        None,
        Some(details),
        terminal_colors,
        Some(Image {
            source: AssetSource::Bundled { path: image },
            opacity: image_opacity,
        }),
        Some(name.to_string()),
    )
}

/// The default theme.
pub fn mnemos_night() -> MnemosTheme {
    solid_theme(
        MNEMOS_NIGHT_NAME,
        0x1B1033FF,
        0xF4F1EAFF,
        0xC6F35AFF,
        Details::Darker,
        mnemos_night_colors(),
    )
}

/// The default light theme.
pub fn mnemos_day() -> MnemosTheme {
    solid_theme(
        MNEMOS_DAY_NAME,
        0xF4F1EAFF,
        0x1B1033FF,
        0x4A6400FF,
        Details::Lighter,
        mnemos_day_colors(),
    )
}

/// High contrast dark theme.
pub fn mnemos_contrast() -> MnemosTheme {
    solid_theme(
        MNEMOS_CONTRAST_NAME,
        0x09050FFF,
        0xFFFFFFFF,
        0xD4FF5CFF,
        Details::Darker,
        TerminalColors::new(MNEMOS_CONTRAST_NORMAL, MNEMOS_CONTRAST_BRIGHT),
    )
}

/// Dark moss theme with a lavender accent.
pub fn mnemos_moss() -> MnemosTheme {
    solid_theme(
        MNEMOS_MOSS_NAME,
        0x141A10FF,
        0xEEF0E2FF,
        0xB9A2FFFF,
        Details::Darker,
        TerminalColors::new(MNEMOS_MOSS_NORMAL, MNEMOS_MOSS_BRIGHT),
    )
}

/// Stone: warm pale sandstone with hazy ink-wash ridges in the lower right, rust accent.
pub fn stone() -> MnemosTheme {
    image_theme(
        STONE_NAME,
        0xEFE9DFFF,
        0x221E1AFF,
        0x7A3E14FF,
        Details::Lighter,
        TerminalColors::new(STONE_NORMAL, STONE_BRIGHT),
        STONE_IMAGE,
        70,
    )
}

/// Slate: layered dusk ridges over a lava field, graded to cool slate, ice-blue accent.
pub fn slate() -> MnemosTheme {
    image_theme(
        SLATE_NAME,
        0x0E1217FF,
        0xDCE3EAFF,
        0x8CC4E6FF,
        Details::Darker,
        TerminalColors::new(SLATE_NORMAL, SLATE_BRIGHT),
        SLATE_IMAGE,
        80,
    )
}

/// Mountain Night: the Milky Way over the Teton Range, warm horizon accent.
pub fn mountain_night() -> MnemosTheme {
    image_theme(
        MOUNTAIN_NIGHT_NAME,
        0x0B0F17FF,
        0xD9DEE8FF,
        0xE2B97FFF,
        Details::Darker,
        TerminalColors::new(MOUNTAIN_NIGHT_NORMAL, MOUNTAIN_NIGHT_BRIGHT),
        MOUNTAIN_NIGHT_IMAGE,
        45,
    )
}

/// Mountain Dawn: alpenglow on Denali, washed to pale rose behind the text, plum accent.
pub fn mountain_dawn() -> MnemosTheme {
    // The plum accent is dark enough that the default 35% selection costs the text too much
    // contrast; 30% keeps the selection visible and the text above 7:1 on it.
    image_theme(
        MOUNTAIN_DAWN_NAME,
        0xF4ECE8FF,
        0x262A36FF,
        0x8A2F48FF,
        Details::Lighter,
        TerminalColors::new(MOUNTAIN_DAWN_NORMAL, MOUNTAIN_DAWN_BRIGHT),
        MOUNTAIN_DAWN_IMAGE,
        62,
    )
    .with_selection_opacities(Some(30), None)
}

/// Fjord: the cliffs of Geirangerfjord over dark water, mint accent.
pub fn fjord() -> MnemosTheme {
    image_theme(
        FJORD_NAME,
        0x0A1315FF,
        0xDDE7E3FF,
        0x94D8B8FF,
        Details::Darker,
        TerminalColors::new(FJORD_NORMAL, FJORD_BRIGHT),
        FJORD_IMAGE,
        40,
    )
}

/// Dracula.
pub fn dracula() -> MnemosTheme {
    solid_theme(
        DRACULA_NAME,
        0x282A36FF,
        0xF8F8F2FF,
        0xBD93F9FF,
        Details::Darker,
        TerminalColors::new(DRACULA_NORMAL, DRACULA_BRIGHT),
    )
}

/// Solarized Dark: base03 ground, base0 text, blue accent.
pub fn solarized_dark() -> MnemosTheme {
    // Solarized keeps its text deliberately soft (base0 on base03 is 4.8:1), so a selection
    // 1.5:1 off the ground leaves little room for the text on top. 32% blue keeps it visible;
    // the context highlight drops to 30% yellow so the text on it stays at about 3:1.
    solid_theme(
        SOLARIZED_DARK_NAME,
        0x002B36FF,
        0x839496FF,
        0x268BD2FF,
        Details::Darker,
        TerminalColors::new(SOLARIZED_NORMAL, SOLARIZED_BRIGHT),
    )
    .with_selection_opacities(Some(32), Some(30))
}

/// Solarized Light: base3 ground, base00 text, blue accent.
pub fn solarized_light() -> MnemosTheme {
    // base00 on base3 is 4.1:1 by design. The default 35% selection is just at the 1.5:1
    // visibility floor, so it goes to 37%.
    solid_theme(
        SOLARIZED_LIGHT_NAME,
        0xFDF6E3FF,
        0x657B83FF,
        0x268BD2FF,
        Details::Lighter,
        TerminalColors::new(SOLARIZED_NORMAL, SOLARIZED_BRIGHT),
    )
    .with_selection_opacities(Some(37), None)
}

/// Gruvbox Dark: bg0 ground, fg1 text, bright yellow accent.
pub fn gruvbox_dark() -> MnemosTheme {
    // Bright yellow is light enough that 25% of it costs the text too much contrast; 19% keeps
    // the selection visible and fg1 near 7:1 on it.
    solid_theme(
        GRUVBOX_DARK_NAME,
        0x282828FF,
        0xEBDBB2FF,
        0xFABD2FFF,
        Details::Darker,
        TerminalColors::new(GRUVBOX_DARK_NORMAL, GRUVBOX_DARK_BRIGHT),
    )
    .with_selection_opacities(Some(19), None)
}

/// Gruvbox Light: bg0 ground, fg1 text, faded yellow accent.
pub fn gruvbox_light() -> MnemosTheme {
    // Faded yellow is close to the cream ground in luminance, so the selection needs 40% to
    // stand 1.5:1 off it.
    solid_theme(
        GRUVBOX_LIGHT_NAME,
        0xFBF1C7FF,
        0x3C3836FF,
        0xB57614FF,
        Details::Lighter,
        TerminalColors::new(GRUVBOX_LIGHT_NORMAL, GRUVBOX_LIGHT_BRIGHT),
    )
    .with_selection_opacities(Some(40), None)
}

/// Tokyo Night (night): blue accent.
pub fn tokyo_night() -> MnemosTheme {
    // 24% blue instead of 25%: one point less keeps the text on a selection near 7:1.
    solid_theme(
        TOKYO_NIGHT_NAME,
        0x1A1B26FF,
        0xC0CAF5FF,
        0x7AA2F7FF,
        Details::Darker,
        TerminalColors::new(TOKYO_NIGHT_NORMAL, TOKYO_NIGHT_BRIGHT),
    )
    .with_selection_opacities(Some(24), None)
}

/// Catppuccin Mocha: mauve accent.
pub fn catppuccin_mocha() -> MnemosTheme {
    // Mauve and yellow are both pale, so both highlights are thinner than the defaults: 21%
    // keeps text on a selection above 7:1 and 28% keeps text on a context selection above 4.5:1.
    solid_theme(
        CATPPUCCIN_MOCHA_NAME,
        0x1E1E2EFF,
        0xCDD6F4FF,
        0xCBA6F7FF,
        Details::Darker,
        TerminalColors::new(CATPPUCCIN_MOCHA_NORMAL, CATPPUCCIN_MOCHA_BRIGHT),
    )
    .with_selection_opacities(Some(21), Some(28))
}

/// Catppuccin Latte: mauve accent.
pub fn catppuccin_latte() -> MnemosTheme {
    // Mauve is dark on Latte's ground, so 35% of it costs the text too much; 30% is enough to
    // stay visible.
    solid_theme(
        CATPPUCCIN_LATTE_NAME,
        0xEFF1F5FF,
        0x4C4F69FF,
        0x8839EFFF,
        Details::Lighter,
        TerminalColors::new(CATPPUCCIN_LATTE_NORMAL, CATPPUCCIN_LATTE_BRIGHT),
    )
    .with_selection_opacities(Some(30), None)
}

/// Rosé Pine (main): iris accent, the colour Rosé Pine gives focus and links.
pub fn rose_pine() -> MnemosTheme {
    solid_theme(
        ROSE_PINE_NAME,
        0x191724FF,
        0xE0DEF4FF,
        0xC4A7E7FF,
        Details::Darker,
        TerminalColors::new(ROSE_PINE_NORMAL, ROSE_PINE_BRIGHT),
    )
}

/// Nord: nord0 ground, nord4 text, nord8 (the primary accent) as the accent.
pub fn nord() -> MnemosTheme {
    // Nord's frost and yellow are pale against a ground that is not very dark, so both
    // highlights are thinner: 22% for the selection and 26% for the context selection.
    solid_theme(
        NORD_NAME,
        0x2E3440FF,
        0xD8DEE9FF,
        0x88C0D0FF,
        Details::Darker,
        TerminalColors::new(NORD_NORMAL, NORD_BRIGHT),
    )
    .with_selection_opacities(Some(22), Some(26))
}

/// One Dark: Atom's syntax background and text, with its accent (the cursor colour).
pub fn one_dark() -> MnemosTheme {
    // The default text is 6.6:1 on the ground. 30% accent keeps a selection visible, and the
    // light yellow context highlight drops to 16% so the text on it stays above 4.5:1.
    solid_theme(
        ONE_DARK_NAME,
        0x282C34FF,
        0xABB2BFFF,
        0x528BFFFF,
        Details::Darker,
        TerminalColors::new(ONE_DARK_NORMAL, ONE_DARK_BRIGHT),
    )
    .with_selection_opacities(Some(30), Some(16))
}

/// Kanagawa (wave): sumiInk3 ground, fujiWhite text, crystalBlue accent.
pub fn kanagawa() -> MnemosTheme {
    solid_theme(
        KANAGAWA_NAME,
        0x1F1F28FF,
        0xDCD7BAFF,
        0x7E9CD8FF,
        Details::Darker,
        TerminalColors::new(KANAGAWA_NORMAL, KANAGAWA_BRIGHT),
    )
}

/// Catppuccin Frappé: mauve accent.
pub fn catppuccin_frappe() -> MnemosTheme {
    // Like Mocha, both highlights are thinner than the defaults on Frappé's lighter ground: 23%
    // mauve keeps text on a selection above 5:1 and 25% yellow keeps text on a context
    // selection above 4.5:1.
    solid_theme(
        CATPPUCCIN_FRAPPE_NAME,
        0x303446FF,
        0xC6D0F5FF,
        0xCA9EE6FF,
        Details::Darker,
        TerminalColors::new(CATPPUCCIN_FRAPPE_NORMAL, CATPPUCCIN_FRAPPE_BRIGHT),
    )
    .with_selection_opacities(Some(23), Some(25))
}

/// Catppuccin Macchiato: mauve accent.
pub fn catppuccin_macchiato() -> MnemosTheme {
    // Mauve and yellow are pale, so 22% keeps text on a selection above 6:1 and 28% keeps text on
    // a context selection above 4.5:1.
    solid_theme(
        CATPPUCCIN_MACCHIATO_NAME,
        0x24273AFF,
        0xCAD3F5FF,
        0xC6A0F6FF,
        Details::Darker,
        TerminalColors::new(CATPPUCCIN_MACCHIATO_NORMAL, CATPPUCCIN_MACCHIATO_BRIGHT),
    )
    .with_selection_opacities(Some(22), Some(28))
}

/// Everforest Dark (medium): bg0 ground, fg text, green accent.
pub fn everforest_dark() -> MnemosTheme {
    // fg on bg0 is 7.4:1, so the pale green and yellow highlights both drop to 22%.
    solid_theme(
        EVERFOREST_DARK_NAME,
        0x2D353BFF,
        0xD3C6AAFF,
        0xA7C080FF,
        Details::Darker,
        TerminalColors::new(EVERFOREST_DARK_ANSI, EVERFOREST_DARK_ANSI),
    )
    .with_selection_opacities(Some(22), Some(22))
}

/// Everforest Light (medium): bg0 ground, fg text, blue accent.
pub fn everforest_light() -> MnemosTheme {
    // Everforest's green highlight is 2.7:1 on the light bg0, below the 3:1 floor for an accent,
    // so the accent is the palette's blue. fg on bg0 is 5.2:1 by design; 41% blue is what it
    // takes for the selection to stand 1.5:1 off the ground.
    solid_theme(
        EVERFOREST_LIGHT_NAME,
        0xFDF6E3FF,
        0x5C6A72FF,
        0x3A94C5FF,
        Details::Lighter,
        TerminalColors::new(EVERFOREST_LIGHT_ANSI, EVERFOREST_LIGHT_ANSI),
    )
    .with_selection_opacities(Some(41), None)
}

/// GitHub Dark: Primer's dark canvas and text, accent blue.
pub fn github_dark() -> MnemosTheme {
    // The ground is near black, so the default 25% blue is only just 1.5:1 off it; 30% is
    // clearly visible and the text stays far above 7:1 on it.
    solid_theme(
        GITHUB_DARK_NAME,
        0x0D1117FF,
        0xE6EDF3FF,
        0x58A6FFFF,
        Details::Darker,
        TerminalColors::new(GITHUB_DARK_NORMAL, GITHUB_DARK_BRIGHT),
    )
    .with_selection_opacities(Some(30), None)
}

/// GitHub Light: Primer's light canvas and text, accent blue.
pub fn github_light() -> MnemosTheme {
    solid_theme(
        GITHUB_LIGHT_NAME,
        0xFFFFFFFF,
        0x1F2328FF,
        0x0969DAFF,
        Details::Lighter,
        TerminalColors::new(GITHUB_LIGHT_NORMAL, GITHUB_LIGHT_BRIGHT),
    )
}

/// Night Owl: blue accent, the colour Night Owl gives keywords and links.
pub fn night_owl() -> MnemosTheme {
    solid_theme(
        NIGHT_OWL_NAME,
        0x011627FF,
        0xD6DEEBFF,
        0x82AAFFFF,
        Details::Darker,
        TerminalColors::new(NIGHT_OWL_NORMAL, NIGHT_OWL_BRIGHT),
    )
}

/// Rosé Pine Dawn: iris accent, as in the main variant.
pub fn rose_pine_dawn() -> MnemosTheme {
    // text on base is 6.7:1 by design; 40% iris is what it takes for the selection to stand
    // 1.5:1 off the ground.
    solid_theme(
        ROSE_PINE_DAWN_NAME,
        0xFAF4EDFF,
        0x575279FF,
        0x907AA9FF,
        Details::Lighter,
        TerminalColors::new(ROSE_PINE_DAWN_NORMAL, ROSE_PINE_DAWN_BRIGHT),
    )
    .with_selection_opacities(Some(40), None)
}

/// Rosé Pine Moon: iris accent, as in the main variant.
pub fn rose_pine_moon() -> MnemosTheme {
    // 23% iris instead of 25%: the default sits right at the 7:1 floor for the text on it.
    solid_theme(
        ROSE_PINE_MOON_NAME,
        0x232136FF,
        0xE0DEF4FF,
        0xC4A7E7FF,
        Details::Darker,
        TerminalColors::new(ROSE_PINE_MOON_NORMAL, ROSE_PINE_MOON_BRIGHT),
    )
    .with_selection_opacities(Some(23), None)
}

/// Tokyo Night Day: blue accent.
pub fn tokyo_night_day() -> MnemosTheme {
    // The day variant's blue text on its grey ground is 4.5:1 by design, which leaves a narrow
    // band for a selection; 40% blue stands 1.5:1 off the ground and keeps the text near 3:1 on it.
    solid_theme(
        TOKYO_NIGHT_DAY_NAME,
        0xE1E2E7FF,
        0x3760BFFF,
        0x2E7DE9FF,
        Details::Lighter,
        TerminalColors::new(TOKYO_NIGHT_DAY_NORMAL, TOKYO_NIGHT_DAY_BRIGHT),
    )
    .with_selection_opacities(Some(40), None)
}

/// The four Mnemos themes, in picker order. These are the themes the onboarding slide offers.
pub fn mnemos() -> [MnemosTheme; 4] {
    [
        mnemos_night(),
        mnemos_day(),
        mnemos_contrast(),
        mnemos_moss(),
    ]
}

/// The image themes, in picker order.
pub fn image() -> [MnemosTheme; 5] {
    [stone(), slate(), mountain_night(), mountain_dawn(), fjord()]
}

/// The community themes, alphabetically by name.
pub fn community() -> [MnemosTheme; 22] {
    [
        catppuccin_frappe(),
        catppuccin_latte(),
        catppuccin_macchiato(),
        catppuccin_mocha(),
        dracula(),
        everforest_dark(),
        everforest_light(),
        github_dark(),
        github_light(),
        gruvbox_dark(),
        gruvbox_light(),
        kanagawa(),
        night_owl(),
        nord(),
        one_dark(),
        rose_pine(),
        rose_pine_dawn(),
        rose_pine_moon(),
        solarized_dark(),
        solarized_light(),
        tokyo_night(),
        tokyo_night_day(),
    ]
}

/// All built-in themes, in picker order: the Mnemos themes, the image themes, then the community
/// themes.
pub fn all() -> Vec<MnemosTheme> {
    mnemos()
        .into_iter()
        .chain(image())
        .chain(community())
        .collect()
}

#[cfg(test)]
#[path = "mnemos_themes_tests.rs"]
mod tests;
