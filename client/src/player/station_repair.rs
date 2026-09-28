//! Forge repair presentation and intent. The inventory is always the last server answer;
//! prices here are previews, never permission to repair or to spend the purse.

use bevy::prelude::*;

use super::{
    ApplyInputMode, ApplyInventory, ApplySnapshots, InputGate, InputMode, Inventory, SelfVitals,
    StationWindow, ViewMode,
};
use crate::net::{
    Outbound, Sent, StationRepairRequest, StructureKind, encode_station_repair_request,
};
use crate::ui::{PlayerMessage, PlayerMessageKind, PublishPlayerMessages};

/// Display-only mirror of RepairSilverPerPoint in server/internal/game/station_repair.go.
/// The contract carries no price quote: the server rechecks and charges its own rate.
/// The source-parity test below makes a balance change update this preview as well.
pub(crate) const REPAIR_SILVER_PER_POINT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RepairPreview {
    pub slot: u16,
    pub item_id: u16,
    pub missing: u16,
    pub price: u32,
}

/// Filter the last authoritative state for display only. No purse, reach or equipment
/// rule is inferred here, and the request path does not treat these rows as eligibility.
pub(crate) fn repair_previews(inventory: &Inventory) -> Vec<RepairPreview> {
    inventory
        .stacks()
        .iter()
        .enumerate()
        .filter_map(|(slot, stack)| {
            if stack.item_id == 0
                || stack.count == 0
                || stack.max_durability == 0
                || stack.durability >= stack.max_durability
            {
                return None;
            }
            let missing = stack.max_durability - stack.durability;
            Some(RepairPreview {
                slot: u16::try_from(slot).ok()?,
                item_id: stack.item_id,
                missing,
                price: (u32::from(missing) * REPAIR_SILVER_PER_POINT).max(1),
            })
        })
        .collect()
}

#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StationRepairClick {
    pub slot: u16,
}

/// Order the UI's clicks before their sender without giving the UI a socket.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct OriginateStationRepair;

pub(super) struct StationRepairPlugin;

impl Plugin for StationRepairPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<StationRepairClick>()
            .add_message::<PlayerMessage>()
            .init_resource::<StationWindow>()
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .add_systems(
                Update,
                send_repair_intents
                    .in_set(PublishPlayerMessages)
                    .after(OriginateStationRepair)
                    .after(ApplyInventory)
                    .after(ApplyInputMode)
                    .after(ApplySnapshots),
            );
    }
}

