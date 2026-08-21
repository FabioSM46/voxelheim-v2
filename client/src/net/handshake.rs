//! The client half of the handshake, as a state machine over decoded messages.
//!
//! Plain Rust: no socket, no channel, no Bevy, no clock. The connection
//! lifecycle around it is awkward to cover exhaustively; the admission rules are
//! the part that must never drift, so they live where a table test reaches every
//! branch. `session.Handshake` on the server is factored out for exactly the same
//! reason, and this is its mirror.
//!
//! The rules come from `schemas/handshake.fbs`: exactly one exchange, always
//! first on a connection, and "a client that receives anything else before
//! `ServerWelcome` is talking to a peer that does not speak this protocol".

use std::fmt;

use super::codec::{
    ActionRefused, InventoryState, Message, MineProgress, Reject, SessionParams, Snapshot,
    WorldClock, WorldUpdate,
};

/// How far the handshake has got.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    /// `ClientHello` is on the wire and the next message decides everything.
    #[default]
    AwaitingWelcome,
    /// A validated `ServerWelcome` has arrived. There is a session.
    Established,
}

/// What a message did to the handshake.
#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    /// The session exists, with these authoritative parameters.
    Established(SessionParams),
    /// The server refused. It closes the connection immediately after, so there
    /// is no phase to move to — the reason is the only thing left to carry.
    Refused(Reject),
    /// A post-handshake payload nothing consumes yet, named for diagnostics.
    ///
    /// Dropped rather than refused, mirroring how the server accepts and drops a
    /// `PlayerInput` it cannot simulate yet: a peer that is merely early must not
    /// be disconnected for it.
    Ignored(&'static str),
    /// Something the world module owns, admitted because a session exists.
    World(WorldUpdate),
    /// One tick of authoritative entity state, admitted because a session exists.
    Snapshot(Snapshot),
    /// The player's complete authoritative inventory, admitted because a session exists.
    Inventory(InventoryState),
    /// Authoritative progress for the voxel currently being mined.
    MineProgress(MineProgress),
    /// The server refused an action, admitted because a session exists.
    ///
    /// Named apart from [`Self::Refused`] above, which is the *connection* being
    /// refused and ends it. This one is an answer inside a session that continues.
    ActionRefused(ActionRefused),
}

