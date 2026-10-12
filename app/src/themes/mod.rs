pub mod theme;
pub mod theme_chooser;
pub mod theme_creator;
pub mod theme_creator_body;
pub mod theme_creator_modal;
pub mod theme_deletion_body;
pub mod theme_deletion_modal;

use mnemos_core::ui::theme::{mnemos_themes, MnemosTheme};

/// The themes offered on the onboarding theme slide, in display order: the four Mnemos themes.
/// The image and community themes are in the theme chooser only.
pub fn onboarding_theme_picker_themes() -> [MnemosTheme; 4] {
    mnemos_themes::mnemos()
}
