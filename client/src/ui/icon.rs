//! What one item looks like drawn flat, inside a pack or hotbar cell.
//!
//! **One shape vocabulary, two renderers.** `player::hands` builds a mesh per
//! [`ItemShape`] for the hand; this module builds a picture per [`ItemShape`] for a cell.
//! Both read the shape from the one row in `player::items`, so a sword is a sword on both
//! surfaces and neither can re-decide what an item is. That is the whole of why the
//! drawings below are keyed on the shape rather than on item ids: eleven items share four
//! pictures, and a twelfth item inherits one by having a shape at all.
//!
//! **Procedural, like everything else this client draws.** A picture is a handful of
//! `bevy_ui` nodes — coloured rectangles, some rounded, some rotated — positioned in
//! percentages of the cell. No image, no sprite sheet, no atlas, and therefore no asset
//! pipeline and no dependency. It also keeps the result readable by a headless test: an
//! icon here is a set of components, not a texture somebody has to look at.
//!
//! **A picture decides nothing.** The shape is presentation, exactly as its row says: what
//! an item does is the server's registry, and a cell drawn as a blade no more swings than
//! a hand drawn as one does.

use bevy::color::LinearRgba;
use bevy::math::Rot2;
use bevy::prelude::*;
use bevy::ui::FocusPolicy;

use crate::player::ItemShape;

/// The picture one cell is drawing: a shape, in the item's colour.
///
/// Both fields come from the same registry row — [`crate::player::item_shape`] and
/// [`crate::player::item_palette_id`] — so a cell cannot disagree with the hand about
/// either half.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StackIcon {
    pub(crate) shape: ItemShape,
    pub(crate) colour: Color,
}

/// One rectangle of a picture.
///
/// Everything is a percentage of the cell rather than a pixel count, so [`super::CELL_SIZE`]
/// stays the only number that decides how big a cell is and an icon follows it for free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct IconPart {
    /// The part's box, as percentages of the cell: distance from its left and top edges,
    /// then width and height.
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    /// Corner rounding, as a percentage of the part's own shorter side. `50` is a circle.
    radius: f32,
    /// How this part is shaded relative to the item's colour — see [`shaded`].
    shade: f32,
    /// Clockwise rotation about the part's own centre, in radians.
    rotation: f32,
}

impl IconPart {
    /// A square-cornered, unrotated part in the item's own colour. Every drawing below is
    /// written as a deviation from this, so a part says only what makes it different.
    const PLAIN: Self = Self {
        left: 0.0,
        top: 0.0,
        width: 0.0,
        height: 0.0,
        radius: 0.0,
        shade: 0.0,
        rotation: 0.0,
    };
}

/// A quarter turn, which is the angle every part of the blade is drawn at.
const QUARTER_TURN: f32 = std::f32::consts::FRAC_PI_4;

/// A cube, in three faces: lit on top, the item's own colour in front, turned away at the
/// side. The same reading a voxel gets in the world, which is what makes a carried block
/// look like the thing it places.
const BLOCK: [IconPart; 3] = [
    IconPart {
        left: 16.0,
        top: 18.0,
        width: 68.0,
        height: 18.0,
        shade: 0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 16.0,
        top: 34.0,
        width: 44.0,
        height: 46.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 58.0,
        top: 34.0,
        width: 26.0,
        height: 46.0,
        shade: -0.42,
        ..IconPart::PLAIN
    },
];

/// A small heap of raw material: three nuggets, the top one catching the light. Round, so
/// it never reads as a cube at a glance.
const MATERIAL: [IconPart; 3] = [
    IconPart {
        left: 24.0,
        top: 40.0,
        width: 30.0,
        height: 30.0,
        radius: 50.0,
        shade: -0.30,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 48.0,
        top: 44.0,
        width: 28.0,
        height: 28.0,
        radius: 50.0,
        shade: -0.12,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 34.0,
        top: 24.0,
        width: 30.0,
        height: 30.0,
        radius: 50.0,
        shade: 0.22,
        ..IconPart::PLAIN
    },
];

/// A sword: a bright bar on the cell's diagonal, a cross guard across it, a grip below.
///
/// The guard is perpendicular *by construction* rather than by a second angle — it is a
/// bar drawn across the cell where the blade is drawn along it, and the same quarter turn
/// then takes them to right angles. One number to change if the tilt ever moves.
const BLADE: [IconPart; 3] = [
    IconPart {
        left: 44.0,
        top: 12.0,
        width: 12.0,
        height: 60.0,
        shade: 0.34,
        rotation: QUARTER_TURN,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 20.0,
        top: 52.0,
        width: 32.0,
        height: 8.0,
        shade: -0.25,
        rotation: QUARTER_TURN,
        ..IconPart::PLAIN
    },
    // Every field is spelled out here, so there is nothing left for `PLAIN` to fill in.
    IconPart {
        left: 24.0,
        top: 54.0,
        width: 11.0,
        height: 16.0,
        radius: 30.0,
        shade: -0.55,
        rotation: QUARTER_TURN,
    },
];

