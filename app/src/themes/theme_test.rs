use super::*;
use crate::util::color::OPAQUE;

#[test]
#[cfg(not(target_family = "wasm"))]
fn in_memory_theme_generation_test() {
    // Mnemos IDE: upstream's sample photo (of unknown provenance) is deleted. This uses the
    // bundled Slate background (dark at every swatch) and then Stone (light), see their
    // ATTRIBUTION.md, so both the dark and the light derivation stay covered.
    let image_path = |name: &str| -> PathBuf {
        [
            env!("CARGO_MANIFEST_DIR"),
            "assets",
            "bundled",
            "jpg",
            "themes",
            name,
        ]
        .iter()
        .collect()
    };
    let slate_bg_path = image_path("slate.jpg");

    let mut in_memory_theme = mnemos_ui::r#async::block_on(InMemoryThemeOptions::new(
        "slate".to_string(),
        slate_bg_path.clone(),
    ))
    .unwrap();

    let slate_bg_path_string = slate_bg_path.to_str().unwrap_or_default().to_owned();
    assert_eq!(
        in_memory_theme.theme(),
        MnemosTheme::new(
            // the theme defaults to the 0th bg color
            ColorU::new(16, 18, 24, OPAQUE).into(),
            // this background color makes it a "dark" theme, so the foreground is white
            ColorU::white(),
            // the most distinct accent color is the 1st one
            ColorU::new(23, 26, 34, OPAQUE).into(),
            None,
            Some(Details::Darker),
            mnemos_night_colors(),
            Some(Image {
                source: AssetSource::LocalFile {
                    path: slate_bg_path_string.clone()
                },
                opacity: 30,
            }),
            Some("slate".to_string()),
        )
    );

    in_memory_theme.chosen_bg_color_index = 2;

    assert_eq!(
        in_memory_theme.theme(),
        MnemosTheme::new(
            // now the background is the 2nd one
            ColorU::new(34, 40, 47, OPAQUE).into(),
            ColorU::white(),
            // now the 0th color is the most distinct color
            ColorU::new(16, 18, 24, OPAQUE).into(),
            None,
            Some(Details::Darker),
            mnemos_night_colors(),
            Some(Image {
                source: AssetSource::LocalFile {
                    path: slate_bg_path_string
                },
                opacity: 30,
            }),
            Some("slate".to_string()),
        )
    );

    let stone_bg_path = image_path("stone.jpg");
    let stone_theme = mnemos_ui::r#async::block_on(InMemoryThemeOptions::new(
        "stone".to_string(),
        stone_bg_path.clone(),
    ))
    .unwrap();
    assert_eq!(
        stone_theme.theme(),
        MnemosTheme::new(
            ColorU::new(205, 196, 181, OPAQUE).into(),
            // a light background makes it a "light" theme, so the foreground is black
            ColorU::black(),
            ColorU::new(230, 223, 212, OPAQUE).into(),
            None,
            Some(Details::Lighter),
            mnemos_day_colors(),
            Some(Image {
                source: AssetSource::LocalFile {
                    path: stone_bg_path.to_str().unwrap_or_default().to_owned()
                },
                opacity: 30,
            }),
            Some("stone".to_string()),
        )
    );
}

const LEGACY_LIGHT: [ThemeKind; 4] = [
    ThemeKind::Light,
    ThemeKind::Snowy,
    ThemeKind::PinkCity,
    ThemeKind::Marble,
];

const LEGACY_DARK: [ThemeKind; 14] = [
    ThemeKind::SentReferralReward,
    ThemeKind::ReceivedReferralReward,
    ThemeKind::Adeberry,
    ThemeKind::Phenomenon,
    ThemeKind::Dark,
    ThemeKind::FancyDracula,
    ThemeKind::CyberWave,
    ThemeKind::SolarFlare,
    ThemeKind::WillowDream,
    ThemeKind::DarkCity,
    ThemeKind::RedRock,
    ThemeKind::JellyFish,
    ThemeKind::Leafy,
    ThemeKind::Koi,
];

