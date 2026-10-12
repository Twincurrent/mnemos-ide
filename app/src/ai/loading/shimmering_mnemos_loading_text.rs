//! Shimmering Mnemos loading text for loading states.
//!
//! Mnemos IDE: upstream prefixed the text with its logo, a glyph patched into the bundled Roboto at
//! U+E500. That glyph was Warp's logo and has been removed from the fonts, so the text is shown on
//! its own.

use mnemos_core::ui::appearance::Appearance;
use mnemos_ui::elements::shimmering_text::{
    ShimmerConfig, ShimmeringTextElement, ShimmeringTextStateHandle,
};
use mnemos_ui::elements::Element;
use mnemos_ui::{AppContext, SingletonEntity};

/// Creates a shimmering text element.
pub fn shimmering_mnemos_loading_text(
    text: impl Into<String>,
    font_size: f32,
    shimmer_handle: ShimmeringTextStateHandle,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    // Use same colors as common.rs for consistency
    let base_color = theme.disabled_text_color(theme.surface_1()).into_solid();
    let shimmer_color = theme.main_text_color(theme.surface_1()).into_solid();

    // Hardcoded shimmer config for consistent animation
    let config = ShimmerConfig::default();

    ShimmeringTextElement::new(
        text.into(),
        appearance.ui_font_family(),
        font_size,
        base_color,
        shimmer_color,
        config,
        shimmer_handle,
    )
    .finish()
}
