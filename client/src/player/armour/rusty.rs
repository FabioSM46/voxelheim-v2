//! The rusty set, sculpted: a knight's helm, a plated cuirass, vambraces and greaves.
//!
//! **Every number is a notch of the model sheet** — `+x` to the character's right, `y` from
//! the feet, `+z` forwards — and every part sits inside the cell `appearance::placed_armour`
//! gives its segment. Those cells are the tables in [`super::super::appearance`] grown by the
//! armour's wrapping tier: the helm and cuirass by one notch on every side, the greaves by half
//! of one. The recess parts are the core showing through the gaps between plates.
//!
//! **Fractions of a notch are deliberate.** The body's faces sit on whole and half notches,
//! and rule 2 of the rig forbids a face of another colour on one of those planes where the two
//! overlap, so the plates here stand on tenths. `no_sculpted_face_shares_a_plane_with_the_body`
//! is what checks it.

use super::super::appearance::ArmourSegment;
use super::{Part, Tone, mirrored, ring};

/// A knight's helm closes over the hair it is worn on.
pub(super) const HIDES_HAIR: bool = true;

/// The parts one segment of the rusty set is cut from.
pub(super) fn parts(segment: ArmourSegment) -> &'static [Part] {
    match segment {
        ArmourSegment::Helmet => &HELM,
        ArmourSegment::Torso => &CUIRASS,
        ArmourSegment::LeftSleeve => &LEFT_VAMBRACE,
        ArmourSegment::RightSleeve => &RIGHT_VAMBRACE,
        ArmourSegment::LeftGreave => &LEFT_GREAVE,
        ArmourSegment::RightGreave => &RIGHT_GREAVE,
    }
}

/// The helm: cell `x` ±5, `y` 26–36, `z` ±5, over a head of ±4 and eyes at 30–31.
///
/// A rounded dome with the face closed by a plate: two eye slits at the height of the eyes, a
/// mouth grille of one bar across and two down, a nasal ridge splitting the slits, a raised
/// brow band all the way round and a low crest front to back. The dark visor backing behind
/// the face plate is what the slits and the grille show.
const HELM: [Part; 13] = [
    // The dome: straight to the crown of the head, then two steps in.
    Part::loft(
        (-4.6, 4.6),
        (-4.6, 4.6),
        &[
            ring(26.0, 0.0, 0.9),
            ring(35.0, 0.0, 0.9),
            ring(35.5, 0.7, 1.1),
            ring(35.8, 1.8, 1.2),
        ],
        Tone::Plate,
    ),
    // The visor backing, in shadow behind the slits and the grille.
    Part::block((-3.8, 3.8), (26.6, 31.4), (4.3, 4.75), Tone::Recess),
    // The face plate above the slits.
    Part::block((-4.4, 4.4), (31.2, 32.6), (4.2, 4.9), Tone::Plate),
    // The cheek guards either side of the slits and the grille.
    Part::block((-4.4, -3.4), (26.0, 31.2), (4.2, 4.9), Tone::Plate),
    Part::block((3.4, 4.4), (26.0, 31.2), (4.2, 4.9), Tone::Plate),
    // The bar under the eye slits.
    Part::block((-3.4, 3.4), (28.8, 29.7), (4.2, 4.9), Tone::Plate),
    // The chin bar under the grille.
    Part::block((-3.4, 3.4), (26.0, 26.7), (4.2, 4.9), Tone::Plate),
    // The grille: one bar across, two down, set a little behind the plates.
    Part::block((-3.4, 3.4), (27.6, 28.0), (4.2, 4.85), Tone::Plate),
    Part::block((-2.4, -1.8), (26.7, 28.8), (4.2, 4.85), Tone::Plate),
    Part::block((1.8, 2.4), (26.7, 28.8), (4.2, 4.85), Tone::Plate),
    // The nasal ridge, standing out to the front of the cell between the slits.
    Part::block((-0.6, 0.6), (28.2, 33.4), (4.2, 5.0), Tone::Plate),
    // The brow band, the full cell all the way round.
    Part::loft(
        (-5.0, 5.0),
        (-5.0, 5.0),
        &[ring(32.6, 0.0, 1.2), ring(33.6, 0.0, 1.2)],
        Tone::Plate,
    ),
    // The crest, rising out of the crown to the top of the cell.
    Part::block((-0.45, 0.45), (35.3, 36.0), (-4.0, 3.4), Tone::Plate),
];

