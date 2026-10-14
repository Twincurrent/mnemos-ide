use super::*;
use crate::ui::color::contrast::relative_luminance;

/// WCAG 2.x contrast ratio, (L1 + 0.05) / (L2 + 0.05) with L1 the lighter colour.
fn contrast(a: ColorU, b: ColorU) -> f32 {
    let (la, lb) = (relative_luminance(a) + 0.05, relative_luminance(b) + 0.05);
    if la > lb {
        la / lb
    } else {
        lb / la
    }
}

fn chromatic(colors: &AnsiColors) -> [(&'static str, AnsiColor); 6] {
    [
        ("red", colors.red),
        ("green", colors.green),
        ("yellow", colors.yellow),
        ("blue", colors.blue),
        ("magenta", colors.magenta),
        ("cyan", colors.cyan),
    ]
}

#[test]
fn names_are_set_and_unique() {
    let names: Vec<String> = all().iter().map(|t| t.name().unwrap()).collect();
    assert_eq!(
        names,
        [
            MNEMOS_NIGHT_NAME,
            MNEMOS_DAY_NAME,
            MNEMOS_CONTRAST_NAME,
            MNEMOS_MOSS_NAME,
            STONE_NAME,
            SLATE_NAME,
            MOUNTAIN_NIGHT_NAME,
            MOUNTAIN_DAWN_NAME,
            FJORD_NAME,
            CATPPUCCIN_FRAPPE_NAME,
            CATPPUCCIN_LATTE_NAME,
            CATPPUCCIN_MACCHIATO_NAME,
            CATPPUCCIN_MOCHA_NAME,
            DRACULA_NAME,
            EVERFOREST_DARK_NAME,
            EVERFOREST_LIGHT_NAME,
            GITHUB_DARK_NAME,
            GITHUB_LIGHT_NAME,
            GRUVBOX_DARK_NAME,
            GRUVBOX_LIGHT_NAME,
            KANAGAWA_NAME,
            NIGHT_OWL_NAME,
            NORD_NAME,
            ONE_DARK_NAME,
            ROSE_PINE_NAME,
            ROSE_PINE_DAWN_NAME,
            ROSE_PINE_MOON_NAME,
            SOLARIZED_DARK_NAME,
            SOLARIZED_LIGHT_NAME,
            TOKYO_NIGHT_NAME,
            TOKYO_NIGHT_DAY_NAME,
        ]
    );
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
    assert!(names.iter().all(|n| !n.to_lowercase().contains("warp")));
}

#[test]
fn community_themes_are_alphabetical() {
    let names: Vec<String> = community().iter().map(|t| t.name().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort_by_key(|n| n.to_lowercase());
    assert_eq!(names, sorted);
}

/// The Mnemos themes and the image themes, which are held to the same bar. For an image theme the
/// bar is measured against its ground colour; the image under it is checked separately (the
/// composite contrast test in `app/src/themes/theme_test.rs`).
fn mnemos_bar() -> Vec<MnemosTheme> {
    mnemos().into_iter().chain(image()).collect()
}

#[test]
fn image_themes_have_bundled_images() {
    use mnemos_ui::assets::asset_cache::AssetSource;
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/assets");
    for theme in all() {
        let name = theme.name().unwrap();
        let is_image_theme = image().iter().any(|t| t.name() == theme.name());
        let Some(background) = theme.background_image() else {
            assert!(!is_image_theme, "{name}: image theme without an image");
            continue;
        };
        assert!(
            is_image_theme,
            "{name}: only the image themes have an image"
        );
        // Bundled, never fetched, and under `bundled/` so every build embeds it.
        let AssetSource::Bundled { path } = background.source else {
            panic!("{name}: the background image is not a bundled asset");
        };
        assert!(path.starts_with("bundled/jpg/themes/"), "{name}: {path}");
        let file = assets.join(path);
        let size = std::fs::metadata(&file)
            .unwrap_or_else(|err| panic!("{name}: {}: {err}", file.display()))
            .len();
        assert!(size <= 700 * 1024, "{name}: {path} is {size} bytes");
        assert!((1..=100).contains(&background.opacity), "{name}: opacity");
    }
}

#[test]
fn foreground_and_accent_contrast_with_background() {
    for theme in mnemos_bar() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        let fg = theme.foreground().into_solid();
        let accent = theme.accent().into_solid();
        let fg_ratio = contrast(fg, bg);
        let accent_ratio = contrast(accent, bg);
        assert!(fg_ratio >= 7.0, "{name}: fg/bg {fg_ratio:.2} < 7.0");
        assert!(
            accent_ratio >= 4.5,
            "{name}: accent/bg {accent_ratio:.2} < 4.5"
        );
    }
}

#[test]
fn ansi_colours_are_readable_on_background() {
    // Only the Mnemos and image themes: the community themes keep their projects' published ANSI
    // colours, several of which are below 4.5:1 by design (Solarized, Gruvbox, Catppuccin
    // Latte, Rosé Pine's pine).
    for theme in mnemos_bar() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        let colors = theme.terminal_colors();
        for (set, ansi) in [("normal", &colors.normal), ("bright", &colors.bright)] {
            for (color_name, color) in chromatic(ansi) {
                let ratio = contrast(color.into(), bg);
                assert!(
                    ratio >= 4.5,
                    "{name}: {set} {color_name} on bg is {ratio:.2} < 4.5"
                );
            }
        }
    }
}

