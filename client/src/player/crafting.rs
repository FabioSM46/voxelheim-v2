//! The display-only recipe mirror, and the one request it originates.
//!
//! **This table decides nothing.** The authoritative copy is
//! `server/internal/game/craft.go`, which is deliberately never sent — for the reason the
//! item registry is not. What crosses the wire is a `RecipeID` and nothing else: no
//! ingredient list, no product, no station. So a drift between the two copies can show a
//! wrong label, and can never create an item.
//!
//! What the mirror buys is the question a player actually asks — *what can I make, what
//! does it cost, and what am I short of* — answered without a round trip. Graying out a
//! row whose materials are short is a courtesy computed from [`Inventory::count`], exactly
//! as [`super::combat::blade_in_hand`] is a courtesy: an honest UI does not ask for
//! something it can already see will be declined. The server re-reads its own slots either
//! way, and a refusal is silence.
//!
//! **Proximity is not mirrored, and that asymmetry is the point.** Whether a forge stands
//! within `ForgeCraftRadius` is something the server can see and this client can only
//! guess at — the structures a snapshot names are the ones in view, not the ones that
//! exist — so a forge recipe stays clickable from anywhere and says what it needs instead
//! of pretending to know. Guessing here would produce the one failure a courtesy must
//! never produce: a row that refuses a craft the server would have granted.

use bevy::prelude::*;

use super::inventory::{ApplyInventory, Inventory};
use super::items::{ITEM_LOG, ITEM_RAW_COAL, ITEM_RAW_IRON, ITEM_STONE};
use super::structures::{ITEM_FORGE, ITEM_TENT};
use super::{ApplyInputMode, ApplySnapshots, InputCadence, InputGate, InputMode, SelfVitals};
use crate::net::{CraftRequest, Outbound, RecipeId, Sent, StructureKind, encode_craft_request};

/// Item id 10, the forge's blade, as `server/internal/game/items.go` appends it.
///
/// Presentation only, exactly as [`super::combat::ITEM_RUSTY_SWORD`] is: it cannot make
/// this item craftable and it cannot make another one a weapon. The server reads its own
/// registry.
pub(super) const ITEM_IRON_SWORD: u16 = 10;

/// Item id 11, what keeps a blade alive. Presentation only, for the reason above.
pub(super) const ITEM_SHARPENING_STONE: u16 = 11;

/// One line of a recipe's cost, or the product it yields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ingredient {
    pub item_id: u16,
    pub count: u16,
}

/// One row of the crafting panel: what it costs, what it yields, where it can be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recipe {
    /// The identity, and the only field that ever reaches the server.
    pub id: RecipeId,
    pub ingredients: &'static [Ingredient],
    pub product: Ingredient,
    /// The structure a player must be standing near, when the recipe needs one.
    ///
    /// `None` is the same fail-open the server's table uses for a station-less recipe, and
    /// it is safe in the same direction: the two recipes that need nothing built are the
    /// ones a player must be able to make with nothing built yet.
    pub station: Option<StructureKind>,
}

impl Recipe {
    /// Whether this client can already see the materials this recipe costs.
    ///
    /// A courtesy and never a verdict. It is read twice — by the panel that grays the row
    /// out and by [`request_craft`] that declines to ask — which is what makes the two
    /// agree by construction rather than by both remembering the same rule. Counted across
    /// every slot, exactly as the server's `consume` spends across every slot.
    pub fn affordable(&self, inventory: &Inventory) -> bool {
        self.ingredients
            .iter()
            .all(|needed| inventory.count(needed.item_id) >= u32::from(needed.count))
    }
}