/// A bundle: wider than it is tall and tied with a cord, so a carried structure never
/// reads as another stackable cube. Same argument as the held view model's, drawn flat.
const BUNDLE: [IconPart; 3] = [
    IconPart {
        left: 14.0,
        top: 30.0,
        width: 72.0,
        height: 14.0,
        radius: 18.0,
        shade: 0.26,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 14.0,
        top: 42.0,
        width: 72.0,
        height: 34.0,
        radius: 14.0,
        ..IconPart::PLAIN
    },
    IconPart {
        left: 44.0,
        top: 28.0,
        width: 12.0,
        height: 50.0,
        shade: -0.48,
        ..IconPart::PLAIN
    },
];

/// The rectangles one shape is drawn from, in the order they are stacked.
///
/// **Exhaustive, with no wildcard arm.** A fifth [`ItemShape`] does not compile until it
/// has been drawn, which is the same guarantee `player::items` gets from `ItemDisplay`'s
/// three mandatory fields: there is no branch for a new shape to fall through into a
/// square. The sweep in the tests below covers what the compiler cannot see — an arm
/// answered with an empty drawing, or with a copy of another shape's.
pub(crate) fn parts(shape: ItemShape) -> &'static [IconPart] {
    match shape {
        ItemShape::Block => &BLOCK,
        ItemShape::Material => &MATERIAL,
        ItemShape::Blade => &BLADE,
        ItemShape::Bundle => &BUNDLE,
    }
}

/// One part's colour: the item's, mixed toward white or black by `shade`.
///
/// **A mix rather than a multiply**, because a multiply cannot separate a dark item from
/// itself. A log is `0.10` linear; scaled to three faces it is `0.10 / 0.08 / 0.06`, which
/// reads as one flat silhouette. Mixing toward white lifts the lit face away from the base
/// at every brightness, and mixing toward black does the same at the other end — so the
/// cube reads for snow and for coal with one pair of numbers.
fn shaded(base: LinearRgba, shade: f32) -> Color {
    let shade = shade.clamp(-1.0, 1.0);
    let target = if shade >= 0.0 { 1.0 } else { 0.0 };
    let towards = shade.abs();
    let mix = |channel: f32| channel + (target - channel) * towards;
    Color::linear_rgba(mix(base.red), mix(base.green), mix(base.blue), base.alpha)
}

/// The node every cell hangs its picture under: one per cell, filling it, drawing nothing
/// of its own.
///
/// It exists so that redrawing a cell is *replacing this node's children* rather than
/// picking the old rectangles out of the cell by hand. Accumulation is therefore not a
/// rule anybody has to remember, in the same way the single tooltip entity is not.
///
/// `FocusPolicy::Pass` is load-bearing and the omission is silent: a node with no policy
/// **blocks**, so an icon laid over its own cell would take the pointer, the cell would
/// fall to `Interaction::None`, and clicking a full slot would stop working while an empty
/// one still did. Every part below carries it for the same reason.
pub(crate) fn host_bundle() -> impl Bundle {
    (
        IconHost,
        DrawnIcon(None),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        FocusPolicy::Pass,
    )
}

/// The icon host inside one cell.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IconHost;

/// What the host is currently drawing, so a refresh can tell "no change" from "redraw".
///
/// The refresh path runs every frame; without this the cell would despawn and respawn its
/// rectangles sixty times a second for a stack nobody touched.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub(crate) struct DrawnIcon(Option<StackIcon>);

/// Draws `next` under `host`, replacing whatever was there, and does nothing at all when
/// that is already what is drawn.
///
/// `None` clears the host, which is how an emptied slot loses its picture — the same call,
/// not a second path.
pub(crate) fn redraw(
    commands: &mut Commands<'_, '_>,
    host: Entity,
    mut drawn: Mut<'_, DrawnIcon>,
    next: Option<StackIcon>,
) {
    // Read through the shared reference and write only on a real change, so a stack
    // nobody touched neither respawns its rectangles every frame nor reports itself as
    // having changed.
    if drawn.0 == next {
        return;
    }
    drawn.0 = next;

    let mut host = commands.entity(host);
    host.despawn_related::<Children>();
    let Some(icon) = next else {
        return;
    };
    let base = icon.colour.to_linear();
    host.with_children(|host| {
        for part in parts(icon.shape) {
            host.spawn(part_bundle(part, base));
        }
    });
}

