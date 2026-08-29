//! What one item id is called, what shape it is held in, and what colour it draws.
//!
//! One row per item, and every reader on this side goes through it: the view model in
//! [`super::hands`] is built from the shape and the colour, `ui`'s pack and hotbar cells
//! draw the colour, the recipe panel spells the name, and a hovered slot reports it.
//!
//! Before this module existed the three facts lived in two tables that did not hold the
//! same items. `hands` owned the shape and the colour — the complete visual opinion, of
//! which only the colour escaped the module — and `super::crafting` owned the name,
//! because the recipe panel needed one first. So the names covered exactly what a recipe
//! mentions: dirt, snow and the rusty sword had none at all, and no test could see that,
//! because the only sweep over the names walked the recipes. A tooltip is the first reader
//! that asks about every item a player can hold, which is what surfaced the gap.
//!
//! **This table is the sole authority on nothing.** `super::combat` reads item ids of its
//! own to decide what a click asks for, `super::structures` reads its own to decide what a
//! place press asks for, `super::inventory` reads one to decide which of two requests a
//! cell click means, and the server reads its registry to decide what it grants. A wrong
//! entry here draws the wrong shape, spells the wrong word, and cannot make an item do
//! anything.

use super::combat::ITEM_RUSTY_SWORD;
use super::crafting::ITEM_WOODEN_SHIELD;
use super::crafting::{
    ITEM_ARROW, ITEM_AXE, ITEM_BOW, ITEM_COOKED_MEAT, ITEM_IRON_CUIRASS, ITEM_IRON_GREAVES,
    ITEM_IRON_HELM, ITEM_IRON_SWORD, ITEM_LEATHER_CAP, ITEM_LEATHER_JERKIN, ITEM_LEATHER_LEGGINGS,
    ITEM_LEATHER_PATCH, ITEM_PICKAXE, ITEM_SHARPENING_STONE, ITEM_SHOVEL, ITEM_WOODEN_SCEPTRE,
};
use super::structures::{ITEM_CAMPFIRE, ITEM_FORGE, ITEM_RUNESTONE, ITEM_TENT};
use crate::world::{BlockId, palette};

// Presentation-only item ids. The server registry remains the sole authority on whether
// any of these can be placed and which block an action actually creates.
//
// These twelve live here because no module *acts* on them — they are ids this client only
// ever draws. Items that a module does act on stay where that module declares them:
// the blade in `super::combat`, the four bundles in `super::structures`, the forge's two
// products and the patch beside them in `super::crafting`. The table below names them from
// there, because one declaration read from several places cannot drift the way two
// declarations of the same number can.
pub(super) const ITEM_STONE: u16 = 1;
pub(super) const ITEM_DIRT: u16 = 2;
pub(super) const ITEM_SNOW: u16 = 3;
pub(super) const ITEM_LOG: u16 = 4;
pub(super) const ITEM_RAW_COAL: u16 = 5;
pub(super) const ITEM_RAW_IRON: u16 = 6;

/// What the dead leave behind.
///
/// Presentation only, exactly as the six above are. The server's registry decides that a
/// vargr leaves a pelt, a draugr leaves bone and a deer leaves meat; nothing on this side
/// routes a click or a key on any of them, which is why they are declared here rather than
/// in the module that would act on them.
///
/// **What two pelts are worked into is no longer one of them.** `ITEM_LEATHER_PATCH` sat
/// in this group until #113, on the strength of a sentence that had stopped being true:
/// `super::inventory`'s `KITS` routes a click on a patch to a mend, so it is declared
/// beside its recipe in `super::crafting` with the other two products this client acts on.
pub(super) const ITEM_BONE: u16 = 13;
pub(super) const ITEM_VARGR_PELT: u16 = 14;
pub(super) const ITEM_RAW_MEAT: u16 = 19;

/// The three blocks worldgen 3 put in the ground: a desert's sand and sandstone,
/// and the gravel patches that break up plains and taiga soil.
///
/// Here for the reason the six block and resource ids above are: nothing on this side
/// *acts* on them. They are placeable — the server's registry gives each of them a block
/// to place — but a place press reads whatever the hotbar holds rather than a list of ids,
/// so this client never spells one of these numbers outside the display table.
pub(super) const ITEM_SAND: u16 = 31;
pub(super) const ITEM_SANDSTONE: u16 = 32;
pub(super) const ITEM_GRAVEL: u16 = 33;

/// The lid a frozen lake is broken off, here for the reason the three above are: nothing on
/// this side acts on it.
///
/// **It was the one item the server had issued that this table had never heard of.** The
/// contiguity sweep below cannot see a *trailing* omission — it derives the expected block
/// from the table's own length — so ice sat at wire id 34 with no row, drawing magenta and
/// reading "unknown item", from the day the server appended it until the day a thirty-fifth
/// item made the hole an interior one. Adding a row for it is what silver's own row costs.
pub(super) const ITEM_ICE: u16 = 34;

/// The coin a draugr carries.
///
/// Declared here rather than in a module that acts on it because nothing on this side does:
/// the inventory window's readout *counts* it, which is reading what the server sent, and no
/// click, key or request is routed on this number. `crate::ui` reaches it through
/// `crate::player`'s re-export for the same reason it reaches [`item_label`] — one
/// declaration read from several places cannot drift the way two of the same number can.
pub(crate) const ITEM_SILVER: u16 = 35;

/// The three materials worldgen 6 builds a settlement out of, here for the reason every
/// block id above is: nothing on this side acts on them either.
pub(super) const ITEM_PLANKS: u16 = 36;
pub(super) const ITEM_COBBLESTONE: u16 = 37;
pub(super) const ITEM_THATCH: u16 = 38;