/// Every built-in kind, in picker order: the Mnemos themes, the image themes, then the community
/// themes alphabetically.
const BUILT_IN_KINDS: [ThemeKind; 31] = [
    ThemeKind::MnemosNight,
    ThemeKind::MnemosDay,
    ThemeKind::MnemosContrast,
    ThemeKind::MnemosMoss,
    ThemeKind::Stone,
    ThemeKind::Slate,
    ThemeKind::MountainNight,
    ThemeKind::MountainDawn,
    ThemeKind::Fjord,
    ThemeKind::CatppuccinFrappe,
    ThemeKind::CatppuccinLatte,
    ThemeKind::CatppuccinMacchiato,
    ThemeKind::CatppuccinMocha,
    ThemeKind::Dracula,
    ThemeKind::EverforestDark,
    ThemeKind::EverforestLight,
    ThemeKind::GithubDark,
    ThemeKind::GithubLight,
    ThemeKind::GruvboxDark,
    ThemeKind::GruvboxLight,
    ThemeKind::Kanagawa,
    ThemeKind::NightOwl,
    ThemeKind::Nord,
    ThemeKind::OneDark,
    ThemeKind::RosePine,
    ThemeKind::RosePineDawn,
    ThemeKind::RosePineMoon,
    ThemeKind::SolarizedDark,
    ThemeKind::SolarizedLight,
    ThemeKind::TokyoNight,
    ThemeKind::TokyoNightDay,
];

/// Where the community themes start in `BUILT_IN_KINDS`, after the Mnemos and image themes.
const COMMUNITY_START: usize = 9;

#[test]
fn built_in_list_is_the_mnemos_image_and_community_themes_in_picker_order() {
    let config = MnemosThemeConfig::new();
    // The chooser sorts by `ThemeKind`'s derived order, so this is the order it lists them in.
    let mut kinds: Vec<ThemeKind> = config.theme_items().map(|(kind, _)| kind.clone()).collect();
    kinds.sort();
    assert_eq!(kinds, BUILT_IN_KINDS);
    let names: Vec<String> = kinds.iter().map(ToString::to_string).collect();
    assert_eq!(
        names[4..COMMUNITY_START],
        ["Stone", "Slate", "Mountain Night", "Mountain Dawn", "Fjord"]
    );
    let mut community = names[COMMUNITY_START..].to_vec();
    community.sort_by_key(|name| name.to_lowercase());
    assert_eq!(names[COMMUNITY_START..], community[..]);
    for (kind, theme) in config.theme_items() {
        assert_eq!(theme.name().as_deref(), Some(kind.to_string().as_str()));
        assert!(!kind.to_string().to_lowercase().contains("warp"));
    }
    let shared: Vec<Option<String>> = mnemos_core::ui::theme::mnemos_themes::all()
        .iter()
        .map(MnemosTheme::name)
        .collect();
    let listed: Vec<Option<String>> = kinds.iter().map(|kind| config.theme(kind).name()).collect();
    assert_eq!(shared, listed);
}

#[test]
fn search_ignores_the_accent_of_rose_pine() {
    assert!(ThemeKind::RosePine.matches("rose"));
    assert!(ThemeKind::RosePine.matches("Rosé"));
    assert!(ThemeKind::TokyoNight.matches("tokyo"));
    assert!(ThemeKind::RosePineDawn.matches("rose pine dawn"));
    assert!(ThemeKind::CatppuccinFrappe.matches("frappe"));
    assert!(!ThemeKind::Nord.matches("rose"));
}

#[test]
fn display_names_of_community_themes() {
    let names: Vec<String> = BUILT_IN_KINDS[COMMUNITY_START..]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        names,
        [
            "Catppuccin Frappé",
            "Catppuccin Latte",
            "Catppuccin Macchiato",
            "Catppuccin Mocha",
            "Dracula",
            "Everforest Dark",
            "Everforest Light",
            "GitHub Dark",
            "GitHub Light",
            "Gruvbox Dark",
            "Gruvbox Light",
            "Kanagawa",
            "Night Owl",
            "Nord",
            "One Dark",
            "Rosé Pine",
            "Rosé Pine Dawn",
            "Rosé Pine Moon",
            "Solarized Dark",
            "Solarized Light",
            "Tokyo Night",
            "Tokyo Night Day",
        ]
    );
}

#[test]
fn default_theme_is_mnemos_night() {
    assert_eq!(ThemeKind::default(), ThemeKind::MnemosNight);
    assert_eq!(
        SelectedSystemThemes::default(),
        SelectedSystemThemes {
            light: ThemeKind::MnemosDay,
            dark: ThemeKind::MnemosNight,
        }
    );
    assert_eq!(
        MnemosThemeConfig::new()
            .theme(&ThemeKind::default())
            .name()
            .as_deref(),
        Some("Mnemos Night")
    );
}

