//! A character's body, as the boxes an [`Appearance`] colours.
//!
//! **One description, two renderers — the relationship [`ItemShape`] already has.** The
//! character screen draws these boxes flat, as `bevy_ui` nodes seen head-on, so a player
//! can see what they are choosing before they enter the world; [`super`] builds the same
//! boxes as meshes for the bodies the snapshots drive. Two tables would be two answers to
//! "what does a shirt colour cover", and the first thing two answers do is disagree.
//!
//! **Nothing here is a gameplay fact.** The collision box is the server's and is
//! `PLAYER_WIDTH` × `PLAYER_HEIGHT` whatever a character looks like — see
//! [`super::constants`] — so a notch is a fraction *of* that box rather than a size of its
//! own. A character with more hair is not a taller character.
//!
//! # The grain
//!
//! One notch is a twelfth of the collided footprint and a thirty-sixth of its height,
//! which is the same length on both axes: 0.05 blocks, a twentieth of a world block. The
//! terrain is cut at one block and a body at a twentieth of one — fine enough for a fist,
//! coarse enough that nothing ever reads as smooth, and the reason the figure looks like
//! something this world's terrain was cut from rather than something licensed into it.
//!
//! # Four rules the numbers below obey
//!
//! 1. **Parts interpenetrate; they never merely touch.** A boot swallows a leg, a leg the
//!    tunic, the neck both. **Nothing checks this one**, deliberately: what a player would
//!    see is rule 2 failing, and two parts *do* legitimately meet along an edge — the
//!    trousers and a fist, at the hip — so a test could not tell a lapse from an exception
//!    without a list nobody would keep.
//! 2. **No two faces of different colours land on the same plane where they overlap.**
//!    Coplanar faces of different materials fight for the depth buffer and flicker at
//!    distance. `no_two_colours_share_a_plane` checks it rather than hoping.
//! 3. **Detail sits half a notch proud of what it wraps.** The hair on every face, the
//!    eyes on the face they look out of — the hat-layer trick every blocky model has used
//!    since Minecraft, and what lets a cap wrap a head without sharing a plane with it.
//!    See [`Layer`].
//! 4. **The body keeps the box the server collides**, from the boots to the crown. What
//!    reaches past it is the arm and the hair, and neither is collided, because neither is
//!    a gameplay fact: a sleeve by a notch on each side and a fist by two, and a topknot
//!    three and a half notches above the crown. Twelve notches across cannot hold a torso,
//!    two legs and two visible arms; Minecraft's own model runs four times further past
//!    its hitbox than this one does. `the_body_keeps_the_box_the_server_collides`
//!    is the table of what may leave it and by how much.
//!
//! # The axes
//!
//! `y` is measured from the feet up, `x` to the character's right, and **`z` along the way
//! they face**. That last one is the model sheet's convention and the *opposite* of Bevy's,
//! where a body at yaw 0 faces `-Z`. It is reconciled in exactly one place — [`placed`] —
//! so the numbers below can be read against the sheet they came from and no caller has to
//! remember a sign.
//!
//! [`ItemShape`]: super::ItemShape

use bevy::prelude::Vec3;

use super::constants::{PLAYER_HEIGHT, PLAYER_WIDTH};
use crate::net::{Appearance, HairModel};

/// How many notches the collided footprint is across, and how many its height is up.
///
/// The grid, and the only two numbers here that are not read out of a box: everything
/// else is notches, and a notch is these two divided into the server's box.
const NOTCHES_ACROSS: f32 = 12.0;
const NOTCHES_UP: f32 = 36.0;

/// One notch, in blocks, across the footprint and up the body.
///
/// Two constants rather than one because they are derived from two of the server's
/// numbers, and deriving both from either would be this file holding a size of its own.
/// They are the same length, and `the_grid_is_square` is what says so.
pub const NOTCH_XZ: f32 = PLAYER_WIDTH / NOTCHES_ACROSS;
pub const NOTCH_Y: f32 = PLAYER_HEIGHT / NOTCHES_UP;

/// How far a detail layer stands off what it wraps, in notches.
///
/// Half a notch, which is the smallest offset this grid can express and the largest one
/// that still reads as *on* the head rather than floating over it.
const PROUD: f32 = 0.5;

