//! The leather set, sculpted: a stitched cap, a strapped jerkin, bracers and padded leggings.
//!
//! **Worked hide, not plate.** Where the rusty set spends its cell on hard plates with a dark
//! core between them, leather is one soft body of hide per segment with darker leather laid over
//! it: straps, lacing, stitched seams and ties. The lighter hide is [`HIDE`], the darker straps
//! and stitching [`STRAP`], and those two are the whole palette. Both are vertex shades of the
//! registry's leather colour, so the set needs no livery of its own.
//!
//! **Every number is a notch of the model sheet**, as in [`super::rusty`]: `+x` to the
//! character's right, `y` from the feet, `+z` forwards, every part inside the cell
//! `appearance::placed_armour` gives its segment. Fractions of a notch keep every face off the
//! body's whole and half notch planes, which `no_sculpted_face_shares_a_plane_with_the_body`
//! checks.
//!
//! **The cap leaves the face open.** A cap with a brim is not a helm, so the front of the head
//! between the side flaps and under the brim is [`FACE`], an opening the containment test is
//! told about rather than a gap it has to miss. The hair still hides: the cap is cut inside the
//! helmet's cell, and every hair model reaches above that cell.

use super::super::appearance::ArmourSegment;
#[cfg(test)]
use super::Opening;
use super::{Part, Tone, mirrored, ring};

/// The hide itself, the lighter of the set's two tones.
const HIDE: Tone = Tone::Plate;

/// The straps, laces, ties and stitching laid over the hide, the darker of the two.
const STRAP: Tone = Tone::Strap;

/// A cap is cut inside the helmet's cell, and every hair model reaches above it.
pub(super) const HIDES_HAIR: bool = true;

/// Where the cap shows the face: between the side flaps, under the brim and above the chin
/// strap, forwards of the cap's own shell. A declaration to the containment test, which is its
/// only reader.
#[cfg(test)]
pub(super) const FACE: Opening = Opening {
    x: (-4.0, 4.0),
    y: (26.9, 31.8),
    z: (3.7, 5.0),
};

/// The parts one segment of the leather set is cut from.
pub(super) fn parts(segment: ArmourSegment) -> &'static [Part] {
    match segment {
        ArmourSegment::Helmet => &CAP,
        ArmourSegment::Torso => &JERKIN,
        ArmourSegment::LeftSleeve => &LEFT_BRACER,
        ArmourSegment::RightSleeve => &RIGHT_BRACER,
        ArmourSegment::LeftGreave => &LEFT_LEGGING,
        ArmourSegment::RightGreave => &RIGHT_LEGGING,
    }
}

/// The places this set leaves open over the body under it.
#[cfg(test)]
pub(super) fn openings(segment: ArmourSegment) -> &'static [Opening] {
    match segment {
        ArmourSegment::Helmet => &[FACE],
        ArmourSegment::Torso
        | ArmourSegment::LeftSleeve
        | ArmourSegment::RightSleeve
        | ArmourSegment::LeftGreave
        | ArmourSegment::RightGreave => &[],
    }
}

/// The cap: cell `x` ±5, `y` 26–36, `z` ±5, over a head of ±4 and eyes at 30–31.
///
/// A soft dome to a folded brim, with a stitched seam front to back over the crown and a
/// stitched edge where the brim folds. Behind the face the hide comes down to the nape, and two
/// side flaps hang over the cheeks, each tied at its foot into a chin strap under the jaw.
const CAP: [Part; 10] = [
    // The dome, from under the brim to the crown, stepping in twice.
    Part::loft(
        (-4.6, 4.6),
        (-4.6, 4.6),
        &[
            ring(31.9, 0.0, 0.9),
            ring(35.0, 0.0, 0.9),
            ring(35.5, 0.7, 1.1),
            ring(35.8, 1.8, 1.2),
        ],
        HIDE,
    ),
    // The shell behind the face: the back of the head and the nape, down to the neck.
    Part::loft(
        (-4.6, 4.6),
        (-4.6, 3.3),
        &[ring(26.0, 0.0, 0.7), ring(32.3, 0.0, 0.7)],
        HIDE,
    ),
    // The brim, tucked in underneath and folded up to the full cell.
    Part::loft(
        (-5.0, 5.0),
        (-5.0, 5.0),
        &[
            ring(31.8, 0.25, 1.2),
            ring(32.4, 0.0, 1.3),
            ring(33.1, 0.0, 1.3),
        ],
        HIDE,
    ),
    // The stitched edge along the top of the fold.
    Part::loft(
        (-4.8, 4.8),
        (-4.8, 4.8),
        &[ring(33.15, 0.0, 1.0), ring(33.45, 0.0, 1.0)],
        STRAP,
    ),
    // The seam over the crown, front to back.
    Part::block((-0.3, 0.3), (35.4, 35.95), (-3.6, 3.6), STRAP),
    // The side flaps over the cheeks, reaching forwards of the shell.
    FLAP,
    FLAP.mirrored(),
    // The ties at the foot of each flap.
    FLAP_TIE,
    FLAP_TIE.mirrored(),
    // The chin strap under the jaw, between the ties.
    Part::block((-4.1, 4.1), (26.1, 26.85), (2.4, 4.4), STRAP),
];