/// A message that breaks the handshake's rules. Every variant ends the
/// connection, because there is no way to resynchronise a peer that has already
/// proven it is not following the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    /// Something other than `ServerWelcome` or `ServerReject` arrived before the
    /// welcome.
    Premature(&'static str),
    /// A second handshake message on a session that already has one.
    Repeated(&'static str),
    /// A payload only a client sends, arriving from the server.
    WrongDirection(&'static str),
    /// A handshake-phase payload arriving on a session that already has a welcome.
    ///
    /// The mirror of [`Self::Premature`] rather than a second name for it, and distinct
    /// from [`Self::Repeated`], which is a *second* welcome or reject.
    /// `ServerCharacterList` before the welcome is exactly on time from V7; after the
    /// welcome the server is answering a phase this session has already left.
    OutOfPhase(&'static str),
    /// Inventory pair count must match the value announced in ServerWelcome.
    InventorySlots { expected: u8, got: usize },
    /// A snapshot's `tick_of_day` is at or beyond the day length announced in
    /// ServerWelcome.
    ///
    /// Only reachable on a session whose welcome declared a clock: a `day_length_ticks`
    /// of zero says the server keeps none, and a snapshot's tick of day is then not
    /// read at all rather than being checked against zero — which nothing could satisfy.
    TickOfDay { day_length: u32, got: u32 },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Premature(kind) => {
                write!(
                    f,
                    "expected ServerWelcome or ServerReject first, got {kind}"
                )
            }
            Self::Repeated(kind) => write!(f, "second {kind} on an established session"),
            Self::WrongDirection(kind) => {
                write!(f, "server sent {kind}, which only a client sends")
            }
            Self::OutOfPhase(kind) => {
                write!(
                    f,
                    "{kind} belongs to the handshake and this session is past it"
                )
            }
            Self::InventorySlots { expected, got } => write!(
                f,
                "InventoryState has {got} slots, want the {expected} announced by ServerWelcome"
            ),
            Self::TickOfDay { day_length, got } => write!(
                f,
                "EntitySnapshot has tick_of_day {got}, want less than the {day_length}-tick day announced by ServerWelcome"
            ),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// The handshake's state. One per connection.
#[derive(Debug, Default)]
pub struct Handshake {
    phase: Phase,
    inventory_slots: Option<u8>,
    clock: WorldClock,
}

impl Handshake {
    /// A handshake that has sent its hello and is waiting for the answer.
    pub fn new() -> Self {
        Self::default()
    }

    /// How far the handshake has got.
    ///
    /// Test-only: production code asks the narrower [`Self::established`], because
    /// the phase is an implementation detail and every caller so far only needs to
    /// know whether there is a session.
    #[cfg(test)]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Whether a session exists. Callers use this to decide whether a failure is
    /// a refusal the player must read or a disconnection from a running game.
    pub fn established(&self) -> bool {
        self.phase == Phase::Established
    }

    /// Feeds the handshake one decoded message.
    ///
    /// Exhaustive over phase × message and free of panics: every combination is
    /// either a transition or a named error, so a peer cannot reach an unhandled
    /// state by sending things in an unexpected order.
    pub fn apply(&mut self, message: Message) -> Result<Transition, HandshakeError> {
        match (self.phase, message) {
            // Direction first: a payload only a client sends is wrong in every
            // phase, and saying so is more useful than "unexpected".
            (_, Message::ClientOnly(kind)) => Err(HandshakeError::WrongDirection(kind)),

            (Phase::AwaitingWelcome, Message::Welcome(params)) => {
                self.phase = Phase::Established;
                self.inventory_slots = Some(params.inventory_slots);
                self.clock = params.clock;
                Ok(Transition::Established(params))
            }
            (Phase::AwaitingWelcome, Message::Reject(reject)) => Ok(Transition::Refused(reject)),
            (Phase::AwaitingWelcome, Message::Deferred(kind)) => {
                Err(HandshakeError::Premature(kind))
            }
            // Terrain before the welcome is refused for the same reason as anything
            // else: `chunk_size` arrives *in* the welcome, so a chunk that precedes
            // it cannot even be expanded, let alone trusted.
            (Phase::AwaitingWelcome, Message::World(update)) => {
                Err(HandshakeError::Premature(world_payload_name(&update)))
            }
            // A snapshot before the welcome is refused for the same reason as terrain:
            // `entity_id` arrives *in* the welcome, so a snapshot that precedes it names
            // nobody the client can identify as itself.
            (Phase::AwaitingWelcome, Message::Snapshot(_)) => {
                Err(HandshakeError::Premature("EntitySnapshot"))
            }
            (Phase::AwaitingWelcome, Message::Inventory(_)) => {
                Err(HandshakeError::Premature("InventoryState"))
            }
            (Phase::AwaitingWelcome, Message::MineProgress(_)) => {
                Err(HandshakeError::Premature("MineProgress"))
            }
            // Refused before the welcome, for the reason everything else is: the one
            // refusal that may precede a session is `ServerReject`, which has its own
            // payload and closes the connection. A server answering an action nobody
            // could have taken yet is not one this client can go on talking to.
            (Phase::AwaitingWelcome, Message::ActionRefused(_)) => {
                Err(HandshakeError::Premature("ActionRefused"))
            }
            // On time from V7, and dropped rather than refused because this build has
            // no character-select screen — that is a separate issue, and the vocabulary
            // landing before the screen is the whole point of a contract-only change.
            // A server that speaks V7's handshake will wait for a selection this client
            // never sends; a server that speaks V6's sends a welcome and nothing here
            // is reached at all. Refusing would disconnect a peer that is merely ahead,
            // which is what `Transition::Ignored` exists to avoid.
            (Phase::AwaitingWelcome, Message::CharacterList(_)) => {
                Ok(Transition::Ignored("ServerCharacterList"))
            }
            // An appearance names an entity id, and `ServerWelcome.entity_id` is how
            // this session learns which one is its own — so an appearance that precedes
            // the welcome describes somebody nobody can identify. Refused for the reason
            // a snapshot is.
            (Phase::AwaitingWelcome, Message::PlayerAppearance(_)) => {
                Err(HandshakeError::Premature("PlayerAppearance"))
            }

            (Phase::Established, Message::Welcome(_)) => {
                Err(HandshakeError::Repeated("ServerWelcome"))
            }
            (Phase::Established, Message::Reject(_)) => {
                Err(HandshakeError::Repeated("ServerReject"))
            }
            (Phase::Established, Message::Deferred(kind)) => Ok(Transition::Ignored(kind)),
            (Phase::Established, Message::World(update)) => Ok(Transition::World(update)),
            (Phase::Established, Message::Snapshot(snapshot)) => {
                // The snapshot's half of the world clock, checked here for the reason the
                // inventory's slot count is: `tick_of_day` is bounded by a number that
                // arrived in the *welcome*, and the codec decodes one frame at a time.
                // This layer is the only one holding both.
                //
                // A zero day length is a server that keeps no clock, and then there is
                // nothing to check — not a bound of zero that every snapshot would fail.
                if self.clock.declared() && snapshot.tick_of_day >= self.clock.day_length_ticks {
                    Err(HandshakeError::TickOfDay {
                        day_length: self.clock.day_length_ticks,
                        got: snapshot.tick_of_day,
                    })
                } else {
                    Ok(Transition::Snapshot(snapshot))
                }
            }
            (Phase::Established, Message::Inventory(inventory)) => {
                // One length for three wire vectors, and deliberately so. `stacks`,
                // `durability` and `max_durability` are each slot-indexed and the codec
                // has already refused any state where they disagree, so a decoded
                // `InventoryState` has exactly one length to check — and checking it
                // here is what ties all three to the `inventory_slots` the welcome
                // announced. This layer is the only one that knows that number.
                let expected = self.inventory_slots.unwrap_or_default();
                if inventory.stacks.len() != usize::from(expected) {
                    Err(HandshakeError::InventorySlots {
                        expected,
                        got: inventory.stacks.len(),
                    })
                } else {
                    Ok(Transition::Inventory(inventory))
                }
            }
            (Phase::Established, Message::MineProgress(progress)) => {
                Ok(Transition::MineProgress(progress))
            }
            // Nothing to check against the welcome, unlike the two above it: a refusal
            // carries no slot count and no tick of day, and its two enums already read
            // every value they cannot name as `Unknown`.
            (Phase::Established, Message::ActionRefused(refused)) => {
                Ok(Transition::ActionRefused(refused))
            }
            // The character phase is over: this session has a character, because it has
            // a welcome. A list arriving now is a server that has lost track of where
            // the handshake is, and there is no way to resynchronise one of those.
            (Phase::Established, Message::CharacterList(_)) => {
                Err(HandshakeError::OutOfPhase("ServerCharacterList"))
            }
            // Admitted because a session exists, and carried no further: the appearance
            // is decoded and validated here, and nothing draws one until the issue that
            // gives players a body worth colouring. `MineProgress` spent Protocol V2 in
            // exactly this state.
            (Phase::Established, Message::PlayerAppearance(_)) => {
                Ok(Transition::Ignored("PlayerAppearance"))
            }
        }
    }
}

/// The schema name of a world payload, for the diagnostics an error carries.
///
/// A `&'static str` rather than the enum itself, so [`HandshakeError`] stays a
/// small `Copy`-ish value that a log line can be written from — the same shape the
/// other three variants already have.
fn world_payload_name(update: &WorldUpdate) -> &'static str {
    match update {
        WorldUpdate::Chunk { .. } => "ChunkData",
        WorldUpdate::Unload { .. } => "ChunkUnload",
        WorldUpdate::Block { .. } => "BlockUpdate",
    }
}

#[cfg(test)]
mod tests {
    use super::super::codec::{
        ActionRefused, BlockCoord, CharacterList, ChunkCoord, InventoryStack, MineProgress,
        PLACEHOLDER_APPEARANCE, PlayerAppearance,
    };
    use super::*;

    fn params() -> SessionParams {
        SessionParams {
            clock: Default::default(),
            entity_id: 7,
            spawn: [0.5, 80.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 1,
            hotbar_slots: 1,
            player_token: crate::net::ANY_TOKEN,
        }
    }

    fn reject() -> Reject {
        Reject {
            code: "PROTOCOL_MISMATCH",
            detail: "server speaks protocol 1, client speaks 2".to_owned(),
        }
    }

    #[test]
    fn a_fresh_handshake_is_awaiting_the_welcome() {
        let handshake = Handshake::new();

        assert_eq!(handshake.phase(), Phase::AwaitingWelcome);
        assert!(!handshake.established());
    }

    #[test]
    fn a_welcome_establishes_the_session() {
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::Welcome(params())),
            Ok(Transition::Established(params()))
        );
        assert_eq!(handshake.phase(), Phase::Established);
        assert!(handshake.established());
    }

    #[test]
    fn a_reject_refuses_the_session_and_preserves_the_reason() {
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::Reject(reject())),
            Ok(Transition::Refused(reject()))
        );
        assert!(
            !handshake.established(),
            "a refused handshake never becomes a session"
        );
    }

    fn chunk() -> WorldUpdate {
        WorldUpdate::Chunk {
            coord: ChunkCoord {
                cx: 0,
                cy: 2,
                cz: 0,
            },
            runs: vec![0, 32768],
        }
    }

    fn unload() -> WorldUpdate {
        WorldUpdate::Unload {
            coord: ChunkCoord {
                cx: 0,
                cy: 2,
                cz: 0,
            },
        }
    }

    fn block() -> WorldUpdate {
        WorldUpdate::Block {
            pos: super::super::codec::BlockCoord { x: 3, y: 70, z: -1 },
            block_id: 0,
        }
    }

    #[test]
    fn anything_else_before_the_welcome_ends_the_connection() {
        let mut handshake = Handshake::new();

        // A payload from a newer contract than this build knows. Named rather than
        // anonymous, because the error is what the player reads.
        assert_eq!(
            handshake.apply(Message::Deferred("SomethingNewer")),
            Err(HandshakeError::Premature("SomethingNewer"))
        );
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            server_tick: 12,
            entities: vec![super::super::codec::EntityState {
                entity_id: 7,
                pos: [0.5, 64.0, 0.5],
                vel: [0.0, 0.0, 0.0],
                yaw: 0.0,
            }],
            drops: vec![],
            ..Default::default()
        }
    }

    /// One durable slot, deliberately: durability crosses this layer as part of a slot
    /// rather than as vectors of its own, so the pass-through test below is what says
    /// nothing here splits it back apart.
    fn inventory() -> InventoryState {
        InventoryState {
            stacks: vec![InventoryStack {
                item_id: 7,
                count: 1,
                durability: 35,
                max_durability: 100,
            }],
        }
    }

    #[test]
    fn an_inventory_before_the_welcome_ends_the_connection() {
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::Inventory(inventory())),
            Err(HandshakeError::Premature("InventoryState"))
        );
    }

    #[test]
    fn an_inventory_after_the_welcome_reaches_the_player_module() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::Inventory(inventory())),
            Ok(Transition::Inventory(inventory()))
        );
        assert!(handshake.established(), "the session survives it");
    }

    #[test]
    fn inventory_slot_count_must_match_the_welcome() {
        for got in [0, 2] {
            let mut handshake = Handshake::new();
            let _ = handshake.apply(Message::Welcome(params()));
            let state = InventoryState {
                stacks: vec![
                    InventoryStack {
                        item_id: 0,
                        count: 0,
                        ..Default::default()
                    };
                    got
                ],
            };

            assert_eq!(
                handshake.apply(Message::Inventory(state)),
                Err(HandshakeError::InventorySlots { expected: 1, got })
            );
        }
    }

    /// A session whose welcome declared a clock, for the tick-of-day tests below.
    fn params_with_clock() -> SessionParams {
        SessionParams {
            clock: WorldClock {
                day_length_ticks: 24_000,
                night_start_ticks: 14_400,
                night_end_ticks: 21_600,
            },
            ..params()
        }
    }

    /// A snapshot's tick of day is bounded by the day length the welcome announced, and
    /// this layer is where the two meet.
    ///
    /// The last tick of the day is legal and the first tick past it is not, which is the
    /// whole of the rule: `tick_of_day < day_length_ticks`. Both sides of that boundary
    /// are asserted, because a `<=` here would pass every test that only checked a value
    /// far from the edge.
    #[test]
    fn a_tick_of_day_must_be_inside_the_announced_day() {
        for (tick_of_day, accepted) in [
            (0, true),
            (23_999, true),
            (24_000, false),
            (u32::MAX, false),
        ] {
            let mut handshake = Handshake::new();
            let _ = handshake.apply(Message::Welcome(params_with_clock()));
            let snapshot = Snapshot {
                tick_of_day,
                ..snapshot()
            };

            let applied = handshake.apply(Message::Snapshot(snapshot.clone()));
            if accepted {
                assert_eq!(applied, Ok(Transition::Snapshot(snapshot)));
            } else {
                assert_eq!(
                    applied,
                    Err(HandshakeError::TickOfDay {
                        day_length: 24_000,
                        got: tick_of_day,
                    })
                );
            }
        }
    }

    /// A server that keeps no clock has no bound to break.
    ///
    /// Nothing is checked rather than everything being checked against zero — which is
    /// the difference between a rule that does not apply and a rule no snapshot could
    /// ever satisfy. Every server in this repository is in this state today, so this is
    /// the path that actually runs.
    #[test]
    fn a_tick_of_day_is_not_read_when_no_clock_was_announced() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));
        let snapshot = Snapshot {
            tick_of_day: u32::MAX,
            ..snapshot()
        };

        assert_eq!(
            handshake.apply(Message::Snapshot(snapshot.clone())),
            Ok(Transition::Snapshot(snapshot))
        );
    }

    #[test]
    fn mining_progress_requires_a_session_then_reaches_its_consumer() {
        let message = || {
            Message::MineProgress(MineProgress {
                pos: BlockCoord { x: 1, y: 2, z: 3 },
                progress: 128,
            })
        };
        let mut fresh = Handshake::new();
        assert_eq!(
            fresh.apply(message()),
            Err(HandshakeError::Premature("MineProgress"))
        );

        let mut established = Handshake::new();
        let _ = established.apply(Message::Welcome(params()));
        assert_eq!(
            established.apply(message()),
            Ok(Transition::MineProgress(MineProgress {
                pos: BlockCoord { x: 1, y: 2, z: 3 },
                progress: 128,
            }))
        );
    }

    #[test]
    fn a_snapshot_before_the_welcome_ends_the_connection() {
        // `entity_id` arrives *in* the welcome, so a snapshot that precedes it names nobody
        // the client can identify as itself.
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::Snapshot(snapshot())),
            Err(HandshakeError::Premature("EntitySnapshot"))
        );
    }

    #[test]
    fn a_snapshot_after_the_welcome_reaches_the_player_module() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::Snapshot(snapshot())),
            Ok(Transition::Snapshot(snapshot()))
        );
        assert!(handshake.established(), "the session survives it");
    }

    #[test]
    fn terrain_before_the_welcome_ends_the_connection() {
        // chunk_size arrives in the welcome, so a chunk that precedes it cannot be
        // expanded at all — there is no length to check the runs against. A block
        // edit fails for the same missing number: without `chunk_size` a world block
        // coordinate cannot be resolved to a chunk and an index inside it.
        for (update, name) in [
            (chunk(), "ChunkData"),
            (unload(), "ChunkUnload"),
            (block(), "BlockUpdate"),
        ] {
            let mut handshake = Handshake::new();
            assert_eq!(
                handshake.apply(Message::World(update)),
                Err(HandshakeError::Premature(name))
            );
        }
    }

    #[test]
    fn terrain_after_the_welcome_reaches_the_world_module() {
        for update in [chunk(), unload(), block()] {
            let mut handshake = Handshake::new();
            let _ = handshake.apply(Message::Welcome(params()));

            assert_eq!(
                handshake.apply(Message::World(update.clone())),
                Ok(Transition::World(update))
            );
            assert!(handshake.established(), "the session survives it");
        }
    }

    #[test]
    fn an_empty_payload_before_the_welcome_ends_the_connection() {
        // An Envelope with no payload decodes as the union's NONE member. It must
        // fail closed rather than count as "nothing happened".
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::Deferred("NONE")),
            Err(HandshakeError::Premature("NONE"))
        );
    }

    #[test]
    fn a_client_only_payload_is_refused_in_either_phase() {
        for kind in [
            "ClientHello",
            "PlayerInput",
            "BlockEditRequest",
            "ChunkResendRequest",
            "MineRequest",
            "InventoryMoveRequest",
        ] {
            let mut fresh = Handshake::new();
            assert_eq!(
                fresh.apply(Message::ClientOnly(kind)),
                Err(HandshakeError::WrongDirection(kind))
            );

            let mut established = Handshake::new();
            let _ = established.apply(Message::Welcome(params()));
            assert_eq!(
                established.apply(Message::ClientOnly(kind)),
                Err(HandshakeError::WrongDirection(kind))
            );
        }
    }

    #[test]
    fn a_deferred_payload_after_the_welcome_is_dropped() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::Deferred("SomethingNewer")),
            Ok(Transition::Ignored("SomethingNewer"))
        );
        assert!(handshake.established(), "the session survives it");
    }

    /// A refusal on a live session is admitted, and before the welcome it is premature.
    ///
    /// The `AwaitingWelcome` half is the one worth stating: the only refusal that may
    /// precede a session is `ServerReject`, which has its own payload and closes the
    /// connection. A server answering an action nobody could have taken yet is not one
    /// this client can go on talking to.
    #[test]
    fn a_refusal_is_admitted_on_a_session_and_premature_before_one() {
        let refused = ActionRefused {
            action: crate::net::RefusedAction::PlaceStructure,
            reason: crate::net::RefusalReason::GroundIsAir,
            anchor: None,
        };

        let mut early = Handshake::new();
        assert_eq!(
            early.apply(Message::ActionRefused(refused)),
            Err(HandshakeError::Premature("ActionRefused"))
        );

        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));
        assert_eq!(
            handshake.apply(Message::ActionRefused(refused)),
            Ok(Transition::ActionRefused(refused))
        );
        assert!(handshake.established(), "the session goes on");
    }

    /// **The property that let tag 20 ship without a version bump.**
    ///
    /// A build one contract behind cannot name the payload it is being sent, so it reads
    /// it as [`Message::Deferred`] — and this layer drops that and keeps the session. That
    /// is the whole argument for appending a union member instead of raising
    /// `ProtocolVersion.Current`: the older peer loses the feedback and nothing else.
    ///
    /// Written as an unnameable tag rather than as `ActionRefused` on purpose. This build
    /// *does* know tag 20, so asserting anything about it here would test the opposite
    /// case; what has to hold is the treatment of a tag nobody in this build can name,
    /// which is exactly what `ActionRefused` was to every build before this one.
    #[test]
    fn a_payload_from_a_newer_contract_is_dropped_and_the_session_goes_on() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        // What `decode` answers for a tag this build cannot name — see
        // `the_fallback_is_reachable_only_for_a_tag_this_build_cannot_name`, which sweeps
        // every byte past the end of the union to prove that is the only way here.
        assert_eq!(
            handshake.apply(Message::Deferred(super::super::codec::UNKNOWN_VARIANT)),
            Ok(Transition::Ignored(super::super::codec::UNKNOWN_VARIANT))
        );
        assert!(handshake.established(), "the session survives it");
    }

    #[test]
    fn a_second_welcome_ends_the_connection() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::Welcome(params())),
            Err(HandshakeError::Repeated("ServerWelcome"))
        );
    }

    #[test]
    fn a_reject_after_the_welcome_ends_the_connection() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::Reject(reject())),
            Err(HandshakeError::Repeated("ServerReject"))
        );
    }

    // -----------------------------------------------------------------------
    // Protocol V7 — the character phase, and what this build does with it
    // -----------------------------------------------------------------------

    fn character_list() -> CharacterList {
        CharacterList {
            characters: Vec::new(),
            max_characters: 3,
        }
    }

    fn player_appearance() -> PlayerAppearance {
        PlayerAppearance {
            entity_id: 1,
            appearance: PLACEHOLDER_APPEARANCE,
        }
    }

    /// A character list before the welcome is **on time** from V7, and this build drops
    /// it rather than refusing: it has no character-select screen yet, and disconnecting
    /// a peer that is merely ahead is what `Transition::Ignored` exists to avoid.
    ///
    /// The consequence is deliberate and worth stating: against a server that speaks
    /// V7's handshake this session then waits for a welcome that will not come, because
    /// nothing here sends a selection. Against the servers in this repository, which
    /// still speak V6's handshake, nothing reaches this arm at all.
    #[test]
    fn a_character_list_before_the_welcome_is_dropped_rather_than_refused() {
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::CharacterList(character_list())),
            Ok(Transition::Ignored("ServerCharacterList"))
        );
        assert_eq!(handshake.phase(), Phase::AwaitingWelcome);
    }

    /// After the welcome the character phase is over: this session has a character,
    /// because it has a welcome.
    #[test]
    fn a_character_list_after_the_welcome_ends_the_connection() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::CharacterList(character_list())),
            Err(HandshakeError::OutOfPhase("ServerCharacterList"))
        );
    }

    /// An appearance names an entity id, and `ServerWelcome.entity_id` is how a session
    /// learns which one is its own — so one that precedes the welcome describes somebody
    /// nobody can identify. Refused for the reason a snapshot is.
    #[test]
    fn an_appearance_before_the_welcome_ends_the_connection() {
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::PlayerAppearance(player_appearance())),
            Err(HandshakeError::Premature("PlayerAppearance"))
        );
    }

    /// Admitted on a session and carried no further: validated at the decode boundary,
    /// drawn by nobody until the issue that gives players a body worth colouring.
    #[test]
    fn an_appearance_after_the_welcome_is_admitted_and_dropped() {
        let mut handshake = Handshake::new();
        let _ = handshake.apply(Message::Welcome(params()));

        assert_eq!(
            handshake.apply(Message::PlayerAppearance(player_appearance())),
            Ok(Transition::Ignored("PlayerAppearance"))
        );
    }

    /// Direction beats phase, and it has to: a payload only a client sends is wrong
    /// wherever it turns up, and saying so is more useful than "out of phase".
    #[test]
    fn the_character_requests_are_refused_for_their_direction_in_either_phase() {
        for kind in ["SelectCharacterRequest", "CreateCharacterRequest"] {
            let mut fresh = Handshake::new();
            assert_eq!(
                fresh.apply(Message::ClientOnly(kind)),
                Err(HandshakeError::WrongDirection(kind))
            );

            let mut established = Handshake::new();
            let _ = established.apply(Message::Welcome(params()));
            assert_eq!(
                established.apply(Message::ClientOnly(kind)),
                Err(HandshakeError::WrongDirection(kind))
            );
        }
    }
}