/// The four recipes, mirroring `recipeTable` in `server/internal/game/craft.go`.
///
/// In the order a player meets them: the forge is the thing you build before you have one,
/// the blade and the stone are what it is for, and the tent is the camp you come back to.
pub const RECIPES: [Recipe; 4] = [
    Recipe {
        id: RecipeId::Forge,
        ingredients: &[
            Ingredient {
                item_id: ITEM_STONE,
                count: 8,
            },
            Ingredient {
                item_id: ITEM_RAW_COAL,
                count: 2,
            },
        ],
        product: Ingredient {
            item_id: ITEM_FORGE,
            count: 1,
        },
        station: None,
    },
    Recipe {
        id: RecipeId::IronSword,
        ingredients: &[
            Ingredient {
                item_id: ITEM_RAW_IRON,
                count: 3,
            },
            Ingredient {
                item_id: ITEM_RAW_COAL,
                count: 2,
            },
            Ingredient {
                item_id: ITEM_LOG,
                count: 1,
            },
        ],
        product: Ingredient {
            item_id: ITEM_IRON_SWORD,
            count: 1,
        },
        station: Some(StructureKind::Forge),
    },
    Recipe {
        id: RecipeId::SharpeningStone,
        ingredients: &[
            Ingredient {
                item_id: ITEM_STONE,
                count: 2,
            },
            Ingredient {
                item_id: ITEM_RAW_COAL,
                count: 1,
            },
        ],
        product: Ingredient {
            item_id: ITEM_SHARPENING_STONE,
            count: 1,
        },
        station: Some(StructureKind::Forge),
    },
    Recipe {
        id: RecipeId::Tent,
        ingredients: &[Ingredient {
            item_id: ITEM_LOG,
            count: 8,
        }],
        product: Ingredient {
            item_id: ITEM_TENT,
            count: 1,
        },
        station: None,
    },
];

/// The mirrored row one wire identity names, when this build has one.
///
/// `Option` rather than a total lookup, and it fails closed: a `RecipeId` with no row is a
/// contract this build does not speak, and asking for it would be asking for something
/// nobody could have seen on screen. Every member of the enum has a row today, and
/// `every_recipe_id_has_exactly_one_row` is what keeps that true.
pub fn recipe(id: RecipeId) -> Option<&'static Recipe> {
    RECIPES.iter().find(|recipe| recipe.id == id)
}

/// A player activating one recipe row.
///
/// The panel reports only which row was pressed. This module owns the turn from a row into
/// wire intent, exactly as [`super::inventory`] owns the turn from a cell click into a
/// move — and for the same reason: there is then one place where a request can leave, and
/// one gate on it.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CraftClick {
    pub recipe: RecipeId,
}

pub(super) struct CraftingPlugin;

impl Plugin for CraftingPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CraftClick>()
            // PlayerPlugin and InventoryPlugin own these in the game; initialising them
            // here keeps this module testable on its own, exactly as they do.
            .init_resource::<Inventory>()
            .init_resource::<InputMode>()
            .init_resource::<InputCadence>()
            .init_resource::<SelfVitals>()
            .add_systems(
                Update,
                request_craft
                    // After the newest complete inventory, so a row activated on the frame
                    // a server state lands is judged against that state rather than the
                    // one it replaced.
                    .after(ApplyInventory)
                    .after(ApplyInputMode)
                    // And after the vitals this frame's snapshot carried, so the death
                    // gate below is never read a frame stale.
                    .after(ApplySnapshots),
            );
    }
}