/// The dark a pair of eyes is drawn in, as `0x00RRGGBB`.
///
/// **A constant of the model rather than a field on the wire.** The contract carries five
/// colours and `schemas/common.fbs` is explicit that a sixth would be a colour the server
/// stores; the eyes are the one part nobody picks, so they are the one part whose colour
/// this side is entitled to decide. Dark enough to read against every skin in the
/// palette, which is the whole of what it has to do.
pub const EYE_COLOUR: u32 = 0x0014_100E;

/// One part of the body, and which of an appearance's colours it takes.
///
/// Six: the five the contract carries — `schemas/common.fbs` has four worn colours plus
/// the hair's — and the eyes, which take [`EYE_COLOUR`] because nobody picks them. The
/// hands are the head's colour rather than a part of their own, which is the same
/// decision the server's own description of an appearance records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyPart {
    /// Head, neck and fists: the skin colour.
    Skin,
    /// The shirt, tunic or coat over the torso, and the sleeves.
    Shirt,
    /// Trousers, breeches or leggings.
    Trousers,
    /// Footwear.
    Shoes,
    /// Hair, whose *shape* is the model and whose colour is its own.
    Hair,
    /// Two of them, one notch each. The one part of a body nobody chooses.
    Eyes,
}

impl BodyPart {
    /// Every part, in the order they are drawn back to front for a viewer standing in
    /// front of the character.
    ///
    /// A hand-written list for the reason `HairModel::ALL` is one — no stable Rust
    /// enumerates variants. **It is not a depth order**, and must not be read as one: the
    /// hair falls behind the shoulders on two models and in front of the face on none,
    /// so what decides which box covers which is the box's own `z`. This order settles
    /// the ties, and the eyes are last so nothing is ever drawn over a face.
    pub const IN_DRAWING_ORDER: [Self; 6] = [
        Self::Shoes,
        Self::Trousers,
        Self::Shirt,
        Self::Skin,
        Self::Hair,
        Self::Eyes,
    ];

    /// The five a player chooses, in the order the character screen offers them.
    ///
    /// [`Self::Eyes`] is deliberately absent: a swatch row is what a *character* wears,
    /// and a colour nobody picked is not one of those. Same table, one reader fewer.
    pub const WORN: [Self; 5] = [
        Self::Skin,
        Self::Shirt,
        Self::Trousers,
        Self::Shoes,
        Self::Hair,
    ];

    /// The colour this part takes out of one appearance.
    pub const fn colour(self, appearance: Appearance) -> u32 {
        match self {
            Self::Skin => appearance.skin_color(),
            Self::Shirt => appearance.shirt_color(),
            Self::Trousers => appearance.trousers_color(),
            Self::Shoes => appearance.shoes_color(),
            Self::Hair => appearance.hair_color(),
            Self::Eyes => EYE_COLOUR,
        }
    }

    /// How this part sits against what is under it. See [`Layer`].
    const fn layer(self) -> Layer {
        match self {
            Self::Skin | Self::Shirt | Self::Trousers | Self::Shoes => Layer::Flush,
            Self::Hair => Layer::Wrapping,
            Self::Eyes => Layer::Facing,
        }
    }
}

/// How a part sits against what it wraps — rule 3 in this module's documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// The box is exactly what the table says. Everything the body is built from.
    Flush,
    /// Half a notch proud on every face, so it wraps what is under it. The hair.
    Wrapping,
    /// Half a notch proud of the face and of nothing else. The eyes — a dot on a cheek
    /// rather than a bead stuck to it.
    Facing,
}

/// One box of the rig, in notches: the low and high bound on each axis.
///
/// `i8`, because the whole figure fits between -8 and 39 and a type that cannot hold a
/// coordinate this grid does not have is one fewer thing to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartBox {
    /// Low and high bound to the character's right.
    pub x: (i8, i8),
    /// Low and high bound above the feet. `0` is the ground they stand on.
    pub y: (i8, i8),
    /// Low and high bound along the way they face. **Positive is forwards** — see this
    /// module's documentation, and [`placed`], which is where that meets Bevy's axes.
    pub z: (i8, i8),
}

/// One box of the rig, in blocks, placed relative to the feet and in Bevy's axes.
///
/// What both renderers actually consume: the mesh builder makes a `Cuboid` of `size` and
/// translates it by `centre`, and the preview reads the same two values and throws the
/// depth away.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedBox {
    /// The box's extent on each axis.
    pub size: Vec3,
    /// Its centre, measured from the point the feet stand on.
    pub centre: Vec3,
}