/// The shapes an item is drawn in.
///
/// Four variants and no "nothing": an empty hand is not an item's shape, so
/// [`super::hands`] spells that `None` and this enum stays total over items. Which is what
/// lets the sweep below assert a shape without also having to exclude a placeholder
/// variant that every material-like item would legitimately share.
///
/// **One vocabulary, two renderers.** [`super::hands`] builds a mesh per variant for the
/// held view model and `ui::icon` builds a flat picture per variant for a pack or hotbar
/// cell. Both read the shape out of the row below rather than deciding one per surface, so
/// what a player sees in a cell is what they see in their hand — and both match on this
/// enum with no wildcard arm, so a fifth variant is drawn twice or it does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemShape {
    /// A cube: what a voxel looks like carried, and what a place press will ask for.
    Block,
    /// A stub of raw material — ore, fuel, a consumable.
    Material,
    /// Long, thin and unmistakably not a cube, so the thing that swings looks different
    /// from the thing that places.
    ///
    /// **A shape is not a capability.** Drawing an item as a blade does not make the left
    /// button swing it — `super::combat` routes on the ids it knows — and it could not,
    /// because what a weapon is belongs to the server's registry.
    Blade,
    /// What a tent or a forge looks like carried. Its own shape, because the place press
    /// means something different while one is in hand — a structure rather than a voxel —
    /// and the hand is where a player sees which of the two they are about to ask for.
    Bundle,
    /// A haft with a head on it: a shovel, a pickaxe, an axe.
    ///
    /// **One shape for the three, and they are told apart by colour**, which is the same
    /// answer three raw materials already get. `ItemShape` is a vocabulary of *kinds* —
    /// four of them before this — rather than a picture per item, and three silhouettes
    /// would be three meshes and three drawings for a difference #175 is the issue for.
    ///
    /// It is a shape of its own rather than a `Blade` because the difference is the one
    /// that matters in the hand: a blade is what the left button swings, and an implement
    /// is emphatically not — an implement does no melee damage at all, which the server's
    /// registry says with a zero. A shape is not a capability, and drawing them alike
    /// would be inviting the reader to think it was.
    Tool,
    /// A compact plate with shoulders: every wearable piece uses one silhouette and the
    /// registry colour distinguishes worked leather from forged iron.
    Armour,
    /// A wooden plate with a metal boss.
    Shield,
    /// A bent stave and string, distinct from a blade in every held and flat renderer.
    Bow,
    /// A wooden shaft capped by a green healing focus.
    Sceptre,
    /// A struck disc: one circle with a smaller darker one inset, which is what a coin reads
    /// as at a cell's size and what nothing else in the vocabulary reads as.
    ///
    /// **Its own variant rather than a [`Self::Material`] in a pale colour**, because money
    /// is the one thing in a pack a player counts rather than spends on a recipe, and a stub
    /// among stubs is exactly what they would fail to find. #454 asked for the hand and the
    /// drop to reuse the material stub; `every_shape_is_drawn_from_its_own_silhouette`
    /// refuses two shapes that share a drop silhouette, so the disc is authored on all three
    /// surfaces instead of on one — which is what `docs/ADDING_AN_ITEM.md` means by
    /// budgeting three drawings for a new shape.
    Coin,
}

impl ItemShape {
    /// Every shape, for the sweeps that must cover the whole vocabulary.
    ///
    /// **This list is not what makes a shape drawn — the compiler is.** Both renderers
    /// match on [`ItemShape`] with no wildcard arm, so a fifth variant fails to build
    /// until it has been given a mesh *and* a drawing; there is no branch for it to fall
    /// through into a square. What the list buys is the other half, the one the name
    /// sweep above established: a test that catches an arm filled in with a placeholder —
    /// an empty drawing, or one copied from another shape.
    ///
    /// No stable Rust enumerates variants, so this array is written by hand and could in
    /// principle fall behind the enum. That would cost a sweep some coverage and nothing
    /// more, because the coverage that matters is the compiler's.
    ///
    /// **It has a runtime reader now, and that reader is what makes it stronger than a
    /// sweep.** `player::drops::create_visuals` builds one mesh per entry, so a shape
    /// missing from this array is a shape no dropped item can be drawn as — not a test
    /// that covers less, but a pelt nobody can see on the ground. It was `#[cfg(test)]`
    /// until #182 for want of anybody needing it.
    ///
    /// **It cannot be pinned the way `net::codec::HairModel::ALL` now is, and the reason
    /// is not effort.** That list is derived from `fb::HairModel::ENUM_VALUES` — flatc's
    /// output, regenerated from the schema — so the contract answers what belongs in it.
    /// [`ItemShape`] has no wire counterpart: it is this client's own vocabulary for how
    /// to draw a thing, invented here and declared nowhere else, so there is nothing to
    /// derive a list from and any pin would be this file agreeing with itself. What
    /// stands in its place is the wildcard-free match above, which is the stronger
    /// guarantee anyway — and it is exactly what `ConnectionState` fell back on for the
    /// same reason.
    pub(crate) const ALL: [Self; 10] = [
        Self::Block,
        Self::Material,
        Self::Blade,
        Self::Bundle,
        Self::Tool,
        Self::Armour,
        Self::Shield,
        Self::Bow,
        Self::Sceptre,
        Self::Coin,
    ];
}

/// Where one item's colour comes from.
///
/// Block-like items deliberately reuse the terrain swatch they represent. Items that are
/// not blocks have their own presentation colours here instead of borrowing a wire block
/// id: the server is free to append that id space, and a forged blade is not ore in stone.
/// Keeping both cases in this enum lets every renderer ask this table one question without
/// teaching the world palette about client-only ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemColour {
    /// Reuse one real block palette entry.
    Block(BlockId),
    /// Dark, cool steel whose wear and oxide remain visible. sRGB `#59636D`.
    WornSteel,
    /// Cleaner, brighter forged steel. sRGB `#8997A3`.
    ForgedSteel,
    /// Worked hide, warm and dark enough to stay distinct from forged steel. sRGB `#7A4E2D`.
    Leather,
    /// Fresh raw meat. A muted red that belongs to no terrain block. sRGB `#9C4F4F`.
    RawMeat,
    /// Cooked meat. A browned swatch distinct from the raw ingredient. sRGB `#8B5A3C`.
    CookedMeat,
    /// Bone-white shaft and point, kept distinct from other material rows. sRGB `#D8C9A3`.
    Arrow,
    /// Struck silver: paler and cooler than forged steel, so a coin is not a small ingot.
    /// sRGB `#BFC7D2`.
    Silver,
}

/// `#59636D`, converted from sRGB to the linear space vertex colours use.
const WORN_STEEL_LINEAR: [f32; 3] = [0.099_899, 0.124_772, 0.152_926];
/// `#8997A3`, converted from sRGB to the linear space vertex colours use.
const FORGED_STEEL_LINEAR: [f32; 3] = [0.250_158, 0.309_469, 0.366_253];
/// `#7A4E2D`, converted from sRGB to the linear space vertex colours use.
const LEATHER_LINEAR: [f32; 3] = [0.194_618, 0.076_185, 0.026_241];
/// `#9C4F4F`, converted from sRGB to the linear space vertex colours use.
const RAW_MEAT_LINEAR: [f32; 3] = [0.332_452, 0.078_187, 0.078_187];
/// `#8B5A3C`, converted from sRGB to the linear space vertex colours use.
const COOKED_MEAT_LINEAR: [f32; 3] = [0.258_183, 0.102_242, 0.045_186];
/// `#D8C9A3`, converted from sRGB to the linear space vertex colours use.
const ARROW_LINEAR: [f32; 3] = [0.686_686, 0.584_078, 0.366_253];
/// `#BFC7D2`, converted from sRGB to the linear space vertex colours use.
const SILVER_LINEAR: [f32; 3] = [0.520_996, 0.571_125, 0.644_480];