/// Sends one `CraftRequest` per activated row, and changes nothing locally.
///
/// **Nothing here spends a material or produces an item.** [`Inventory`] is the last
/// complete state the server sent and stays exactly that until the next one arrives; an
/// accepted craft becomes visible then, and a refused one is indistinguishable from
/// nothing happening — which is correct, and the same shape a refused block edit already
/// has.
fn request_craft(
    mut clicks: MessageReader<CraftClick>,
    gate: InputGate<'_>,
    inventory: Res<Inventory>,
    cadence: Res<InputCadence>,
    outbound: Option<ResMut<Outbound>>,
) {
    // The screen these rows live on is closed while the server says this player is dead,
    // and the toggle that would reopen it is refused in `ui/mod.rs`. This is the wire half
    // of that rule rather than a second copy of it: a row activated on the frame they died
    // must not be replayed into a craft when they come back.
    if gate.dead() || gate.mode() != InputMode::Inventory {
        clicks.read().for_each(drop);
        return;
    }

    let mut outbound = outbound;
    for click in clicks.read().copied() {
        let Some(recipe) = recipe(click.recipe) else {
            continue;
        };
        // The same predicate the row's own disabled state is drawn from. A short recipe is
        // one the server would refuse, and asking anyway would spend an outbound slot on a
        // frame nobody wants an answer to.
        if !recipe.affordable(&inventory) {
            continue;
        }
        let Some(outbound) = outbound.as_deref_mut() else {
            continue;
        };

        let request = CraftRequest {
            recipe: click.recipe,
            // The counter `PlayerInput`, mining, placement and the swing all share, so the
            // server can order this against the input frame carrying the same number.
            client_tick: cadence.client_tick,
        };
        match outbound.send(encode_craft_request(&request)) {
            Sent::Queued => {}
            Sent::Dropped => warn!(
                "the outbound queue was full; a craft of {:?} never reached the server",
                request.recipe
            ),
            Sent::Closed => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;

    use super::*;
    use crate::net::{InventoryStack, LifeState, PlayerVitals};
    use crate::player::items::item_label;
    use crate::wire::voxelheim::net as fb;

    /// One stack of `count` of `item_id`, as a server state carries it.
    fn stack(item_id: u16, count: u16) -> InventoryStack {
        InventoryStack {
            item_id,
            count,
            ..Default::default()
        }
    }

    fn app(stacks: Vec<InventoryStack>) -> (App, Receiver<Vec<u8>>) {
        let mut app = App::new();
        let (outbound, sent) = Outbound::to_a_test(16);
        app.add_plugins(MinimalPlugins)
            .add_plugins(CraftingPlugin)
            .insert_resource(outbound)
            .insert_resource(Inventory::from_stacks(stacks))
            .insert_resource(InputMode::Inventory);
        (app, sent)
    }

    /// Every recipe the frames name, in the order they left.
    fn crafted(sent: &Receiver<Vec<u8>>) -> Vec<fb::RecipeID> {
        let mut found = Vec::new();
        while let Ok(frame) = sent.try_recv() {
            let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
            if let Some(request) = envelope.payload_as_craft_request() {
                found.push(request.recipe());
            }
        }
        found
    }

    fn activate(app: &mut App, recipe: RecipeId) {
        app.world_mut().write_message(CraftClick { recipe });
        app.update();
    }

    /// Replaces the vitals exactly as an accepted snapshot does.
    fn say_dead(app: &mut App, dead: bool) {
        app.insert_resource(SelfVitals::from_server(PlayerVitals {
            health: if dead { 0 } else { 100 },
            max_health: 100,
            life_state: if dead {
                LifeState::Dead
            } else {
                LifeState::Alive
            },
            respawn_ticks: if dead { 60 } else { 0 },
            invulnerable: false,
        }));
    }

    #[test]
    fn the_mirror_is_the_four_agreed_recipes_verbatim() {
        // The server's `recipeTable`, read off `server/internal/game/craft.go`. A drift
        // here shows a wrong label rather than creating an item — but a wrong label is
        // exactly what a player plans against, so it is pinned.
        assert_eq!(RECIPES.len(), 4);

        let cost = |id: RecipeId| -> Vec<(u16, u16)> {
            recipe(id)
                .expect("every member has a row")
                .ingredients
                .iter()
                .map(|line| (line.item_id, line.count))
                .collect()
        };
        assert_eq!(
            cost(RecipeId::Forge),
            vec![(ITEM_STONE, 8), (ITEM_RAW_COAL, 2)]
        );
        assert_eq!(
            cost(RecipeId::IronSword),
            vec![(ITEM_RAW_IRON, 3), (ITEM_RAW_COAL, 2), (ITEM_LOG, 1)]
        );
        assert_eq!(
            cost(RecipeId::SharpeningStone),
            vec![(ITEM_STONE, 2), (ITEM_RAW_COAL, 1)]
        );
        assert_eq!(cost(RecipeId::Tent), vec![(ITEM_LOG, 8)]);

        for (id, product, station) in [
            (RecipeId::Forge, ITEM_FORGE, None),
            (
                RecipeId::IronSword,
                ITEM_IRON_SWORD,
                Some(StructureKind::Forge),
            ),
            (
                RecipeId::SharpeningStone,
                ITEM_SHARPENING_STONE,
                Some(StructureKind::Forge),
            ),
            (RecipeId::Tent, ITEM_TENT, None),
        ] {
            let row = recipe(id).expect("every member has a row");
            assert_eq!(
                row.product,
                Ingredient {
                    item_id: product,
                    count: 1
                },
                "{id:?}"
            );
            assert_eq!(row.station, station, "{id:?}");
        }
    }

    #[test]
    fn every_recipe_id_has_exactly_one_row() {
        // What makes the fail-closed lookup in `recipe` unreachable rather than merely
        // unlikely, and what catches a fifth wire member arriving with no row to show it.
        for id in [
            RecipeId::Forge,
            RecipeId::IronSword,
            RecipeId::SharpeningStone,
            RecipeId::Tent,
        ] {
            assert_eq!(
                RECIPES.iter().filter(|row| row.id == id).count(),
                1,
                "{id:?}"
            );
        }
    }

    /// Every item this mirror mentions has a row in the display registry.
    ///
    /// The names moved to [`super::items`] with the rest of an item's display facts, and
    /// the sweep there is the one that covers every item a player can hold. This test
    /// stays because it checks the other direction and the sweep cannot: it walks the
    /// *recipes* and asks the registry, so a fifth recipe naming an item nobody registered
    /// fails here rather than in a panel spelling out "unknown item 0/8".
    #[test]
    fn every_item_the_recipes_name_has_a_registry_row() {
        for row in RECIPES {
            for line in row.ingredients.iter().chain(std::iter::once(&row.product)) {
                assert_ne!(
                    item_label(line.item_id),
                    "unknown item",
                    "item {} appears in a recipe with no display row",
                    line.item_id
                );
            }
        }
    }

    #[test]
    fn affordability_flips_exactly_at_the_required_counts() {
        // One short is short; exactly enough is enough. The boundary is the whole of what
        // this courtesy has to get right.
        let forge = recipe(RecipeId::Forge).expect("the forge has a row");

        let short = Inventory::from_stacks(vec![stack(ITEM_STONE, 7), stack(ITEM_RAW_COAL, 2)]);
        assert!(!forge.affordable(&short));

        let exact = Inventory::from_stacks(vec![stack(ITEM_STONE, 8), stack(ITEM_RAW_COAL, 2)]);
        assert!(forge.affordable(&exact));

        // Counted across slots, exactly as the server spends across slots: four and four
        // is eight, and a per-slot test would have called this short.
        let split = Inventory::from_stacks(vec![
            stack(ITEM_STONE, 4),
            stack(ITEM_STONE, 4),
            stack(ITEM_RAW_COAL, 2),
        ]);
        assert!(forge.affordable(&split));
    }

    #[test]
    fn an_affordable_row_sends_one_craft_and_changes_no_count() {
        let (mut app, sent) = app(vec![stack(ITEM_LOG, 8)]);
        let before = app.world().resource::<Inventory>().clone();

        activate(&mut app, RecipeId::Tent);

        assert_eq!(crafted(&sent), vec![fb::RecipeID::Tent]);
        assert_eq!(
            *app.world().resource::<Inventory>(),
            before,
            "a craft request spent a material locally"
        );

        // And silence from the server needs no rollback, because nothing was spent.
        for _ in 0..4 {
            app.update();
        }
        assert!(crafted(&sent).is_empty(), "one activation sent twice");
        assert_eq!(*app.world().resource::<Inventory>(), before);
    }

    #[test]
    fn a_short_row_sends_nothing() {
        let (mut app, sent) = app(vec![stack(ITEM_LOG, 7)]);
        activate(&mut app, RecipeId::Tent);
        assert!(crafted(&sent).is_empty(), "a short recipe asked anyway");
    }

    /// Proximity is the server's call, so a forge recipe leaves this client from anywhere.
    ///
    /// The structures a snapshot names are the ones in view; a client that required one
    /// before sending would refuse crafts the server would have granted.
    #[test]
    fn a_forge_recipe_is_sent_with_no_forge_in_sight() {
        let (mut app, sent) = app(vec![stack(ITEM_STONE, 2), stack(ITEM_RAW_COAL, 1)]);
        activate(&mut app, RecipeId::SharpeningStone);
        assert_eq!(crafted(&sent), vec![fb::RecipeID::SharpeningStone]);
    }

    #[test]
    fn a_row_activated_while_dead_or_closed_never_becomes_a_request() {
        let (mut app, sent) = app(vec![stack(ITEM_LOG, 8)]);

        say_dead(&mut app, true);
        activate(&mut app, RecipeId::Tent);
        assert!(crafted(&sent).is_empty(), "a dead player crafted");

        // Coming back is not a replay: the activation that arrived while dead is gone.
        say_dead(&mut app, false);
        for _ in 0..3 {
            app.update();
        }
        assert!(crafted(&sent).is_empty());

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        activate(&mut app, RecipeId::Tent);
        assert!(
            crafted(&sent).is_empty(),
            "a row was activated with the screen closed"
        );
    }

    #[test]
    fn the_request_carries_the_shared_client_tick() {
        let (mut app, sent) = app(vec![stack(ITEM_LOG, 8)]);
        app.world_mut().resource_mut::<InputCadence>().client_tick = 77;
        activate(&mut app, RecipeId::Tent);

        let frame = sent.try_recv().expect("one craft was sent");
        let envelope = fb::root_as_envelope(&frame).expect("the client's own bytes are valid");
        let request = envelope
            .payload_as_craft_request()
            .expect("the payload is a craft request");
        assert_eq!(request.client_tick(), 77);
    }
}
