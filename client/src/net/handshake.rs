//! The client half of the handshake, as a state machine over decoded messages.
//!
//! Plain Rust: no socket, no channel, no Bevy, no clock. The connection
//! lifecycle around it is awkward to cover exhaustively; the admission rules are
//! the part that must never drift, so they live where a table test reaches every
//! branch. `session.Handshake` on the server is factored out for exactly the same
//! reason, and this is its mirror.
//!
//! The rules come from `schemas/handshake.fbs`, and from V7 they describe three
//! exchanges rather than one:
//!
//! ```text
//!     ClientHello
//!         -> ServerCharacterList
//!             -> SelectCharacterRequest | CreateCharacterRequest
//!                 -> ServerWelcome
//! ```
//!
//! `ServerReject` is legal in place of any server message and closes the connection, and
//! "a client that receives anything else before `ServerWelcome` is talking to a peer that
//! does not speak this protocol".
//!
//! **The middle exchange is the one a person is inside**, which is why this module has a
//! phase for it and one input that is not a message: [`Handshake::chose`]. A welcome
//! answers a choice, so a client that has not made one has been sent an answer to a
//! question it never asked — and the spawn in that welcome belongs to a character nobody
//! picked.

use std::fmt;

use super::codec::{
    ActionRefused, CharacterList, ChatMessage, InventoryState, LeaveStarted, LifeState, Message,
    MineProgress, PartyInvite, PlayerAppearance, Reject, SessionParams, Snapshot, WorldClock,
    WorldUpdate,
};

/// How far the handshake has got.
///
/// **Four phases where V6 had two**, and the two in the middle are the character phase
/// this contract added: a hello is answered with the account's characters, and only a
/// selection or a creation earns a welcome. `schemas/handshake.fbs` holds the reason —
/// `ServerWelcome.spawn` belongs to a character, so it cannot be sent before there is
/// one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Phase {
    /// `ClientHello` is on the wire and the account's characters are what answers it.
    #[default]
    AwaitingCharacters,
    /// The list arrived and nothing has been sent back. **The one phase this client
    /// spends waiting for a person** rather than for a peer: what ends it is a player
    /// choosing, which reaches this state machine through [`Handshake::chose`] rather
    /// than through a message.
    Choosing,
    /// A choice is on the wire and the welcome is what answers it.
    AwaitingWelcome,
    /// A validated `ServerWelcome` has arrived. There is a session.
    Established,
}

/// What a message did to the handshake.
#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    /// The characters this account owns here, and the number it may hold. The player
    /// chooses one; nothing about that choice is decided in this module.
    Characters(CharacterList),
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
    /// What one visible player looks like, admitted because a session exists.
    ///
    /// It names an entity rather than answering about one this session already has, and
    /// that is not a check this layer can make: `schemas/player.fbs` says the appearance
    /// and the snapshot streams are not ordered against each other, so an appearance for
    /// an entity no snapshot has mentioned yet is the ordinary case rather than a fault.
    Appearance(PlayerAppearance),
    /// The server refused an action, admitted because a session exists.
    ///
    /// Named apart from [`Self::Refused`] above, which is the *connection* being
    /// refused and ends it. This one is an answer inside a session that continues.
    ActionRefused(ActionRefused),
    /// The authoritative leave timer for this session.
    Leaving(LeaveStarted),
    /// One world-chat line accepted and attributed by the authoritative server.
    Chat(ChatMessage),
    /// One still-live invitation issued by the authoritative server.
    PartyInvite(PartyInvite),
}