/// The right side flap. Its inner face sits inside the head where it reaches forwards of the
/// shell, so no gap opens between the flap and the cheek.
const FLAP: Part = Part::block((3.85, 4.9), (26.4, 32.0), (-1.4, 3.7), HIDE);

/// The tie across the foot of the right flap.
const FLAP_TIE: Part = Part::block((3.95, 4.97), (26.2, 26.8), (-1.2, 3.5), STRAP);

/// The jerkin: cell `x` ±6, `y` 13–26, `z` ±5, over a tunic of ±5, 14–25, ±4.
///
/// A body of hide sloping in to the shoulders, flaring into a skirt under a belt with a buckle.
/// The front is laced up the middle, and two straps cross the chest over the lacing from each
/// shoulder to the opposite hip, each with a buckle high on the chest. The straps are cut as
/// steps, which is how this grid draws a diagonal.
const JERKIN: [Part; 25] = [
    // The body, sloping in to the shoulders.
    Part::loft(
        (-5.45, 5.45),
        (-4.65, 4.65),
        &[
            ring(13.3, 0.0, 0.6),
            ring(24.8, 0.0, 0.6),
            ring(25.6, 0.8, 0.9),
        ],
        HIDE,
    ),
    // The skirt under the belt, flaring at the hem.
    Part::loft(
        (-5.8, 5.8),
        (-4.85, 4.85),
        &[ring(13.0, 0.0, 1.0), ring(14.6, 0.2, 1.0)],
        HIDE,
    ),
    // The belt and its buckle.
    Part::loft(
        (-5.7, 5.7),
        (-4.8, 4.8),
        &[ring(14.9, 0.0, 0.9), ring(16.1, 0.0, 0.9)],
        STRAP,
    ),
    Part::block((-0.9, 0.9), (14.8, 16.2), (4.5, 4.98), HIDE),
    // The lacing: the slit up the front and four laces across it.
    Part::block((-0.25, 0.25), (16.3, 24.6), (4.45, 4.8), STRAP),
    LACE[0],
    LACE[1],
    LACE[2],
    LACE[3],
    // The two crossed straps, and a buckle on each.
    STRAP_STEPS[0],
    STRAP_STEPS[1],
    STRAP_STEPS[2],
    STRAP_STEPS[3],
    STRAP_STEPS[4],
    STRAP_STEPS[5],
    STRAP_STEPS[6],
    MIRRORED_STEPS[0],
    MIRRORED_STEPS[1],
    MIRRORED_STEPS[2],
    MIRRORED_STEPS[3],
    MIRRORED_STEPS[4],
    MIRRORED_STEPS[5],
    MIRRORED_STEPS[6],
    STRAP_BUCKLE,
    STRAP_BUCKLE.mirrored(),
];

/// The laces across the front slit, clear of the crossing straps.
const LACE: [Part; 4] = [
    Part::block((-0.85, 0.85), (16.9, 17.25), (4.45, 4.75), STRAP),
    Part::block((-0.85, 0.85), (18.2, 18.55), (4.45, 4.75), STRAP),
    Part::block((-0.85, 0.85), (22.3, 22.65), (4.45, 4.75), STRAP),
    Part::block((-0.85, 0.85), (23.7, 24.05), (4.45, 4.75), STRAP),
];

