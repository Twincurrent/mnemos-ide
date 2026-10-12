use mnemos_ui::{
    elements::{
        Align, ConstrainedBox, Container, CrossAxisAlignment, Flex, MainAxisSize, ParentElement,
        Shrinkable, SizeConstraintCondition, SizeConstraintSwitch,
    },
    Element,
};

const LEFT_COLUMN_WIDTH: f32 = 580.;
const LEFT_COLUMN_CONTENT_MAX_WIDTH: f32 = 800.;
const MIN_RIGHT_COLUMN_WIDTH: f32 = 540.;

/// The minimum window width at which the two-column layout is shown (left content + right panel).
/// Below this width, `static_left` collapses to a single left-only column.
pub const TWO_COLUMN_MIN_WIDTH: f32 = LEFT_COLUMN_WIDTH + MIN_RIGHT_COLUMN_WIDTH;

/// Creates a two-column layout with a fixed-width left column and flexible right column.
///
/// The left column's *content* is constrained to [`LEFT_COLUMN_CONTENT_MAX_WIDTH`] and is always
/// horizontally centered within the left panel.
///
/// If the available width is too narrow for the right column to have at least
/// [`MIN_RIGHT_COLUMN_WIDTH`], we instead render only the left column and center it.
///
/// # Arguments
/// * `left` - Builder for the element to display in the left column ([`LEFT_COLUMN_WIDTH`] px)
/// * `right` - Builder for the element to display in the right column (flexible width)
///
/// # Returns
/// A `Box<dyn Element>` containing the responsive layout
pub fn static_left(
    left: impl Fn() -> Box<dyn Element>,
    right: impl FnOnce() -> Box<dyn Element>,
) -> Box<dyn Element> {
    let max_width_for_two_columns = LEFT_COLUMN_WIDTH + MIN_RIGHT_COLUMN_WIDTH;

    let left_constrained = || {
        ConstrainedBox::new(left())
            .with_max_width(LEFT_COLUMN_CONTENT_MAX_WIDTH)
            .finish()
    };

    // Narrow layout: show only the left section, centered.
    // Use Align instead of a max-sized Flex so we can safely center even when the incoming
    // height constraint is unbounded.
    let left_only = Align::new(left_constrained()).finish();

    // Default layout: fixed-width left + flexible right.
    // Use Align instead of a max-sized Flex so we can safely center even when the incoming
    // height constraint is unbounded.
    let left_centered = Align::new(left_constrained()).finish();

    let left_fixed_width = Container::new(
        ConstrainedBox::new(left_centered)
            .with_width(LEFT_COLUMN_WIDTH)
            .finish(),
    )
    .finish();

    let right_flexible = Shrinkable::new(1., Container::new(right()).finish()).finish();

    let two_column_layout = Container::new(
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(left_fixed_width)
            .with_child(right_flexible)
            .finish(),
    )
    .finish();

    SizeConstraintSwitch::new(
        two_column_layout,
        vec![(
            SizeConstraintCondition::WidthLessThan(max_width_for_two_columns),
            left_only,
        )],
    )
    .finish()
}