/// A message that breaks the handshake's rules. Every variant ends the
/// connection, because there is no way to resynchronise a peer that has already
/// proven it is not following the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    /// A payload that belongs to a session, arriving before there is one.
    Premature(&'static str),
    /// A second handshake message on a session that already has one.
    Repeated(&'static str),
    /// A payload only a client sends, arriving from the server.
    WrongDirection(&'static str),
    /// A welcome answering a choice this client has not made.
    ///
    /// **The one rule this state machine keeps that it cannot read off the wire.** A
    /// welcome is the answer to a selection or a creation; before one has gone out there
    /// is no question for it to be answering, and a client that took it anyway would
    /// enter the world as a character nobody picked. What makes it checkable is
    /// [`Handshake::chose`], which the session thread calls where it writes the frame.
    ///
    /// It replaced `Unanswerable`, which is what this build answered a character list
    /// with while the screen that chooses one did not exist. That screen exists now, so
    /// the message is answerable and the error it produced is gone rather than left as a
    /// state nothing can reach.
    Unchosen,
    /// A handshake-phase payload arriving on a session that already has a welcome.
    ///
    /// The mirror of [`Self::Premature`] rather than a second name for it, and distinct
    /// from [`Self::Repeated`], which is a *second* welcome or reject.
    /// `ServerCharacterList` before the welcome is exactly on time; after the welcome the
    /// server is answering a phase this session has already left.
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
    /// A snapshot states this session's own death twice and the two statements disagree.
    ///
    /// `self_vitals.life_state` and `dead_players` are one fact written once each way, and
    /// `schemas/player.fbs` requires them to agree. Checked here rather than in the codec for
    /// the reason [`Self::TickOfDay`] is — the entity id arrived in the *welcome*, and the
    /// codec sees one frame at a time.
    ///
    /// **It is also the reason V10 moved `ProtocolVersion::Current`.** A V9 server never
    /// sends the vector, so a client built against this contract would connect perfectly and
    /// end the session the first time it died; refusing that peer at the handshake is what
    /// turns it into a sentence somebody can read.
    OwnDeathDisagrees {
        vitals_say_dead: bool,
        entity_id: u64,
    },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Premature(kind) => {
                write!(f, "{kind} arrived before this connection had a session")
            }
            Self::Repeated(kind) => write!(f, "second {kind} on an established session"),
            Self::WrongDirection(kind) => {
                write!(f, "server sent {kind}, which only a client sends")
            }
            Self::Unchosen => write!(
                f,
                "ServerWelcome arrived before a character was chosen: the server answered a \
                 question this client had not asked"
            ),
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
            Self::OwnDeathDisagrees {
                vitals_say_dead,
                entity_id,
            } => write!(
                f,
                "EntitySnapshot says self_vitals dead={vitals_say_dead} while dead_players says \
                 dead={} for this session's own entity {entity_id}",
                !vitals_say_dead
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
    /// This session's own entity id, from the welcome. `None` until there is one, which is
    /// every phase in which no snapshot may arrive anyway.
    entity_id: Option<u64>,
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

    /// Records that a selection or a creation has gone out.
    ///
    /// **The one input to this state machine that is not a message from the server**, and
    /// it is what makes [`HandshakeError::Unchosen`] checkable: a welcome is the answer to
    /// a choice, so the client has to know it asked. It is called by the session thread
    /// where the frame is written — the same thread that feeds every message in here, so
    /// there is no ordering between two channels to get right.
    ///
    /// A choice sent in any other phase is ignored rather than refused. Sending one
    /// before the list has arrived is this client's own bug and not a peer's, and there
    /// is nothing to end a connection over: the server will refuse the frame in the terms
    /// it chooses, which is the answer a player should read.
    #[must_use = "a choice made outside the phase must never reach the socket"]
    pub fn chose(&mut self) -> bool {
        if self.phase != Phase::Choosing {
            return false;
        }
        self.phase = Phase::AwaitingWelcome;
        true
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

            // -- The character phase, in the order it runs --------------------------

            // The answer to the hello. What this client does with it is a screen; what
            // this module does is admit it exactly once and in exactly one phase.
            (Phase::AwaitingCharacters, Message::CharacterList(list)) => {
                self.phase = Phase::Choosing;
                Ok(Transition::Characters(list))
            }
            // A second list is a server that has lost track of the exchange. There is no
            // way to resynchronise one of those, and re-opening the screen over a choice
            // that may already be on the wire is the worst available guess.
            (Phase::Choosing | Phase::AwaitingWelcome, Message::CharacterList(_)) => {
                Err(HandshakeError::Repeated("ServerCharacterList"))
            }
            // The character phase is over: this session has a character, because it has a
            // welcome.
            (Phase::Established, Message::CharacterList(_)) => {
                Err(HandshakeError::OutOfPhase("ServerCharacterList"))
            }

            (Phase::AwaitingWelcome, Message::Welcome(params)) => {
                self.phase = Phase::Established;
                self.inventory_slots = Some(params.inventory_slots);
                self.clock = params.clock;
                self.entity_id = Some(params.entity_id);
                Ok(Transition::Established(params))
            }
            // A welcome nobody asked for. See [`HandshakeError::Unchosen`]: the spawn in
            // it belongs to a character this player has not picked.
            (Phase::AwaitingCharacters | Phase::Choosing, Message::Welcome(_)) => {
                Err(HandshakeError::Unchosen)
            }
            (Phase::Established, Message::Welcome(_)) => {
                Err(HandshakeError::Repeated("ServerWelcome"))
            }

            // A refusal is legal in place of any server message before the welcome, and
            // it closes the connection — so there is no phase to move to.
            (Phase::Established, Message::Reject(_)) => {
                Err(HandshakeError::Repeated("ServerReject"))
            }
            (_, Message::Reject(reject)) => Ok(Transition::Refused(reject)),

            // -- Everything a session carries, once there is one --------------------
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
                //
                // The recipient's own death is checked in the same breath and for the same
                // reason: which body is this session's own is a number the *welcome* carried.
                // The codec has already refused a vector naming anybody outside the snapshot;
                // this is the half of that invariant it could not see.
                //
                // Both directions, deliberately. A server that forgets to name a dead
                // recipient leaves that player watching their own body stand through their
                // death; one that names a living recipient lays them out while they are still
                // playing. Neither is a frame to go on reading.
                let entity_id = self.entity_id.unwrap_or_default();
                let vitals_say_dead = snapshot.self_vitals.life_state == LifeState::Dead;

                if self.clock.declared() && snapshot.tick_of_day >= self.clock.day_length_ticks {
                    Err(HandshakeError::TickOfDay {
                        day_length: self.clock.day_length_ticks,
                        got: snapshot.tick_of_day,
                    })
                } else if vitals_say_dead != snapshot.dead_players.contains(&entity_id) {
                    Err(HandshakeError::OwnDeathDisagrees {
                        vitals_say_dead,
                        entity_id,
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
            // Nothing to check against the welcome either, and for a reason worth
            // stating: the entity id is checked by the codec against 0, and *not* against
            // this session's own — an appearance for the viewer's own entity arrives the
            // same way as everybody else's, which is what `schemas/player.fbs` says and
            // what makes a player's own body one case rather than two.
            (Phase::Established, Message::PlayerAppearance(appearance)) => {
                Ok(Transition::Appearance(appearance))
            }
            (Phase::Established, Message::LeaveStarted(started)) => {
                Ok(Transition::Leaving(started))
            }
            (Phase::Established, Message::Chat(message)) => Ok(Transition::Chat(message)),
            (Phase::Established, Message::PartyInvite(invite)) => {
                Ok(Transition::PartyInvite(invite))
            }
            // Decoded and validated now; ECS ownership lands with the loot UI issue.
            (Phase::Established, Message::LootState(_)) => Ok(Transition::Ignored("LootState")),
            (Phase::Established, Message::LootClosed(_)) => Ok(Transition::Ignored("LootClosed")),

            // -- And the same payloads before there is a session --------------------
            //
            // One arm each rather than one wildcard, so the name in the failure is the
            // payload's own. Every one of them needs something the welcome carries:
            // `chunk_size` to expand a chunk, `entity_id` to know which body is this
            // player's, `inventory_slots` to check a pack against. A refusal that
            // preceded a session would be answering an action nobody could have taken.
            (_, Message::Deferred(kind)) => Err(HandshakeError::Premature(kind)),
            (_, Message::World(update)) => {
                Err(HandshakeError::Premature(world_payload_name(&update)))
            }
            (_, Message::Snapshot(_)) => Err(HandshakeError::Premature("EntitySnapshot")),
            (_, Message::Inventory(_)) => Err(HandshakeError::Premature("InventoryState")),
            (_, Message::MineProgress(_)) => Err(HandshakeError::Premature("MineProgress")),
            (_, Message::ActionRefused(_)) => Err(HandshakeError::Premature("ActionRefused")),
            (_, Message::PlayerAppearance(_)) => Err(HandshakeError::Premature("PlayerAppearance")),
            (_, Message::LeaveStarted(_)) => Err(HandshakeError::Premature("LeaveStarted")),
            (_, Message::Chat(_)) => Err(HandshakeError::Premature("ChatMessage")),
            (_, Message::PartyInvite(_)) => Err(HandshakeError::Premature("PartyInvite")),
            (_, Message::LootState(_)) => Err(HandshakeError::Premature("LootState")),
            (_, Message::LootClosed(_)) => Err(HandshakeError::Premature("LootClosed")),
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
        PLACEHOLDER_APPEARANCE, PlayerAppearance, PlayerVitals,
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
            inventory_slots: 2,
            hotbar_slots: 1,
            equipment_slots: 1,
            player_token: crate::net::ANY_TOKEN,
        }
    }

    fn reject() -> Reject {
        Reject {
            code: "PROTOCOL_MISMATCH",
            detail: "server speaks protocol 1, client speaks 2".to_owned(),
        }
    }

    /// A handshake that has been all the way through the phase: the list arrived, a
    /// choice went out, and the welcome answered it.
    ///
    /// One helper because every test about a *session* needs one and none of them is
    /// about how it was reached — the tests that are about that drive the three steps by
    /// hand, at the bottom of this file.
    fn established() -> Handshake {
        let mut handshake = Handshake::new();
        let admitted = handshake.apply(Message::CharacterList(character_list()));
        assert!(matches!(admitted, Ok(Transition::Characters(_))));
        assert!(
            handshake.chose(),
            "a list had arrived, so a choice is legal"
        );
        let welcomed = handshake.apply(Message::Welcome(params()));
        assert_eq!(welcomed, Ok(Transition::Established(params())));
        handshake
    }

    #[test]
    fn a_fresh_handshake_is_waiting_for_the_account_s_characters() {
        let handshake = Handshake::new();

        assert_eq!(handshake.phase(), Phase::AwaitingCharacters);
        assert!(!handshake.established());
    }

    /// The whole exchange, in the order the contract puts it in.
    #[test]
    fn a_list_then_a_choice_then_a_welcome_establishes_the_session() {
        let mut handshake = Handshake::new();

        assert_eq!(
            handshake.apply(Message::CharacterList(character_list())),
            Ok(Transition::Characters(character_list()))
        );
        assert_eq!(handshake.phase(), Phase::Choosing);
        assert!(!handshake.established(), "choosing is not playing");

        assert!(handshake.chose(), "the phase was waiting for exactly this");
        assert_eq!(handshake.phase(), Phase::AwaitingWelcome);
        assert!(!handshake.established(), "asking is not playing either");

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
            stacks: vec![
                InventoryStack {
                    item_id: 7,
                    count: 1,
                    durability: 35,
                    max_durability: 100,
                },
                InventoryStack::default(),
            ],
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
        let mut handshake = established();

        assert_eq!(
            handshake.apply(Message::Inventory(inventory())),
            Ok(Transition::Inventory(inventory()))
        );
        assert!(handshake.established(), "the session survives it");
    }

    #[test]
    fn inventory_slot_count_must_match_the_welcome() {
        for got in [0, 1, 3] {
            let mut handshake = established();
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
                Err(HandshakeError::InventorySlots { expected: 2, got })
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
            let _ = handshake.apply(Message::CharacterList(character_list()));
            assert!(handshake.chose());
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
        let mut handshake = established();
        let snapshot = Snapshot {
            tick_of_day: u32::MAX,
            ..snapshot()
        };

        assert_eq!(
            handshake.apply(Message::Snapshot(snapshot.clone())),
            Ok(Transition::Snapshot(snapshot))
        );
    }

    /// **This session's own death is stated twice in one frame, and this layer is where the
    /// two are made to agree.**
    ///
    /// Which body is the recipient's own is a number the *welcome* carried, so the codec —
    /// which sees one frame at a time — cannot check it and this layer can. Both directions
    /// are refused, because both are a server that has lost track of one of its own players.
    ///
    /// It is also the check a V9 server would fail, which is why V10 moved
    /// `ProtocolVersion::Current`: an absent vector reads as nobody dead, so the third row
    /// below is exactly the frame such a server sends on the tick this client dies.
    #[test]
    fn a_snapshot_must_state_this_sessions_own_death_the_same_way_twice() {
        let dead = PlayerVitals {
            health: 0,
            max_health: 100,
            hunger: 50,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: LifeState::Dead,
            respawn_ticks: 40,
            invulnerable: false,
        };

        // The last row is the one that keeps this from being written as "dead_players must be
        // empty unless the recipient is dead": somebody *else* dying is passed straight
        // through, which is the ordinary frame this check must never refuse.
        for (name, self_vitals, dead_players, accepted) in [
            (
                "alive, and named by nobody",
                PlayerVitals::unharmed(),
                vec![],
                true,
            ),
            ("dead, and named", dead, vec![7], true),
            (
                "dead, and not named — what a V9 server sends",
                dead,
                vec![],
                false,
            ),
            (
                "alive, and named anyway",
                PlayerVitals::unharmed(),
                vec![7],
                false,
            ),
            (
                "alive, while somebody else is down",
                PlayerVitals::unharmed(),
                vec![9],
                true,
            ),
        ] {
            let mut handshake = established();
            let snapshot = Snapshot {
                self_vitals,
                dead_players,
                entities: vec![
                    super::super::codec::EntityState {
                        entity_id: 7,
                        pos: [0.5, 64.0, 0.5],
                        vel: [0.0, 0.0, 0.0],
                        yaw: 0.0,
                    },
                    super::super::codec::EntityState {
                        entity_id: 9,
                        pos: [4.5, 64.0, 0.5],
                        vel: [0.0, 0.0, 0.0],
                        yaw: 0.0,
                    },
                ],
                ..snapshot()
            };

            let applied = handshake.apply(Message::Snapshot(snapshot.clone()));
            if accepted {
                assert_eq!(applied, Ok(Transition::Snapshot(snapshot)), "{name}");
            } else {
                assert_eq!(
                    applied,
                    Err(HandshakeError::OwnDeathDisagrees {
                        vitals_say_dead: self_vitals.life_state == LifeState::Dead,
                        entity_id: 7,
                    }),
                    "{name}"
                );
            }
        }
    }

    /// **And on every snapshot of the session, not only the first one after the welcome.**
    ///
    /// The check above builds a fresh handshake per row, so on its own it cannot tell
    /// "checked once, when the session starts" from "checked always" — and the two differ
    /// by the entire lifetime of a connection. This drives *one* established handshake
    /// through a run of frames the contract allows, a death and its respawn among them,
    /// and only then hands it the frame a V9 server sends. The refusal arrives there,
    /// deep in the session, which is where a player actually dies.
    ///
    /// It pins a property of where the check sits rather than one of this test.
    /// `Message::Snapshot` is admitted in `Phase::Established` and in no other — every
    /// earlier phase answers `Premature("EntitySnapshot")` — and `session::pump` feeds
    /// every frame it decodes, for the whole life of the connection, through
    /// [`Handshake::apply`]. There is no second path a snapshot can take. An edit that
    /// moved this agreement to a one-shot check beside the welcome would leave the test
    /// above green and fail this one, which is the whole reason it is written separately.
    #[test]
    fn the_own_death_agreement_is_checked_on_every_snapshot_not_just_the_first() {
        let dead = PlayerVitals {
            health: 0,
            max_health: 100,
            hunger: 50,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state: LifeState::Dead,
            respawn_ticks: 40,
            invulnerable: false,
        };
        let frame = |self_vitals: PlayerVitals, dead_players: Vec<u64>| Snapshot {
            self_vitals,
            dead_players,
            entities: vec![
                super::super::codec::EntityState {
                    entity_id: 7,
                    pos: [0.5, 64.0, 0.5],
                    vel: [0.0, 0.0, 0.0],
                    yaw: 0.0,
                },
                super::super::codec::EntityState {
                    entity_id: 9,
                    pos: [4.5, 64.0, 0.5],
                    vel: [0.0, 0.0, 0.0],
                    yaw: 0.0,
                },
            ],
            ..snapshot()
        };

        let mut handshake = established();

        // A session's worth of legal frames, in the order a player lives them: standing,
        // a neighbour goes down, this player goes down and is named for it, and the
        // respawn clears both. Every one of them passes through the same check.
        for (name, self_vitals, dead_players) in [
            ("alive, nobody down", PlayerVitals::unharmed(), vec![]),
            ("alive, a neighbour down", PlayerVitals::unharmed(), vec![9]),
            ("dead, and named", dead, vec![7]),
            ("dead, and named beside the neighbour", dead, vec![9, 7]),
            ("respawned, nobody down", PlayerVitals::unharmed(), vec![]),
        ] {
            let snapshot = frame(self_vitals, dead_players);
            assert_eq!(
                handshake.apply(Message::Snapshot(snapshot.clone())),
                Ok(Transition::Snapshot(snapshot)),
                "{name}"
            );
            assert_eq!(handshake.phase(), Phase::Established, "{name}");
        }

        // The sixth frame of the session rather than the first, and the point of the
        // test: this is what a V9 server sends on the tick this player dies, and it is
        // refused here exactly as it would have been immediately after the welcome.
        assert_eq!(
            handshake.apply(Message::Snapshot(frame(dead, vec![]))),
            Err(HandshakeError::OwnDeathDisagrees {
                vitals_say_dead: true,
                entity_id: 7,
            }),
            "a death the server forgot to name, six frames into the session"
        );

        // And the mirror direction is still live that late too — a server that lays out
        // a player who is still playing is the same lost track of the same fact.
        assert_eq!(
            handshake.apply(Message::Snapshot(frame(PlayerVitals::unharmed(), vec![7]))),
            Err(HandshakeError::OwnDeathDisagrees {
                vitals_say_dead: false,
                entity_id: 7,
            }),
            "a living recipient named among the dead, late in the session"
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

        let mut live = established();
        assert_eq!(
            live.apply(message()),
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
        let mut handshake = established();

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
            let mut handshake = established();

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

            let mut live = established();
            assert_eq!(
                live.apply(Message::ClientOnly(kind)),
                Err(HandshakeError::WrongDirection(kind))
            );
        }
    }

    #[test]
    fn a_deferred_payload_after_the_welcome_is_dropped() {
        let mut handshake = established();

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

        let mut handshake = established();
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
        let mut handshake = established();

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
        let mut handshake = established();

        assert_eq!(
            handshake.apply(Message::Welcome(params())),
            Err(HandshakeError::Repeated("ServerWelcome"))
        );
    }

    #[test]
    fn a_reject_after_the_welcome_ends_the_connection() {
        let mut handshake = established();

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
            name: "Test Character".to_owned(),
            worn_head: 0,
            worn_chest: 0,
            worn_legs: 0,
            level: 1,
        }
    }

    /// A second list is a server that has lost track of the exchange, and there is no
    /// way to resynchronise one of those.
    ///
    /// Both phases after the first are refused, and the second is the one worth stating:
    /// a choice may already be on the wire when it arrives, so re-opening the screen over
    /// it would be this client answering a question twice.
    #[test]
    fn a_second_character_list_ends_the_connection() {
        let mut choosing = Handshake::new();
        let _ = choosing.apply(Message::CharacterList(character_list()));
        assert_eq!(
            choosing.apply(Message::CharacterList(character_list())),
            Err(HandshakeError::Repeated("ServerCharacterList"))
        );

        let mut asked = Handshake::new();
        let _ = asked.apply(Message::CharacterList(character_list()));
        assert!(asked.chose());
        assert_eq!(
            asked.apply(Message::CharacterList(character_list())),
            Err(HandshakeError::Repeated("ServerCharacterList"))
        );
    }

    /// **A welcome before a character was chosen is a protocol error**, in both phases
    /// where no choice has gone out.
    ///
    /// It is the rule this contract's shape exists for: `ServerWelcome.spawn` is where a
    /// player is standing, and where they stand depends on which character they picked —
    /// so a welcome that precedes the picking carries a position for somebody nobody
    /// chose. A client that took it would enter the world as a character its player had
    /// never seen.
    ///
    /// This is also the one rule here that is not a function of the messages received,
    /// and [`Handshake::chose`] is why it can be checked at all.
    #[test]
    fn a_welcome_before_a_choice_ends_the_connection() {
        let mut before_the_list = Handshake::new();
        assert_eq!(
            before_the_list.apply(Message::Welcome(params())),
            Err(HandshakeError::Unchosen)
        );
        assert_eq!(before_the_list.phase(), Phase::AwaitingCharacters);

        let mut choosing = Handshake::new();
        let _ = choosing.apply(Message::CharacterList(character_list()));
        assert_eq!(
            choosing.apply(Message::Welcome(params())),
            Err(HandshakeError::Unchosen)
        );
        // The phase does not advance on a failure, and the session ends above this
        // layer — `net/session.rs` turns a handshake error into a protocol failure.
        assert_eq!(choosing.phase(), Phase::Choosing);
    }

    /// The failure says what the server did and what it answered, because that string is
    /// the whole diagnosis: it reaches a log and a status line.
    #[test]
    fn the_unchosen_failure_names_what_went_wrong() {
        let rendered = HandshakeError::Unchosen.to_string();
        assert!(
            rendered.contains("ServerWelcome") && rendered.contains("chosen"),
            "the failure does not diagnose itself: {rendered}"
        );
    }

    /// A choice sent in a phase that is not waiting for one moves nothing, **and says
    /// so**, which is the half `net/session.rs` needs.
    ///
    /// It is a no-op rather than a panic or an error because this client getting its own
    /// state wrong is not a reason to end a connection the *server* is still following.
    /// But the answer is load-bearing: the session thread writes the selection frame only
    /// when this returns `true`, because after `Established` the writer thread owns that
    /// socket and a second writer is what `transport.Conn` does not survive.
    #[test]
    fn a_choice_outside_the_phase_changes_nothing_and_says_so() {
        let mut fresh = Handshake::new();
        assert!(
            !fresh.chose(),
            "no list has arrived, so there is nothing to answer"
        );
        assert_eq!(fresh.phase(), Phase::AwaitingCharacters);

        let mut live = established();
        assert!(!live.chose(), "this session has a character already");
        assert_eq!(live.phase(), Phase::Established);

        // And twice in the one phase that accepts a choice: the second is refused, which
        // is what keeps a double press off the wire even if one reached this far.
        let mut once = Handshake::new();
        let _ = once.apply(Message::CharacterList(character_list()));
        assert!(once.chose());
        assert!(!once.chose(), "a second choice is not a choice");
        assert_eq!(once.phase(), Phase::AwaitingWelcome);
    }

    /// After the welcome the character phase is over: this session has a character,
    /// because it has a welcome.
    #[test]
    fn a_character_list_after_the_welcome_ends_the_connection() {
        let mut handshake = established();

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

    /// Admitted on a session and carried through whole: validated at the decode boundary,
    /// and handed to the module that draws bodies exactly as it arrived.
    ///
    /// The entity id is deliberately **not** checked against this session's own. A viewer's
    /// own appearance arrives the same way as everybody else's, and an appearance for an
    /// entity no snapshot has mentioned yet is the ordinary case rather than a fault —
    /// `schemas/player.fbs` says the two streams are not ordered against each other.
    #[test]
    fn an_appearance_after_the_welcome_is_carried_to_the_renderer() {
        let mut handshake = established();
        let described = player_appearance();

        assert_eq!(
            handshake.apply(Message::PlayerAppearance(described.clone())),
            Ok(Transition::Appearance(described))
        );
    }

    #[test]
    fn a_leave_countdown_only_belongs_to_an_established_session() {
        let started = LeaveStarted {
            remaining_ms: 10_000,
        };
        let mut early = Handshake::new();
        assert_eq!(
            early.apply(Message::LeaveStarted(started)),
            Err(HandshakeError::Premature("LeaveStarted"))
        );

        let mut admitted = established();
        assert_eq!(
            admitted.apply(Message::LeaveStarted(started)),
            Ok(Transition::Leaving(started))
        );
        assert!(
            admitted.established(),
            "leaving remains an established session until the server closes it"
        );
    }

    #[test]
    fn chat_and_party_invites_only_belong_to_an_established_session() {
        let chat = ChatMessage {
            sender_entity_id: 8,
            sender_name: "Eivor".to_owned(),
            text: "hello".to_owned(),
        };
        let invite = PartyInvite {
            from_entity_id: 8,
            from_name: "Eivor".to_owned(),
            expires_ms: 5_000,
        };

        let mut early = Handshake::new();
        assert_eq!(
            early.apply(Message::Chat(chat.clone())),
            Err(HandshakeError::Premature("ChatMessage"))
        );
        let mut early = Handshake::new();
        assert_eq!(
            early.apply(Message::PartyInvite(invite.clone())),
            Err(HandshakeError::Premature("PartyInvite"))
        );

        let mut live = established();
        assert_eq!(
            live.apply(Message::Chat(chat.clone())),
            Ok(Transition::Chat(chat))
        );
        assert_eq!(
            live.apply(Message::PartyInvite(invite.clone())),
            Ok(Transition::PartyInvite(invite))
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

            let mut live = established();
            assert_eq!(
                live.apply(Message::ClientOnly(kind)),
                Err(HandshakeError::WrongDirection(kind))
            );
        }
    }
}
