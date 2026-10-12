use mnemos_ui::color::ColorU;

use crate::ui::color::blend::Blend;

use super::Fill;

// The fixed palette of the first-run onboarding overlays (callouts, progress dots, launch
// modals), which do not follow the user's theme. It is the app icon's: dark violet plate
// #1B1033, off-white #F4F1EA and lime #C6F35A. The name is upstream's.
const PHENOMENON_BACKGROUND: u32 = 0x1B1033FF;
const PHENOMENON_FOREGROUND: u32 = 0xF4F1EAFF;
const PHENOMENON_ACCENT: u32 = 0xC6F35AFF;
const PHENOMENON_HIGHLIGHT: u32 = 0xD4FF5CFF;
const PHENOMENON_SURFACE: u32 = 0x2A1D47FF;
const PHENOMENON_BODY_TEXT: u32 = 0xF4F1EAE5;
const PHENOMENON_LABEL_TEXT: u32 = 0xF4F1EA99;
const PHENOMENON_DISABLED_LABEL_TEXT: u32 = 0xF4F1EA80;
const PHENOMENON_SUBTLE_BORDER: u32 = 0xF4F1EA33;
const PHENOMENON_MODAL_BACKGROUND: u32 = 0x2A1D47FF;
const PHENOMENON_MODAL_BADGE_BACKGROUND: u32 = 0xC6F35A1A;
const PHENOMENON_MODAL_BADGE_TEXT: u32 = 0xC6F35AFF;
const PHENOMENON_MODAL_TITLE_TEXT: u32 = 0xF4F1EAFF;
const PHENOMENON_MODAL_FEATURE_TITLE_TEXT: u32 = 0xE9E4DAFF;
const PHENOMENON_MODAL_FEATURE_DESCRIPTION_TEXT: u32 = 0xB3AAC8FF;
const PHENOMENON_MODAL_BUTTON_BACKGROUND: u32 = 0xF4F1EAFF;
const PHENOMENON_MODAL_BUTTON_TEXT: u32 = 0x1B1033FF;
const PHENOMENON_MODAL_BUTTON_HOVER_OVERLAY: u32 = 0x1B10331F;
const PHENOMENON_MODAL_CLOSE_BUTTON_TEXT: u32 = 0xF4F1EAFF;
const PHENOMENON_MODAL_CLOSE_BUTTON_HOVER: u32 = 0x1B1033BF;

pub struct PhenomenonStyle;

impl PhenomenonStyle {
    pub fn background() -> ColorU {
        ColorU::from_u32(PHENOMENON_BACKGROUND)
    }

    pub fn foreground() -> ColorU {
        ColorU::from_u32(PHENOMENON_FOREGROUND)
    }

    pub fn accent() -> ColorU {
        ColorU::from_u32(PHENOMENON_ACCENT)
    }

    /// A brighter lime, for hover states and the active progress dot.
    pub fn highlight() -> ColorU {
        ColorU::from_u32(PHENOMENON_HIGHLIGHT)
    }

    pub fn body_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_BODY_TEXT)
    }

    pub fn label_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_LABEL_TEXT)
    }

    pub fn disabled_label_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_DISABLED_LABEL_TEXT)
    }

    pub fn subtle_border() -> ColorU {
        ColorU::from_u32(PHENOMENON_SUBTLE_BORDER)
    }

    pub fn modal_background() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_BACKGROUND)
    }

    pub fn modal_badge_background() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_BADGE_BACKGROUND)
    }

    pub fn modal_badge_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_BADGE_TEXT)
    }

    pub fn modal_title_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_TITLE_TEXT)
    }

    pub fn modal_feature_title_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_FEATURE_TITLE_TEXT)
    }

    pub fn modal_feature_description_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_FEATURE_DESCRIPTION_TEXT)
    }

    pub fn modal_button_background() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_BUTTON_BACKGROUND)
    }

    pub fn modal_button_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_BUTTON_TEXT)
    }

    pub fn modal_button_hover_overlay() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_BUTTON_HOVER_OVERLAY)
    }

    pub fn modal_close_button_text() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_CLOSE_BUTTON_TEXT)
    }

    pub fn modal_close_button_hover() -> ColorU {
        ColorU::from_u32(PHENOMENON_MODAL_CLOSE_BUTTON_HOVER)
    }

    /// Callout body: a lifted violet, so off-white text stays well above 4.5:1.
    pub fn tinted_surface() -> Fill {
        Fill::Solid(ColorU::from_u32(PHENOMENON_SURFACE))
    }

    pub fn surface_border() -> ColorU {
        Self::accent()
    }

    pub fn primary_button_background(hovered: bool) -> Fill {
        Fill::Solid(if hovered {
            Self::highlight()
        } else {
            Self::accent()
        })
    }

    /// Violet on the lime button.
    pub fn primary_button_text() -> ColorU {
        Self::background()
    }

    pub fn modal_button_background_fill(hovered: bool) -> Fill {
        if hovered {
            Fill::Solid(Self::modal_button_background())
                .blend(&Fill::Solid(Self::modal_button_hover_overlay()))
        } else {
            Fill::Solid(Self::modal_button_background())
        }
    }

    pub fn segmented_control_background() -> Fill {
        Fill::Solid(Self::foreground()).with_opacity(8)
    }

    pub fn selected_chip_background() -> Fill {
        Fill::Solid(Self::foreground())
    }

    pub fn selected_chip_text() -> ColorU {
        Self::background()
    }

    pub fn selected_chip_border() -> Fill {
        Fill::Solid(Self::accent())
    }

    pub fn unselected_chip_background() -> Fill {
        Fill::Solid(Self::foreground()).with_opacity(8)
    }

    pub fn unselected_chip_text() -> ColorU {
        Self::body_text()
    }
}