#[test]
fn ansi_black_is_not_the_background() {
    // Some upstream ports set black to the background itself (gruvbox.vim, onedark.vim), which
    // makes black text invisible. Every theme here must keep black off the background.
    for theme in all() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        let black: ColorU = theme.terminal_colors().normal.black.into();
        assert_ne!(black, bg, "{name}: normal black is the background colour");
    }
}

#[test]
fn contrast_theme_is_high_contrast() {
    let theme = mnemos_contrast();
    let bg = theme.background().into_solid();
    assert!(contrast(theme.foreground().into_solid(), bg) >= 15.0);
    for (color_name, color) in chromatic(&theme.terminal_colors().normal) {
        let ratio = contrast(color.into(), bg);
        assert!(ratio >= 8.0, "normal {color_name} {ratio:.2} < 8.0");
    }
}

#[test]
fn light_themes_are_light_and_others_are_dark() {
    use crate::ui::theme::ColorScheme;
    let light = [
        MNEMOS_DAY_NAME,
        STONE_NAME,
        MOUNTAIN_DAWN_NAME,
        CATPPUCCIN_LATTE_NAME,
        EVERFOREST_LIGHT_NAME,
        GITHUB_LIGHT_NAME,
        GRUVBOX_LIGHT_NAME,
        ROSE_PINE_DAWN_NAME,
        SOLARIZED_LIGHT_NAME,
        TOKYO_NIGHT_DAY_NAME,
    ];
    for theme in all() {
        let name = theme.name().unwrap();
        let expected = if light.contains(&name.as_str()) {
            ColorScheme::DarkOnLight
        } else {
            ColorScheme::LightOnDark
        };
        assert_eq!(theme.inferred_color_scheme(), expected, "{name}");
        let details = if expected == ColorScheme::DarkOnLight {
            crate::ui::theme::color::CustomDetails::lighter_details()
        } else {
            crate::ui::theme::color::CustomDetails::darker_details()
        };
        assert_eq!(theme.details(), details, "{name}: details");
    }
}

/// `overlay` (which may be translucent) composited over the theme background.
fn over_background(theme: &MnemosTheme, overlay: ColorU) -> ColorU {
    use crate::ui::color::blend::Blend as _;
    theme.background().into_solid().blend(&overlay)
}

#[test]
fn text_selection_is_visible_and_keeps_text_readable() {
    for theme in mnemos_bar() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        let fg = theme.foreground().into_solid();
        let selection = over_background(&theme, theme.text_selection_color().into_solid());
        let visible = contrast(selection, bg);
        let readable = contrast(fg, selection);
        assert!(visible >= 1.5, "{name}: selection/bg {visible:.2} < 1.5");
        assert!(readable >= 7.0, "{name}: fg/selection {readable:.2} < 7.0");

        let context = over_background(&theme, theme.text_selection_as_context_color().into_solid());
        let readable = contrast(fg, context);
        assert!(
            readable >= 4.5,
            "{name}: fg/context selection {readable:.2} < 4.5"
        );

        let parameter = over_background(&theme, theme.workflow_parameter_highlight_color());
        let readable = contrast(fg, parameter);
        assert!(
            readable >= 7.0,
            "{name}: fg/workflow parameter {readable:.2} < 7.0"
        );
    }
}