impl ItemColour {
    fn linear_rgba(self) -> [f32; 4] {
        match self {
            Self::Block(block) => palette::linear_rgba(block),
            Self::WornSteel => {
                let [r, g, b] = WORN_STEEL_LINEAR;
                [r, g, b, 1.0]
            }
            Self::ForgedSteel => {
                let [r, g, b] = FORGED_STEEL_LINEAR;
                [r, g, b, 1.0]
            }
            Self::Leather => {
                let [r, g, b] = LEATHER_LINEAR;
                [r, g, b, 1.0]
            }
            Self::RawMeat => {
                let [r, g, b] = RAW_MEAT_LINEAR;
                [r, g, b, 1.0]
            }
            Self::CookedMeat => {
                let [r, g, b] = COOKED_MEAT_LINEAR;
                [r, g, b, 1.0]
            }
            Self::Arrow => {
                let [r, g, b] = ARROW_LINEAR;
                [r, g, b, 1.0]
            }
            Self::Silver => {
                let [r, g, b] = SILVER_LINEAR;
                [r, g, b, 1.0]
            }
        }
    }
}

/// The generated surface an item's material wears, when it wears one.
///
/// **A livery belongs to a material, not to an item**, which is the whole reason the second
/// one costs a row here rather than a generator. There are about thirty item ids below and
/// roughly five materials among them; a haft, a bow stave, a shield plate and a sceptre
/// shaft are the same wood, and a helm, a cuirass, greaves and the iron sword are the same
/// forged iron. So the set of liveries is small and closed, each row names one or names
/// `None`, and a new item costs a row.
///
/// **A vocabulary of *kinds* with no wildcard arm in the generator**, exactly as
/// [`ItemShape`] is one in either renderer: a third variant does not compile until
/// `super::livery` has been told how to draw it. Nothing there matches on an item id.
///
/// **A livery has to earn its place, and the default answer is no livery.** A material whose
/// flat colour already reads correctly does not get one: grain on a log carried in the hand
/// would be worth it because a bare cube is the flattest thing in the game, grain on a
/// raw-meat stub is a texture nobody will look at. `client/AGENTS.md` lists which materials
/// have one and why the rest do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Livery {
    /// Worn steel: oxide, warm and dark, eaten *into* the metal rather than laid over it.
    WornSteel,
    /// Forged steel: the marks of the work — an unground flat over the ridge, hammer
    /// banding, grinding streaks and a sparse forge scale. Colour only; it takes nothing
    /// out of the geometry.
    ForgedSteel,
    /// Wood: grain, running the length of the piece. Colour only — grain is a surface, not
    /// erosion — and the strongest case in the set, because a bare cube carried in the hand
    /// is the flattest thing in the game.
    Wood,
}

impl Livery {
    /// Every livery, for the sweeps and for the generator that mints one image per entry.
    ///
    /// Hand-written for the reason [`ItemShape::ALL`] is: no stable Rust enumerates
    /// variants. What makes a livery *drawn* is still the compiler — `super::livery`
    /// matches with no wildcard arm — and this list is what lets the image layout and the
    /// tests be written once for however many there are.
    pub(crate) const ALL: [Self; 3] = [Self::WornSteel, Self::ForgedSteel, Self::Wood];

    /// Which band of the generated image this livery occupies.
    pub(crate) fn band(self) -> usize {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .expect("every livery is in ALL")
    }
}

/// Everything this client has an opinion about for one item id.
///
/// Three facts, all mandatory, which is the compiler's half of *every item has a complete
/// entry*: a row cannot be written without a name, a shape and a colour. The sweep test
/// below is the other half — it is what catches a row whose fields are filled in with a
/// placeholder rather than left out.
///
/// **A field for a capability would not belong here.** What an item can do is the server's
/// answer, and a client-side copy of it would be a cheat vector by construction. A later
/// fact that is genuinely presentation — a drawn icon, a rarity tint — is a fourth field
/// and needs no restructuring to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ItemDisplay {
    /// The wire id, from `server/internal/game/items.go`, which appends and never
    /// renumbers.
    pub(super) item_id: u16,
    /// Lower case, because the recipe panel builds its headings from it — `ui/inventory.rs`
    /// upper-cases the product to match the section titles beside it, and a tooltip prints
    /// it as it stands.
    pub(super) name: &'static str,
    pub(super) shape: ItemShape,
    /// The colour this item presents as.
    ///
    /// A block-like item names the terrain swatch it represents; an item with no honest
    /// block counterpart names an item-only swatch. Item ids and block ids remain two
    /// registries: a log is item 4 and draws as [`ItemColour::Block`]`(`[`palette::LOG`]`)`.
    colour: ItemColour,
    /// The generated surface this item's material wears, when it wears one.
    ///
    /// **The fourth field the doc above anticipated**, and a presentation fact like the
    /// other three: what an item *does* is still the server's answer. `None` means "draws
    /// as a flat colour", which is most of this table.
    ///
    /// **Explicit per row, and never derived from [`ItemColour`]** — which is the finding
    /// that decided the shape of #420. `ItemColour::Block(`[`palette::LOG`]`)` is worn by
    /// the log, the campfire, the wooden shield, the bow and the sceptre, and *also* by the
    /// **axe**, whose swatch is the ground it works rather than what it is made of, and by
    /// the **leather patch**, which is bark-coloured worked hide. Two of those seven are not
    /// wood. A livery inferred from the colour would grain them both.
    livery: Option<Livery>,
}

