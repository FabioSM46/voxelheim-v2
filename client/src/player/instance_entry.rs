//! The crossing the server has offered, and the one answer it may become.
//!
//! **This module asks and reports; it never decides who may cross.** The offer is
//! authored by the server, the terms in it are the server's numbers, and accepting sends
//! an intent that the server re-decides from scratch — the run may have reset, ended, or
//! stopped being the run behind that arch since the offer was written. What arrives if it
//! agrees is a `WorldChange`, exactly as it is for an ordinary crossing; what arrives if
//! it does not is an `ActionRefused`. Nothing here draws either conclusion in advance.
//!
//! **An offer is not durable, and the server owes no frame withdrawing one.** That makes
//! dropping a pending prompt a requirement rather than a tidy-up, and the list is the
//! contract's: a world change, a disconnect, a later crossing, and the run's own reset.
//! Three of those are visible here. The fourth is not visible to any client, which is the
//! whole reason an acceptance is a request and never an outcome.

use bevy::prelude::*;

use super::{ApplyInputMode, InputMode, SelfVitals};
use crate::net::{
    EntryOfferInbox, InstanceEntryAnswer, InstanceEntryOffer, Outbound, Session,
    encode_instance_entry_answer,
};

/// The one offer this client is currently asking the player about.
///
/// One, because the server holds one: a later crossing supersedes the earlier offer and
/// the server has already forgotten it. Keeping the first would leave a button on screen
/// that answers an id every later answer is refused with `EntryOfferUnknown`.
#[derive(Resource, Debug, Default)]
pub struct EntryOffer(Option<InstanceEntryOffer>);

impl EntryOffer {
    /// The offer the dialog draws, if there is one.
    pub fn current(&self) -> Option<&InstanceEntryOffer> {
        self.0.as_ref()
    }

    /// Replaces whatever was pending. The caller is announcing the server's supersession,
    /// not choosing between two live offers.
    fn open(&mut self, offer: InstanceEntryOffer) {
        self.0 = Some(offer);
    }

    /// Spends the pending offer, if it is the one being answered.
    ///
    /// **The id is what makes this exact rather than nearly right.** A superseding offer
    /// and a click on the old one can share a frame — the inbox is drained before the UI
    /// runs and the answer is read after it — so an answer that named nothing would land
    /// on whichever offer happened to be pending when it was read. That is the duplicate
    /// acceptance this method exists to make impossible: an id answers its own offer once
    /// and never another's.
    fn spend(&mut self, offer_id: u64) -> Option<InstanceEntryOffer> {
        if self.0.is_some_and(|offer| offer.offer_id == offer_id) {
            self.0.take()
        } else {
            None
        }
    }

    fn drop_pending(&mut self) {
        self.0 = None;
    }

    /// Puts one offer on screen as `reconcile_entry_offer` would. Test-only, so the
    /// dialog can be driven without a socket, and private to this type for the reason
    /// `SelfVitals::from_server` is: an offer is a thing the server states, and a
    /// constructor a system could reach would be a place to invent one.
    #[cfg(test)]
    pub(crate) fn open_for_test(&mut self, offer: InstanceEntryOffer) {
        self.open(offer);
    }

    /// Ends the offer as a death, a disconnect or a world change would.
    #[cfg(test)]
    pub(crate) fn clear_for_test(&mut self) {
        self.drop_pending();
    }
}

/// One decision a player made about the offer named in it.
///
/// It carries the id rather than relying on whatever is pending when it is read, for the
/// reason [`EntryOffer::spend`] gives. It is an *intent to answer*: it becomes a wire
/// frame only if the offer it names is still the one this client is holding.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryOfferAnswer {
    pub offer_id: u64,
    pub accept: bool,
}

pub(super) struct InstanceEntryPlugin;

impl Plugin for InstanceEntryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EntryOffer>()
            .init_resource::<EntryOfferInbox>()
            .init_resource::<InputMode>()
            .init_resource::<SelfVitals>()
            .add_message::<EntryOfferAnswer>()
            .add_systems(
                Update,
                reconcile_entry_offer
                    .after(crate::net::DrainNetwork)
                    .before(ApplyInputMode),
            )
            .add_systems(Update, send_entry_offer_answer.after(ApplyInputMode));
    }
}