/// Where one box of one part sits, in blocks, relative to the feet.
///
/// **The one place the model sheet's axes meet Bevy's**, and the one place rule 3 is
/// applied: a wrapping part is grown half a notch on every face and a facing part half a
/// notch forwards, so the hair never shares a plane with the skull and the eyes never
/// share one with the face.
pub fn placed(part: BodyPart, cell: PartBox) -> PlacedBox {
    let (grow, forward) = match part.layer() {
        Layer::Flush => (0.0, 0.0),
        Layer::Wrapping => (PROUD, 0.0),
        Layer::Facing => (0.0, PROUD),
    };

    let x = (f32::from(cell.x.0) - grow, f32::from(cell.x.1) + grow);
    let y = (f32::from(cell.y.0) - grow, f32::from(cell.y.1) + grow);
    let z = (
        f32::from(cell.z.0) - grow,
        f32::from(cell.z.1) + grow + forward,
    );

    PlacedBox {
        size: Vec3::new(
            (x.1 - x.0) * NOTCH_XZ,
            (y.1 - y.0) * NOTCH_Y,
            (z.1 - z.0) * NOTCH_XZ,
        ),
        centre: Vec3::new(
            (x.0 + x.1) / 2.0 * NOTCH_XZ,
            (y.0 + y.1) / 2.0 * NOTCH_Y,
            // The sheet measures forwards as +z and a body faces -Z here. One negation,
            // in one place, so every number below reads against the sheet it came from.
            -(z.0 + z.1) / 2.0 * NOTCH_XZ,
        ),
    }
}

/// The boxes one part is drawn from.
///
/// Total over [`BodyPart`], and the hair model is a parameter rather than a second
/// function for exactly that reason: [`BodyPart::Hair`] is a part like any other, and a
/// caller that had to remember to ask somewhere else for it is a caller that will forget.
/// Every other part ignores the model.
pub const fn boxes(part: BodyPart, hair: HairModel) -> &'static [PartBox] {
    match part {
        BodyPart::Shoes => &SHOES,
        BodyPart::Trousers => &TROUSERS,
        BodyPart::Shirt => &SHIRT,
        BodyPart::Skin => &SKIN,
        BodyPart::Eyes => &EYES,
        BodyPart::Hair => hair_boxes(hair),
    }
}

/// The smallest box that holds every part of every body, measured from the feet.
///
/// **Wider and taller than the box the server collides**, because two parts deliberately
/// leave it: the fists sideways and the topknot upwards. A renderer that framed a
/// character by the collided box would clip both, so the one that has to fit a whole
/// person into a panel asks the rig how big a person can get instead of copying a number
/// out of it.
pub fn envelope() -> PlacedBox {
    let mut low = Vec3::MAX;
    let mut high = Vec3::MIN;

    for model in HairModel::ALL {
        for part in BodyPart::IN_DRAWING_ORDER {
            for cell in boxes(part, model) {
                let box_ = placed(part, *cell);
                low = low.min(box_.centre - box_.size / 2.0);
                high = high.max(box_.centre + box_.size / 2.0);
            }
        }
    }

    PlacedBox {
        size: high - low,
        centre: (low + high) / 2.0,
    }
}

/// The boxes one hair model is drawn from, which is the whole of what a hair model *is*
/// on this side.
///
/// The contract carries a model rather than a shape — `schemas/common.fbs` says a colour
/// is a value both sides can hold without agreeing on any asset and a shape is not — so
/// this is the client's own reading of five names, and the only thing it has to be is
/// five silhouettes a player can tell apart at the distance where telling people apart
/// matters.
///
/// A shaved head still gets a box, because it still has stubble: the contract says the
/// hair colour is read whatever the model is, and a model that drew nothing would make
/// one of the five choices invisible and read as a missing asset rather than as a choice.
pub const fn hair_boxes(model: HairModel) -> &'static [PartBox] {
    match model {
        HairModel::Shaved => &SHAVED,
        HairModel::Cropped => &CROPPED,
        HairModel::Braided => &BRAIDED,
        HairModel::Loose => &LOOSE,
        HairModel::Topknot => &TOPKNOT,
    }
}

