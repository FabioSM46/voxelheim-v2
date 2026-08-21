//! A character's body, as the parts an [`Appearance`] colours.
//!
//! **One description, two renderers — the relationship [`ItemShape`] already has.** The
//! character screen draws these parts flat, as `bevy_ui` nodes, so a player can see what
//! they are choosing before they enter the world; the issue that gives players a body
//! worth colouring builds the same parts as meshes. Two tables would be two answers to
//! "what does a shirt colour cover", and the first thing two answers do is disagree.
//!
//! **Nothing here is a gameplay fact.** The collision box is the server's and is
//! `PLAYER_WIDTH` × `PLAYER_HEIGHT` whatever a character looks like — see
//! [`super::constants`] — so these are fractions *of* that box rather than sizes of their
//! own. A character with more hair is not a taller character.
//!
//! [`ItemShape`]: super::ItemShape

use crate::net::{Appearance, HairModel};

/// One part of the body, and which of an appearance's colours it takes.
///
/// Five, because that is how many colours the contract carries: `schemas/common.fbs` has
/// four worn colours plus the hair's, and a sixth part would be a part with nothing to
/// paint it. The hands are the head's colour rather than a part of their own, which is
/// the same decision the server's own description of an appearance records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyPart {
    /// Head and hands: the skin colour.
    Skin,
    /// The shirt, tunic or coat over the torso.
    Shirt,
    /// Trousers, breeches or leggings.
    Trousers,
    /// Footwear.
    Shoes,
    /// Hair, whose *shape* is the model and whose colour is its own.
    Hair,
}

impl BodyPart {
    /// Every part, in the order they are drawn back to front: the body first and the
    /// hair over it.
    ///
    /// A hand-written list for the reason `HairModel::ALL` is one — no stable Rust
    /// enumerates variants — and the order is load-bearing rather than incidental: hair
    /// overlaps the head, so a renderer that drew it first would draw a bald character.
    pub const IN_DRAWING_ORDER: [Self; 5] = [
        Self::Shoes,
        Self::Trousers,
        Self::Shirt,
        Self::Skin,
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
        }
    }
}

/// Where one part sits on the body, in fractions of the collision box.
///
/// `bottom` and `top` are measured from the feet up, so `0.0` is the ground a player
/// stands on and `1.0` is the crown; `width` is a fraction of `PLAYER_WIDTH`. Fractions
/// rather than metres because the box belongs to the server: a change to
/// `game.PlayerHeight` should move every part with it rather than leaving five numbers to
/// be found and edited.
///
/// A part may reach past `1.0` — a topknot does — and that is deliberate. Hair is not
/// what the server collides, so nothing about the box moves when somebody grows some.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartExtent {
    pub bottom: f32,
    pub top: f32,
    pub width: f32,
}

/// Where each part of the body sits.
///
/// Proportions of a standing figure, chosen to read at the size a preview panel draws
/// them and to leave the parts distinguishable: feet under trousers, trousers under a
/// shirt that stops at the shoulders, and a head above it.
pub const fn extent(part: BodyPart) -> PartExtent {
    match part {
        BodyPart::Shoes => PartExtent {
            bottom: 0.0,
            top: 0.06,
            width: 0.92,
        },
        BodyPart::Trousers => PartExtent {
            bottom: 0.05,
            top: 0.47,
            width: 0.82,
        },
        BodyPart::Shirt => PartExtent {
            bottom: 0.44,
            top: 0.80,
            width: 1.0,
        },
        BodyPart::Skin => PartExtent {
            bottom: 0.78,
            top: 1.0,
            width: 0.58,
        },
        // The one part whose extent is not fixed: see [`hair_extent`].
        BodyPart::Hair => hair_extent(HairModel::Cropped),
    }
}