/// Takes whatever the server has offered, and drops a prompt that can no longer be true.
///
/// The order inside is deliberate: an offer that arrived in the same frame as a death or
/// a disconnect is dropped by the second half rather than left on screen for one frame
/// over a character who cannot act.
fn reconcile_entry_offer(
    mut inbox: ResMut<EntryOfferInbox>,
    session: Option<Res<Session>>,
    vitals: Res<SelfVitals>,
    mut offer: ResMut<EntryOffer>,
    mut mode: ResMut<InputMode>,
) {
    if let Some(arrived) = inbox.take() {
        offer.open(arrived);
        // Playing is the only mode a crossing can be requested from, so this replaces
        // nothing a player is in the middle of. It is asked rather than assumed because
        // a frame can carry a chat line or a pause press alongside the offer, and a
        // dialog that stole the keyboard from either would be deciding for the player.
        if *mode == InputMode::Playing {
            set_mode(&mut mode, InputMode::EntryOffer);
        }
    }

    // **Nothing is sent on either of these.** A disconnect has no writer left, and a
    // death leaves an offer the server will supersede on the next crossing or forget with
    // the run. Sending an answer for an offer the server may already have released would
    // come back as `EntryOfferUnknown` — a refusal on screen that the player never caused.
    // The world change is not asked about here at all: `reset_world` clears this resource
    // with the world it belonged to.
    if session.is_none() || vitals.dead() {
        offer.drop_pending();
    }

    // The dialog is the only thing this mode is for, so it cannot outlive the offer.
    if offer.current().is_none() && *mode == InputMode::EntryOffer {
        set_mode(&mut mode, InputMode::Playing);
    }
}

/// Turns one answered offer into one frame.
///
/// **A refusal is written out and is worth sending.** It costs the character nothing, it
/// leaves them exactly where they are standing, and it is what lets the server forget the
/// offer at once rather than hold it until something else invalidates it.
fn send_entry_offer_answer(
    mut answers: MessageReader<EntryOfferAnswer>,
    mut offer: ResMut<EntryOffer>,
    outbound: Option<ResMut<Outbound>>,
    mut mode: ResMut<InputMode>,
) {
    let mut outbound = outbound;
    for answer in answers.read() {
        // Spending it first is what makes a second press inert: the offer is gone from
        // this client before anything is written, exactly as it is gone from the server
        // the moment either answer reaches it.
        let Some(spent) = offer.spend(answer.offer_id) else {
            continue;
        };
        if *mode == InputMode::EntryOffer {
            set_mode(&mut mode, InputMode::Playing);
        }
        let Some(outbound) = outbound.as_deref_mut() else {
            continue;
        };
        outbound.send(encode_instance_entry_answer(&InstanceEntryAnswer {
            offer_id: spent.offer_id,
            accept: answer.accept,
        }));
    }
}

