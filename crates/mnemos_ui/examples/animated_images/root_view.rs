use instant::Instant;
use mnemos_ui::{
    assets::asset_cache::AssetSource,
    elements::{
        CacheOption, ConstrainedBox, CrossAxisAlignment, Flex, Image, ParentElement, Stack,
    },
    AppContext, Element, Entity, TypedActionView, View,
};

pub struct RootView {
    animation_start_time: Instant,
}

impl RootView {
    pub fn new() -> Self {
        RootView {
            animation_start_time: Instant::now(),
        }
    }
}

impl Default for RootView {
    fn default() -> Self {
        Self::new()
    }
}

impl Entity for RootView {
    type Event = ();
}
impl View for RootView {
    fn ui_name() -> &'static str {
        "RootView"
    }

    fn render(&self, _: &AppContext) -> Box<dyn Element> {
        // Mnemos IDE: upstream's 17 MB full-window background GIF is gone; the two small
        // animated GIFs below still exercise animation.
        Stack::new()
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(
                        ConstrainedBox::new(
                            Image::new(
                                AssetSource::Bundled {
                                    path: "numbers-1000ms.gif",
                                },
                                CacheOption::BySize,
                            )
                            .enable_animation_with_start_time(self.animation_start_time)
                            .finish(),
                        )
                        .with_height(350.)
                        .with_width(350.)
                        .finish(),
                    )
                    .with_child(
                        ConstrainedBox::new(
                            Image::new(
                                AssetSource::Bundled {
                                    path: "numbers-750ms.gif",
                                },
                                CacheOption::BySize,
                            )
                            .enable_animation_with_start_time(self.animation_start_time)
                            .finish(),
                        )
                        .with_height(350.)
                        .with_width(350.)
                        .finish(),
                    )
                    .finish(),
            )
            .finish()
    }
}

impl TypedActionView for RootView {
    type Action = ();
}