/// The cuirass: cell `x` ±6, `y` 13–26, `z` ±5, over a tunic of ±5, 14–25, ±4.
///
/// A breastplate with a centre ridge and a sloping top, two pauldrons over the shoulders, a
/// gorget round the neck and a fauld of two lames at the waist. The core shows between the
/// lames and under the breastplate.
const CUIRASS: [Part; 8] = [
    // The core.
    Part::loft(
        (-5.25, 5.25),
        (-4.35, 4.35),
        &[ring(13.2, 0.0, 0.5), ring(24.6, 0.0, 0.5)],
        Tone::Recess,
    ),
    // The lower lame of the fauld, flaring at the hem.
    Part::loft(
        (-5.9, 5.9),
        (-4.9, 4.9),
        &[ring(13.0, 0.0, 1.0), ring(14.5, 0.2, 1.0)],
        Tone::Plate,
    ),
    // The upper lame.
    Part::loft(
        (-5.8, 5.8),
        (-4.8, 4.8),
        &[ring(14.8, 0.0, 1.0), ring(16.4, 0.15, 1.0)],
        Tone::Plate,
    ),
    // The breastplate, sloping in to the shoulders.
    Part::loft(
        (-5.45, 5.45),
        (-4.75, 4.75),
        &[
            ring(16.7, 0.0, 0.6),
            ring(24.8, 0.0, 0.6),
            ring(25.6, 0.8, 0.9),
        ],
        Tone::Plate,
    ),
    // The centre ridge down the front, to the front of the cell.
    Part::block((-0.35, 0.35), (17.2, 25.0), (4.2, 5.0), Tone::Plate),
    // The pauldrons, domed over each shoulder.
    PAULDRON,
    PAULDRON.mirrored(),
    // The gorget round the neck.
    Part::loft(
        (-2.6, 2.6),
        (-2.6, 2.6),
        &[ring(25.2, 0.0, 0.6), ring(26.0, 0.0, 0.6)],
        Tone::Plate,
    ),
];

/// The left pauldron. Its top stops short of the cell so it never lies on the plane of the
/// vambrace's shoulder lame beside it.
const PAULDRON: Part = Part::loft(
    (-6.0, -3.0),
    (-4.4, 4.4),
    &[
        ring(22.8, 0.0, 1.0),
        ring(25.0, 0.0, 1.0),
        ring(25.8, 0.6, 0.8),
    ],
    Tone::Plate,
);

/// The left vambrace: cell `x` −8 to −4, `y` 20–26, `z` ±2, over a sleeve of −7 to −5, ±1.
///
/// Segmented: a flared cuff at the wrist, two lames up the arm and a shoulder lame that rounds
/// over the top, with the core showing between them.
const LEFT_VAMBRACE: [Part; 5] = [
    Part::loft(
        (-7.35, -4.65),
        (-1.35, 1.35),
        &[ring(20.3, 0.0, 0.3), ring(25.6, 0.0, 0.3)],
        Tone::Recess,
    ),
    // The cuff.
    Part::loft(
        (-8.0, -4.0),
        (-2.0, 2.0),
        &[ring(20.0, 0.0, 0.9), ring(20.9, 0.35, 0.9)],
        Tone::Plate,
    ),
    Part::loft(
        (-7.75, -4.25),
        (-1.75, 1.75),
        &[ring(21.2, 0.0, 0.6), ring(22.7, 0.0, 0.6)],
        Tone::Plate,
    ),
    Part::loft(
        (-7.75, -4.25),
        (-1.75, 1.75),
        &[ring(23.0, 0.0, 0.6), ring(24.4, 0.0, 0.6)],
        Tone::Plate,
    ),
    // The shoulder lame.
    Part::loft(
        (-7.95, -4.05),
        (-1.95, 1.95),
        &[
            ring(24.7, 0.0, 0.7),
            ring(25.4, 0.0, 0.7),
            ring(26.0, 0.7, 0.9),
        ],
        Tone::Plate,
    ),
];