/// Where the hair sits, which is the whole of what a hair model *is* on this side.
///
/// The contract carries a model rather than a shape — `schemas/common.fbs` says a colour
/// is a value both sides can hold without agreeing on any asset and a shape is not — so
/// this is the client's own reading of five names, and the only thing it has to be is
/// five silhouettes a player can tell apart.
///
/// A shaved head still gets one, because it still has stubble: the contract says the
/// hair colour is read whatever the model is, and a model that drew nothing would make
/// one of the six choices invisible.
pub const fn hair_extent(model: HairModel) -> PartExtent {
    match model {
        HairModel::Shaved => PartExtent {
            bottom: 0.94,
            top: 1.01,
            width: 0.60,
        },
        HairModel::Cropped => PartExtent {
            bottom: 0.90,
            top: 1.03,
            width: 0.64,
        },
        HairModel::Braided => PartExtent {
            bottom: 0.70,
            top: 1.03,
            width: 0.68,
        },
        HairModel::Loose => PartExtent {
            bottom: 0.62,
            top: 1.03,
            width: 0.74,
        },
        HairModel::Topknot => PartExtent {
            bottom: 0.92,
            top: 1.09,
            width: 0.52,
        },
    }
}

/// The parts of one appearance, back to front, each with its extent and its colour.
///
/// The one function both renderers call, so "which colour covers which part of a body"
/// is answered once. The hair's extent comes from the model rather than from the table,
/// which is why this exists at all instead of two lookups at each call site.
pub fn parts(appearance: Appearance) -> [(BodyPart, PartExtent, u32); 5] {
    BodyPart::IN_DRAWING_ORDER.map(|part| {
        let extent = match part {
            BodyPart::Hair => hair_extent(appearance.hair_model()),
            other => extent(other),
        };
        (part, extent, part.colour(appearance))
    })
}

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

    /// The body covers the whole of the box, with no gap between one part and the next.
    ///
    /// Asserted as an overlap rather than as exact numbers: what matters is that a player
    /// never sees a stripe of background through their own character, and the parts are
    /// deliberately allowed to overlap so a small change to one does not open one.
    #[test]
    fn the_parts_cover_the_body_from_the_ground_to_the_crown() {
        let ordered = [
            BodyPart::Shoes,
            BodyPart::Trousers,
            BodyPart::Shirt,
            BodyPart::Skin,
        ];

        assert_eq!(
            extent(BodyPart::Shoes).bottom,
            0.0,
            "the feet start at the ground"
        );
        assert_eq!(
            extent(BodyPart::Skin).top,
            1.0,
            "the head reaches the crown"
        );
        for pair in ordered.windows(2) {
            let (below, above) = (extent(pair[0]), extent(pair[1]));
            assert!(
                above.bottom <= below.top,
                "{:?} starts at {} and {:?} ends at {}, which leaves a gap",
                pair[1],
                above.bottom,
                pair[0],
                below.top
            );
        }
    }

    /// Every hair model draws something, and no two draw the same thing.
    ///
    /// The first half is the contract's — a shaved head still has stubble, and the hair
    /// colour is read whatever the model is — and the second is what makes the choice a
    /// choice: five names that all drew one silhouette would be one option wearing five
    /// labels.
    #[test]
    fn every_hair_model_is_a_silhouette_of_its_own() {
        let mut seen: Vec<(HairModel, PartExtent)> = Vec::new();
        for model in HairModel::ALL {
            let extent = hair_extent(model);
            assert!(
                extent.top > extent.bottom && extent.width > 0.0,
                "{model:?} draws nothing at all"
            );
            for (other, drawn) in &seen {
                assert!(
                    (drawn.bottom - extent.bottom).abs() > f32::EPSILON
                        || (drawn.top - extent.top).abs() > f32::EPSILON
                        || (drawn.width - extent.width).abs() > f32::EPSILON,
                    "{model:?} and {other:?} draw the same hair"
                );
            }
            seen.push((model, extent));
        }
    }

    /// The hair a body is drawn with is the model that appearance names, which is what
    /// makes the preview live rather than a picture of one character.
    #[test]
    fn the_body_wears_the_hair_the_appearance_names() {
        for model in HairModel::ALL {
            let worn = Appearance::new(0, 0, 0, 0, model, 0).expect("black is a colour");
            let (part, extent, _) = parts(worn)[4];

            assert_eq!(
                part,
                BodyPart::Hair,
                "the hair is drawn last, over the head"
            );
            assert_eq!(extent, hair_extent(model));
        }
    }
}