/// One rectangle of a picture, as the nodes that draw it.
fn part_bundle(part: &IconPart, base: LinearRgba) -> impl Bundle {
    (
        IconRect,
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(part.left),
            top: Val::Percent(part.top),
            width: Val::Percent(part.width),
            height: Val::Percent(part.height),
            border_radius: BorderRadius::all(Val::Percent(part.radius)),
            ..default()
        },
        BackgroundColor(shaded(base, part.shade)),
        UiTransform::from_rotation(Rot2::radians(part.rotation)),
        FocusPolicy::Pass,
    )
}

/// One rectangle of a drawn icon. Carried so a test can count what a cell drew without
/// walking the hierarchy looking for anything with a background.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IconRect;

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether one shape's answer is a drawing rather than a placeholder standing in for
    /// one.
    ///
    /// The predicate the sweep applies, extracted so `the_sweep_rejects_a_shape_that_is_not_drawn`
    /// can assert the failure mode on fixtures instead of the sweep only ever running over
    /// answers that already pass. Three ways to not be a drawing: nothing at all, a part
    /// with no area, and a part that has wandered outside the cell it is drawn in.
    fn is_a_drawing(parts: &[IconPart]) -> bool {
        !parts.is_empty()
            && parts.iter().all(|part| {
                part.width > 0.0
                    && part.height > 0.0
                    && part.left >= 0.0
                    && part.top >= 0.0
                    && part.left + part.width <= 100.0
                    && part.top + part.height <= 100.0
            })
    }

    /// The counterpart to the registry's name sweep: every shape in the vocabulary is
    /// drawn, and no two are drawn the same.
    ///
    /// The second half is what "a shape with none silently falls back to a square" would
    /// look like once the compiler has forced an arm to exist: an arm that answers with
    /// somebody else's picture.
    #[test]
    fn every_shape_has_a_drawing_of_its_own() {
        for shape in ItemShape::ALL {
            assert!(
                is_a_drawing(parts(shape)),
                "{shape:?} has no drawing: {:?}",
                parts(shape)
            );
        }

        for (index, shape) in ItemShape::ALL.iter().enumerate() {
            for other in &ItemShape::ALL[index + 1..] {
                assert_ne!(
                    parts(*shape),
                    parts(*other),
                    "{shape:?} and {other:?} draw the same picture"
                );
            }
        }
    }

    /// The sweep's teeth, on fixtures rather than on the drawings that already pass.
    #[test]
    fn the_sweep_rejects_a_shape_that_is_not_drawn() {
        assert!(!is_a_drawing(&[]), "an empty drawing passed the sweep");
        assert!(
            !is_a_drawing(&[IconPart {
                width: 40.0,
                ..IconPart::PLAIN
            }]),
            "a part with no height passed the sweep"
        );
        assert!(
            !is_a_drawing(&[IconPart {
                left: 80.0,
                top: 10.0,
                width: 40.0,
                height: 40.0,
                ..IconPart::PLAIN
            }]),
            "a part hanging off the side of the cell passed the sweep"
        );
        assert!(
            is_a_drawing(&[IconPart {
                left: 10.0,
                top: 10.0,
                width: 80.0,
                height: 80.0,
                ..IconPart::PLAIN
            }]),
            "an honest one-rectangle drawing failed the sweep"
        );
    }

    /// The property a multiply does not have: three faces stay three faces at both ends of
    /// the brightness range.
    #[test]
    fn shading_separates_a_dark_item_from_itself_and_a_bright_one_too() {
        for base in [
            LinearRgba::new(0.030, 0.034, 0.042, 1.0),
            LinearRgba::new(0.888, 0.913, 0.930, 1.0),
        ] {
            let lit = shaded(base, 0.30).to_linear();
            let plain = shaded(base, 0.0).to_linear();
            let turned = shaded(base, -0.42).to_linear();
            assert!(
                lit.red - plain.red > 0.02,
                "the lit face did not separate from the base: {lit:?} vs {plain:?}"
            );
            assert!(
                plain.red - turned.red > 0.005,
                "the turned face did not separate from the base: {plain:?} vs {turned:?}"
            );
            // The item's own colour is what the unshaded part draws, which is what keeps
            // the swatch a cell shows the swatch the registry named.
            assert_eq!(plain, base);
        }
    }

    /// Alpha is the item's, never the shade's: a mix that touched it would fade an icon in
    /// and out with its own lighting.
    #[test]
    fn shading_leaves_the_alpha_channel_alone() {
        let base = LinearRgba::new(0.2, 0.3, 0.4, 0.5);
        for shade in [-1.0, -0.42, 0.0, 0.3, 1.0] {
            assert!(
                (shaded(base, shade).to_linear().alpha - 0.5).abs() < f32::EPSILON,
                "shade {shade} moved the alpha channel"
            );
        }
    }
}