#[test]
fn legacy_themes_migrate_to_mnemos_themes() {
    for kind in LEGACY_LIGHT {
        assert_eq!(kind.migrated(), Some(ThemeKind::MnemosDay), "{kind:?}");
        assert!(kind.is_legacy_builtin());
    }
    for kind in LEGACY_DARK {
        assert_eq!(kind.migrated(), Some(ThemeKind::MnemosNight), "{kind:?}");
        assert!(kind.is_legacy_builtin());
    }
    for kind in BUILT_IN_KINDS {
        assert_eq!(kind.migrated(), None, "{kind:?}");
        assert!(!kind.is_legacy_builtin());
    }
    let custom: ThemeKind = CustomTheme::new("Mine".into(), PathBuf::from("mine.yaml")).into();
    assert_eq!(custom.migrated(), None);
}

#[test]
fn saved_legacy_theme_renders_as_its_replacement() {
    // Legacy values are never rewritten (that would reach older builds through sync), so a
    // light legacy theme must render as Mnemos Day and the rest as Mnemos Night.
    let config = MnemosThemeConfig::new();
    for kind in LEGACY_LIGHT {
        assert_eq!(kind.resolved(), ThemeKind::MnemosDay, "{kind:?}");
        assert_eq!(
            config.theme(&kind).name().as_deref(),
            Some("Mnemos Day"),
            "{kind:?}"
        );
    }
    for kind in LEGACY_DARK {
        assert_eq!(kind.resolved(), ThemeKind::MnemosNight, "{kind:?}");
        assert_eq!(
            config.theme(&kind).name().as_deref(),
            Some("Mnemos Night"),
            "{kind:?}"
        );
    }
    for kind in BUILT_IN_KINDS {
        assert_eq!(kind.resolved(), kind);
        assert_eq!(
            config.theme(&kind).name().as_deref(),
            Some(kind.to_string().as_str())
        );
    }
    let custom: ThemeKind = CustomTheme::new("Mine".into(), PathBuf::from("mine.yaml")).into();
    assert_eq!(custom.resolved(), custom);
}

#[test]
fn settings_file_values_round_trip_and_legacy_values_still_parse() {
    use serde_json::json;
    use settings_value::SettingsValue as _;

    assert_eq!(
        ThemeKind::MnemosNight.to_file_value(),
        json!("mnemos_night")
    );
    assert_eq!(
        ThemeKind::from_file_value(&json!("mnemos_night")),
        Some(ThemeKind::MnemosNight)
    );
    assert_eq!(
        ThemeKind::from_file_value(&json!("mnemos_contrast")),
        Some(ThemeKind::MnemosContrast)
    );
    // A settings.toml written by an older build must still parse, or the settings layer
    // inhibits writes to the theme key. The revived community kinds keep the file values those
    // builds already write and parse.
    for (value, kind) in [
        ("dracula", ThemeKind::Dracula),
        ("solarized_dark", ThemeKind::SolarizedDark),
        ("solarized_light", ThemeKind::SolarizedLight),
        ("gruvbox_dark", ThemeKind::GruvboxDark),
        ("gruvbox_light", ThemeKind::GruvboxLight),
    ] {
        assert_eq!(
            ThemeKind::from_file_value(&json!(value)),
            Some(kind.clone())
        );
        assert_eq!(kind.to_file_value(), json!(value));
    }
    for (value, kind) in [
        ("tokyo_night", ThemeKind::TokyoNight),
        ("catppuccin_mocha", ThemeKind::CatppuccinMocha),
        ("catppuccin_latte", ThemeKind::CatppuccinLatte),
        ("rose_pine", ThemeKind::RosePine),
        ("nord", ThemeKind::Nord),
        ("one_dark", ThemeKind::OneDark),
        ("kanagawa", ThemeKind::Kanagawa),
        ("catppuccin_frappe", ThemeKind::CatppuccinFrappe),
        ("catppuccin_macchiato", ThemeKind::CatppuccinMacchiato),
        ("everforest_dark", ThemeKind::EverforestDark),
        ("everforest_light", ThemeKind::EverforestLight),
        ("github_dark", ThemeKind::GithubDark),
        ("github_light", ThemeKind::GithubLight),
        ("night_owl", ThemeKind::NightOwl),
        ("rose_pine_dawn", ThemeKind::RosePineDawn),
        ("rose_pine_moon", ThemeKind::RosePineMoon),
        ("tokyo_night_day", ThemeKind::TokyoNightDay),
        ("stone", ThemeKind::Stone),
        ("slate", ThemeKind::Slate),
        ("mountain_night", ThemeKind::MountainNight),
        ("mountain_dawn", ThemeKind::MountainDawn),
        ("fjord", ThemeKind::Fjord),
    ] {
        assert_eq!(kind.to_file_value(), json!(value));
        assert_eq!(ThemeKind::from_file_value(&json!(value)), Some(kind));
    }
    // The native store uses serde JSON.
    assert_eq!(
        serde_json::from_str::<ThemeKind>("\"Dark\"").unwrap(),
        ThemeKind::Dark
    );
}