// ---------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------
//
// Thirteen boxes and a haircut — seventeen in all wearing the loosest of them, fourteen
// with a shaved head. Every one is in notches, feet at y = 0, +x to the character's right
// and +z the way they face, and every one was drawn before it was written down, which is
// why each carries the reason it is the size it is.

/// Feet together, the way a toy figure stands, and two notches of toe in front of the
/// legs — which from above, where a player is when somebody is below them on a slope, is
/// the arrow that says which way that somebody is pointed.
const SHOES: [PartBox; 2] = [
    PartBox {
        x: (-5, 0),
        y: (0, 4),
        z: (-4, 5),
    },
    PartBox {
        x: (0, 5),
        y: (0, 4),
        z: (-4, 5),
    },
];

/// Two legs with a two-notch slot between them, each sunk a notch into its boot.
const TROUSERS: [PartBox; 2] = [
    PartBox {
        x: (-4, -1),
        y: (3, 15),
        z: (-3, 3),
    },
    PartBox {
        x: (1, 4),
        y: (3, 15),
        z: (-3, 3),
    },
];

/// A torso that overhangs the legs by a notch all round — the tunic hem, which reads as a
/// belt without spending a colour on one, and the wire carries five colours and not six —
/// and two thin sleeves hung from inside the shoulders.
const SHIRT: [PartBox; 3] = [
    PartBox {
        x: (-5, 5),
        y: (14, 25),
        z: (-4, 4),
    },
    PartBox {
        x: (-7, -5),
        y: (18, 25),
        z: (-1, 1),
    },
    PartBox {
        x: (5, 7),
        y: (18, 25),
        z: (-1, 1),
    },
];

/// The neck, of which two notches show; the head, eight notches and 22% of the height;
/// and two blunt fists at the hips, wider than the sleeves they hang out of.
///
/// The fists reach two notches past the collided footprint on each side, where the sleeves
/// above them reach one. Nothing collides a hand, and twelve notches across cannot hold a
/// torso, two legs and two visible arms.
const SKIN: [PartBox; 4] = [
    PartBox {
        x: (-2, 2),
        y: (23, 28),
        z: (-2, 2),
    },
    PartBox {
        x: (-4, 4),
        y: (27, 35),
        z: (-4, 4),
    },
    PartBox {
        x: (-8, -4),
        y: (15, 19),
        z: (-2, 2),
    },
    PartBox {
        x: (4, 8),
        y: (15, 19),
        z: (-2, 2),
    },
];

/// Where a world-space item is held, at the centre of the character's right fist.
///
/// Derived from the same box the rig draws rather than copying its notches into the
/// renderer. A later arm pose can therefore move the fist and its item from one
/// attachment point instead of reconciling two model sheets.
pub fn held_item_anchor() -> Vec3 {
    placed(BodyPart::Skin, SKIN[3]).centre
}

/// One notch each, a notch and a half apart, which is where eyes are.
///
/// The smallest thing this grid can say, and the thing that turns a mannequin into
/// somebody: a face reads at four times the distance a silhouette does.
const EYES: [PartBox; 2] = [
    PartBox {
        x: (-2, -1),
        y: (30, 31),
        z: (3, 4),
    },
    PartBox {
        x: (1, 2),
        y: (30, 31),
        z: (3, 4),
    },
];

/// A two-notch skullcap and nothing else. Stubble reads as a choice; an absent box would
/// read as a missing asset.
const SHAVED: [PartBox; 1] = [PartBox {
    x: (-4, 4),
    y: (34, 36),
    z: (-4, 4),
}];

/// A full cap and a short nape: the default, and the one that says nothing in particular
/// about the person wearing it.
const CROPPED: [PartBox; 2] = [
    PartBox {
        x: (-4, 4),
        y: (32, 36),
        z: (-4, 4),
    },
    PartBox {
        x: (-4, 4),
        y: (28, 33),
        z: (-5, -3),
    },
];

/// Two side braids to the jaw and one down the spine. The Norse one, and the only model
/// that shows from behind at any distance.
const BRAIDED: [PartBox; 4] = [
    PartBox {
        x: (-4, 4),
        y: (32, 36),
        z: (-4, 4),
    },
    PartBox {
        x: (-5, -3),
        y: (27, 33),
        z: (-2, 2),
    },
    PartBox {
        x: (3, 5),
        y: (27, 33),
        z: (-2, 2),
    },
    PartBox {
        x: (-1, 1),
        y: (20, 33),
        z: (-6, -4),
    },
];