fn send_repair_intents(
    mut clicks: MessageReader<StationRepairClick>,
    gate: InputGate<'_>,
    window: Res<StationWindow>,
    outbound: Option<ResMut<Outbound>>,
    mut messages: MessageWriter<PlayerMessage>,
) {
    // Drain on every frame, including a closed/dead screen, so a stale click can never
    // be replayed at a different station after the player returns.
    let presses: Vec<_> = clicks.read().copied().collect();
    if gate.dead()
        || gate.mode() != InputMode::Station
        || window.station() != Some(StructureKind::Forge)
    {
        return;
    }
    let Some(mut outbound) = outbound else {
        return;
    };
    for click in presses {
        // No local affordability, reach or wear gate, and no optimistic inventory edit.
        match outbound.send(encode_station_repair_request(&StationRepairRequest {
            target_slot: click.slot,
        })) {
            Sent::Queued => {}
            Sent::Dropped | Sent::Closed => {
                messages.write(PlayerMessage::new(
                    PlayerMessageKind::Error,
                    "Your repair request did not reach the server; try again.",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{InventoryStack, LifeState, PlayerVitals};
    use crate::wire::voxelheim::net as fb;

    fn worn(durability: u16, max_durability: u16) -> InventoryStack {
        InventoryStack {
            item_id: 10,
            count: 1,
            durability,
            max_durability,
        }
    }

    #[test]
    fn preview_rate_is_pinned_to_the_server_constant() {
        let source = include_str!("../../../server/internal/game/station_repair.go");
        let declaration = format!("const RepairSilverPerPoint uint32 = {REPAIR_SILVER_PER_POINT}");
        assert!(
            source.lines().any(|line| line.trim() == declaration),
            "update the display rate when the server rate changes"
        );
    }

    #[test]
    fn previews_list_all_worn_slots_without_an_affordability_gate() {
        let mut stacks = vec![InventoryStack::default(); 41];
        stacks[0] = worn(99, 100);
        stacks[10] = worn(0, 200);
        stacks[40] = worn(20, 300);
        stacks[1] = worn(100, 100);
        stacks[2] = worn(0, 0);
        stacks[3] = InventoryStack {
            count: 0,
            ..worn(0, 100)
        };
        let inventory = Inventory::from_state(stacks, 0);
        let before = inventory.clone();
        assert_eq!(
            repair_previews(&inventory),
            vec![
                RepairPreview {
                    slot: 0,
                    item_id: 10,
                    missing: 1,
                    price: 1
                },
                RepairPreview {
                    slot: 10,
                    item_id: 10,
                    missing: 200,
                    price: 200
                },
                RepairPreview {
                    slot: 40,
                    item_id: 10,
                    missing: 280,
                    price: 280
                },
            ]
        );
        assert_eq!(inventory, before);
        assert_eq!(
            repair_previews(&Inventory::from_stacks(vec![worn(0, u16::MAX)]))[0].price,
            u32::from(u16::MAX)
        );
    }

    #[test]
    fn each_click_sends_one_slot_without_mutating_or_rechecking_inventory() {
        let (outbound, received) = Outbound::to_a_test(8);
        let inventory = Inventory::from_state(vec![worn(99, 100)], 0);
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(StationRepairPlugin)
            .insert_resource(outbound)
            .insert_resource(inventory.clone())
            .insert_resource(InputMode::Station)
            .insert_resource(StationWindow::at(1, StructureKind::Forge));
        // A row can become stale between drawing and dispatch. The server answers it,
        // even when the index or purse no longer matches what was on screen.
        for slot in [0, u16::MAX] {
            app.world_mut().write_message(StationRepairClick { slot });
        }
        app.update();
        let slots: Vec<_> = received
            .try_iter()
            .map(|frame| {
                fb::root_as_envelope(&frame)
                    .unwrap()
                    .payload_as_station_repair_request()
                    .unwrap()
                    .target_slot()
            })
            .collect();
        assert_eq!(slots, vec![0, u16::MAX]);
        app.update();
        assert!(received.try_recv().is_err());
        assert_eq!(app.world().resource::<Inventory>(), &inventory);
    }

    #[test]
    fn hidden_nonforge_or_dead_screens_discard_clicks_instead_of_replaying_them() {
        for (mode, kind, dead) in [
            (InputMode::Playing, StructureKind::Forge, false),
            (InputMode::Station, StructureKind::LeatherBench, false),
            (InputMode::Station, StructureKind::ArmourBench, false),
            (InputMode::Station, StructureKind::Forge, true),
        ] {
            let (outbound, received) = Outbound::to_a_test(8);
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .add_plugins(StationRepairPlugin)
                .insert_resource(outbound)
                .insert_resource(mode)
                .insert_resource(StationWindow::at(1, kind));
            if dead {
                app.insert_resource(SelfVitals::from_server(PlayerVitals {
                    health: 0,
                    life_state: LifeState::Dead,
                    ..PlayerVitals::unharmed()
                }));
            }
            app.world_mut()
                .write_message(StationRepairClick { slot: 0 });
            app.update();
            app.insert_resource(InputMode::Station)
                .insert_resource(StationWindow::at(1, StructureKind::Forge))
                .insert_resource(SelfVitals::default());
            app.update();
            assert!(received.try_recv().is_err());
        }
    }
}