/// The image themes' backgrounds are embedded in the app (never fetched), and the theme's text
/// stays readable over the image itself, not only over the ground colour. The app draws the image
/// with `cover` and the ground colour over it at `100 - opacity` percent; this composites the same
/// way at a reduced size and measures against the composite. Two regions are held to the full bar:
/// the area where a terminal's output usually sits (the left 60% and top 70%) and the bottom band
/// at full width, where the input editor and the newest output sit. There the foreground must keep
/// 7:1, and the accent and the six chromatic normal ANSI colours 4.5:1. The foreground must also
/// keep 6.5:1 over the whole frame. The floors are the 99th percentile pixel, so a few bright
/// stars or snow pixels do not count.
#[test]
fn image_themes_are_embedded_and_readable_over_their_image() {
    use mnemos_core::ui::color::contrast::relative_luminance;

    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 200;

    fn contrast(a: f32, b: f32) -> f32 {
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
    /// The contrast of `color` that all but the lowest 1% of `backgrounds` reach.
    fn p99(color: ColorU, backgrounds: &[f32]) -> f32 {
        let lum = relative_luminance(color);
        let mut ratios: Vec<f32> = backgrounds.iter().map(|&bg| contrast(lum, bg)).collect();
        ratios.sort_by(f32::total_cmp);
        ratios[((ratios.len() as f32) * 0.01) as usize]
    }

    let config = MnemosThemeConfig::new();
    for kind in &BUILT_IN_KINDS[4..COMMUNITY_START] {
        let theme = config.theme(kind);
        let name = theme.name().unwrap();
        let background = theme
            .background_image()
            .unwrap_or_else(|| panic!("{name}: no background image"));
        let AssetSource::Bundled { path } = background.source else {
            panic!("{name}: the background image is not bundled");
        };
        let bytes = <crate::Assets as rust_embed::RustEmbed>::get(path)
            .unwrap_or_else(|| panic!("{name}: {path} is not embedded"))
            .data;
        let image = image::load_from_memory(&bytes)
            .unwrap_or_else(|err| panic!("{name}: {path}: {err}"))
            .resize_exact(WIDTH, HEIGHT, image::imageops::FilterType::Triangle)
            .to_rgb8();

        let ground = theme.background().into_solid();
        let weight = f32::from(background.opacity) / 100.;
        // Luminance of each composited pixel, per region.
        let mut text_area = Vec::new();
        let mut bottom_band = Vec::new();
        let mut frame = Vec::new();
        for (x, y, pixel) in image.enumerate_pixels() {
            let mix = |image: u8, ground: u8| {
                (f32::from(image) * weight + f32::from(ground) * (1. - weight)).round() as u8
            };
            let composite = relative_luminance(ColorU::new(
                mix(pixel[0], ground.r),
                mix(pixel[1], ground.g),
                mix(pixel[2], ground.b),
                OPAQUE,
            ));
            frame.push(composite);
            if x < WIDTH * 6 / 10 && y < HEIGHT * 7 / 10 {
                text_area.push(composite);
            }
            if y >= HEIGHT * 8 / 10 {
                bottom_band.push(composite);
            }
        }

        let fg = theme.foreground().into_solid();
        let normal = &theme.terminal_colors().normal;
        let mut floors = vec![
            ("fg", fg, 7.0),
            ("accent", theme.accent().into_solid(), 4.5),
        ];
        for (color_name, color) in [
            ("red", normal.red),
            ("green", normal.green),
            ("yellow", normal.yellow),
            ("blue", normal.blue),
            ("magenta", normal.magenta),
            ("cyan", normal.cyan),
        ] {
            floors.push((color_name, color.into(), 4.5));
        }
        for (region, backgrounds) in [("text area", &text_area), ("bottom band", &bottom_band)] {
            for (color_name, color, floor) in &floors {
                let ratio = p99(*color, backgrounds);
                assert!(
                    ratio >= *floor,
                    "{name}: {color_name} over the {region} {ratio:.2} < {floor}"
                );
            }
        }
        let ratio = p99(fg, &frame);
        assert!(
            ratio >= 6.5,
            "{name}: fg over the whole image {ratio:.2} < 6.5"
        );
    }
}
