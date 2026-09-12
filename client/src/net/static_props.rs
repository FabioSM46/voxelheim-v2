//! Immutable decoration has no structure ownership, targeting or gameplay requests.
use super::{BlockCoord, DecodeError, Facing, fb};
use std::collections::HashSet;

/// The world's one capital may author this many roots, including future light fixtures.
pub const MAX_STATIC_PROPS: usize = 256;

/// Geometry family. Variants 0..3 change presentation, never physical extents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaticPropKind {
    BanquetTable,
    Chair,
    Bench,
    Throne,
    Bookcase,
    Desk,
    Counter,
    Barrel,
    EquipmentRack,
    CouncilTable,
    Rug,
    Runner,
    Banner,
    Shield,
    Trophy,
    FeastSetting,
    WallSconce,
    FloorCandelabrum,
    TableCandelabrum,
}

impl StaticPropKind {
    fn from_wire(kind: fb::StaticPropKind) -> Option<Self> {
        match kind {
            fb::StaticPropKind::BanquetTable => Some(Self::BanquetTable),
            fb::StaticPropKind::Chair => Some(Self::Chair),
            fb::StaticPropKind::Bench => Some(Self::Bench),
            fb::StaticPropKind::Throne => Some(Self::Throne),
            fb::StaticPropKind::Bookcase => Some(Self::Bookcase),
            fb::StaticPropKind::Desk => Some(Self::Desk),
            fb::StaticPropKind::Counter => Some(Self::Counter),
            fb::StaticPropKind::Barrel => Some(Self::Barrel),
            fb::StaticPropKind::EquipmentRack => Some(Self::EquipmentRack),
            fb::StaticPropKind::CouncilTable => Some(Self::CouncilTable),
            fb::StaticPropKind::Rug => Some(Self::Rug),
            fb::StaticPropKind::Runner => Some(Self::Runner),
            fb::StaticPropKind::Banner => Some(Self::Banner),
            fb::StaticPropKind::Shield => Some(Self::Shield),
            fb::StaticPropKind::Trophy => Some(Self::Trophy),
            fb::StaticPropKind::FeastSetting => Some(Self::FeastSetting),
            fb::StaticPropKind::WallSconce => Some(Self::WallSconce),
            fb::StaticPropKind::FloorCandelabrum => Some(Self::FloorCandelabrum),
            fb::StaticPropKind::TableCandelabrum => Some(Self::TableCandelabrum),
            _ => None,
        }
    }
}

/// Origin is the horizontal cell centre and the exact floor plane, with North along -Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPropState {
    pub prop_id: u64,
    pub kind: StaticPropKind,
    pub origin: BlockCoord,
    pub facing: Facing,
    pub variant: u8,
}