/// Every item id this client knows, in the order the server's registry appends them.
///
/// The order is load-bearing only as documentation; [`display`] searches by id. What the
/// sweep does insist on is that the ids form the contiguous block an append-only registry
/// produces, so a sixteenth item cannot quietly arrive as id 20 with a hole behind it.
pub(super) const ITEMS: [ItemDisplay; 39] = [
    ItemDisplay {
        item_id: ITEM_STONE,
        name: "stone",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::STONE),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_DIRT,
        name: "dirt",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::DIRT),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_SNOW,
        name: "snow",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::SNOW),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_LOG,
        name: "log",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::LOG),
        livery: Some(Livery::Wood),
    },
    ItemDisplay {
        item_id: ITEM_RAW_COAL,
        name: "coal",
        shape: ItemShape::Material,
        colour: ItemColour::Block(palette::COAL_ORE),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_RAW_IRON,
        name: "raw iron",
        shape: ItemShape::Material,
        colour: ItemColour::Block(palette::IRON_ORE),
        livery: None,
    },
    // The starter blade. `super::combat` reads the same id to decide what a click asks
    // for; the row here is only what it looks like and what it is called.
    ItemDisplay {
        item_id: ITEM_RUSTY_SWORD,
        name: "rusty sword",
        shape: ItemShape::Blade,
        colour: ItemColour::WornSteel,
        livery: Some(Livery::WornSteel),
    },
    // The two items that plant an entity rather than a voxel. Iron for the forge, canvas
    // for the tent, so a player can see which of the two they are carrying.
    ItemDisplay {
        item_id: ITEM_FORGE,
        name: "forge",
        shape: ItemShape::Bundle,
        colour: ItemColour::Block(palette::IRON_ORE),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_TENT,
        name: "tent",
        shape: ItemShape::Bundle,
        colour: ItemColour::Block(palette::SNOW),
        livery: None,
    },
    // The forge's two products. The iron blade is a `Blade` beside the rusty one, in clean
    // forged steel rather than worn steel so the two are told apart in the hand as well as
    // in the pack; the sharpening stone is a consumable and reads as raw material.
    ItemDisplay {
        item_id: ITEM_IRON_SWORD,
        name: "iron sword",
        shape: ItemShape::Blade,
        colour: ItemColour::ForgedSteel,
        livery: Some(Livery::ForgedSteel),
    },
    ItemDisplay {
        item_id: ITEM_SHARPENING_STONE,
        name: "sharpening stone",
        shape: ItemShape::Material,
        colour: ItemColour::Block(palette::STONE),
        livery: None,
    },
    // The third bundle, and the first whose point is the ground *around* it. A `Bundle`
    // beside the tent and the forge because the place press means the same thing while
    // one is in hand — a structure rather than a voxel — and firewood, because that is
    // what a fire is carried as.
    ItemDisplay {
        item_id: ITEM_CAMPFIRE,
        name: "campfire",
        shape: ItemShape::Bundle,
        colour: ItemColour::Block(palette::LOG),
        livery: Some(Livery::Wood),
    },
    // What a hunt leaves, and what it is worked into. All three are `Material`, because
    // that is what the shape vocabulary has for a thing you carry and spend — and because
    // a shape is not a capability: drawing the patch as a `Blade` would not make the left
    // button swing it, and drawing it as a `Bundle` would suggest a place press it has no
    // answer for.
    //
    // Three different swatches, which is the whole of what stops them being three
    // identical cells: bone-white for the bones, wet earth for a raw hide, and bark for
    // one that has been worked. None of the three collides with an existing
    // shape-and-colour pair.
    ItemDisplay {
        item_id: ITEM_BONE,
        name: "bone",
        shape: ItemShape::Material,
        colour: ItemColour::Block(palette::SNOW),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_VARGR_PELT,
        name: "vargr pelt",
        shape: ItemShape::Material,
        colour: ItemColour::Block(palette::DIRT),
        livery: None,
    },
    ItemDisplay {
        // **The other `Block(palette::LOG)` that is not wood**: bark is what a worked hide
        // looks like, which is why it borrows that swatch, and grain on it would be wood.
        item_id: ITEM_LEATHER_PATCH,
        name: "leather patch",
        shape: ItemShape::Material,
        colour: ItemColour::Block(palette::LOG),
        livery: None,
    },
    // The three implements, one shape and three swatches — which is what stops them being
    // three identical cells, exactly as it does for the bone, the pelt and the patch above.
    // Each swatch is the ground its tool is for, so what a player reads off the colour is
    // the thing the tool is good at: earth for the shovel, stone for the pickaxe, wood for
    // the axe.
    ItemDisplay {
        item_id: ITEM_SHOVEL,
        name: "shovel",
        shape: ItemShape::Tool,
        colour: ItemColour::Block(palette::DIRT),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_PICKAXE,
        name: "pickaxe",
        shape: ItemShape::Tool,
        colour: ItemColour::Block(palette::STONE),
        livery: None,
    },
    ItemDisplay {
        // **`Block(palette::LOG)` and no livery, deliberately.** Every implement's swatch is
        // *the ground its tool is for* rather than what it is made of — earth for the shovel,
        // stone for the pickaxe, wood for the axe — so an axe is bark-coloured because it
        // fells trees. Graining it would be reading the colour as a material, which is the
        // exact mistake the livery column is explicit per row to avoid.
        item_id: ITEM_AXE,
        name: "axe",
        shape: ItemShape::Tool,
        colour: ItemColour::Block(palette::LOG),
        livery: None,
    },
    // The hunted ingredient and its cooked product. Both are `Material`, while distinct
    // item-only swatches keep the raw and cooked forms legible in the same pack.
    ItemDisplay {
        item_id: ITEM_RAW_MEAT,
        name: "raw meat",
        shape: ItemShape::Material,
        colour: ItemColour::RawMeat,
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_COOKED_MEAT,
        name: "cooked meat",
        shape: ItemShape::Material,
        colour: ItemColour::CookedMeat,
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_LEATHER_CAP,
        name: "leather cap",
        shape: ItemShape::Armour,
        colour: ItemColour::Leather,
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_LEATHER_JERKIN,
        name: "leather jerkin",
        shape: ItemShape::Armour,
        colour: ItemColour::Leather,
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_LEATHER_LEGGINGS,
        name: "leather leggings",
        shape: ItemShape::Armour,
        colour: ItemColour::Leather,
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_IRON_HELM,
        name: "iron helm",
        shape: ItemShape::Armour,
        colour: ItemColour::ForgedSteel,
        livery: Some(Livery::ForgedSteel),
    },
    ItemDisplay {
        item_id: ITEM_IRON_CUIRASS,
        name: "iron cuirass",
        shape: ItemShape::Armour,
        colour: ItemColour::ForgedSteel,
        livery: Some(Livery::ForgedSteel),
    },
    ItemDisplay {
        item_id: ITEM_IRON_GREAVES,
        name: "iron greaves",
        shape: ItemShape::Armour,
        colour: ItemColour::ForgedSteel,
        livery: Some(Livery::ForgedSteel),
    },
    ItemDisplay {
        item_id: ITEM_WOODEN_SHIELD,
        name: "wooden shield",
        shape: ItemShape::Shield,
        colour: ItemColour::Block(palette::LOG),
        livery: Some(Livery::Wood),
    },
    ItemDisplay {
        item_id: ITEM_BOW,
        name: "bow",
        shape: ItemShape::Bow,
        colour: ItemColour::Block(palette::LOG),
        livery: Some(Livery::Wood),
    },
    ItemDisplay {
        item_id: ITEM_ARROW,
        name: "arrow",
        shape: ItemShape::Material,
        colour: ItemColour::Arrow,
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_WOODEN_SCEPTRE,
        name: "wooden sceptre",
        shape: ItemShape::Sceptre,
        colour: ItemColour::Block(palette::LOG),
        livery: Some(Livery::Wood),
    },
    // What a desert and a gravel bar are dug into. Three plain block items, each
    // naming the terrain swatch it came out of, so a pack holding sand and
    // sandstone reads as two layers of the same place rather than two anonymous
    // stubs.
    //
    // **`ItemShape::Block` rather than `Material`, deliberately.** All three are
    // placeable — the server registry gives each of them a `places` block, and
    // `world.Placeable` says yes — and `Block` is documented right above as "what a
    // voxel looks like carried, and what a place press will ask for". Drawing them
    // as `Material` would be the one thing a shape can get wrong: telling a player
    // that the thing in their hand has no place press behind it. Stone, dirt, snow
    // and the log are `Block` for the same reason.
    ItemDisplay {
        item_id: ITEM_SAND,
        name: "sand",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::SAND),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_SANDSTONE,
        name: "sandstone",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::SANDSTONE),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_GRAVEL,
        name: "gravel",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::GRAVEL),
        livery: None,
    },
    // The lid off a frozen lake, on exactly the terms the three above are held: it places
    // the voxel it came out of, so it is a `Block` rather than a `Material`, and it names
    // that voxel's own swatch.
    ItemDisplay {
        item_id: ITEM_ICE,
        name: "ice",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::ICE),
        livery: None,
    },
    // The coin. Its own shape and its own swatch, because it is the one item in a pack that
    // is read as a *number* rather than as a material — and no livery, because a struck disc
    // at this size is a colour and a rim, not a surface.
    //
    // **Nothing in this row says what silver is for.** There is nothing to buy yet (#459),
    // and when there is, that will be the server's answer and not this table's.
    ItemDisplay {
        item_id: ITEM_SILVER,
        name: "silver",
        shape: ItemShape::Coin,
        colour: ItemColour::Silver,
        livery: None,
    },
    // The three a settlement is built from. `Block` for all of them, for the reason ice
    // is: the server's registry gives each of them a voxel to place, and a shape that
    // said otherwise would be telling a player there is no place press behind what they
    // are holding.
    ItemDisplay {
        item_id: ITEM_PLANKS,
        name: "planks",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::PLANKS),
        livery: Some(Livery::Wood),
    },
    ItemDisplay {
        item_id: ITEM_COBBLESTONE,
        name: "cobblestone",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::COBBLESTONE),
        livery: None,
    },
    ItemDisplay {
        item_id: ITEM_THATCH,
        name: "thatch",
        shape: ItemShape::Block,
        colour: ItemColour::Block(palette::THATCH),
        livery: None,
    },
    // The fourth carried structure. A Bundle rather than a new shape because the item is
    // the packed stone a place press plants; the dedicated monolith and its rune are the
    // standing structure's drawing in `super::structures`.
    ItemDisplay {
        item_id: ITEM_RUNESTONE,
        name: "runestone",
        shape: ItemShape::Bundle,
        colour: ItemColour::Block(palette::STONE),
        livery: None,
    },
];