/// One step of the strap from the left shoulder to the right hip: one and a fifth notches down
/// and one and three twentieths across per step, each overlapping the next so the strap reads as
/// one band.
const fn strap_step(step: usize) -> Part {
    let down = 1.2 * step as f32;
    let across = -4.0 + 1.15 * step as f32;
    Part::block(
        (across - 0.75, across + 0.75),
        (23.2 - down, 24.5 - down),
        (4.5, 4.9),
        STRAP,
    )
}

/// The strap from the left shoulder to the right hip, top step first.
const STRAP_STEPS: [Part; 7] = [
    strap_step(0),
    strap_step(1),
    strap_step(2),
    strap_step(3),
    strap_step(4),
    strap_step(5),
    strap_step(6),
];

/// The strap from the right shoulder to the left hip.
const MIRRORED_STEPS: [Part; 7] = mirrored(STRAP_STEPS);

/// The buckle on the left shoulder's strap, standing proud of it high on the chest.
const STRAP_BUCKLE: Part = Part::block((-3.4, -2.3), (22.15, 23.15), (4.6, 5.0), HIDE);

/// The left bracer: cell `x` −8 to −4, `y` 20–26, `z` ±2, over a sleeve of −7 to −5, ±1.
///
/// A sleeve of hide rounded over the shoulder, with a turned cuff at the wrist and two straps
/// round the arm, each fastened by a buckle on the outside.
const LEFT_BRACER: [Part; 6] = [
    Part::loft(
        (-7.45, -4.55),
        (-1.45, 1.45),
        &[
            ring(20.0, 0.0, 0.4),
            ring(25.7, 0.0, 0.4),
            ring(26.0, 0.5, 0.5),
        ],
        HIDE,
    ),
    // The cuff.
    Part::loft(
        (-7.85, -4.15),
        (-1.85, 1.85),
        &[ring(20.0, 0.0, 0.7), ring(20.8, 0.3, 0.7)],
        HIDE,
    ),
    // The two straps.
    Part::loft(
        (-7.7, -4.3),
        (-1.7, 1.7),
        &[ring(21.6, 0.0, 0.6), ring(22.3, 0.0, 0.6)],
        STRAP,
    ),
    Part::loft(
        (-7.7, -4.3),
        (-1.7, 1.7),
        &[ring(23.6, 0.0, 0.6), ring(24.3, 0.0, 0.6)],
        STRAP,
    ),
    // Their buckles, on the outside of the arm.
    Part::block((-7.95, -7.55), (21.7, 22.2), (-0.4, 0.4), HIDE),
    Part::block((-7.95, -7.55), (23.7, 24.2), (-0.4, 0.4), HIDE),
];

const RIGHT_BRACER: [Part; 6] = mirrored(LEFT_BRACER);

/// The left legging: cell `x` −4.5 to −0.5, `y` 2.5–15.5, `z` ±3.5, over a leg of −4 to −1, ±3.
///
/// A leg of hide turned back into a cuff at the boot, a padded knee standing forward of it, a
/// strap under the knee buckled on the outside, and a lace line down the outer leg with laces
/// across it.
const LEFT_LEGGING: [Part; 10] = [
    Part::loft(
        (-4.35, -0.65),
        (-3.35, 3.35),
        &[ring(2.7, 0.0, 0.5), ring(15.3, 0.0, 0.5)],
        HIDE,
    ),
    // The cuff at the boot.
    Part::loft(
        (-4.5, -0.5),
        (-3.5, 3.5),
        &[ring(2.5, 0.0, 0.9), ring(3.6, 0.25, 0.8)],
        HIDE,
    ),
    // The padded knee, rounded top and bottom.
    Part::loft(
        (-4.45, -0.55),
        (-1.0, 3.5),
        &[
            ring(10.6, 0.3, 0.8),
            ring(11.2, 0.0, 0.9),
            ring(12.8, 0.0, 0.9),
            ring(13.4, 0.3, 0.8),
        ],
        HIDE,
    ),
    // The strap under the knee, and its buckle on the outside.
    Part::loft(
        (-4.4, -0.6),
        (-3.45, 3.45),
        &[ring(9.6, 0.0, 0.8), ring(10.3, 0.0, 0.8)],
        STRAP,
    ),
    Part::block((-4.5, -4.15), (9.5, 10.4), (-0.6, 0.6), HIDE),
    // The lace line down the outer leg, and the laces across it.
    Part::block((-4.47, -4.2), (3.8, 15.2), (-0.25, 0.25), STRAP),
    Part::block((-4.49, -4.3), (5.0, 5.3), (-0.55, 0.55), STRAP),
    Part::block((-4.49, -4.3), (6.6, 6.9), (-0.55, 0.55), STRAP),
    Part::block((-4.49, -4.3), (8.2, 8.5), (-0.55, 0.55), STRAP),
    Part::block((-4.49, -4.3), (13.85, 14.15), (-0.55, 0.55), STRAP),
];

