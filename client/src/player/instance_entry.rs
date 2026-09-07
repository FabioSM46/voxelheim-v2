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
pub struct EntryOffer {
    current: Option<InstanceEntryOffer>,
    /// Whether a frame has already begun with this offer's dialog owning the controls.
    ///
    /// **A key press cannot answer a dialog that was not on screen when it was made.**
    /// The offer is drained before the UI runs, so the frame that opens the dialog can
    /// also carry an `Escape` the player pressed at something else — the pause menu they
    /// were about to open. Answering it there would refuse an offer nobody ever saw, and
    /// silently, since a refusal produces no frame the player can see. See
    /// [`Self::answerable`].
    presented: bool,
}

impl EntryOffer {
    /// The offer the dialog draws, if there is one.
    pub fn current(&self) -> Option<&InstanceEntryOffer> {
        self.current.as_ref()
    }

    /// The id a key press may answer, which is not the same question as [`Self::current`].
    ///
    /// `None` while the dialog has not yet owned the controls for a whole frame. The
    /// pointer needs no such guard — a button cannot be clicked before it is drawn — so
    /// this exists for the keyboard, where the press and the dialog are independent.
    pub(crate) fn answerable(&self) -> Option<u64> {
        self.presented
            .then(|| self.current.map(|offer| offer.offer_id))?
    }

    /// Records that a frame has begun with this dialog up.
    ///
    /// A lower bound on "drawn" rather than a claim about a rendered frame: the mode was
    /// already `EntryOffer` when this frame started, so the press being read now was made
    /// against a screen that had the dialog on it.
    fn mark_presented(&mut self) {
        self.presented = true;
    }

    /// Replaces whatever was pending. The caller is announcing the server's supersession,
    /// not choosing between two live offers.
    ///
    /// A replacement is unpresented again, deliberately: the new terms are not the ones
    /// the player has been reading, and a press in flight belongs to the offer it was
    /// aimed at rather than to the one that took its place.
    fn open(&mut self, offer: InstanceEntryOffer) {
        self.current = Some(offer);
        self.presented = false;
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
        if self.current.is_some_and(|offer| offer.offer_id == offer_id) {
            self.presented = false;
            self.current.take()
        } else {
            None
        }
    }

    fn drop_pending(&mut self) {
        self.current = None;
        self.presented = false;
    }

    /// Puts one offer on screen as `reconcile_entry_offer` would. Test-only, so the
    /// dialog can be driven without a socket, and private to this type for the reason
    /// `SelfVitals::from_server` is: an offer is a thing the server states, and a
    /// constructor a system could reach would be a place to invent one.
    #[cfg(test)]
    pub(crate) fn open_for_test(&mut self, offer: InstanceEntryOffer) {
        self.open(offer);
    }

    /// Marks the offer as one the player has had a frame to read, which is what
    /// `reconcile_entry_offer` does on any frame that begins with the dialog up.
    ///
    /// Test-only and separate from [`Self::open_for_test`] deliberately: a test about the
    /// keyboard guard has to be able to build both states, and an arrival that presented
    /// itself in the same breath could not express the one the guard exists for.
    #[cfg(test)]
    pub(crate) fn present_for_test(&mut self) {
        self.mark_presented();
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
    // Read before anything below can change the mode, so this answers "did the frame
    // *begin* with the dialog up" rather than "is it up now".
    if *mode == InputMode::EntryOffer {
        offer.mark_presented();
    }

    if let Some(arrived) = inbox.take() {
        offer.open(arrived);
    }

    // **An offer waits for the controls; it never takes them.** A frame can carry a chat
    // line, an open pack or a pause press alongside the offer, and a dialog that stole
    // the keyboard from any of them would be deciding for the player.
    //
    // **It is asked every frame rather than only on the one the offer arrived on**, and
    // that is the whole of the difference between an offer a player answers and one that
    // is held for ever. An offer that arrives behind another surface used to be stored
    // and then never presented: the surface closes, `choose_input_mode` returns to
    // `Playing`, and nothing looked at the offer again — so the dialog stayed hidden, the
    // player never saw the crossing they had asked for, and the server went on holding an
    // offer nobody could answer. Reviewed on #1054 and pinned by
    // `an_offer_held_behind_a_surface_is_presented_when_the_player_returns_to_play`.
    //
    // Running *before* `ApplyInputMode` is what keeps the press that closes the blocking
    // surface from also answering the dialog: on that frame the mode is still the
    // surface's, so nothing opens, and the dialog appears on the frame after. The keyboard
    // guard in `EntryOffer::answerable` is what makes that a property rather than an
    // accident of ordering.
    if offer.current().is_some() && *mode == InputMode::Playing {
        set_mode(&mut mode, InputMode::EntryOffer);
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

    /// A player in the middle of a chat line or a pause menu keeps them, for as long as
    /// they keep them. The offer is still held: the server made it, and nothing here
    /// withdraws one.
    #[test]
    fn an_offer_arriving_over_another_surface_does_not_steal_the_keyboard() {
        for occupied in [InputMode::Chat, InputMode::Menu, InputMode::Inventory] {
            let mut app = app();
            *app.world_mut().resource_mut::<InputMode>() = occupied;
            deliver(&mut app, offer(7));
            app.update();
            // And it goes on waiting rather than seizing the controls a frame later.
            app.update();

            assert_eq!(*app.world().resource::<InputMode>(), occupied);
            assert!(app.world().resource::<EntryOffer>().current().is_some());
        }
    }

    /// **A key press cannot answer a dialog that was not on screen when it was made.**
    /// The offer is drained before the UI runs, so the frame that opens the dialog can
    /// carry an `Escape` the player aimed at the pause menu. Refusing there would give
    /// away a crossing nobody saw, and silently.
    #[test]
    fn an_offer_is_not_answerable_by_a_key_until_it_has_been_on_screen() {
        let mut app = app();
        deliver(&mut app, offer(7));
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::EntryOffer);
        assert_eq!(app.world().resource::<EntryOffer>().answerable(), None);

        // One frame with the dialog up is what makes the press belong to it.
        app.update();
        assert_eq!(app.world().resource::<EntryOffer>().answerable(), Some(7));
    }

    /// A replacement is unpresented again: the terms are not the ones the player has been
    /// reading, so a press in flight belongs to the offer it was aimed at.
    #[test]
    fn a_replacing_offer_is_not_answerable_by_a_key_either_until_it_has_been_seen() {
        let mut app = app();
        deliver(&mut app, offer(7));
        app.update();
        app.update();
        assert_eq!(app.world().resource::<EntryOffer>().answerable(), Some(7));

        deliver(&mut app, offer(8));
        app.update();
        assert_eq!(app.world().resource::<EntryOffer>().answerable(), None);
        app.update();
        assert_eq!(app.world().resource::<EntryOffer>().answerable(), Some(8));
    }

    /// The measurement for the review finding on this file, written before the fix.
    #[test]
    fn an_offer_held_behind_a_surface_is_presented_when_the_player_returns_to_play() {
        let mut app = app();
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        deliver(&mut app, offer(7));
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Chat);

        // The player closes the chat line. Nothing else happens.
        *app.world_mut().resource_mut::<InputMode>() = InputMode::Playing;
        app.update();

        assert_eq!(*app.world().resource::<InputMode>(), InputMode::EntryOffer);
        assert_eq!(
            app.world().resource::<EntryOffer>().current(),
            Some(&offer(7))
        );
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