pub(super) fn decode(snapshot: &fb::EntitySnapshot) -> Result<Vec<StaticPropState>, DecodeError> {
    let Some(list) = snapshot.static_props() else {
        return Ok(Vec::new());
    };
    let invalid = DecodeError::InvalidStaticProp;
    if list.len() > MAX_STATIC_PROPS {
        return Err(invalid("count"));
    }
    let mut states = Vec::with_capacity(list.len());
    let mut ids = HashSet::with_capacity(list.len());
    for state in list.iter() {
        let prop_id = state.prop_id();
        if prop_id == 0 || !ids.insert(prop_id) {
            return Err(invalid("identity"));
        }
        let kind = StaticPropKind::from_wire(state.kind()).ok_or(invalid("kind"))?;
        let facing = Facing::from_wire(state.facing()).ok_or(invalid("facing"))?;
        let variant = state.variant();
        if variant > 3 {
            return Err(invalid("variant"));
        }
        let origin = state.origin().ok_or(invalid("origin"))?;
        if [origin.x(), origin.y(), origin.z()]
            .into_iter()
            .any(|v| !(-16_777_216..16_777_216).contains(&v))
        {
            return Err(invalid("origin bounds"));
        }
        states.push(StaticPropState {
            prop_id,
            kind,
            origin: BlockCoord {
                x: origin.x(),
                y: origin.y(),
                z: origin.z(),
            },
            facing,
            variant,
        });
    }
    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flatbuffers::FlatBufferBuilder;

    #[derive(Clone, Copy)]
    struct WireProp {
        id: u64,
        kind: u8,
        facing: u8,
        variant: u8,
        origin: Option<[i32; 3]>,
    }
    impl Default for WireProp {
        fn default() -> Self {
            Self {
                id: 1,
                kind: 2,
                facing: 1,
                variant: 0,
                origin: Some([-9, 21, 17]),
            }
        }
    }
    fn read(props: &[WireProp]) -> Result<Vec<StaticPropState>, DecodeError> {
        let mut b = FlatBufferBuilder::new();
        let entries: Vec<_> = props
            .iter()
            .map(|p| {
                let origin = p.origin.map(|p| fb::BlockCoord::new(p[0], p[1], p[2]));
                fb::StaticPropState::create(
                    &mut b,
                    &fb::StaticPropStateArgs {
                        prop_id: p.id,
                        kind: fb::StaticPropKind(p.kind),
                        origin: origin.as_ref(),
                        facing: fb::Facing(p.facing),
                        variant: p.variant,
                    },
                )
            })
            .collect();
        let entries = b.create_vector(&entries);
        let vitals = fb::PlayerVitals::create(&mut b, &fb::PlayerVitalsArgs::default());
        let snapshot = fb::EntitySnapshot::create(
            &mut b,
            &fb::EntitySnapshotArgs {
                self_vitals: Some(vitals),
                static_props: Some(entries),
                ..Default::default()
            },
        );
        b.finish(snapshot, None);
        decode(&flatbuffers::root::<fb::EntitySnapshot>(b.finished_data()).unwrap())
    }
    #[test]
    fn every_kind_pose_and_variant_survives_without_interpolation() {
        for kind in 1..=19 {
            for facing in 1..=4 {
                for variant in 0..=3 {
                    let got = read(&[WireProp {
                        kind,
                        facing,
                        variant,
                        ..Default::default()
                    }])
                    .unwrap();
                    assert_eq!(got.len(), 1);
                    assert_eq!(got[0].prop_id, 1);
                    assert_eq!(
                        got[0].origin,
                        BlockCoord {
                            x: -9,
                            y: 21,
                            z: 17
                        }
                    );
                    assert_eq!(
                        got[0].kind,
                        StaticPropKind::from_wire(fb::StaticPropKind(kind)).unwrap()
                    );
                    assert_eq!(
                        got[0].facing,
                        Facing::from_wire(fb::Facing(facing)).unwrap()
                    );
                    assert_eq!(got[0].variant, variant);
                }
            }
        }
    }
    #[test]
    fn count_is_bounded_before_allocating_and_ids_are_unique() {
        let props: Vec<_> = (1..=256)
            .map(|id| WireProp {
                id,
                ..Default::default()
            })
            .collect();
        assert_eq!(read(&props).unwrap().len(), 256);
        let mut over = props.clone();
        over.push(WireProp {
            id: 257,
            ..Default::default()
        });
        assert_eq!(read(&over), Err(DecodeError::InvalidStaticProp("count")));
        assert_eq!(
            read(&[props[0], props[0]]),
            Err(DecodeError::InvalidStaticProp("identity"))
        );
        assert!(read(&[]).unwrap().is_empty());
    }
    #[test]
    fn malformed_props_fail_closed() {
        for (prop, field) in [
            (
                WireProp {
                    id: 0,
                    ..Default::default()
                },
                "identity",
            ),
            (
                WireProp {
                    kind: 0,
                    ..Default::default()
                },
                "kind",
            ),
            (
                WireProp {
                    kind: 20,
                    ..Default::default()
                },
                "kind",
            ),
            (
                WireProp {
                    facing: 0,
                    ..Default::default()
                },
                "facing",
            ),
            (
                WireProp {
                    facing: 5,
                    ..Default::default()
                },
                "facing",
            ),
            (
                WireProp {
                    variant: 4,
                    ..Default::default()
                },
                "variant",
            ),
            (
                WireProp {
                    origin: None,
                    ..Default::default()
                },
                "origin",
            ),
            (
                WireProp {
                    origin: Some([16777216, 0, 0]),
                    ..Default::default()
                },
                "origin bounds",
            ),
            (
                WireProp {
                    origin: Some([0, -16777217, 0]),
                    ..Default::default()
                },
                "origin bounds",
            ),
            (
                WireProp {
                    origin: Some([0, 0, 16777216]),
                    ..Default::default()
                },
                "origin bounds",
            ),
        ] {
            assert_eq!(read(&[prop]), Err(DecodeError::InvalidStaticProp(field)));
        }
    }
}