const RIGHT_LEGGING: [Part; 10] = mirrored(LEFT_LEGGING);

#[cfg(test)]
mod tests {
    use super::super::super::appearance::placed;
    use super::super::super::appearance::{BodyPart, BodyPiece, NOTCH_XZ, NOTCH_Y, piece_boxes};
    use super::*;
    use crate::net::HairModel;

    /// **Two tones and no third**: every segment shows the hide and the darker leather over
    /// it, and nothing in the set is drawn as a plate's shadowed core.
    #[test]
    fn every_leather_segment_is_hide_and_strap_and_nothing_else() {
        assert!(
            STRAP.shade() < HIDE.shade(),
            "the straps are the darker leather"
        );
        for segment in ArmourSegment::ALL {
            let parts = parts(segment);
            for tone in [HIDE, STRAP] {
                assert!(
                    parts.iter().any(|part| part.tone == tone),
                    "leather {segment:?} has no {tone:?}"
                );
            }
            assert!(
                parts
                    .iter()
                    .all(|part| part.tone == HIDE || part.tone == STRAP),
                "leather {segment:?} draws a third tone"
            );
        }
    }

    /// **The cap shows the face.** A point just in front of each eye is covered by no part at
    /// all, and it lies in the opening the containment test was told about.
    #[test]
    fn the_cap_leaves_the_eyes_open() {
        for eye in piece_boxes(BodyPiece::Eyes, HairModel::Shaved) {
            let placed_eye = placed(BodyPart::Eyes, *eye);
            let (x, y) = (
                placed_eye.centre.x / NOTCH_XZ,
                placed_eye.centre.y / NOTCH_Y,
            );
            let z = 4.75;
            let covering: Vec<&Part> = CAP
                .iter()
                .filter(|part| {
                    let rings = part.rings();
                    (part.x.0..=part.x.1).contains(&x)
                        && (rings[0].y..=rings[rings.len() - 1].y).contains(&y)
                        && (part.z.0..=part.z.1).contains(&z)
                })
                .collect();
            assert!(
                covering.is_empty(),
                "the cap closes over the eye at ({x}, {y}): {covering:?}"
            );
            assert!(
                FACE.holds(bevy::prelude::Vec3::new(x, y, z)),
                "the eye at ({x}, {y}) is outside the face opening"
            );
        }
    }

    /// The brim sits above the eyes and the chin strap below the jaw, so the opening between
    /// them is the face and neither band crosses it.
    #[test]
    fn the_brim_and_the_chin_strap_frame_the_face() {
        let eyes = piece_boxes(BodyPiece::Eyes, HairModel::Shaved);
        let eye_top = eyes
            .iter()
            .map(|eye| f32::from(eye.y.1))
            .fold(f32::MIN, f32::max);
        let brim = CAP[2].rings();
        let chin = CAP[9].rings();
        assert!(brim[0].y > eye_top, "the brim hangs over the eyes");
        assert!(
            (brim[0].y - FACE.y.1).abs() < 1e-5,
            "the brim is the top of the face"
        );
        assert!(chin[1].y < FACE.y.0, "the chin strap reaches into the face");
    }
}