/// The row one item id has, when this build has one.
///
/// `Option` rather than a total lookup, and it fails open in the only direction it can: an
/// id with no row is a server one contract ahead, not a corrupt one, so each reader below
/// supplies its own honest fallback rather than this function inventing a row.
pub(super) fn display(item_id: u16) -> Option<&'static ItemDisplay> {
    ITEMS.iter().find(|row| row.item_id == item_id)
}

/// The display name of one item id.
///
/// The fallback is reachable — an id this build has never heard of has no name to give —
/// and it says so rather than guessing. `the_registry_names_every_item_id_this_client_declares`
/// is what keeps it unreachable for every id this build *does* know.
pub fn item_label(item_id: u16) -> &'static str {
    display(item_id).map_or("unknown item", |row| row.name)
}

/// The linear colour one item id presents as, for every renderer that draws it.
///
/// An unknown id reaches the block palette's loud placeholder — the honest answer to a
/// version skew, rather than a plausible shade this module invented. Known items resolve
/// here too, so held meshes, dropped meshes and flat icons cannot choose different sources.
pub(crate) fn item_linear_rgba(item_id: u16) -> [f32; 4] {
    display(item_id).map_or_else(
        || palette::linear_rgba(BlockId::MAX),
        |row| row.colour.linear_rgba(),
    )
}

/// The shape one item id is drawn in.
///
/// [`ItemShape::Material`] for an unknown id: a stub of *something* carryable is the least
/// wrong guess, and the colour is already shouting about the skew, so the shape does not
/// need a second placeholder to mean the same thing.
pub(crate) fn item_shape(item_id: u16) -> ItemShape {
    display(item_id).map_or(ItemShape::Material, |row| row.shape)
}

/// The livery one item id wears, when it wears one.
///
/// `None` for an unknown id, which is the same answer as "wears none" and is the right one:
/// a build that has never heard of an item cannot know what its surface looks like.
///
/// **This is what the renderers ask instead of naming an item.** `super::hands` reached its
/// one liveried blade through `if item_id == ITEM_RUSTY_SWORD`, which is the shape that
/// does not survive a second liveried item.
pub(crate) fn item_livery(item_id: u16) -> Option<Livery> {
    display(item_id).and_then(|row| row.livery)
}

/// Every distinct shape-and-livery pair an item in this build actually presents as, for the
/// liveried items only.
///
/// **What `super::drops` builds its extra meshes from.** That cache is keyed on
/// `(ItemShape, Option<Livery>)` since #418, because a livery decides geometry as well as
/// colour — the rusty blade is pitted and the iron one is not — and the cross product of
/// every shape with every livery would mint meshes for combinations no item is. Deriving the
/// pairs from the table means the cache holds exactly what can be drawn, and two items
/// sharing a shape and a livery land on one entry, which is what a shape-keyed cache was for
/// in the first place.
pub(super) fn liveried_shapes() -> Vec<(ItemShape, Livery)> {
    let mut pairs: Vec<(ItemShape, Livery)> = Vec::new();
    for row in ITEMS {
        let Some(livery) = row.livery else {
            continue;
        };
        if !pairs.contains(&(row.shape, livery)) {
            pairs.push((row.shape, livery));
        }
    }
    pairs
}

/// Every item id this build has a row for.
///
/// Test-only, and derived from [`ITEMS`] rather than listed beside it, so a sweep in
/// another module cannot fall behind this table the way a second list would. `ui`'s cell
/// tests use it to assert that every item a player can hold draws a picture, which is the
/// same question [`every_known_item_has_a_name_a_shape_and_a_colour`] asks one layer down.
#[cfg(test)]
pub(crate) fn known_item_ids() -> impl Iterator<Item = u16> {
    ITEMS.iter().map(|row| row.item_id)
}