const RIGHT_VAMBRACE: [Part; 5] = mirrored(LEFT_VAMBRACE);

/// The left greave: cell `x` −4.5 to −0.5, `y` 2.5–15.5, `z` ±3.5, over a leg of −4 to −1, ±3.
///
/// An ankle flare inside the top of the boot, a shin plate with a ridge down its front, a knee
/// cop that bulges forward and leaves the back of the knee open, and a lame over the thigh.
const LEFT_GREAVE: [Part; 6] = [
    Part::loft(
        (-4.32, -0.68),
        (-3.32, 3.32),
        &[ring(2.7, 0.0, 0.6), ring(15.1, 0.0, 0.6)],
        Tone::Recess,
    ),
    // The ankle flare.
    Part::loft(
        (-4.5, -0.5),
        (-3.5, 3.5),
        &[ring(2.5, 0.0, 1.0), ring(4.7, 0.3, 0.9)],
        Tone::Plate,
    ),
    // The shin plate.
    Part::loft(
        (-4.4, -0.6),
        (-3.4, 3.4),
        &[ring(5.0, 0.0, 0.7), ring(11.4, 0.0, 0.7)],
        Tone::Plate,
    ),
    // The ridge down the shin, to the front of the cell.
    Part::block((-2.85, -2.15), (5.4, 11.2), (3.2, 3.5), Tone::Plate),
    // The knee cop.
    Part::loft(
        (-4.5, -0.5),
        (-2.8, 3.5),
        &[
            ring(11.7, 0.2, 0.9),
            ring(12.6, 0.0, 1.0),
            ring(13.5, 0.0, 1.0),
            ring(14.1, 0.35, 1.0),
        ],
        Tone::Plate,
    ),
    // The thigh lame, closing under the fauld.
    Part::loft(
        (-4.4, -0.6),
        (-3.4, 3.4),
        &[ring(14.4, 0.0, 0.7), ring(15.5, 0.1, 0.7)],
        Tone::Plate,
    ),
];

const RIGHT_GREAVE: [Part; 6] = mirrored(LEFT_GREAVE);

#[cfg(test)]
mod tests {
    use super::super::super::appearance::{BodyPart, placed};
    use super::*;
    use bevy::prelude::Vec3;

    /// **The slits are where the eyes are.** A point just in front of each eye is covered by
    /// the dark visor backing and by no plate, so from the front a player sees a slit exactly
    /// where the face under it looks out.
    #[test]
    fn the_helm_slits_open_in_front_of_the_eyes() {
        use super::super::super::appearance::{BodyPiece, NOTCH_XZ, NOTCH_Y, piece_boxes};
        use crate::net::HairModel;

        for eye in piece_boxes(BodyPiece::Eyes, HairModel::Shaved) {
            let placed_eye = placed(BodyPart::Eyes, *eye);
            let x = placed_eye.centre.x / NOTCH_XZ;
            let y = placed_eye.centre.y / NOTCH_Y;
            let in_front = Vec3::new(x, y, 4.95);
            let covering: Vec<&Part> = HELM
                .iter()
                .filter(|part| {
                    let rings = part.rings();
                    (part.x.0..=part.x.1).contains(&in_front.x)
                        && (rings[0].y..=rings[rings.len() - 1].y).contains(&in_front.y)
                        && part.z.1 >= 4.75
                })
                .collect();
            assert!(
                covering.iter().all(|part| part.tone == Tone::Recess),
                "a plate closes the slit in front of the eye at ({x}, {y}): {covering:?}"
            );
            assert!(
                covering.iter().any(|part| part.tone == Tone::Recess),
                "nothing dark shows through the slit at ({x}, {y})"
            );
        }
    }
}