#[test]
fn status_colours_are_readable_on_background() {
    for theme in all() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        for (status, color) in [
            ("error", theme.ui_error_color()),
            ("warning", theme.ui_warning_color()),
            ("yellow", theme.ui_yellow_color()),
            ("green", theme.ui_green_color()),
        ] {
            let ratio = contrast(color, bg);
            assert!(ratio >= 4.5, "{name}: {status}/bg {ratio:.2} < 4.5");
        }
    }
}

/// The foreground/background contrast of a community theme, where its published palette sets it
/// below the 7:1 of the Mnemos themes. The palette is kept as published; these floors pin it.
fn community_foreground_floor(name: &str) -> f32 {
    match name {
        // Solarized's base0 on base03 and base00 on base3 are low contrast by design.
        SOLARIZED_DARK_NAME => 4.7,
        SOLARIZED_LIGHT_NAME => 4.1,
        // Atom's default text on its syntax background.
        ONE_DARK_NAME => 6.5,
        // Everforest Light's fg on bg0, Rosé Pine Dawn's text on base and Tokyo Night Day's
        // blue text on its grey ground.
        EVERFOREST_LIGHT_NAME => 5.1,
        ROSE_PINE_DAWN_NAME => 6.6,
        TOKYO_NIGHT_DAY_NAME => 4.5,
        _ => 7.0,
    }
}

#[test]
fn community_foreground_and_accent_contrast_with_background() {
    for theme in community() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        let fg_ratio = contrast(theme.foreground().into_solid(), bg);
        let floor = community_foreground_floor(&name);
        assert!(fg_ratio >= floor, "{name}: fg/bg {fg_ratio:.2} < {floor}");
        // The accent is the project's own highlight colour, so it is held to the 3:1 WCAG floor
        // for non-text UI rather than the 4.5:1 for text (Solarized blue on base3 is 3.4:1).
        let accent_ratio = contrast(theme.accent().into_solid(), bg);
        assert!(
            accent_ratio >= 3.0,
            "{name}: accent/bg {accent_ratio:.2} < 3.0"
        );
    }
}

/// The same bar as the Mnemos themes: a selection 1.5:1 off the background, with the
/// foreground readable on it at 7:1 (selection and workflow parameter) and 4.5:1 (context
/// selection). A selection 1.5:1 off the background cannot keep more than fg/bg divided by 1.5
/// against the foreground, so for a palette whose own fg/bg is below about 11:1 the readability
/// floors are capped at fg/bg / 1.6 (a little slack for the 1.5). The derived highlights of those
/// themes are tuned in `mnemos_themes.rs` to meet it.
#[test]
fn community_text_selection_is_visible_and_keeps_text_readable() {
    for theme in community() {
        let name = theme.name().unwrap();
        let bg = theme.background().into_solid();
        let fg = theme.foreground().into_solid();
        let cap = contrast(fg, bg) / 1.6;
        let selection_floor = cap.min(7.0);
        let context_floor = cap.min(4.5);

        let selection = over_background(&theme, theme.text_selection_color().into_solid());
        let visible = contrast(selection, bg);
        let readable = contrast(fg, selection);
        assert!(visible >= 1.5, "{name}: selection/bg {visible:.2} < 1.5");
        assert!(
            readable >= selection_floor,
            "{name}: fg/selection {readable:.2} < {selection_floor:.2}"
        );

        let context = over_background(&theme, theme.text_selection_as_context_color().into_solid());
        let readable = contrast(fg, context);
        assert!(
            readable >= context_floor,
            "{name}: fg/context selection {readable:.2} < {context_floor:.2}"
        );

        let parameter = over_background(&theme, theme.workflow_parameter_highlight_color());
        let readable = contrast(fg, parameter);
        assert!(
            readable >= selection_floor,
            "{name}: fg/workflow parameter {readable:.2} < {selection_floor:.2}"
        );
    }
}