/// Whether one row carries all three facts rather than a placeholder standing in for one.
///
/// The predicate the sweep applies, extracted so that
/// `the_sweep_rejects_a_row_that_is_missing_a_fact` can assert the failure mode on a
/// fixture instead of the sweep only ever being run against rows that pass. The shape
/// column is absent from it deliberately: [`ItemShape`] has no unknown variant, so a row
/// with no shape does not compile and there is nothing here left to check.
#[cfg(test)]
fn row_is_complete(row: &ItemDisplay) -> bool {
    !row.name.is_empty()
        && row.name != item_label(u16::MAX)
        && row.colour.linear_rgba() != palette::linear_rgba(BlockId::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An id no build will have a row for, used to exercise every fallback at once.
    const NO_SUCH_ITEM: u16 = 4242;

    /// The sweep this issue exists for: every known item, all three facts, no placeholders.
    #[test]
    fn every_known_item_has_a_name_a_shape_and_a_colour() {
        for row in ITEMS {
            assert!(
                row_is_complete(&row),
                "item {} has an incomplete display row: {row:?}",
                row.item_id
            );
            // Each reader reaches this row rather than a second opinion of its own, which
            // is the whole of what "one registry" buys.
            assert_eq!(display(row.item_id), Some(&row), "item {}", row.item_id);
            assert_eq!(item_label(row.item_id), row.name, "item {}", row.item_id);
            assert_eq!(
                item_linear_rgba(row.item_id),
                row.colour.linear_rgba(),
                "item {}",
                row.item_id
            );
            assert_eq!(item_shape(row.item_id), row.shape, "item {}", row.item_id);
            assert_eq!(item_livery(row.item_id), row.livery, "item {}", row.item_id);
        }

        // **Deliberately not part of `row_is_complete`.** The other three facts have no
        // honest empty value; `None` is what almost every item's livery legitimately is. So
        // the sweep insists on what it can: the accessor answers what the row says, and at
        // least one item wears one, so the generator cannot be left drawing for nobody.
        assert!(
            ITEMS.iter().any(|row| row.livery.is_some()),
            "no item wears a livery, so the generator is drawing something nobody samples"
        );

        // **The two items whose colour is wood and whose material is not**, asserted by name
        // so the omission is a decision on the record rather than something nobody noticed.
        // `ItemColour::Block(palette::LOG)` is worn by seven rows; the axe borrows it because
        // its swatch is *the ground its tool is for*, and the leather patch because bark is
        // what a worked hide looks like. A livery inferred from the colour would grain both,
        // which is why the column is explicit per row.
        for item_id in [ITEM_AXE, ITEM_LEATHER_PATCH] {
            assert_eq!(
                display(item_id).and_then(|row| row.livery),
                None,
                "item {item_id} has been given a livery from its colour rather than its \
                 material"
            );
            assert_eq!(
                display(item_id).map(|row| row.colour),
                Some(ItemColour::Block(palette::LOG)),
                "item {item_id} no longer borrows the log swatch, so the clause above pins \
                 nothing"
            );
        }

        // The server's registry appends and never renumbers, so the ids it has issued are
        // 1..N with no holes. Checking that catches a duplicated row and a row given an
        // id nobody issued, neither of which any per-row assertion above can see.
        let mut ids: Vec<u16> = ITEMS.iter().map(|row| row.item_id).collect();
        ids.sort_unstable();
        let expected: Vec<u16> = (1..=u16::try_from(ITEMS.len()).expect("a small table")).collect();
        assert_eq!(
            ids, expected,
            "the registry's ids are not the contiguous block an append-only server issues"
        );
    }

    /// The direction the table cannot check by itself.
    ///
    /// The ids the rest of the client spells out, gathered from the modules that declare
    /// them. A sixteenth item added to `super::combat` or `super::structures` and not
    /// added here is a slot that draws magenta and reads "unknown item"; this list is
    /// where that is noticed. The length check is what stops the list going stale in the
    /// other direction, so neither half can drift without the other failing.
    #[test]
    fn the_registry_names_every_item_id_this_client_declares() {
        let declared = [
            ITEM_STONE,
            ITEM_DIRT,
            ITEM_SNOW,
            ITEM_LOG,
            ITEM_RAW_COAL,
            ITEM_RAW_IRON,
            ITEM_RUSTY_SWORD,
            ITEM_FORGE,
            ITEM_TENT,
            ITEM_IRON_SWORD,
            ITEM_SHARPENING_STONE,
            ITEM_CAMPFIRE,
            ITEM_BONE,
            ITEM_VARGR_PELT,
            ITEM_LEATHER_PATCH,
            ITEM_SHOVEL,
            ITEM_PICKAXE,
            ITEM_AXE,
            ITEM_RAW_MEAT,
            ITEM_COOKED_MEAT,
            ITEM_LEATHER_CAP,
            ITEM_LEATHER_JERKIN,
            ITEM_LEATHER_LEGGINGS,
            ITEM_IRON_HELM,
            ITEM_IRON_CUIRASS,
            ITEM_IRON_GREAVES,
            ITEM_WOODEN_SHIELD,
            ITEM_BOW,
            ITEM_ARROW,
            ITEM_WOODEN_SCEPTRE,
            ITEM_SAND,
            ITEM_SANDSTONE,
            ITEM_GRAVEL,
            ITEM_ICE,
            ITEM_SILVER,
            ITEM_PLANKS,
            ITEM_COBBLESTONE,
            ITEM_THATCH,
            ITEM_RUNESTONE,
        ];
        for item_id in declared {
            assert!(
                display(item_id).is_some(),
                "item {item_id} is named by this client and has no display row"
            );
        }
        assert_eq!(
            ITEMS.len(),
            declared.len(),
            "the registry holds a row for an id nothing else declares, or this list is stale"
        );
    }

    /// The coin, and the block that was missing when it arrived.
    ///
    /// **The ids are pinned by name**, for the reason every id in this table is: the server
    /// appends and never renumbers, so 34 and 35 are the numbers a persisted pack already
    /// holds and no others will do.
    ///
    /// Ice is here because silver could not be added without it. The contiguity sweep above
    /// derives what it expects from this table's own length, so it cannot see a *trailing*
    /// omission — ice sat at 34 with no row from the day the server issued it — and the
    /// thirty-fifth item is what turned that hole into an interior one.
    #[test]
    fn the_coin_and_the_ice_beside_it_carry_their_pinned_ids() {
        assert_eq!(ITEM_ICE, 34);
        assert_eq!(ITEM_SILVER, 35);

        let ice = display(ITEM_ICE).expect("ice is registered");
        assert_eq!(ice.name, "ice");
        assert_eq!(ice.shape, ItemShape::Block);
        assert_eq!(ice.colour, ItemColour::Block(palette::ICE));

        let silver = display(ITEM_SILVER).expect("silver is registered");
        assert_eq!(silver.name, "silver");
        assert_eq!(silver.shape, ItemShape::Coin);
        assert_eq!(silver.colour, ItemColour::Silver);
        // No livery: a struck disc at a cell's size is a colour and a rim, not a surface.
        assert_eq!(silver.livery, None);

        // **Nothing else is a coin**, which is the whole of what earns the shape its own
        // variant: if a second item ever shares it they are told apart by colour, exactly as
        // the three implements and the three armour pieces are, and that is a decision
        // somebody has to make here rather than inherit.
        let coins: Vec<u16> = ITEMS
            .iter()
            .filter(|row| row.shape == ItemShape::Coin)
            .map(|row| row.item_id)
            .collect();
        assert_eq!(coins, vec![ITEM_SILVER]);

        // And the coin's swatch is its own rather than a steel it would be mistaken for in a
        // pack: pale and cool, but not the forged blade's colour.
        assert_ne!(
            item_linear_rgba(ITEM_SILVER),
            item_linear_rgba(ITEM_IRON_SWORD)
        );
    }

    /// The three a settlement is built out of.
    ///
    /// **The sibling of the test above, and the ids it pins moved once already.** This
    /// branch first wrote them as 35, 36 and 37; silver landed on `develop` at 35 and
    /// pushed all three up by one. An id that can move without a test noticing is a
    /// persisted pack that a later build reads as something else, so the numbers are
    /// written out here rather than derived from the table's order.
    ///
    /// Planks carry the wood livery for the reason the log does — sawn timber is still
    /// timber — and the other two carry none: dressed rubble and straw have no grain to
    /// draw at a cell's size.
    #[test]
    fn the_three_a_settlement_is_built_from_carry_their_pinned_ids() {
        assert_eq!(ITEM_PLANKS, 36);
        assert_eq!(ITEM_COBBLESTONE, 37);
        assert_eq!(ITEM_THATCH, 38);

        for (item_id, name, swatch, livery) in [
            (ITEM_PLANKS, "planks", palette::PLANKS, Some(Livery::Wood)),
            (ITEM_COBBLESTONE, "cobblestone", palette::COBBLESTONE, None),
            (ITEM_THATCH, "thatch", palette::THATCH, None),
        ] {
            let row = display(item_id).expect("a settlement block is registered");
            assert_eq!(row.name, name);
            // `Block` rather than `Material`: the server's registry gives each of these
            // a voxel to place, and a shape that said otherwise would be telling a
            // player there is no place press behind what they are holding.
            assert_eq!(row.shape, ItemShape::Block);
            assert_eq!(row.colour, ItemColour::Block(swatch));
            assert_eq!(row.livery, livery);
        }
    }

    #[test]
    fn the_runestone_has_its_appended_id_label_and_bundle_icon() {
        assert_eq!(ITEM_RUNESTONE, 39);
        assert_eq!(item_label(ITEM_RUNESTONE), "runestone");
        assert_eq!(item_shape(ITEM_RUNESTONE), ItemShape::Bundle);
        assert_eq!(
            item_linear_rgba(ITEM_RUNESTONE),
            palette::linear_rgba(palette::STONE)
        );
    }

    /// The three the recipe-driven name table could never have covered.
    ///
    /// Dirt and snow are placeable blocks no recipe mentions; the rusty sword is the item
    /// a player starts holding. All three were "unknown item" before this registry, and a
    /// tooltip is where that would have been read.
    #[test]
    fn the_three_items_no_recipe_mentions_are_named_too() {
        assert_eq!(item_label(ITEM_DIRT), "dirt");
        assert_eq!(item_label(ITEM_SNOW), "snow");
        assert_eq!(item_label(ITEM_RUSTY_SWORD), "rusty sword");
    }

    /// The decision this bug carries: blades have item colours, not borrowed terrain ids.
    ///
    /// Pin both names rather than merely asserting that the rusty sword is no longer snow.
    /// Otherwise a later edit could move either blade back onto another plausible-looking
    /// block swatch and quietly recreate the same category mistake one shade over.
    #[test]
    fn blades_name_worn_and_forged_steel_colours() {
        let rusty = display(ITEM_RUSTY_SWORD).expect("the starter blade is registered");
        let iron = display(ITEM_IRON_SWORD).expect("the forged blade is registered");
        assert_eq!(rusty.colour, ItemColour::WornSteel);
        assert_eq!(iron.colour, ItemColour::ForgedSteel);

        let worn = item_linear_rgba(ITEM_RUSTY_SWORD);
        let forged = item_linear_rgba(ITEM_IRON_SWORD);
        assert_ne!(worn, palette::linear_rgba(palette::SNOW));
        assert_ne!(forged, palette::linear_rgba(palette::IRON_ORE));
        assert_ne!(worn, forged, "the two blades need distinct swatches");
        for channel in 0..3 {
            assert!(
                worn[channel] < forged[channel],
                "worn steel channel {channel} is not darker than forged steel"
            );
        }
    }

    /// The readable sRGB declarations and the linear constants the renderers consume agree.
    #[test]
    fn item_linear_values_match_their_srgb() {
        fn srgb_to_linear(byte: u8) -> f32 {
            let value = f32::from(byte) / 255.0;
            if value <= 0.040_45 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }

        for (name, srgb, linear) in [
            ("worn steel", [0x59, 0x63, 0x6D], WORN_STEEL_LINEAR),
            ("forged steel", [0x89, 0x97, 0xA3], FORGED_STEEL_LINEAR),
            ("leather", [0x7A, 0x4E, 0x2D], LEATHER_LINEAR),
            ("silver", [0xBF, 0xC7, 0xD2], SILVER_LINEAR),
        ] {
            for (channel, (got, byte)) in linear.iter().zip(srgb).enumerate() {
                let want = srgb_to_linear(byte);
                assert!(
                    (got - want).abs() < 1e-6,
                    "{name} channel {channel}: the constant says {got}, sRGB {srgb:?} says {want}"
                );
            }
        }
    }

    /// The three a hunt puts in the pack, at the ids the server appended them at.
    ///
    /// The contiguity check in the sweep above is what says there is no hole behind 15;
    /// this is what says the three rows are the *right* three, which no property of the
    /// block can. `ui`'s cell sweep draws each of them from these same rows.
    #[test]
    fn what_a_hunt_leaves_is_named_and_drawn() {
        for (item_id, name) in [
            (ITEM_BONE, "bone"),
            (ITEM_VARGR_PELT, "vargr pelt"),
            (ITEM_LEATHER_PATCH, "leather patch"),
        ] {
            assert_eq!(item_label(item_id), name, "item {item_id}");
            assert_eq!(item_shape(item_id), ItemShape::Material, "item {item_id}");
            assert!(
                display(item_id).is_some_and(row_is_complete),
                "item {item_id} has an incomplete display row"
            );
        }

        // The ids themselves, pinned: the server's registry appends and never renumbers,
        // and a row under the wrong number draws somebody else's item in this pack.
        assert_eq!(
            [ITEM_BONE, ITEM_VARGR_PELT, ITEM_LEATHER_PATCH],
            [13, 14, 15]
        );

        // And no two of them are the same cell. They share a shape, so the swatch is the
        // only thing left to tell them apart, and a pack holding all three would
        // otherwise be three slots a player has to count.
        let swatch = item_linear_rgba;
        assert_ne!(swatch(ITEM_BONE), swatch(ITEM_VARGR_PELT));
        assert_ne!(swatch(ITEM_BONE), swatch(ITEM_LEATHER_PATCH));
        assert_ne!(swatch(ITEM_VARGR_PELT), swatch(ITEM_LEATHER_PATCH));
    }

    #[test]
    fn both_meats_have_their_appended_ids_and_distinct_displays() {
        assert_eq!(ITEM_RAW_MEAT, 19);
        assert_eq!(item_label(ITEM_RAW_MEAT), "raw meat");
        assert_eq!(item_shape(ITEM_RAW_MEAT), ItemShape::Material);
        assert_eq!(
            display(ITEM_RAW_MEAT).map(|row| row.colour),
            Some(ItemColour::RawMeat)
        );
        assert_eq!(ITEM_COOKED_MEAT, 20);
        assert_eq!(item_label(ITEM_COOKED_MEAT), "cooked meat");
        assert_eq!(item_shape(ITEM_COOKED_MEAT), ItemShape::Material);
        assert_eq!(
            display(ITEM_COOKED_MEAT).map(|row| row.colour),
            Some(ItemColour::CookedMeat)
        );
        assert_ne!(
            item_linear_rgba(ITEM_RAW_MEAT),
            item_linear_rgba(ITEM_COOKED_MEAT)
        );
    }

    #[test]
    fn both_armour_sets_have_their_appended_ids_and_materials() {
        for (item_id, name) in [
            (ITEM_LEATHER_CAP, "leather cap"),
            (ITEM_LEATHER_JERKIN, "leather jerkin"),
            (ITEM_LEATHER_LEGGINGS, "leather leggings"),
        ] {
            assert_eq!(item_label(item_id), name);
            assert_eq!(item_shape(item_id), ItemShape::Armour);
            assert_eq!(
                display(item_id).map(|row| row.colour),
                Some(ItemColour::Leather)
            );
        }
        for (item_id, name) in [
            (ITEM_IRON_HELM, "iron helm"),
            (ITEM_IRON_CUIRASS, "iron cuirass"),
            (ITEM_IRON_GREAVES, "iron greaves"),
        ] {
            assert_eq!(item_label(item_id), name);
            assert_eq!(item_shape(item_id), ItemShape::Armour);
            assert_eq!(
                display(item_id).map(|row| row.colour),
                Some(ItemColour::ForgedSteel)
            );
        }
        assert_eq!(
            [
                ITEM_LEATHER_CAP,
                ITEM_LEATHER_JERKIN,
                ITEM_LEATHER_LEGGINGS,
                ITEM_IRON_HELM,
                ITEM_IRON_CUIRASS,
                ITEM_IRON_GREAVES,
            ],
            [21, 22, 23, 24, 25, 26]
        );
        assert_ne!(
            item_linear_rgba(ITEM_LEATHER_CAP),
            item_linear_rgba(ITEM_IRON_HELM)
        );
    }

    #[test]
    fn wooden_shield_has_its_appended_id_name_shape_and_log_colour() {
        assert_eq!(ITEM_WOODEN_SHIELD, 27);
        assert_eq!(item_label(ITEM_WOODEN_SHIELD), "wooden shield");
        assert_eq!(item_shape(ITEM_WOODEN_SHIELD), ItemShape::Shield);
        assert_eq!(
            item_linear_rgba(ITEM_WOODEN_SHIELD),
            palette::linear_rgba(palette::LOG)
        );
    }

    /// What an id with no row does in every reader, asserted rather than assumed.
    ///
    /// This is exactly the state a real item added to the client without a registry row
    /// would be in, so it is worth knowing precisely: a name that says so, a carryable
    /// shape, and the palette's loud placeholder instead of a plausible colour.
    #[test]
    fn an_item_with_no_row_falls_back_in_every_reader() {
        assert_eq!(display(NO_SUCH_ITEM), None);
        assert_eq!(item_label(NO_SUCH_ITEM), "unknown item");
        assert_eq!(item_shape(NO_SUCH_ITEM), ItemShape::Material);
        assert_eq!(
            item_linear_rgba(NO_SUCH_ITEM),
            palette::linear_rgba(BlockId::MAX),
            "an unknown item drew a plausible colour instead of the placeholder"
        );
        assert_eq!(
            item_livery(NO_SUCH_ITEM),
            None,
            "an unknown item claimed a livery, so a version skew would draw somebody \
             else's surface"
        );
    }

    /// The sweep's teeth, on fixtures rather than on the table that already passes.
    ///
    /// A test that only ever runs a predicate over rows which satisfy it proves the rows
    /// and not the predicate. These three are the shapes a careless addition actually
    /// takes: a row left with the fallback for a name, and a row given a palette entry no
    /// colour answers to.
    #[test]
    fn the_sweep_rejects_a_row_that_is_missing_a_fact() {
        let nameless = ItemDisplay {
            item_id: NO_SUCH_ITEM,
            name: "unknown item",
            shape: ItemShape::Material,
            colour: ItemColour::Block(palette::STONE),
            livery: None,
        };
        assert!(
            !row_is_complete(&nameless),
            "a row named after the fallback passed the sweep"
        );

        let unnamed = ItemDisplay {
            name: "",
            ..nameless
        };
        assert!(
            !row_is_complete(&unnamed),
            "a row with an empty name passed the sweep"
        );

        let colourless = ItemDisplay {
            name: "mystery",
            colour: ItemColour::Block(BlockId::MAX),
            ..nameless
        };
        assert!(
            !row_is_complete(&colourless),
            "a row whose colour is the unknown placeholder passed the sweep"
        );

        let complete = ItemDisplay {
            name: "mystery",
            ..nameless
        };
        assert!(
            row_is_complete(&complete),
            "a row carrying all three facts failed the sweep"
        );
    }
}