/// Writes the mode only when it actually changes, so an unchanged mode does not mark the
/// resource and wake every system that watches it. `trade.rs` and `vendor.rs` hold the
/// same three lines for the same reason.
fn set_mode(mode: &mut ResMut<'_, InputMode>, next: InputMode) {
    if **mode != next {
        **mode = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{
        ANY_TOKEN, BlockCoord, LifeState, PlayerVitals, SessionBinding, SessionParams, WorldClock,
    };

    fn terms() -> SessionBinding {
        SessionBinding {
            arch: BlockCoord {
                x: -96,
                y: 61,
                z: 704,
            },
            bosses_defeated: 1,
            bosses_total: 2,
            resets_at_unix: 1_800_000_000,
        }
    }

    fn offer(offer_id: u64) -> InstanceEntryOffer {
        InstanceEntryOffer {
            offer_id,
            terms: terms(),
        }
    }

    fn session() -> Session {
        Session(SessionParams {
            entity_id: 7,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            clock: WorldClock::default(),
            voice_range_blocks: 0.0,
        })
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(session())
            .add_plugins(InstanceEntryPlugin);
        app
    }

    fn deliver(app: &mut App, offer: InstanceEntryOffer) {
        app.world_mut()
            .resource_mut::<EntryOfferInbox>()
            .push_for_test(offer);
    }

    #[test]
    fn an_arriving_offer_opens_the_dialog_and_owns_the_controls() {
        let mut app = app();
        deliver(&mut app, offer(7));
        app.update();

        assert_eq!(
            app.world().resource::<EntryOffer>().current(),
            Some(&offer(7))
        );
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::EntryOffer);
    }

    /// A player in the middle of a chat line or a pause menu keeps them. The offer is
    /// still held: the server made it, and nothing here withdraws one.
    #[test]
    fn an_offer_arriving_over_another_surface_does_not_steal_the_keyboard() {
        for occupied in [InputMode::Chat, InputMode::Menu, InputMode::Inventory] {
            let mut app = app();
            *app.world_mut().resource_mut::<InputMode>() = occupied;
            deliver(&mut app, offer(7));
            app.update();

            assert_eq!(*app.world().resource::<InputMode>(), occupied);
            assert!(app.world().resource::<EntryOffer>().current().is_some());
        }
    }

    #[test]
    fn a_later_offer_replaces_the_one_the_server_has_already_forgotten() {
        let mut app = app();
        deliver(&mut app, offer(7));
        app.update();
        deliver(&mut app, offer(8));
        app.update();

        assert_eq!(
            app.world().resource::<EntryOffer>().current(),
            Some(&offer(8))
        );
    }

    /// The frame in which a superseding offer and a click on the old one meet. Without
    /// the id, the answer would land on an offer the player never read.
    #[test]
    fn an_answer_never_lands_on_an_offer_it_did_not_name() {
        let mut app = app();
        deliver(&mut app, offer(7));
        app.update();

        deliver(&mut app, offer(8));
        app.world_mut().write_message(EntryOfferAnswer {
            offer_id: 7,
            accept: true,
        });
        app.update();

        // The superseding offer is still pending, unanswered and still on screen.
        assert_eq!(
            app.world().resource::<EntryOffer>().current(),
            Some(&offer(8))
        );
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::EntryOffer);
    }

    /// Accepting twice is one answer, because the offer is spent before anything is
    /// written — the same moment the server forgets it.
    #[test]
    fn an_offer_is_answered_once_however_many_times_it_is_pressed() {
        for accept in [true, false] {
            let mut app = app();
            deliver(&mut app, offer(7));
            app.update();

            app.world_mut().write_message(EntryOfferAnswer {
                offer_id: 7,
                accept,
            });
            app.world_mut().write_message(EntryOfferAnswer {
                offer_id: 7,
                accept,
            });
            app.update();

            assert!(app.world().resource::<EntryOffer>().current().is_none());
            assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);

            // And a press after the dialog is gone answers nothing at all.
            app.world_mut().write_message(EntryOfferAnswer {
                offer_id: 7,
                accept,
            });
            app.update();
            assert!(app.world().resource::<EntryOffer>().current().is_none());
        }
    }

    /// Neither of these sends anything: see `reconcile_entry_offer`.
    #[test]
    fn a_disconnect_or_a_death_drops_a_pending_offer_and_returns_the_controls() {
        for kill in [true, false] {
            let mut app = app();
            deliver(&mut app, offer(7));
            app.update();
            assert_eq!(*app.world().resource::<InputMode>(), InputMode::EntryOffer);

            if kill {
                let mut vitals = PlayerVitals::unharmed();
                vitals.health = 0;
                vitals.life_state = LifeState::Dead;
                app.insert_resource(SelfVitals::from_server(vitals));
            } else {
                app.world_mut().remove_resource::<Session>();
            }
            app.update();

            assert!(app.world().resource::<EntryOffer>().current().is_none());
            assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        }
    }

    /// The frame after an accepted crossing is the world change it asked for, and the
    /// world change takes this resource with it. Nothing is sent and nothing is left on
    /// screen; the same path a refused crossing that ended the run would take.
    #[test]
    fn a_world_change_takes_a_pending_offer_with_the_world_it_belonged_to() {
        let mut app = app();
        deliver(&mut app, offer(7));
        app.update();

        crate::world::transition::reset::<EntryOffer>(app.world_mut());
        app.update();

        assert!(app.world().resource::<EntryOffer>().current().is_none());
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
    }

    /// Both verdicts reach the wire, and each names the offer it answers and nothing
    /// else: no destination, no position, no binding.
    #[test]
    fn both_verdicts_are_sent_as_the_offer_scoped_intent_and_nothing_more() {
        for accept in [true, false] {
            let mut app = app();
            let (outbound, frames) = Outbound::to_a_test(8);
            app.insert_resource(outbound);
            deliver(&mut app, offer(7));
            app.update();

            app.world_mut().write_message(EntryOfferAnswer {
                offer_id: 7,
                accept,
            });
            app.update();

            let sent: Vec<Vec<u8>> = frames.try_iter().collect();
            assert_eq!(
                sent,
                vec![encode_instance_entry_answer(&InstanceEntryAnswer {
                    offer_id: 7,
                    accept,
                })]
            );
        }
    }
}