/// A curtain over the shoulders and panels that frame the face to the jaw. The broadest
/// head in the set, and the one that changes the silhouette most.
const LOOSE: [PartBox; 4] = [
    PartBox {
        x: (-4, 4),
        y: (32, 36),
        z: (-4, 4),
    },
    PartBox {
        x: (-5, 5),
        y: (23, 33),
        z: (-6, -4),
    },
    PartBox {
        x: (-5, -3),
        y: (27, 33),
        z: (-4, 4),
    },
    PartBox {
        x: (3, 5),
        y: (27, 33),
        z: (-4, 4),
    },
];

/// Tight to the skull, then a knot standing three notches above the box the server
/// collides. The one part of the model that leaves that box upwards — and hair is not
/// collided, so nothing about the physics changes when somebody grows some.
const TOPKNOT: [PartBox; 2] = [
    PartBox {
        x: (-4, 4),
        y: (33, 35),
        z: (-4, 4),
    },
    PartBox {
        x: (-2, 2),
        y: (34, 39),
        z: (-2, 2),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn an_appearance() -> Appearance {
        Appearance::new(
            0x00C6_8642,
            0x008C_3B2B,
            0x003B_3226,
            0x002A_211B,
            HairModel::Braided,
            0x006B_4423,
        )
        .expect("every colour is inside the contract's range")
    }

    /// Every box a body draws, with the part it belongs to, for one appearance.
    fn drawn(appearance: Appearance) -> Vec<(BodyPart, PlacedBox)> {
        BodyPart::IN_DRAWING_ORDER
            .into_iter()
            .flat_map(|part| {
                boxes(part, appearance.hair_model())
                    .iter()
                    .map(move |cell| (part, placed(part, *cell)))
            })
            .collect()
    }

    /// Whether two spans overlap over a positive length.
    ///
    /// Strict on purpose. Two coplanar faces fight only where they cover the *same area*,
    /// and boxes that meet along an edge — the trousers and a fist do, at the hip — cover
    /// none of it. Reading a shared edge as an overlap would report a flicker that cannot
    /// happen and, worse, would have to be silenced somewhere.
    fn overlaps(a: (f32, f32), b: (f32, f32)) -> bool {
        a.0 < b.1 && b.0 < a.1
    }

    /// The extents of one placed box, as (low, high) per axis.
    fn spans(placed: PlacedBox) -> [(f32, f32); 3] {
        [
            (
                placed.centre.x - placed.size.x / 2.0,
                placed.centre.x + placed.size.x / 2.0,
            ),
            (
                placed.centre.y - placed.size.y / 2.0,
                placed.centre.y + placed.size.y / 2.0,
            ),
            (
                placed.centre.z - placed.size.z / 2.0,
                placed.centre.z + placed.size.z / 2.0,
            ),
        ]
    }

    /// A notch is the same length across the body as it is up it.
    ///
    /// A test rather than a `const` assertion, which is what the relationships in
    /// [`super::super::constants`] get: those are properties of the build, where this one
    /// is arithmetic on two of the server's numbers and needs a tolerance to be
    /// answerable at all.
    #[test]
    fn the_grid_is_square() {
        assert!(
            (NOTCH_XZ - NOTCH_Y).abs() < 1e-6,
            "a notch is {NOTCH_XZ} across and {NOTCH_Y} up: the rig is authored on one grid"
        );
    }

    /// How far past the box the server collides each part is allowed to reach, in
    /// notches: sideways, upwards, and front to back.
    ///
    /// **The table is the point of the test.** "Nothing leaves the box" would be false and
    /// "something leaves the box" would be unfalsifiable; what is worth pinning is exactly
    /// which parts leave it and by how much, because every entry is a decision somebody
    /// made and could undo by moving one number.
    fn allowance(part: BodyPart) -> (f32, f32, f32) {
        match part {
            // Everything a player stands on or in stays inside what the server collides.
            BodyPart::Shoes | BodyPart::Trousers | BodyPart::Eyes => (0.0, 0.0, 0.0),
            // The arm. A sleeve is a notch outside the footprint and the fist below it is
            // two, because twelve notches across cannot hold a torso, two legs and two
            // visible arms — and nothing collides a hand.
            BodyPart::Shirt => (1.0, 0.0, 0.0),
            BodyPart::Skin => (2.0, 0.0, 0.0),
            // Hair is not collided at all. The topknot stands three notches above the
            // crown and its wrapping layer half a notch more; the same layer is what puts
            // a curtain half a notch behind the back.
            BodyPart::Hair => (0.0, 3.5, 0.5),
        }
    }

    /// Every part keeps the box the server collides, except where [`allowance`] says it
    /// does not.
    ///
    /// The collided box is the *gameplay* fact here — nothing in this file changes it, and
    /// this is what says so. A character standing below the ground would be the loudest
    /// failure and is checked with no allowance at all: there is no reason for one.
    #[test]
    fn the_body_keeps_the_box_the_server_collides() {
        let half = PLAYER_WIDTH / 2.0;

        for model in HairModel::ALL {
            let worn = Appearance::new(0, 0, 0, 0, model, 0).expect("black is a colour");
            for (part, box_) in drawn(worn) {
                let [x, y, z] = spans(box_);
                let (sideways, up, deep) = allowance(part);
                let (sideways, up, deep) = (sideways * NOTCH_XZ, up * NOTCH_Y, deep * NOTCH_XZ);

                assert!(y.0 >= -f32::EPSILON, "{part:?} reaches below the ground");
                assert!(
                    y.1 <= PLAYER_HEIGHT + up + f32::EPSILON,
                    "{part:?} reaches {} above the crown, where {up} is allowed",
                    y.1 - PLAYER_HEIGHT
                );
                assert!(
                    x.0 >= -half - sideways - f32::EPSILON && x.1 <= half + sideways + f32::EPSILON,
                    "{part:?} spans {x:?} sideways, where {sideways} past {half} is allowed"
                );
                assert!(
                    z.0 >= -half - deep - f32::EPSILON && z.1 <= half + deep + f32::EPSILON,
                    "{part:?} spans {z:?} front to back, where {deep} past {half} is allowed"
                );
            }
        }
    }

    /// No two faces of different colours land on the same plane where they overlap.
    ///
    /// **Rule 2, and the reason rule 3 exists.** Two coplanar faces of different materials
    /// fight for the depth buffer, and what a player sees is a seam that flickers as they
    /// walk — worst at the distance where a body is smallest and hardest to read. It is a
    /// property of the numbers rather than of the renderer, so it is checked here.
    ///
    /// What counts as an overlap is a *positive* area — see [`overlaps`], and the edge two
    /// parts legitimately meet along.
    #[test]
    fn no_two_colours_share_a_plane() {
        for model in HairModel::ALL {
            let worn = Appearance::new(0, 0, 0, 0, model, 0).expect("black is a colour");
            let all = drawn(worn);

            for (i, (part, one)) in all.iter().enumerate() {
                for (other, two) in &all[i + 1..] {
                    if part == other {
                        continue;
                    }
                    let (a, b) = (spans(*one), spans(*two));
                    for axis in 0..3 {
                        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
                        if !overlaps(a[u], b[u]) || !overlaps(a[v], b[v]) {
                            continue;
                        }
                        for side in [a[axis].0, a[axis].1] {
                            for face in [b[axis].0, b[axis].1] {
                                assert!(
                                    (side - face).abs() > f32::EPSILON,
                                    "{part:?} and {other:?} share the plane {side} on axis \
                                     {axis} while wearing {model:?}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Every part takes its own colour, and no two take the same field. A body drawn
    /// with the shirt colour on the legs is the failure this catches, and it is one a
    /// screenshot would show and a type would not.
    #[test]
    fn each_part_takes_the_colour_it_is_named_for() {
        let worn = an_appearance();

        assert_eq!(BodyPart::Skin.colour(worn), worn.skin_color());
        assert_eq!(BodyPart::Shirt.colour(worn), worn.shirt_color());
        assert_eq!(BodyPart::Trousers.colour(worn), worn.trousers_color());
        assert_eq!(BodyPart::Shoes.colour(worn), worn.shoes_color());
        assert_eq!(BodyPart::Hair.colour(worn), worn.hair_color());
    }

    /// The eyes are the same dark whatever a player chose, because nobody chose them.
    ///
    /// The half of the test above that cannot be written as "its own field": a sixth
    /// colour on the wire is exactly what this part is not, and a build that started
    /// reading one out of the appearance would fail here.
    #[test]
    fn the_eyes_are_not_a_colour_anybody_picked() {
        let worn = an_appearance();
        let other = Appearance::new(0, 0, 0, 0, HairModel::Shaved, 0).expect("black is a colour");

        assert_eq!(BodyPart::Eyes.colour(worn), EYE_COLOUR);
        assert_eq!(BodyPart::Eyes.colour(other), EYE_COLOUR);
        assert!(
            !BodyPart::WORN.contains(&BodyPart::Eyes),
            "a swatch row is what a character wears, and nobody wears these"
        );
    }

    /// Every hair model draws something, and no two draw the same thing.
    ///
    /// The first half is the contract's — a shaved head still has stubble, and the hair
    /// colour is read whatever the model is — and the second is what makes the choice a
    /// choice: five names that all drew one silhouette would be one option wearing five
    /// labels.
    #[test]
    fn every_hair_model_is_a_silhouette_of_its_own() {
        let mut seen: Vec<(HairModel, Vec<PlacedBox>)> = Vec::new();
        for model in HairModel::ALL {
            let cut: Vec<PlacedBox> = hair_boxes(model)
                .iter()
                .map(|cell| placed(BodyPart::Hair, *cell))
                .collect();

            assert!(!cut.is_empty(), "{model:?} draws nothing at all");
            assert!(
                cut.iter().all(|box_| box_.size.min_element() > 0.0),
                "{model:?} has a box with no volume"
            );
            for (other, drawn) in &seen {
                assert_ne!(*drawn, cut, "{model:?} and {other:?} draw the same hair");
            }
            seen.push((model, cut));
        }
    }

    /// The frame a whole person fits in is bigger than the box the server collides, in
    /// exactly the two directions the model sheet says something leaves it.
    ///
    /// A preview sized from the collided box would cut the knuckles off every character
    /// and the knot off one of them, which is a thing nobody would notice until they
    /// chose that haircut.
    #[test]
    fn the_envelope_holds_the_parts_that_leave_the_collided_box() {
        let frame = envelope();

        assert!(
            frame.size.x > PLAYER_WIDTH,
            "the fists leave the footprint, so the frame is wider than it"
        );
        assert!(
            frame.size.y > PLAYER_HEIGHT,
            "the topknot leaves the box upwards, so the frame is taller than it"
        );
        assert!(
            (frame.centre.y - frame.size.y / 2.0).abs() < f32::EPSILON,
            "a character stands on the bottom of the frame"
        );
    }

    /// The hair a body is drawn with is the model that appearance names, which is what
    /// makes the preview live rather than a picture of one character.
    #[test]
    fn the_body_wears_the_hair_the_appearance_names() {
        for model in HairModel::ALL {
            let worn = Appearance::new(0, 0, 0, 0, model, 0).expect("black is a colour");

            assert_eq!(boxes(BodyPart::Hair, worn.hair_model()), hair_boxes(model));
        }
    }

    /// A viewer in front of a character sees the face and not the back of the head.
    ///
    /// **This is the one assertion that catches the model sheet's `+z` being carried into
    /// Bevy's axes without the negation [`placed`] applies**, and it is why it survived
    /// #181. It used to read a `PlacedBox::nearness` — a painter's sort key the flat
    /// preview needed and a depth buffer does not — and that method went with the flat
    /// preview. The property it was really testing is about the rig, not about a renderer,
    /// so it is asserted on the axis directly: a body faces -Z, so what is nearer a viewer
    /// in front of it has the *lower* z.
    #[test]
    fn what_faces_the_viewer_is_nearer_than_what_is_behind_it() {
        let eye = placed(BodyPart::Eyes, EYES[0]);
        let head = placed(BodyPart::Skin, SKIN[1]);
        let nape = placed(BodyPart::Hair, CROPPED[1]);

        assert!(
            eye.centre.z < head.centre.z,
            "the eyes sit proud of the face they look out of"
        );
        assert!(
            head.centre.z < nape.centre.z,
            "the back of the head is behind the front of it"
        );
    }
}
