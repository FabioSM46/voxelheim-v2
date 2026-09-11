use super::*;

fn args(position: Option<&fb::Vec3>) -> fb::BlowLandedArgs<'_> {
    fb::BlowLandedArgs {
        tick: u32::MAX,
        attacker_entity_id: 0,
        target_entity_id: 7,
        position,
        kind: fb::BlowKind::Melee,
        target: fb::BlowTarget::Player,
        target_mob_kind: fb::MobKind::Unknown,
    }
}

fn encode(args: &fb::BlowLandedArgs<'_>) -> Vec<u8> {
    let mut builder = FlatBufferBuilder::new();
    let payload = fb::BlowLanded::create(&mut builder, args);
    finish_envelope(builder, fb::Payload::BlowLanded, payload.as_union_value())
}

#[test]
fn blow_kind_and_target_pairs_fail_closed_for_every_unknown_member() {
    let position = fb::Vec3::new(1.0, 2.0, 3.0);
    for kind in [0, 1, 2, 3, 4, 255] {
        for target in [0, 1, 2, 255] {
            for species in [0, 1, 2, 3, 4, 5, 255] {
                let mut wire = args(Some(&position));
                wire.kind = fb::BlowKind(kind);
                wire.target = fb::BlowTarget(target);
                wire.target_mob_kind = fb::MobKind(species);
                let decoded = decode(&encode(&wire));
                let valid = (1..=4).contains(&kind)
                    && ((target == 1 && species == 0)
                        || (target == 2 && (1..=5).contains(&species)));
                if valid {
                    let Message::BlowLanded(blow) = decoded.unwrap() else {
                        panic!("not a blow");
                    };
                    assert_eq!(blow.tick, u32::MAX);
                    assert_eq!(blow.attacker_entity_id, 0);
                    assert_eq!(blow.target_entity_id, 7);
                    assert_eq!(blow.position, [1.0, 2.0, 3.0]);
                    assert_eq!(
                        blow.kind,
                        [
                            BlowKind::Melee,
                            BlowKind::Arrow,
                            BlowKind::EnergyOrb,
                            BlowKind::MobMelee
                        ][kind as usize - 1]
                    );
                } else {
                    assert_eq!(decoded, Err(DecodeError::InvalidBlowLanded));
                }
            }
        }
    }
}

#[test]
fn blow_position_and_identity_are_required_but_anonymous_attacker_and_tick_zero_are_legal() {
    assert_eq!(
        decode(&encode(&args(None))),
        Err(DecodeError::MissingBlowPosition)
    );
    assert_eq!(
        DecodeError::MissingBlowPosition.to_string(),
        "BlowLanded carries no position"
    );
    for axis in 0..3 {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut values = [0.0; 3];
            values[axis] = bad;
            let position = fb::Vec3::new(values[0], values[1], values[2]);
            assert_eq!(
                decode(&encode(&args(Some(&position)))),
                Err(DecodeError::InvalidBlowLanded)
            );
        }
    }
    let position = fb::Vec3::new(1.0, 2.0, 3.0);
    let mut wire = args(Some(&position));
    wire.target_entity_id = 0;
    assert_eq!(decode(&encode(&wire)), Err(DecodeError::InvalidBlowLanded));
    wire.target_entity_id = 7;
    wire.tick = 0;
    assert!(matches!(decode(&encode(&wire)), Ok(Message::BlowLanded(_))));
    wire.attacker_entity_id = 8;
    assert!(matches!(decode(&encode(&wire)), Ok(Message::BlowLanded(_))));
}

#[test]
fn a_blow_requires_the_exact_snapshot_tick_position_and_target_species() {
    let mut snapshot = Snapshot {
        server_tick: u32::MAX,
        ..Snapshot::default()
    };
    let mut blow = BlowLanded {
        tick: u32::MAX,
        attacker_entity_id: 0,
        target_entity_id: 7,
        position: [1.0, 2.0, 3.0],
        kind: BlowKind::Melee,
        target: BlowTarget::Player,
    };
    assert!(!blow.matches_snapshot(&snapshot));
    snapshot.entities.push(EntityState {
        entity_id: 7,
        pos: blow.position,
        vel: [0.0; 3],
        yaw: 0.0,
        health: 100,
        max_health: 100,
    });
    assert!(blow.matches_snapshot(&snapshot));
    blow.tick = 0;
    assert!(!blow.matches_snapshot(&snapshot));
    snapshot.server_tick = 0;
    assert!(blow.matches_snapshot(&snapshot));
    blow.position[0] += 0.01;
    assert!(!blow.matches_snapshot(&snapshot));
    blow.position[0] = 1.0;
    blow.target = BlowTarget::Mob(MobKind::Draugr);
    assert!(!blow.matches_snapshot(&snapshot));
    snapshot.mobs.push(MobState {
        entity_id: 7,
        kind: MobKind::Draugr,
        pos: blow.position,
        vel: [0.0; 3],
        yaw: 0.0,
        health: 0,
        max_health: 10,
        action: MobAction::Corpse,
        target_entity_id: 0,
    });
    assert!(
        blow.matches_snapshot(&snapshot),
        "a visible killing blow remains audible"
    );
    blow.target = BlowTarget::Mob(MobKind::Vargr);
    assert!(!blow.matches_snapshot(&snapshot));
    snapshot.mobs[0].kind = MobKind::Vargr;
    assert!(blow.matches_snapshot(&snapshot));
    snapshot.mobs[0].entity_id = 8;
    assert!(!blow.matches_snapshot(&snapshot));
}

#[test]
fn requests_from_the_client_do_not_decode_as_landed_blows() {
    let request = encode_attack_request(&AttackRequest {
        slot: 0,
        client_tick: 2,
    });
    assert!(matches!(
        decode(&request),
        Ok(Message::ClientOnly("AttackRequest"))
    ));
}
