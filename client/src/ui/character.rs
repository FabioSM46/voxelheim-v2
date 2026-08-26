//! The character screen: which of this account's characters is going in, and how to
//! make another.
//!
//! It is up exactly while [`CharacterChoice`] exists — the server has sent this
//! account's characters and is waiting for one — and it comes down when the exchange
//! ends, whichever way it ends. `Option<Res<CharacterChoice>>` is what encodes that, the
//! same shape the login screen and the server list use for their own resources.
//!
//! **Nothing here decides anything, and two halves of that are worth stating.** A row
//! writes a [`ChooseCharacter`]; the network boundary owns the socket and the frame. And
//! whether a *name* may be worn is the server's rule — this screen sends what was typed
//! and renders the answer, because "already taken" and "not acceptable" are the server's
//! two different answers and a client that guessed at either would be holding an opinion
//! about a world it can only see part of.
//!
//! **A refused creation closes the connection, and the client re-opens only the two a
//! different name remedies.** `schemas/handshake.fbs` answers one with `ServerReject`,
//! which ends the socket. `CHARACTER_NAME_TAKEN` and `CHARACTER_NAME_REFUSED` keep this
//! form up while the network boundary reconnects on the same route, then put the
//! server's sentence beside the name field. Every other reject remains terminal. The
//! draft already outlives an exchange, so the name and colours never need reconstructing.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::ui::UiGlobalTransform;
use bevy::window::PrimaryWindow;
use std::f32::consts::TAU;

use super::{BUTTON, button_colour};
use crate::net::{Appearance, CharacterChoice, ChooseCharacter, HairModel, Session};

use crate::player::{
    BodyPart, BodyVisualsPlugin, Daylight, Dressing, PlacedBox, WorldCamera, body_envelope,
    resting_piece_transform,
};

pub(super) struct CharacterUiPlugin;

impl Plugin for CharacterUiPlugin {
    fn build(&self, app: &mut App) {
        // The preview is a body, and bodies are dressed out of `player`'s wardrobe. Built
        // here as well as by `PlayerPlugin` so this screen stands up headlessly on its own,
        // and guarded because Bevy panics on a unique plugin added twice.
        if !app.is_plugin_added::<BodyVisualsPlugin>() {
            app.add_plugins(BodyVisualsPlugin);
        }
        app.init_resource::<Draft>()
            .init_resource::<PlayAs>()
            .init_resource::<PreviewState>()
            // Bevy's `InputPlugin` registers this one in a running client; registering it
            // here as well is what lets this screen be driven headlessly, which is the
            // same reason `ui/mod.rs` registers the messages its panels write.
            // `add_message` is idempotent.
            .add_message::<KeyboardInput>()
            .add_systems(Startup, spawn_character_screen)
            .add_systems(
                Update,
                (
                    // The list first: it is what a fresh exchange sets the focus from,
                    // and every system below reads the focus it leaves.
                    rebuild_rows,
                    answer_from_the_launch,
                    navigate,
                    type_the_name,
                    row_clicks,
                    field_clicks,
                    (show_character_screen, refresh_screen),
                    // The model last, and in this order. `keep_the_preview` reads the
                    // focus the systems above leave, `turn_the_preview` advances the angle
                    // `place_the_preview` then writes into a transform, and the backdrop is
                    // independent of all three.
                    (
                        keep_the_preview,
                        turn_the_preview,
                        place_the_preview,
                        paint_the_backdrop,
                    ),
                )
                    .chain(),
            );
    }
}

// ---------------------------------------------------------------------------
// The palettes
// ---------------------------------------------------------------------------

/// A colour a character may be made from, checked where it is written.
///
/// `0x00RRGGBB` is the whole of what the wire carries — `schemas/common.fbs` is
/// authoritative — and this is a compile-time assertion rather than a runtime one, so a
/// palette entry outside that range is a build failure instead of an appearance the
/// server would refuse.
const fn worn(colour: u32) -> u32 {
    assert!(
        colour & 0xFF00_0000 == 0,
        "a palette colour is 0x00RRGGBB, with the top eight bits reserved"
    );
    colour
}

/// **The colours are stated rather than picked freely, and the reason is the world
/// rather than the interface.**
///
/// A free colour picker makes every character an island: one player arrives in fluorescent
/// pink and the settlement stops looking like one place. What these five rows are is the
/// palette a dark Norse world could actually produce — undyed wool, the dyes a settlement
/// grows or trades for (madder, woad, weld, walnut), leather at the shades it tans to,
/// and hair at the colours hair comes in. Nothing here is bright, because nothing in this
/// world is.
///
/// **The server accepts any colour inside `0x00RRGGBB`**, so this table is an offer and
/// not a rule: a later screen may widen it, and a character created against an older
/// build wearing a colour this list no longer has is drawn exactly as it was stored. What
/// a client must never do is *narrow* what it will render, and nothing here does.
struct Palette {
    /// What the row is called on screen.
    label: &'static str,
    colours: &'static [u32],
}

/// Skin, from deepest to palest. Six, because a row of swatches has to stay readable at
/// the size a panel draws it and because these are ranges rather than an inventory of
/// people.
const SKIN: &[u32] = &[
    worn(0x0032_2016),
    worn(0x005C_4033),
    worn(0x008D_5524),
    worn(0x00C6_8642),
    worn(0x00E0_AC69),
    worn(0x00F1_C27D),
];

/// What a shirt is dyed with. Undyed wool first, then the four dyes a Norse settlement
/// actually had — madder red, woad blue, weld yellow, walnut brown — plus the greens and
/// greys that come of over-dyeing them.
const SHIRT: &[u32] = &[
    worn(0x00D8_CBB4),
    worn(0x008C_3B2B),
    worn(0x002F_4858),
    worn(0x00B0_8A2E),
    worn(0x005A_4632),
    worn(0x004B_5D3A),
    worn(0x0038_3A3E),
    worn(0x005C_2A2A),
];

/// Trousers: the same dyes worn out, which is what working clothes look like.
const TROUSERS: &[u32] = &[
    worn(0x003B_3226),
    worn(0x004A_4038),
    worn(0x002E_3440),
    worn(0x005A_4632),
    worn(0x006B_5B4B),
    worn(0x001F_1B18),
];

/// Leather, at the shades it tans to. Nothing here is dyed, because footwear was not.
const SHOES: &[u32] = &[
    worn(0x002A_211B),
    worn(0x004A_3728),
    worn(0x006B_5340),
    worn(0x001C_1C1C),
    worn(0x007A_6A55),
];

/// Hair, at the colours hair comes in — and grey, because not everybody who sails is
/// young.
const HAIR: &[u32] = &[
    worn(0x001B_1614),
    worn(0x003B_2A1E),
    worn(0x006B_4423),
    worn(0x008C_3B18),
    worn(0x00C7_A85C),
    worn(0x00B9_B4AC),
];

/// The five rows, in the order they are offered.
///
/// Head down: skin, then what is worn over it from the shoulders to the ground, then the
/// hair. The order matters only to a player reading the panel, which is reason enough for
/// it to be the order a person would describe somebody in.
const PALETTES: [Palette; 5] = [
    Palette {
        label: "SKIN",
        colours: SKIN,
    },
    Palette {
        label: "SHIRT",
        colours: SHIRT,
    },
    Palette {
        label: "TROUSERS",
        colours: TROUSERS,
    },
    Palette {
        label: "SHOES",
        colours: SHOES,
    },
    Palette {
        label: "HAIR",
        colours: HAIR,
    },
];

/// Which row of [`PALETTES`] the hair's colour is, named rather than counted.
const HAIR_PALETTE: usize = 4;

/// What a fresh draft looks like, and the fallback [`Draft::appearance`] cannot reach.
///
/// Built through [`Appearance::new`] in a `const`, so "the starting character is one this
/// contract allows" is a compile error rather than a sentence — the same shape
/// `PLACEHOLDER_APPEARANCE` has one module over.
const STARTING_APPEARANCE: Appearance = match Appearance::new(
    SKIN[2],
    SHIRT[0],
    TROUSERS[0],
    SHOES[1],
    HairModel::Cropped,
    HAIR[1],
) {
    Ok(appearance) => appearance,
    Err(_) => panic!("the starting appearance is not one this contract allows"),
};

// ---------------------------------------------------------------------------
// The draft
// ---------------------------------------------------------------------------

/// Which half of the screen is up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    /// The account's characters, and a way to make another when there is room.
    #[default]
    Choosing,
    /// The one being made.
    Creating,
}

/// One thing the keyboard can be on while a character is being made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// The name. Typing goes here and nowhere else.
    Name,
    /// One row of [`PALETTES`], by index.
    Colour(usize),
    /// Which hair model, out of [`HairModel::ALL`].
    Hair,
    /// Ask the server for it.
    Create,
    /// Back to the list without asking.
    Back,
}

/// Every field, in the order the keyboard walks them.
///
/// The hair *model* sits between the clothes and the hair colour, because that is the
/// order the preview reads in: what shape the hair is, and then what colour it is.
const FIELDS: [Field; 9] = [
    Field::Name,
    Field::Colour(0),
    Field::Colour(1),
    Field::Colour(2),
    Field::Colour(3),
    Field::Hair,
    Field::Colour(HAIR_PALETTE),
    Field::Create,
    Field::Back,
];

/// The longest name this screen will hold, **in bytes**.
///
/// **A bound on a text field and not a rule about names**, and it mirrors
/// `persist.MaxNameBytes` because the server measures the same thing the same way. This
/// counted *characters* until review found it: thirty-two of them is inside any character
/// count and twice over the byte limit as soon as they are CJK or emoji, and the refusal
/// that earns is a `ServerReject` — which by contract ends the connection and now costs a
/// reconnect before the same form can explain it. A name this accepts can still be
/// refused; that refusal is the server's to make, and it should not be one this screen
/// composed on purpose.
const NAME_LIMIT_BYTES: usize = 64;

/// What the player has chosen so far.
///
/// It deliberately **outlives one exchange**: a retryable creation refusal closes the
/// connection but leaves this form in place, and the fresh character list re-enables the
/// same draft rather than reconstructing six choices. Only the list focus is re-derived.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
struct Draft {
    mode: Mode,
    /// Which row is focused while choosing. Clamped by [`rows`] rather than trusted.
    row: usize,
    /// Which field is focused while creating, as an index into [`FIELDS`].
    field: usize,
    name: String,
    /// One index per row of [`PALETTES`].
    colour: [usize; PALETTES.len()],
    /// An index into [`HairModel::ALL`].
    hair: usize,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            mode: Mode::Choosing,
            row: 0,
            field: 0,
            name: String::new(),
            // The starting character, spelled as the indexes the swatches highlight. It
            // is the same one [`STARTING_APPEARANCE`] names, and the test at the bottom
            // is what keeps the two from drifting.
            colour: [2, 0, 0, 1, 1],
            hair: 1,
        }
    }
}

impl Draft {
    /// The character as it stands, which is what the preview draws and what a creation
    /// asks for.
    ///
    /// The `Err` arm is unreachable: every entry of every palette is checked at compile
    /// time by [`worn`], and every index is clamped by the control that moves it. It
    /// answers with the starting character rather than unwrapping, because a panic in a
    /// UI system takes the whole client down and there is a perfectly good character to
    /// draw instead.
    fn appearance(&self) -> Appearance {
        let colour = |row: usize| {
            PALETTES[row]
                .colours
                .get(self.colour[row])
                .copied()
                .unwrap_or(PALETTES[row].colours[0])
        };
        let model = HairModel::ALL
            .get(self.hair)
            .copied()
            .unwrap_or(HairModel::Cropped);

        Appearance::new(
            colour(0),
            colour(1),
            colour(2),
            colour(3),
            model,
            colour(HAIR_PALETTE),
        )
        .unwrap_or(STARTING_APPEARANCE)
    }

    /// Which field the keyboard is on.
    fn focused(&self) -> Field {
        FIELDS[self.field.min(FIELDS.len() - 1)]
    }

    /// Moves the choice on the focused row by `step`, wrapping.
    ///
    /// Wrapping rather than stopping, because these are rings of a handful of colours:
    /// a player holding left to see the last swatch should not have to let go and press
    /// right.
    fn cycle(&mut self, step: isize) {
        match self.focused() {
            Field::Colour(row) => {
                let count = PALETTES[row].colours.len();
                self.colour[row] = wrap(self.colour[row], step, count);
            }
            Field::Hair => self.hair = wrap(self.hair, step, HairModel::ALL.len()),
            // Nothing to cycle. A name is typed and the two controls are pressed.
            Field::Name | Field::Create | Field::Back => {}
        }
    }
}

/// One step around a ring of `len` things.
fn wrap(index: usize, step: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as isize;
    let next = (index.min(len as usize - 1) as isize + step).rem_euclid(len);
    next as usize
}

/// What a row of the list stands for.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// Play the character this id names.
    Play(u64),
    /// Make a new one.
    Create,
}

/// The rows the list currently offers, in the order they are drawn.
///
/// The creation is a row like any other rather than a control beside them, which is what
/// makes one focus index cover the whole screen — and it is offered only while the server
/// says there is room.
fn rows(choice: &CharacterChoice) -> Vec<Row> {
    let mut rows: Vec<Row> = choice
        .characters()
        .iter()
        .map(|character| Row::Play(character.character_id))
        .collect();
    if choice.has_room() {
        rows.push(Row::Create);
    }
    rows
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

#[derive(Component)]
struct CharacterRoot;

/// The container rows are spawned into and cleared out of.
#[derive(Component)]
struct RowList;

/// The half of the panel that is up while choosing.
#[derive(Component)]
struct ChoosingPanel;

/// And the half that is up while creating.
#[derive(Component)]
struct CreatingPanel;

/// The line under either half.
#[derive(Component)]
struct CharacterStatus;

/// The name as it has been typed, plus the cursor.
#[derive(Component)]
struct NameField;

/// The server's answer to the last name submitted, directly under that field.
#[derive(Component)]
struct NameRefusal;

/// One control on the creation form.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct FormField(Field);

/// One swatch: which palette row it belongs to, and which colour in it.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct Swatch {
    row: usize,
    index: usize,
}

/// The label that names the hair model currently chosen.
#[derive(Component)]
struct HairLabel;

/// The hole in the layout the turning model shows through.
///
/// A node with no background, and the whole of what the UI contributes to the preview:
/// the model is not drawn by `bevy_ui` at all, it stands in the world in front of the one
/// camera. What this node is *for* is the layout — it reserves the space beside the panel
/// so nothing else claims it — and its computed rect is where the model is placed, which
/// is what keeps the two agreeing when the window is resized.
///
/// It sits outside the panel rather than inside it, and it has to: a `bevy_ui` parent
/// draws behind its children, so a transparent node inside an opaque panel shows the
/// panel and not the world.
#[derive(Component)]
struct PreviewStage;

/// The turning body itself: a world entity, not a node.
///
/// One per character screen, spawned when the screen goes up and despawned when the world
/// arrives. Its children are the independently drawn pieces, exactly as [`crate::player`]
/// builds a body's, resting on their authored pivots.
#[derive(Component)]
struct PreviewModel;

/// One part of the turning body, so a re-dress can find them.
#[derive(Component)]
struct PreviewPart;

/// Between the server list's 45 and the login screen's 50. A player choosing a character
/// has signed in and picked a server, and neither of those screens is up behind this one.
const CHARACTER_LAYER: i32 = 47;

const PANEL: Color = Color::srgb(0.065, 0.075, 0.095);

/// The focused control's edge. Amber, which is the one colour this UI already uses to
/// mean "this is the one selected" — the hotbar's own [`super::SELECTED_EDGE`].
const FOCUS_EDGE: Color = Color::srgb(1.0, 0.72, 0.25);
/// And everything else's, which is the dark edge a cell already has.
const IDLE_EDGE: Color = Color::srgba(0.0, 0.0, 0.0, 0.0);

/// How much bigger the preview frame is than the rig it holds.
///
/// A little air on every side, so a pair of fists is *inside* the panel rather than
/// flush against its edge. Applied to both axes at once, which is what keeps the frame
/// the shape the envelope is and the figure in the middle of it.
const PREVIEW_MARGIN: f32 = 1.12;

/// How wide the stage the model turns on is drawn, in logical pixels. Its height is not a
/// constant: it comes from the rig, through [`preview_frame`], so a notch is the same
/// length across the stage as it is up it and the proportions on screen are the body's.
///
/// Wider than the flat swatch stack it replaced, because it now holds a figure that turns
/// — a body seen from the side is deeper than it is wide, and a stage cut to the front
/// view would clip an elbow every half turn.
const PREVIEW_WIDTH: f32 = 200.0;

/// How far in front of the camera the model stands, in blocks.
///
/// Any distance draws the same figure — it is scaled to the stage either way — so what
/// this number picks is the perspective: near enough that the turn reads as a turn rather
/// than as an orthographic slide, far enough that a nose is not wider than a shoulder.
const PREVIEW_DISTANCE: f32 = 3.0;

/// How fast the model turns, in radians per second.
///
/// A little under twelve seconds a revolution: slow enough to look at, fast enough that a
/// player who wants the back of a haircut does not wait for it. Presentation, and it
/// decides nothing — nothing reads this angle back.
const PREVIEW_TURN: f32 = 0.55;

/// What the camera clears to while the character screen is up.
///
/// **The screen is not a window onto the world**, and there is no world behind it anyway:
/// this client has no session while a character is being chosen. Flat and dark, chosen for
/// the screen, rather than the sky colour a world with no clock happens to keep — which is
/// a daylight blue, and reads as a game running behind a menu.
///
/// Put back the moment a session exists. `Daylight::FIXED` is where it goes back to,
/// because that is what `player::camera` spawns the camera with and what a world with no
/// clock keeps for ever.
const BACKDROP: Color = Color::srgb(0.020, 0.024, 0.032);

type ChangedButton = (Changed<Interaction>, With<Button>);

/// The two halves of the panel, and the two lines of text that are not the name. Named
/// because a query filter spelled inline three times is three chances to get one of the
/// `Without`s wrong — and Bevy needs every one of them, since the same component is
/// written by two queries in one system.
type ChoosingHalf = (
    With<ChoosingPanel>,
    Without<CharacterRoot>,
    Without<CreatingPanel>,
);
type CreatingHalf = (
    With<CreatingPanel>,
    Without<CharacterRoot>,
    Without<ChoosingPanel>,
);
type SwatchEdge = (Without<Row>, Without<FormField>);
type NameText = (
    With<NameField>,
    Without<NameRefusal>,
    Without<HairLabel>,
    Without<CharacterStatus>,
);
type NameRefusalText = (
    With<NameRefusal>,
    Without<NameField>,
    Without<HairLabel>,
    Without<CharacterStatus>,
);
type HairText = (
    With<HairLabel>,
    Without<NameField>,
    Without<NameRefusal>,
    Without<CharacterStatus>,
);

type StatusText = (
    With<CharacterStatus>,
    Without<NameField>,
    Without<NameRefusal>,
    Without<HairLabel>,
);

fn spawn_character_screen(mut commands: Commands) {
    commands
        .spawn((
            CharacterRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(28.0),
                ..default()
            },
            // **Transparent, and it used to be a near-opaque sheet.** The dark ground is
            // now the camera's, set by `paint_the_backdrop` while this screen is up — which
            // is what lets the turning model be seen at all, since `bevy_ui` draws over the
            // world and an overlay at 98% would have left a figure nobody could make out.
            // Nothing is lost by moving it: there is no world behind this screen to hide.
            BackgroundColor(Color::NONE),
            Visibility::Hidden,
            GlobalZIndex(CHARACTER_LAYER),
        ))
        .with_children(|overlay| {
            // Beside the panel rather than inside it. A `bevy_ui` parent draws behind its
            // children, so a hole cut in an opaque panel shows the panel.
            spawn_preview_stage(overlay);
            overlay
                .spawn((
                    Node {
                        width: Val::Px(620.0),
                        display: Display::Flex,
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(14.0),
                        padding: UiRect::all(Val::Px(28.0)),
                        border_radius: BorderRadius::all(Val::Px(8.0)),
                        ..default()
                    },
                    BackgroundColor(PANEL),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        Text::new("WHO IS GOING IN"),
                        TextFont {
                            font_size: FontSize::Px(26.0),
                            ..default()
                        },
                        TextColor(Color::WHITE),
                        TextShadow::default(),
                    ));

                    panel
                        .spawn(Node {
                            width: Val::Percent(100.0),
                            display: Display::Flex,
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(20.0),
                            ..default()
                        })
                        .with_children(|body| {
                            body.spawn((
                                Node {
                                    flex_grow: 1.0,
                                    display: Display::Flex,
                                    flex_direction: FlexDirection::Column,
                                    row_gap: Val::Px(8.0),
                                    ..default()
                                },
                                ChoosingPanel,
                            ))
                            .with_child((
                                RowList,
                                Node {
                                    width: Val::Percent(100.0),
                                    display: Display::Flex,
                                    flex_direction: FlexDirection::Column,
                                    row_gap: Val::Px(8.0),
                                    ..default()
                                },
                            ));

                            spawn_form(body);
                        });

                    panel.spawn((
                        CharacterStatus,
                        Text::new(""),
                        TextFont {
                            font_size: FontSize::Px(15.0),
                            ..default()
                        },
                        TextColor(Color::srgb(0.62, 0.66, 0.74)),
                        Node {
                            max_width: Val::Percent(100.0),
                            ..default()
                        },
                    ));
                });
        });
}

/// The creation form: a name, five rows of swatches, the hair model, and two controls.
fn spawn_form(parent: &mut ChildSpawnerCommands<'_>) {
    parent
        .spawn((
            CreatingPanel,
            Node {
                flex_grow: 1.0,
                // Down until somebody asks for it, and out of the layout while it is:
                // see `show_character_screen` for why this is a display and not a
                // visibility.
                display: Display::None,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|form| {
            // The name, which is the one field a player types into.
            form.spawn((
                FormField(Field::Name),
                Button,
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(38.0),
                    align_items: AlignItems::Center,
                    padding: UiRect::horizontal(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(BUTTON),
                BorderColor::all(IDLE_EDGE),
            ))
            .with_child((
                NameField,
                Text::new(""),
                TextFont {
                    font_size: FontSize::Px(17.0),
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
            form.spawn((
                NameRefusal,
                Text::new(""),
                TextFont {
                    font_size: FontSize::Px(14.0),
                    ..default()
                },
                TextColor(Color::srgb(0.95, 0.34, 0.30)),
                Node {
                    max_width: Val::Percent(100.0),
                    ..default()
                },
            ));

            for (row, palette) in PALETTES.iter().enumerate() {
                spawn_palette_row(form, row, palette);
            }

            // The hair model, which is a name rather than a colour: the wire carries a
            // member and the client maps it to a shape.
            form.spawn((
                FormField(Field::Hair),
                Button,
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(32.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::horizontal(Val::Px(10.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::all(Val::Px(4.0)),
                    ..default()
                },
                BackgroundColor(BUTTON),
                BorderColor::all(IDLE_EDGE),
            ))
            .with_children(|field| {
                field.spawn((
                    Text::new("HAIR MODEL"),
                    TextFont {
                        font_size: FontSize::Px(13.0),
                        ..default()
                    },
                    TextColor(Color::srgb(0.62, 0.66, 0.74)),
                ));
                field.spawn((
                    HairLabel,
                    Text::new(HairModel::Cropped.label()),
                    TextFont {
                        font_size: FontSize::Px(15.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                ));
            });

            for (field, label) in [(Field::Create, "CREATE"), (Field::Back, "BACK")] {
                form.spawn((
                    FormField(field),
                    Button,
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Px(36.0),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border: UiRect::all(Val::Px(2.0)),
                        border_radius: BorderRadius::all(Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(BUTTON),
                    BorderColor::all(IDLE_EDGE),
                ))
                .with_child((
                    Text::new(label),
                    TextFont {
                        font_size: FontSize::Px(16.0),
                        ..default()
                    },
                    TextColor(Color::WHITE),
                    TextShadow::default(),
                ));
            }
        });
}

/// One labelled row of swatches.
fn spawn_palette_row(parent: &mut ChildSpawnerCommands<'_>, row: usize, palette: &Palette) {
    parent
        .spawn((
            FormField(Field::Colour(row)),
            Node {
                width: Val::Percent(100.0),
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(4.0)),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BorderColor::all(IDLE_EDGE),
        ))
        .with_children(|line| {
            line.spawn((
                Text::new(palette.label),
                TextFont {
                    font_size: FontSize::Px(13.0),
                    ..default()
                },
                TextColor(Color::srgb(0.62, 0.66, 0.74)),
                Node {
                    width: Val::Px(78.0),
                    ..default()
                },
            ));
            for (index, colour) in palette.colours.iter().enumerate() {
                line.spawn((
                    Swatch { row, index },
                    Button,
                    Node {
                        width: Val::Px(26.0),
                        height: Val::Px(26.0),
                        border: UiRect::all(Val::Px(2.0)),
                        border_radius: BorderRadius::all(Val::Px(3.0)),
                        ..default()
                    },
                    BackgroundColor(swatch_colour(*colour)),
                    BorderColor::all(IDLE_EDGE),
                ));
            }
        });
}

/// The frame the preview draws inside: the whole rig, plus [`PREVIEW_MARGIN`].
///
/// Read from `player::appearance` rather than written down here, because how big a
/// character can get is a property of the body and not of this panel — and two of the
/// rig's parts deliberately leave the box the server collides, so a frame sized from that
/// box would clip a pair of knuckles off everybody and a knot off one haircut.
fn preview_frame() -> PlacedBox {
    let mut frame = body_envelope();
    frame.size *= PREVIEW_MARGIN;
    frame
}

/// The stage the model turns on: a hole in the layout, and nothing else.
///
/// **The preview used to be flat nodes**, one per box of the rig, laid out by percentage
/// inside a panel with the depth thrown away and a painter's `ZIndex` standing in for it.
/// It is now the actual rig, in the world, in front of the one camera — the same meshes
/// and the same materials the world dresses a body from, so the preview cannot disagree
/// with what a player will see of themselves.
///
/// The objection that kept it flat was that a second camera and a render target would put
/// the result out of reach of a headless test. That is true of a texture and there is no
/// texture here: the model is entities carrying `Mesh3d` and `MeshMaterial3d`, which
/// `player/tests.rs` already reads headlessly. And there is no second camera — the rule in
/// `player/camera.rs` stands untouched.
fn spawn_preview_stage(parent: &mut ChildSpawnerCommands<'_>) {
    let frame = preview_frame();
    // Square notches: the stage is as much taller than it is wide as the rig is.
    let height = PREVIEW_WIDTH * frame.size.y / frame.size.x;

    parent.spawn((
        PreviewStage,
        Node {
            width: Val::Px(PREVIEW_WIDTH),
            height: Val::Px(height),
            flex_shrink: 0.0,
            ..default()
        },
    ));
}

// ---------------------------------------------------------------------------
// The model that turns
// ---------------------------------------------------------------------------

/// What the model is currently wearing and how far round it has turned.
///
/// The angle lives here rather than being read back off the transform, for the reason a
/// mob's interpolated yaw does: recovering it would mean inverting a quaternion that
/// `place_the_preview` also writes a translation and a scale into. The appearance is what
/// makes a re-dress free — an unchanged draft changes nothing.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
struct PreviewState {
    worn: Option<Appearance>,
    turned: f32,
}

impl Default for PreviewState {
    fn default() -> Self {
        Self {
            worn: None,
            // Not zero: face-on is the least informative angle a rig has — the nose, the
            // knot and the fists all hide behind the silhouette — so the model starts a
            // little turned and the first thing a player sees is a person with depth.
            turned: 0.6,
        }
    }
}

/// Who the preview is of: the draft while one is being made, the focused character while
/// one is being chosen.
///
/// So the model is always of whoever is about to go in — which is what makes it a preview
/// rather than a decoration. It moved out of `refresh_screen` with the flat nodes it used
/// to lay out, and is a function of its own because the model and the tests both ask it.
fn previewed(draft: &Draft, choice: &CharacterChoice) -> Appearance {
    let focused = rows(choice).get(draft.row).copied();
    match (draft.mode, focused) {
        (Mode::Choosing, Some(Row::Play(id))) => choice
            .characters()
            .iter()
            .find(|character| character.character_id == id)
            .map_or_else(|| draft.appearance(), |character| character.appearance),
        _ => draft.appearance(),
    }
}

/// Keeps exactly one model alive for as long as the screen is, wearing whoever is chosen.
///
/// **Despawned and respawned per part, and that is allowed here where it is not for a
/// body.** `player::dress_bodies` swaps handles in place because a body carries an
/// identity and an interpolation that respawning would restart, and because it would blink
/// a figure standing in the world. This model has neither: the turn lives on the parent,
/// which is untouched, and rebuilding ten children on the key press that cycles a haircut
/// is cheaper than the query pair that would avoid it.
fn keep_the_preview(
    mut commands: Commands,
    choice: Option<Res<CharacterChoice>>,
    draft: Res<Draft>,
    session: Option<Res<Session>>,
    mut dressing: Dressing<'_>,
    mut state: ResMut<PreviewState>,
    models: Query<(Entity, Option<&Children>), With<PreviewModel>>,
) {
    // The world has arrived, or the exchange is over. Either way the screen is not up and
    // the model goes with it — the same life the screen's own nodes have.
    if choice.is_none() || session.is_some() {
        for (model, _) in &models {
            commands.entity(model).despawn();
        }
        if state.worn.is_some() {
            state.worn = None;
        }
        return;
    }

    let Some(choice) = choice else {
        // Unreachable: the early return above is the `None` case. Answered rather than
        // unwrapped, because a screen is the last thing that should panic.
        return;
    };
    let worn = previewed(&draft, &choice);
    let model = match models.iter().next() {
        Some((model, children)) => {
            if state.worn == Some(worn) {
                return;
            }
            for child in children.into_iter().flatten() {
                commands.entity(*child).despawn();
            }
            model
        }
        None => commands
            .spawn((
                PreviewModel,
                Transform::default(),
                // The screen is an overlay over a client that may have no world at all, so
                // nothing else is guaranteed to give this a visibility to inherit.
                Visibility::Visible,
            ))
            .id(),
    };

    let Some(mut wardrobe) = dressing.wardrobe() else {
        // The meshes do not exist yet. Nothing is recorded as worn, so the next frame
        // tries again rather than leaving a bare figure standing.
        return;
    };
    // The same wardrobe the world dresses a body from: the same meshes, and the same
    // material per colour. Not a copy of the tables — the acceptance criterion is that
    // this cannot disagree with what a player will see of themselves.
    let outfit = wardrobe.outfit(worn);
    commands.entity(model).with_children(|parent| {
        for (piece, mesh, material) in outfit {
            parent.spawn((
                PreviewPart,
                Mesh3d(mesh),
                MeshMaterial3d(material),
                resting_piece_transform(piece),
            ));
        }
    });
    state.worn = Some(worn);
}

/// Turns the model on the spot.
///
/// About the world's up axis and nothing else — it rotates, it does not walk. Wrapped
/// rather than left to grow, so the angle stays a number `f32` can still add a frame's
/// worth to after an hour on the screen.
fn turn_the_preview(
    time: Res<Time>,
    mut state: ResMut<PreviewState>,
    mut models: Query<&mut Transform, With<PreviewModel>>,
) {
    if state.worn.is_none() {
        return;
    }
    state.turned = (state.turned + PREVIEW_TURN * time.delta_secs()).rem_euclid(TAU);

    // The rotation is written here and the translation and scale by `place_the_preview`,
    // each touching only its own field. Splitting them is what lets the model turn on a
    // screen with no camera to be placed in front of — which is every headless test, and
    // also the frame before `PlayerCameraPlugin`'s `Startup` has run.
    let turned = Quat::from_rotation_y(state.turned);
    for mut transform in &mut models {
        if transform.rotation != turned {
            transform.rotation = turned;
        }
    }
}

/// Where in the world a point on the screen is, at a given distance in front of the camera.
///
/// **This is the coupling the issue named as the risk, and this function is all of it.**
/// The model stands in the world and the stage is laid out in screen space, so the two
/// have to agree — and keep agreeing when the window is resized or its aspect changes.
///
/// It is done through the projection rather than through `Camera::viewport_to_world`
/// because that one needs a viewport, which a headless app has none of: the maths would
/// then be the one part of this feature no test could reach. The vertical field of view is
/// what Bevy holds fixed across a resize, so the half-height at a distance is a constant
/// and the half-width is that times the aspect the projection carries.
fn world_point(
    camera: &GlobalTransform,
    fov: f32,
    aspect: f32,
    screen: Vec2,
    distance: f32,
) -> Vec3 {
    let half_height = distance * (fov / 2.0).tan();
    let half_width = half_height * aspect;
    camera.translation()
        + camera.forward() * distance
        + camera.right() * (screen.x * half_width)
        + camera.up() * (screen.y * half_height)
}

/// Stands the model in front of the camera, inside the stage, at the size the stage is.
///
/// Re-run every frame rather than on a resize event: the stage's computed rect is the
/// input, and taffy rewrites that whenever anything about the layout moves. A frame of
/// arithmetic over one entity is cheaper than being wrong about which events change it.
fn place_the_preview(
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&GlobalTransform, &Projection), With<WorldCamera>>,
    stages: Query<(&ComputedNode, &UiGlobalTransform), With<PreviewStage>>,
    mut models: Query<&mut Transform, With<PreviewModel>>,
) {
    let Some((camera, projection)) = cameras.iter().next() else {
        return;
    };
    let Projection::Perspective(perspective) = projection else {
        // Orthographic or custom: the half-height below is a perspective quantity and
        // there is no honest answer for one. Nothing is moved rather than something being
        // put in the wrong place.
        return;
    };

    // Where the stage is, as a fraction of the window from its centre: -1..1 across and
    // up. Both the rect and the window are read in physical pixels, so the scale factor
    // cancels and no display-scaling term is needed.
    let (centre, stage_height) = match (windows.iter().next(), stages.iter().next()) {
        (Some(window), Some((node, stage))) => {
            let size = window.physical_size().as_vec2();
            if size.x <= 0.0 || size.y <= 0.0 {
                return;
            }
            // `UiGlobalTransform`, not `GlobalTransform` — `bevy_ui`'s layout writes the
            // first and leaves the second at whatever transform propagation made of a
            // node's default `Transform`, which is the identity. Reading the wrong one put
            // every stage at the origin, which is the top-left corner of the screen.
            //
            // Its translation *is* the node's centre: `ui_layout_system` adds a
            // `local_center` and reads the pair back as `Rect::from_center_size(transform
            // .translation, node.size())`. Both are physical pixels, and so is
            // `Window::physical_size`, so the scale factor cancels and no display-scaling
            // term is needed.
            let middle = stage.translation;
            (
                Vec2::new(
                    middle.x / size.x * 2.0 - 1.0,
                    // Screen y grows downward and the frustum's grows up.
                    1.0 - middle.y / size.y * 2.0,
                ),
                node.size().y / size.y,
            )
        }
        // No window, or a layout that has not been computed: dead centre, at the height
        // the stage is written to be. This is the headless case, and it is the one the
        // tests run in — what they can assert is the distance, the scale and the turn,
        // which is everything except where on a screen there is no screen.
        _ => (
            Vec2::ZERO,
            PREVIEW_WIDTH * preview_frame().size.y / preview_frame().size.x / 720.0,
        ),
    };

    let frame = preview_frame();
    let half_height = PREVIEW_DISTANCE * (perspective.fov / 2.0).tan();
    // The rig is scaled so its framed height fills the stage's share of the window. The
    // vertical is the axis to anchor on: the field of view Bevy keeps fixed across a
    // resize is the vertical one, so this holds the figure the same size on screen
    // whatever the window's aspect becomes.
    let scale = (stage_height * half_height * 2.0) / frame.size.y;

    let stand = world_point(
        camera,
        perspective.fov,
        perspective.aspect_ratio,
        centre,
        PREVIEW_DISTANCE,
    );

    // The rig is authored from the feet up, so the model is dropped by half its framed
    // height to put the middle of the figure — rather than its ankles — in the middle of
    // the stage.
    let translation = stand - camera.up() * (frame.centre.y * scale);
    let scale = Vec3::splat(scale);
    for mut transform in &mut models {
        if transform.translation != translation {
            transform.translation = translation;
        }
        if transform.scale != scale {
            transform.scale = scale;
        }
    }
}

/// Gives the camera the screen's own flat backdrop while the screen is up.
///
/// Restored the moment a session exists, to the value `player::camera` spawns it with —
/// read from `Daylight::FIXED` rather than written down again, so a world with no clock
/// and a client that has just left this screen agree by construction.
fn paint_the_backdrop(
    choice: Option<Res<CharacterChoice>>,
    session: Option<Res<Session>>,
    mut cameras: Query<&mut Camera, With<WorldCamera>>,
) {
    let up = choice.is_some() && session.is_none();
    for mut camera in &mut cameras {
        // `ClearColorConfig` is not `PartialEq`, so the comparison is on the colour the two
        // cases actually differ in. Written only on a change for the reason every other
        // write on this screen is: `Mut` marks the component changed on `DerefMut` whether
        // or not the value moved.
        let current = match camera.clear_color {
            ClearColorConfig::Custom(colour) => Some(colour),
            _ => None,
        };

        if up {
            if current != Some(BACKDROP) {
                camera.clear_color = ClearColorConfig::Custom(BACKDROP);
            }
        } else if current == Some(BACKDROP) {
            // **Put back once, and only what this screen put there.** Writing the fixed
            // sky whenever a session exists would have been this system overwriting
            // `player::sky::drive_the_sky` on every frame of every world with a clock —
            // which is every world that has one, so the day would never have turned. Found
            // by the review of this pull request.
            //
            // `Daylight::FIXED` is the right value to restore to and the wrong one to keep
            // asserting: it is what `player::camera` spawns the camera with, so a world
            // with no clock lands exactly where it started, and a world with a clock is
            // corrected by its own system on the very next frame.
            camera.clear_color = ClearColorConfig::Custom(Daylight::FIXED.sky);
        }
    }
}

/// A palette colour as Bevy holds one. The wire's `0x00RRGGBB` is sRGB, which is what
/// `srgb_u8` takes — there is no conversion decision here and there must not be one.
fn swatch_colour(colour: u32) -> Color {
    Color::srgb_u8(
        ((colour >> 16) & 0xFF) as u8,
        ((colour >> 8) & 0xFF) as u8,
        (colour & 0xFF) as u8,
    )
}

/// The character the command line named, if it named one.
///
/// **`--name` used to be a display name and is now the person going in.** Before V7 the
/// server read `ClientHello.player_name` and settled a character from it — an account
/// with one wearing that name played it, an account with none had it created — so
/// `--name Eivor` was how a development launch said who to be. V7 moved that decision
/// onto the wire, where it belongs, and left the hello's field carrying nothing anybody
/// reads. This is the same sentence in the new grammar: the client *asks* for the
/// character with that name, and the server answers as it does for any other request.
///
/// **It is a launch option and not a way past this screen.** With no `--name` the screen
/// waits for a person, which is the path every player takes; with one it is answered
/// before it is drawn, which is what lets an unattended check — `scripts/interop-check.sh`
/// — reach a world at all. Nothing about the request is privileged: a name the server
/// refuses is refused here too, and the refusal lands on the screen the same way.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct PlayAs(Option<String>);

impl PlayAs {
    /// The character `--name` (or `VOXELHEIM_NAME`) asked for.
    pub fn named(name: impl Into<String>) -> Self {
        Self(Some(name.into()))
    }

    /// Whom to ask for, or `None` when the launch named nobody.
    fn wanted(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// Answers the character phase from the command line, when the launch named somebody.
///
/// Two requests and one silence, and which of the three happens is decided from the list
/// the server sent rather than from anything remembered here:
///
///   - a listed character wearing that name is played;
///   - otherwise, if the account has room, one is created under it — wearing
///     [`STARTING_APPEARANCE`], because a command line names a person and not a face;
///   - and with a full roster holding nobody by that name there is nothing to ask for, so
///     the screen stays up and a person chooses.
///
/// **Asked once per exchange.** [`CharacterChoice::answered`] is set by the network
/// boundary in the frame it sends the frame, which is not necessarily this one, so the
/// guard is local as well: a second `SelectCharacterRequest` after a welcome is a
/// protocol error that ends the session, and this system must never be what causes one.
///
/// **And spent once the player has actually been in the world**, which is what #184 added.
/// Leaving a world now lands back on its character screen, and a launch flag that answered
/// that exchange too would send the player straight back in — a control that cannot be
/// used. The line is drawn at a [`Session`] having existed rather than at the exchange
/// number. A wholly new pre-session exchange still gets the launch's answer, which is
/// what `a_second_exchange_is_answered_like_the_first` holds. A name-refusal reconnect is
/// not wholly new: `CharacterChoice` stays present and the local `asked` guard prevents
/// the launch from submitting the same refused name forever.
fn answer_from_the_launch(
    choice: Option<Res<CharacterChoice>>,
    play_as: Res<PlayAs>,
    session: Option<Res<Session>>,
    mut chosen: MessageWriter<ChooseCharacter>,
    mut asked: Local<bool>,
    mut spent: Local<bool>,
) {
    if session.is_some() {
        *spent = true;
    }
    let Some(choice) = choice else {
        // The exchange is over — established, refused or disconnected. The next one is a
        // new question, and gets a new answer unless the launch's has been spent.
        *asked = false;
        return;
    };
    if *spent {
        return;
    }
    let Some(wanted) = play_as.wanted() else {
        return;
    };
    if *asked || choice.answered() {
        return;
    }

    let listed = choice
        .characters()
        .iter()
        .find(|character| character.name == wanted);
    let request = match listed {
        Some(character) => ChooseCharacter::Play(character.character_id),
        None if choice.has_room() => ChooseCharacter::Create {
            name: wanted.to_owned(),
            appearance: STARTING_APPEARANCE,
        },
        // Nothing to ask for. Said once, because this system runs every frame the screen
        // is up, and said at all because the launch asked for something it did not get.
        None => {
            warn!(
                "no character here is called {wanted}, and this account holds as many as \
                 the world allows; choose one on the screen"
            );
            *asked = true;
            return;
        }
    };

    chosen.write(request);
    *asked = true;
}

/// Whether the character screen owns the screen this frame.
///
/// Read by `ui/mod.rs` as well: the pointer belongs to whatever is on top, and a control
/// nobody can click is not a control. It is up exactly while an exchange is live, which
/// is the presence of [`CharacterChoice`] and nothing else — the login screen cannot be
/// up at the same time, because a client that has not signed in has no session to be
/// choosing on.
pub(super) fn character_is_up(choice: Option<&CharacterChoice>) -> bool {
    choice.is_some()
}

fn show_character_screen(
    choice: Option<Res<CharacterChoice>>,
    draft: Res<Draft>,
    mut roots: Query<&mut Visibility, With<CharacterRoot>>,
    mut choosing: Query<&mut Node, ChoosingHalf>,
    mut creating: Query<&mut Node, CreatingHalf>,
) {
    let up = character_is_up(choice.as_deref());
    let next = if up {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut roots {
        if *visibility != next {
            *visibility = next;
        }
    }

    // **`Display`, not `Visibility`, and the difference is the whole of this fix.** The
    // two halves are flex siblings that both grow, and `bevy_ui` lays a hidden node out
    // exactly as it lays out a visible one — only `Display::None` takes a node out of
    // taffy. Switched by visibility, the half that was down went on claiming half the
    // panel, so the list drew into half its width beside an empty column. Nothing caught
    // it because every test here is headless and reads components rather than a layout.
    let (list, form) = match (up, draft.mode) {
        (true, Mode::Choosing) => (Display::Flex, Display::None),
        (true, Mode::Creating) => (Display::None, Display::Flex),
        // The root is hidden, so neither half is drawn either way. Left as it was rather
        // than written every frame: a `Node` written on an idle frame marks the component
        // changed for every consumer of it, and taffy is one of them.
        (false, _) => return,
    };
    for mut node in &mut choosing {
        if node.display != list {
            node.display = list;
        }
    }
    for mut node in &mut creating {
        if node.display != form {
            node.display = form;
        }
    }
}

/// Replaces the rows when the list changes, and sets the focus when an exchange begins.
///
/// **Two questions, and they are deliberately not the same one.** The row *entities* are
/// rebuilt when what the server offers differs from what is drawn — never on every frame,
/// for the reason the server list's own rebuild gives: respawning every button on every
/// frame is a pointer that can never finish a press. The *focus* is set when the resource
/// is added, which is once per exchange, so a preselection cannot fight the arrow keys.
fn rebuild_rows(
    choice: Option<Res<CharacterChoice>>,
    mut draft: ResMut<Draft>,
    mut drawn: Local<Vec<Row>>,
    containers: Query<Entity, With<RowList>>,
    existing: Query<Entity, With<Row>>,
    mut commands: Commands,
) {
    let Some(choice) = choice else {
        // The exchange is over, and what was drawn belonged to it. `Row::Play` carries an
        // id and nothing else — not the name, not the face, not the limit — and character
        // ids are minted per world, so the next server's list can be identical row for row
        // and describe different people. Forgetting here is what makes the comparison
        // below a comparison within one exchange, which is the only thing it can be.
        drawn.clear();
        return;
    };
    let offered = rows(&choice);

    if choice.is_added() {
        // The character this client played here last, or the first row. Both are a
        // preselection and neither is a decision: the server is told nothing until
        // somebody presses something.
        draft.row = choice
            .preselect()
            .and_then(|id| offered.iter().position(|row| *row == Row::Play(id)))
            .unwrap_or(0);
        draft.mode = if offered.iter().any(|row| matches!(row, Row::Play(_))) {
            Mode::Choosing
        } else {
            // An account with no characters here has exactly one thing it can do, and
            // making them press "new character" first would be a screen asking a question
            // with one answer. The draft itself is deliberately left alone — see [`Draft`].
            Mode::Creating
        };
    }

    // **A row index can outlive the list it was chosen from.** The focus is set once per
    // exchange, on `is_added` — but a second `ServerCharacterList` inside one exchange
    // replaces the resource without re-adding it, so a shorter list leaves `draft.row`
    // pointing past its end. Every reader then finds nothing there: no row is highlighted
    // and Enter does nothing, silently.
    //
    // Clamped here rather than at each reader, which is what the review of #163 asked for
    // and is the better half of the choice it offered. The three guards this issue removed
    // defended against a list that cannot decode; this defends against an index that is
    // genuinely stale, and it does it where the change is known instead of masking it at
    // one of the places that reads it.
    if draft.row >= offered.len() {
        draft.row = offered.len().saturating_sub(1);
    }

    if *drawn == offered {
        return;
    }
    drawn.clone_from(&offered);

    for row in &existing {
        commands.entity(row).despawn();
    }
    for container in &containers {
        commands.entity(container).with_children(|parent| {
            for row in &offered {
                spawn_row(parent, *row, &choice);
            }
        });
    }
}

fn spawn_row(parent: &mut ChildSpawnerCommands<'_>, row: Row, choice: &CharacterChoice) {
    let label = match row {
        Row::Play(id) => choice
            .characters()
            .iter()
            .find(|character| character.character_id == id)
            .map_or_else(|| "?".to_owned(), |character| character.name.clone()),
        Row::Create => format!(
            "NEW CHARACTER  ({} of {})",
            choice.characters().len(),
            choice.max_characters()
        ),
    };

    parent
        .spawn((
            row,
            Button,
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(46.0),
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(12.0)),
                column_gap: Val::Px(10.0),
                border: UiRect::all(Val::Px(2.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(BUTTON),
            BorderColor::all(IDLE_EDGE),
        ))
        .with_children(|button| {
            // A character's own colours beside its name, so two characters are told
            // apart by more than a word. The swatches are the same values the preview
            // draws with, which is what makes them the *character's* rather than a
            // decoration.
            if let Row::Play(id) = row
                && let Some(character) = choice
                    .characters()
                    .iter()
                    .find(|character| character.character_id == id)
            {
                for part in BodyPart::WORN {
                    button.spawn((
                        Node {
                            width: Val::Px(12.0),
                            height: Val::Px(22.0),
                            border_radius: BorderRadius::all(Val::Px(2.0)),
                            ..default()
                        },
                        BackgroundColor(swatch_colour(part.colour(character.appearance))),
                    ));
                }
            }
            button.spawn((
                Text::new(label),
                TextFont {
                    font_size: FontSize::Px(18.0),
                    ..default()
                },
                TextColor(Color::WHITE),
                TextShadow::default(),
            ));
        });
}

/// The keyboard: move the focus, cycle a choice, take a row, go back.
///
/// **Every control on this screen is reachable from the keyboard**, which is the half of
/// the acceptance criterion a pointer cannot cover: a player who has just typed a name
/// should not have to reach for the mouse to confirm it.
fn navigate(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    choice: Option<Res<CharacterChoice>>,
    mut draft: ResMut<Draft>,
    mut choices: MessageWriter<ChooseCharacter>,
) {
    let (Some(keys), Some(choice)) = (keys, choice) else {
        return;
    };
    if choice.answered() {
        // The server has the question and the answer is a welcome. Nothing here may
        // send a second one — see `CharacterChoice::answered`.
        return;
    }

    let offered = rows(&choice);
    match draft.mode {
        Mode::Choosing => {
            // `offered` is never empty, so none of this defends against one that is.
            // `rows` returns a row per character plus a creation while there is room,
            // and `codec::character_list` refuses a list whose `max_characters` is zero
            // or smaller than the count it just sent — so an account with no characters
            // is offered a creation and an account with characters is offered them. The
            // three guards that used to be here (`len().max(1)`, `min(count - 1)`, and
            // an `is_empty` on Escape) each covered a state that cannot decode, and
            // together they made a reachable list look like the uncertain case.
            //
            // A row index *can* outlive its list, which is a different thing and a real
            // one — `rebuild_rows` clamps it there, where the list changing is known.
            let count = offered.len();
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::Tab) {
                draft.row = wrap(draft.row, 1, count);
            }
            if keys.just_pressed(KeyCode::ArrowUp) {
                draft.row = wrap(draft.row, -1, count);
            }
            if (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter))
                && let Some(row) = offered.get(draft.row).copied()
            {
                take(row, &mut draft, &mut choices);
            }
        }
        Mode::Creating => {
            if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::Tab) {
                draft.field = wrap(draft.field, 1, FIELDS.len());
            }
            if keys.just_pressed(KeyCode::ArrowUp) {
                draft.field = wrap(draft.field, -1, FIELDS.len());
            }
            if keys.just_pressed(KeyCode::ArrowRight) {
                draft.cycle(1);
            }
            if keys.just_pressed(KeyCode::ArrowLeft) {
                draft.cycle(-1);
            }
            if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter) {
                match draft.focused() {
                    Field::Create => ask_to_create(&draft, &mut choices),
                    Field::Back => draft.mode = Mode::Choosing,
                    // Enter on a colour or a name does nothing: there is one control that
                    // asks the server for a character, and it is the one labelled with it.
                    Field::Name | Field::Colour(_) | Field::Hair => {}
                }
            }
            // Escape leaves the form rather than the screen. The screen itself is not
            // dismissible — a session that has been offered a character list is waiting
            // for one — which is the rule the login screen keeps for the same reason.
            if keys.just_pressed(KeyCode::Escape) {
                draft.mode = Mode::Choosing;
            }
        }
    }
}

/// Typing, which goes to the name and nowhere else.
///
/// Read off `KeyboardInput` rather than `ButtonInput<KeyCode>`, because what a key
/// *produces* is a layout's business: a French keyboard's `A` is where an English one has
/// `Q`, and a client reading key codes would spell names in the wrong alphabet.
fn type_the_name(
    mut typed: MessageReader<KeyboardInput>,
    choice: Option<Res<CharacterChoice>>,
    mut draft: ResMut<Draft>,
) {
    let editing = choice.is_some_and(|choice| !choice.answered())
        && draft.mode == Mode::Creating
        && draft.focused() == Field::Name;
    if !editing {
        // Still drained: a key pressed while the field was not focused must not arrive in
        // it three frames later.
        typed.clear();
        return;
    }

    for key in typed.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        match &key.logical_key {
            Key::Backspace => {
                draft.name.pop();
            }
            // One character as the layout produced it, control characters and the
            // multi-character sequences an input method can produce included — the
            // filtering is by what a *name* may hold rather than by how many bytes
            // arrived. The server decides whether the result is acceptable.
            Key::Character(text) => {
                for character in text.chars() {
                    push_name(&mut draft.name, character);
                }
            }
            Key::Space => push_name(&mut draft.name, ' '),
            _ => {}
        }
    }
}

/// Adds one character to a draft name, unless it is one no name may hold.
///
/// Control characters are refused here because they are refused everywhere: a name
/// carrying a newline or a terminal escape is a name that rewrites the log line it is
/// printed in, which is the server's own reason for refusing one. This is the one place
/// this screen filters anything, and it filters what a *text field* may hold rather than
/// what a name may be — the length, the emptiness and the shape are the server's.
fn push_name(name: &mut String, character: char) {
    // Measured the way the server measures it: the character that would cross the limit
    // is refused whole, never split.
    if character.is_control() || name.len() + character.len_utf8() > NAME_LIMIT_BYTES {
        return;
    }
    name.push(character);
}

/// A row takes the character it names, or opens the form.
fn row_clicks(
    mut rows: Query<(&Interaction, &Row, &mut BackgroundColor), ChangedButton>,
    choice: Option<Res<CharacterChoice>>,
    mut draft: ResMut<Draft>,
    mut choices: MessageWriter<ChooseCharacter>,
) {
    let answered = choice.is_none_or(|choice| choice.answered());
    for (interaction, row, mut colour) in &mut rows {
        colour.0 = button_colour(interaction);
        if *interaction == Interaction::Pressed && !answered {
            take(*row, &mut draft, &mut choices);
        }
    }
}

/// What pressing a row does: play that character, or move to the form.
fn take(row: Row, draft: &mut Draft, choices: &mut MessageWriter<'_, ChooseCharacter>) {
    match row {
        Row::Play(character) => {
            choices.write(ChooseCharacter::Play(character));
        }
        Row::Create => draft.mode = Mode::Creating,
    }
}

/// Asks the server for the character in the draft.
///
/// The name is sent exactly as it was typed, the empty string included. What names a
/// world accepts is the server's rule and its refusal is one a player can read and act
/// on; a client that pre-judged it would be holding an opinion about the names of
/// characters it cannot see.
fn ask_to_create(draft: &Draft, choices: &mut MessageWriter<'_, ChooseCharacter>) {
    choices.write(ChooseCharacter::Create {
        name: draft.name.clone(),
        appearance: draft.appearance(),
    });
}

/// The form's controls: focus follows the pointer's press, and a swatch is a choice.
fn field_clicks(
    mut fields: Query<(&Interaction, &FormField, &mut BackgroundColor), ChangedButton>,
    swatches: Query<(&Interaction, &Swatch), ChangedButton>,
    choice: Option<Res<CharacterChoice>>,
    mut draft: ResMut<Draft>,
    mut choices: MessageWriter<ChooseCharacter>,
) {
    let answered = choice.is_none_or(|choice| choice.answered());

    for (interaction, field, mut colour) in &mut fields {
        colour.0 = button_colour(interaction);
        if *interaction != Interaction::Pressed || answered {
            continue;
        }
        if let Some(index) = FIELDS.iter().position(|known| *known == field.0) {
            draft.field = index;
        }
        match field.0 {
            Field::Create => ask_to_create(&draft, &mut choices),
            Field::Back => draft.mode = Mode::Choosing,
            // Pressing the hair row cycles it, which is what a control with one visible
            // value and no arrows has to do to be usable with a pointer at all.
            Field::Hair => draft.cycle(1),
            Field::Name | Field::Colour(_) => {}
        }
    }

    for (interaction, swatch) in &swatches {
        if *interaction != Interaction::Pressed || answered {
            continue;
        }
        if let Some(index) = FIELDS
            .iter()
            .position(|known| *known == Field::Colour(swatch.row))
        {
            draft.field = index;
        }
        draft.colour[swatch.row] = swatch.index;
    }
}

/// Writes everything that follows from the draft: the focus, the swatches, the preview,
/// the name and the line under it.
///
/// One system rather than five, because they all read the same two resources and each
/// would otherwise repeat the same change check. It runs only when something moved, for
/// the reason every other refresh here does: writing an unchanged value marks the
/// component changed for every consumer of it.
#[allow(clippy::too_many_arguments)]
fn refresh_screen(
    draft: Res<Draft>,
    choice: Option<Res<CharacterChoice>>,
    mut drawn_rows: Query<(&Row, &mut BorderColor)>,
    mut fields: Query<(&FormField, &mut BorderColor), Without<Row>>,
    mut swatches: Query<(&Swatch, &mut BorderColor), SwatchEdge>,

    mut name: Query<&mut Text, NameText>,
    mut name_refusal: Query<&mut Text, NameRefusalText>,
    mut hair: Query<&mut Text, HairText>,
    mut status: Query<&mut Text, StatusText>,
) {
    // Two resources, not three. `ConnectionState` was a third trigger while `describe`
    // read it; nothing this system writes is derived from the state any more, so waking
    // on a change to it could only ever rewrite the same values.
    let moved = draft.is_changed() || choice.as_ref().is_some_and(|choice| choice.is_changed());
    if !moved {
        return;
    }
    let Some(choice) = choice else {
        return;
    };

    let offered = rows(&choice);
    let focused_row = offered.get(draft.row).copied();
    for (row, mut border) in &mut drawn_rows {
        let edge = if draft.mode == Mode::Choosing && Some(*row) == focused_row {
            FOCUS_EDGE
        } else {
            IDLE_EDGE
        };
        set_border(&mut border, edge);
    }

    for (field, mut border) in &mut fields {
        let edge = if draft.mode == Mode::Creating && field.0 == draft.focused() {
            FOCUS_EDGE
        } else {
            IDLE_EDGE
        };
        set_border(&mut border, edge);
    }

    for (swatch, mut border) in &mut swatches {
        let edge = if draft.colour[swatch.row] == swatch.index {
            Color::WHITE
        } else {
            IDLE_EDGE
        };
        set_border(&mut border, edge);
    }

    for mut text in &mut name {
        let line = if draft.name.is_empty() {
            "NAME...".to_owned()
        } else {
            draft.name.clone()
        };
        if text.0 != line {
            *text = Text::new(line);
        }
    }

    let refusal = choice.creation_refusal().unwrap_or_default();
    for mut text in &mut name_refusal {
        if text.0 != refusal {
            *text = Text::new(refusal.to_owned());
        }
    }

    for mut text in &mut hair {
        let line = HairModel::ALL
            .get(draft.hair)
            .map_or("", |model| model.label());
        if text.0 != line {
            *text = Text::new(line.to_owned());
        }
    }

    let line = describe(&draft, &choice);
    for mut text in &mut status {
        if text.0 != line {
            *text = Text::new(line.clone());
        }
    }
}

fn set_border(border: &mut BorderColor, edge: Color) {
    let next = BorderColor::all(edge);
    if *border != next {
        *border = next;
    }
}

/// The line under the panel.
///
/// It says what the screen is waiting for, which is a different thing in each of its
/// three states — and after a choice has gone out it says so, because a screen that
/// looked unchanged while a request was in flight is one a player presses again.
///
/// **It does not read [`ConnectionState`]**, and the retryable refusal does not change
/// that. The network boundary carries the server's sentence on [`CharacterChoice`], and
/// [`refresh_screen`] writes it beside the name; all other rejects take this screen down
/// and remain the status screen's responsibility. One rejection therefore still has one
/// renderer.
fn describe(draft: &Draft, choice: &CharacterChoice) -> String {
    if choice.answered() {
        return "Asking the server...".to_owned();
    }
    match draft.mode {
        Mode::Choosing if choice.characters().is_empty() => {
            "No character on this world yet. Make one.".to_owned()
        }
        Mode::Choosing => "Pick who is going in. Arrow keys move, Enter goes.".to_owned(),
        Mode::Creating if choice.has_room() => {
            "Name the character and choose how they look. Left and right change a row.".to_owned()
        }
        // Reachable by pressing the creation row and then losing the room for one, which
        // cannot happen inside one exchange — the list does not change under it. Stated
        // anyway, because a screen offering a control the server will refuse should say
        // so rather than let the refusal explain it.
        Mode::Creating => format!(
            "This account already holds the {} characters this world allows.",
            choice.max_characters()
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::asset::AssetPlugin;
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    // The screen no longer reads the connection state — see `describe`. The tests still
    // insert it, because the plugin is built under the state the game builds it under.
    use crate::net::{CharacterSummary, ConnectionState};

    /// Builds the screen headlessly. `MinimalPlugins` has no renderer, so the nodes are
    /// spawned and updated but never drawn — which is exactly the part worth asserting,
    /// and it needs no display.
    fn headless(choice: CharacterChoice) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            // The preview is a real body now, so the assets its meshes and materials live
            // in have to exist. `MinimalPlugins` has no renderer and needs none: `Assets<T>`
            // is an ordinary resource, which is exactly what `player/tests.rs` relies on.
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_message::<ChooseCharacter>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(ConnectionState::Choosing)
            .insert_resource(choice)
            .add_plugins(CharacterUiPlugin);
        app.update();
        app
    }

    /// A session: what makes the character screen go down, and what spends the launch's
    /// answer.
    ///
    /// The values are not read by anything under test here — what matters is that the
    /// resource exists, because its presence *is* "the world has arrived".
    ///
    /// One copy serving both. #181 and #184 each added their own, in different halves of
    /// this module, and git merged the pair without a conflict to report — two definitions
    /// of one name, in a tree that then did not compile. Worth knowing before adding a
    /// third: a clean merge is not a compiling one.
    fn a_session() -> Session {
        Session(crate::net::SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.5, 64.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    /// The parts of the turning model, and the colour each is wearing.
    ///
    /// Read off `StandardMaterial::base_color` rather than a `BackgroundColor`: the
    /// preview is meshes and materials now, which is the shape `player/tests.rs` already
    /// asserts headlessly.
    fn preview_colours(app: &mut App) -> Vec<Color> {
        let world = app.world_mut();
        let mut parts =
            world.query_filtered::<&MeshMaterial3d<StandardMaterial>, With<PreviewPart>>();
        let handles: Vec<_> = parts
            .iter(world)
            .map(|material| material.0.clone())
            .collect();
        let materials = world.resource::<Assets<StandardMaterial>>();
        handles
            .iter()
            .filter_map(|handle| materials.get(handle))
            .map(|material| material.base_color)
            .collect()
    }

    /// The same, launched with `--name` naming somebody.
    fn headless_playing_as(choice: CharacterChoice, name: &str) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            // The character screen's preview is a real body, so the assets its meshes and
            // materials live in have to exist. `Assets<T>` is an ordinary resource, which
            // is what keeps this headless.
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_message::<ChooseCharacter>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(ConnectionState::Choosing)
            .insert_resource(choice)
            .insert_resource(PlayAs::named(name))
            .add_plugins(CharacterUiPlugin);
        app.update();
        app
    }

    /// A character the server might have listed.
    fn character(id: u64, name: &str) -> CharacterSummary {
        CharacterSummary {
            character_id: id,
            name: name.to_owned(),
            appearance: STARTING_APPEARANCE,
        }
    }

    /// One key press, complete with the release that a real frame would bring.
    ///
    /// `ButtonInput::just_pressed` is edge-triggered against a frame boundary, and in a
    /// running client `InputPlugin` is what moves that boundary. Here the test does it, so
    /// a press cannot leak into the frame after the one it was made in.
    fn press(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(key);
        keys.clear();
    }

    /// Types text into the screen, one key at a time, the way a keyboard delivers it.
    fn type_text(app: &mut App, text: &str) {
        for character in text.chars() {
            let logical = if character == ' ' {
                Key::Space
            } else {
                Key::Character(character.to_string().into())
            };
            app.world_mut().write_message(KeyboardInput {
                key_code: KeyCode::KeyA,
                logical_key: logical,
                state: ButtonState::Pressed,
                text: None,
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
        }
        app.update();
    }

    /// What the screen has asked the network boundary for.
    fn asked(app: &App) -> Vec<ChooseCharacter> {
        let messages = app.world().resource::<Messages<ChooseCharacter>>();
        let mut cursor = messages.get_cursor();
        cursor.read(messages).cloned().collect()
    }

    /// The labels on the rows, in the order they are drawn.
    ///
    /// Read off the container's `Children` rather than from a query, because that is the
    /// order a player sees and a query's is unspecified — the list's own order is the
    /// server's, and following it is what makes two launches show the same panel.
    fn row_labels(app: &mut App) -> Vec<String> {
        let world = app.world_mut();
        let mut containers = world.query_filtered::<&Children, With<RowList>>();
        let rows: Vec<Entity> = containers
            .iter(world)
            .flat_map(|children| children.iter())
            .collect();

        rows.into_iter()
            .filter(|row| world.get::<Row>(*row).is_some())
            .filter_map(|row| {
                let children = world.get::<Children>(row)?;
                children
                    .iter()
                    .find_map(|child| world.get::<Text>(child).map(|text| text.0.clone()))
            })
            .collect()
    }

    fn draft(app: &App) -> Draft {
        app.world().resource::<Draft>().clone()
    }

    /// The sentence drawn directly under the name field.
    fn name_refusal(app: &mut App) -> String {
        let world = app.world_mut();
        let mut refusal = world.query_filtered::<&Text, With<NameRefusal>>();
        refusal
            .iter(world)
            .next()
            .expect("the name refusal line exists")
            .0
            .clone()
    }

    /// **Two characters, two rows, and a way to make a third.** The names are the
    /// server's and the order is the server's; nothing here sorts or invents one.
    #[test]
    fn two_characters_become_two_rows_and_a_way_to_make_another() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor"), character(7, "Sigrun")],
            3,
        ));

        let labels = row_labels(&mut app);
        assert_eq!(labels.len(), 3, "{labels:?}");
        assert_eq!(labels[0], "Eivor");
        assert_eq!(labels[1], "Sigrun");
        assert!(labels[2].starts_with("NEW CHARACTER"), "{labels:?}");
        assert!(
            labels[2].contains('3'),
            "the row says what the limit is: {labels:?}"
        );
    }

    /// A shorter list moves the focus onto a row that exists.
    ///
    /// The focus is chosen once per exchange, and a second `ServerCharacterList` replaces
    /// `CharacterChoice` without re-adding it — so nothing would move a row index that the
    /// new list is too short for. Every reader takes it through `offered.get`, which
    /// answers `None`: no row highlighted, and Enter silently doing nothing. Found by the
    /// review of #163, which is also why the clamp is in `rebuild_rows` rather than back
    /// where the removed guards were.
    #[test]
    fn a_shorter_list_pulls_the_focus_back_onto_a_row() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![
                character(900, "Eivor"),
                character(7, "Sigrun"),
                character(11, "Ulf"),
            ],
            3,
        ));
        app.update();

        // The last row of three, chosen with the arrow keys.
        app.world_mut().resource_mut::<Draft>().row = 2;
        app.update();
        assert_eq!(focused(&mut app), Some(Row::Play(11)));

        // The server sends a shorter list inside the same exchange.
        app.insert_resource(CharacterChoice::for_a_test(
            vec![character(900, "Eivor")],
            3,
        ));
        app.update();

        assert_eq!(
            draft(&app).row,
            1,
            "the focus stayed past the end of the list it was chosen from"
        );
        assert_eq!(
            focused(&mut app),
            Some(Row::Create),
            "nothing is focused, so Enter does nothing and no row is drawn lit"
        );
    }

    /// The row the screen would act on, read the way `navigate` and `refresh_screen` do.
    fn focused(app: &mut App) -> Option<Row> {
        let choice = app.world().resource::<CharacterChoice>().clone();
        rows(&choice).get(draft(app).row).copied()
    }

    /// The list this screen navigates is never empty, which is what let three guards go.
    ///
    /// `navigate` used to carry `len().max(1)`, `min(count - 1)` and an `is_empty` on
    /// Escape. Each covered a list with no rows, and no such list decodes:
    /// `codec::character_list` refuses `max_characters == 0` and refuses a maximum
    /// smaller than the count it just sent, so either the account has characters to play
    /// or it has room to make one. Swept over the whole range a `u8` maximum allows,
    /// rather than spot-checked, because the interesting case is the boundary — an
    /// account exactly at its limit is the one with no creation row.
    #[test]
    fn every_list_the_contract_permits_offers_at_least_one_row() {
        for max in 1..=8u8 {
            for held in 0..=usize::from(max) {
                let characters: Vec<_> = (0..held)
                    .map(|index| character(index as u64 + 1, "Eivor"))
                    .collect();
                let choice = CharacterChoice::for_a_test(characters, max);
                let offered = rows(&choice);
                assert!(
                    !offered.is_empty(),
                    "{held} of at most {max} offered no row at all"
                );
                // The boundary, stated so a change to `has_room` cannot quietly make the
                // sweep pass for the wrong reason.
                assert_eq!(
                    offered.contains(&Row::Create),
                    held < usize::from(max),
                    "{held} of at most {max}"
                );
            }
        }
    }

    /// An account holding as many as the world allows is offered no creation. The server
    /// would refuse one — `CHARACTER_LIMIT_REACHED` — and a screen that offered it anyway
    /// would be inviting a refusal it had already been told about.
    #[test]
    fn an_account_at_its_limit_is_offered_no_creation() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![character(1, "Eivor"), character(2, "Sigrun")],
            2,
        ));

        let labels = row_labels(&mut app);
        assert_eq!(labels.len(), 2, "{labels:?}");
        assert!(
            !labels.iter().any(|label| label.contains("NEW")),
            "{labels:?}"
        );
    }

    /// The character this client played here last is the one the focus starts on, so the
    /// common case is one press. It is a preselection and not a decision: nothing has been
    /// sent, and the arrow keys move it like any other.
    #[test]
    fn the_character_played_here_last_is_the_one_focused() {
        let app = headless(
            CharacterChoice::for_a_test(vec![character(900, "Eivor"), character(7, "Sigrun")], 3)
                .preselecting(7),
        );

        assert_eq!(draft(&app).row, 1, "the second row is Sigrun's");
        assert!(asked(&app).is_empty(), "a preselection asked for something");
    }

    /// A remembered character that is no longer listed preselects nothing rather than a
    /// row that is not there.
    #[test]
    fn a_character_that_is_no_longer_there_preselects_the_first_row() {
        let app = headless(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3).preselecting(4242),
        );

        assert_eq!(draft(&app).row, 0);
    }

    /// **Pressing a row asks to play that character** — by the id the server minted,
    /// which is the one kind of identifier a client may echo back.
    #[test]
    fn pressing_a_row_asks_to_play_that_character() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor"), character(7, "Sigrun")],
            3,
        ));

        let world = app.world_mut();
        let mut query = world.query::<(Entity, &Row)>();
        let sigrun = query
            .iter(world)
            .find(|(_, row)| **row == Row::Play(7))
            .map(|(entity, _)| entity)
            .expect("a row for the second character");
        *world
            .get_mut::<Interaction>(sigrun)
            .expect("a row is a button") = Interaction::Pressed;
        app.update();

        assert_eq!(asked(&app), vec![ChooseCharacter::Play(7)]);
    }

    /// The keyboard reaches the same thing the pointer does: arrows move, Enter goes.
    #[test]
    fn the_arrow_keys_move_the_focus_and_enter_takes_the_row() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor"), character(7, "Sigrun")],
            3,
        ));

        press(&mut app, KeyCode::ArrowDown);
        assert_eq!(draft(&app).row, 1);
        press(&mut app, KeyCode::Enter);

        assert_eq!(asked(&app), vec![ChooseCharacter::Play(7)]);
    }

    /// An account with no characters here lands on the form, because a screen asking a
    /// question with one answer is a screen asking nothing.
    #[test]
    fn an_account_with_no_characters_starts_on_the_form() {
        let app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        assert_eq!(draft(&app).mode, Mode::Creating);
    }

    /// **A name and a face reach the server exactly as they were chosen.**
    ///
    /// The whole creation path in one test: type a name, move to a colour row, change it,
    /// and press the control. What is asserted is the *message* rather than the screen,
    /// because the message is what the server acts on.
    #[test]
    fn a_name_and_a_face_reach_the_server_as_they_were_chosen() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        assert_eq!(draft(&app).mode, Mode::Creating, "an empty account creates");

        type_text(&mut app, "Halvar");
        // Down to the skin row, and one step along it.
        press(&mut app, KeyCode::ArrowDown);
        press(&mut app, KeyCode::ArrowRight);
        let chosen = draft(&app);
        assert_eq!(chosen.focused(), Field::Colour(0));
        assert_eq!(chosen.colour[0], 3, "the skin moved one along its row");

        // Down to the control that asks, and press it.
        for _ in 0..6 {
            press(&mut app, KeyCode::ArrowDown);
        }
        assert_eq!(draft(&app).focused(), Field::Create);
        press(&mut app, KeyCode::Enter);

        let asked = asked(&app);
        assert_eq!(asked.len(), 1, "{asked:?}");
        let ChooseCharacter::Create { name, appearance } = &asked[0] else {
            panic!("the screen asked to play rather than to create: {asked:?}");
        };
        assert_eq!(name, "Halvar");
        assert_eq!(appearance.skin_color(), SKIN[3]);
        assert_eq!(appearance.shirt_color(), SHIRT[0]);
        assert_eq!(appearance.hair_model(), HairModel::ALL[1]);
    }

    /// **The name is sent as it was typed, including one the server will refuse.**
    ///
    /// Whether a name may be worn is the server's rule — it answers
    /// `CHARACTER_NAME_REFUSED`, which is a refusal with a reply — so a client that
    /// pre-judged it would be holding an opinion about a world it can only see part of.
    #[test]
    fn a_name_the_server_will_refuse_is_still_sent() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        type_text(&mut app, "   ");
        for _ in 0..7 {
            press(&mut app, KeyCode::ArrowDown);
        }
        press(&mut app, KeyCode::Enter);

        let asked = asked(&app);
        let ChooseCharacter::Create { name, .. } = &asked[0] else {
            panic!("{asked:?}");
        };
        assert_eq!(name, "   ", "the client trimmed or judged the name");
    }

    /// Control characters are the one thing the text field will not hold, and it is a
    /// bound on a *field* rather than a rule about names: a name carrying a newline is one
    /// that rewrites the line it is printed in, which is the server's own reason for
    /// refusing one.
    #[test]
    fn a_control_character_never_reaches_the_draft() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        type_text(&mut app, "Ei");
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::Enter,
            logical_key: Key::Character("\n".into()),
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        type_text(&mut app, "vor");

        assert_eq!(draft(&app).name, "Eivor");
    }

    /// Typing is heard only by the field it is for. A key pressed while the focus is on a
    /// colour row is not a letter waiting to arrive in the name three frames later.
    #[test]
    fn typing_reaches_the_name_only_while_it_is_focused() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        press(&mut app, KeyCode::ArrowDown);
        assert_eq!(draft(&app).focused(), Field::Colour(0));
        type_text(&mut app, "nope");
        assert_eq!(draft(&app).name, "");

        press(&mut app, KeyCode::ArrowUp);
        type_text(&mut app, "Eivor");
        assert_eq!(draft(&app).name, "Eivor");
    }

    /// **A second press asks for nothing more.**
    ///
    /// A welcome is the answer to a choice, so the server leaves the character phase the
    /// moment it takes one — and a second selection then arrives on a session that is in
    /// the world, where it is a protocol error that closes the connection.
    #[test]
    fn a_second_press_asks_for_nothing_more() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor")],
            3,
        ));

        press(&mut app, KeyCode::Enter);
        assert_eq!(asked(&app), vec![ChooseCharacter::Play(900)]);

        // Emptied so what follows is read against nothing rather than against the message
        // that has already gone out: a buffer holds a message for two frames.
        app.world_mut()
            .resource_mut::<Messages<ChooseCharacter>>()
            .clear();
        // What the network boundary leaves behind once it has sent one.
        app.world_mut().insert_resource(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3).already_answered(),
        );
        app.update();

        press(&mut app, KeyCode::Enter);
        let world = app.world_mut();
        let mut query = world.query::<(Entity, &Row)>();
        let row = query
            .iter(world)
            .next()
            .map(|(entity, _)| entity)
            .expect("a row to press");
        *world
            .get_mut::<Interaction>(row)
            .expect("a row is a button") = Interaction::Pressed;
        app.update();

        assert!(
            asked(&app).is_empty(),
            "the screen asked again after the exchange had been answered"
        );
    }

    /// The focused control is the one a player can see, in both halves of the screen.
    /// A keyboard that moves an invisible focus is a keyboard nobody can use.
    #[test]
    fn the_focused_control_is_the_one_with_the_bright_edge() {
        let mut app = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor"), character(7, "Sigrun")],
            3,
        ));
        press(&mut app, KeyCode::ArrowDown);

        let world = app.world_mut();
        let mut rows = world.query::<(&Row, &BorderColor)>();
        let bright: Vec<Row> = rows
            .iter(world)
            .filter(|(_, border)| **border == BorderColor::all(FOCUS_EDGE))
            .map(|(row, _)| *row)
            .collect();
        assert_eq!(bright, vec![Row::Play(7)], "exactly one row is focused");
    }

    /// **The preview is the rig, and it turns.**
    ///
    /// The whole of what replaced the flat swatch stack: entities carrying a mesh and a
    /// material under one parent that rotates about the vertical axis. Asserted on the
    /// parent rather than on a child, because the turn belongs to the body as a whole —
    /// and on the axis, because a rotation about any other one is a figure falling over.
    #[test]
    fn the_preview_is_a_body_that_turns_on_the_spot() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));
        app.update();

        let world = app.world_mut();
        let mut models = world.query_filtered::<&Children, With<PreviewModel>>();
        let parts: Vec<usize> = models.iter(world).map(|children| children.len()).collect();
        assert_eq!(parts.len(), 1, "one model stands on the screen");
        assert_eq!(
            parts[0],
            crate::player::BodyPiece::ALL.len(),
            "the model is drawn from every part of the rig"
        );

        let before = app.world().resource::<PreviewState>().turned;
        app.update();
        let after = app.world().resource::<PreviewState>().turned;
        assert!(
            after > before,
            "the model did not turn: {before} -> {after}"
        );

        let world = app.world_mut();
        let mut placed = world.query_filtered::<&Transform, With<PreviewModel>>();
        let turned = placed
            .iter(world)
            .next()
            .copied()
            .expect("the model is placed");
        let (axis, angle) = turned.rotation.to_axis_angle();
        assert!(angle > 0.0, "the transform carries no rotation");
        assert!(
            axis.abs_diff_eq(Vec3::Y, 1e-4),
            "the model turns about {axis:?} rather than the vertical"
        );
    }

    /// **The preview wears what is being chosen**, which is what makes it worth having at
    /// all: a player picking a shirt colour sees the shirt change.
    #[test]
    fn the_preview_wears_what_is_chosen() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        // Down to the shirt row and along it, so the colour under test is not the one a
        // fresh draft starts with.
        press(&mut app, KeyCode::ArrowDown);
        press(&mut app, KeyCode::ArrowDown);
        press(&mut app, KeyCode::ArrowRight);
        app.update();

        let expected = swatch_colour(SHIRT[draft(&app).colour[1]]);
        assert!(
            preview_colours(&mut app).contains(&expected),
            "the shirt colour a player just chose is not on the model"
        );
    }

    /// While a character is being *chosen* the preview is that character's, so the screen
    /// always shows whoever is about to go in.
    #[test]
    fn the_preview_shows_the_character_the_focus_is_on() {
        let worn = Appearance::new(
            SKIN[5],
            SHIRT[2],
            TROUSERS[2],
            SHOES[2],
            HairModel::Topknot,
            HAIR[5],
        )
        .expect("every colour is one this screen offers");
        let mut app = headless(CharacterChoice::for_a_test(
            vec![CharacterSummary {
                character_id: 900,
                name: "Eivor".to_owned(),
                appearance: worn,
            }],
            3,
        ));
        app.update();

        assert!(
            preview_colours(&mut app).contains(&swatch_colour(SKIN[5])),
            "the focused character's skin is not what the model is wearing"
        );
    }

    /// The model lives exactly as long as the screen does.
    ///
    /// Both ends of it: nothing before a character list arrives, and nothing left standing
    /// once the world has. A model that outlived the screen would be a figure turning in
    /// the middle of the world the player had just walked into.
    #[test]
    fn the_model_exists_only_while_the_screen_does() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_message::<ChooseCharacter>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(ConnectionState::Connecting)
            .add_plugins(CharacterUiPlugin);
        app.update();
        assert_eq!(models(&mut app), 0, "no list, no model");

        app.insert_resource(CharacterChoice::for_a_test(Vec::new(), 3));
        app.update();
        assert_eq!(
            models(&mut app),
            1,
            "the screen is up and nothing stands on it"
        );

        // The world arrives. The screen's own nodes go down with it, and so does this.
        app.insert_resource(a_session());
        app.update();
        assert_eq!(models(&mut app), 0, "the model outlived the screen");
    }

    fn models(app: &mut App) -> usize {
        let world = app.world_mut();
        let mut query = world.query_filtered::<Entity, With<PreviewModel>>();
        query.iter(world).count()
    }

    /// The camera wears the screen's flat backdrop while the screen is up, and gets the
    /// world's sky back when the world arrives.
    ///
    /// **Read from `Daylight::FIXED` rather than restated**, which is half the point of
    /// the assertion: a world with no clock and a client that has just left this screen
    /// have to agree about that colour, and they do it by reading one constant.
    #[test]
    fn the_backdrop_is_the_screens_while_the_screen_is_up() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        let camera = app.world_mut().spawn((WorldCamera, Camera::default())).id();
        app.update();
        assert_eq!(clear_colour(&app, camera), Some(BACKDROP));

        app.insert_resource(a_session());
        app.update();
        assert_eq!(clear_colour(&app, camera), Some(Daylight::FIXED.sky));
    }

    /// A sky somebody else is driving is left alone.
    ///
    /// **The half of the backdrop that had a bug in it**, found by the review of this pull
    /// request. Restoring `Daylight::FIXED.sky` whenever a session existed made this system
    /// overwrite `player::sky::drive_the_sky` on every frame of every world with a clock —
    /// so the day would never have turned. It now puts back only what it put there.
    #[test]
    fn a_sky_this_screen_did_not_set_is_left_alone() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        let camera = app.world_mut().spawn((WorldCamera, Camera::default())).id();
        app.update();
        assert_eq!(
            clear_colour(&app, camera),
            Some(BACKDROP),
            "the screen is up"
        );

        // The world arrives, and its clock paints a dusk nobody here chose.
        let dusk = Color::srgb(0.42, 0.21, 0.11);
        app.insert_resource(a_session());
        app.update();
        app.world_mut()
            .entity_mut(camera)
            .get_mut::<Camera>()
            .expect("the camera is still there")
            .clear_color = ClearColorConfig::Custom(dusk);

        for _ in 0..3 {
            app.update();
        }
        assert_eq!(
            clear_colour(&app, camera),
            Some(dusk),
            "the character screen repainted a sky it did not set"
        );
    }

    fn clear_colour(app: &App, camera: Entity) -> Option<Color> {
        match app.world().get::<Camera>(camera)?.clear_color {
            ClearColorConfig::Custom(colour) => Some(colour),
            _ => None,
        }
    }

    /// The model lands inside its stage, from a layout this test supplies by hand.
    ///
    /// **The coupling end to end**, and the test the review of this pull request earned:
    /// the placement reads `UiGlobalTransform` and `ComputedNode`, and reading the *other*
    /// transform — `GlobalTransform`, which `bevy_ui`'s layout does not write — put every
    /// stage at the origin and the model in the top-left corner of the screen. A test that
    /// only checked `world_point`'s arithmetic could not see that, because the arithmetic
    /// was right.
    ///
    /// The layout values are inserted rather than computed: `MinimalPlugins` runs no taffy,
    /// and what is under test is which components are read and what is done with them.
    #[test]
    fn the_model_lands_inside_the_stage_the_layout_gave_it() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        // A 1600x900 window with the stage's centre a quarter of the way in from the left
        // and halfway up: screen fraction (-0.5, 0).
        let window = app
            .world_mut()
            .spawn((
                Window {
                    resolution: bevy::window::WindowResolution::new(1600, 900),
                    ..default()
                },
                PrimaryWindow,
            ))
            .id();
        let _ = window;
        let camera = app
            .world_mut()
            .spawn((
                WorldCamera,
                Camera::default(),
                Projection::default(),
                Transform::default(),
                GlobalTransform::default(),
            ))
            .id();
        let _ = camera;

        let mut stage = app
            .world_mut()
            .query_filtered::<Entity, With<PreviewStage>>();
        let stage = stage
            .iter(app.world())
            .next()
            .expect("the screen reserves a stage");
        app.world_mut().entity_mut(stage).insert((
            ComputedNode {
                size: Vec2::new(200.0, 360.0),
                ..ComputedNode::DEFAULT
            },
            UiGlobalTransform::from_xy(400.0, 450.0),
        ));
        app.update();

        let world = app.world_mut();
        let mut placed = world.query_filtered::<&Transform, With<PreviewModel>>();
        let model = placed
            .iter(world)
            .next()
            .copied()
            .expect("the model is placed");

        // Left of centre, because the stage is: a camera looks along -Z, so its right is
        // +X and a stage at screen fraction -0.5 puts the model at negative x.
        assert!(
            model.translation.x < -0.1,
            "the stage is left of centre and the model is at {}",
            model.translation.x
        );
        assert!(
            (model.translation.z + PREVIEW_DISTANCE).abs() < 1e-4,
            "the model is not {PREVIEW_DISTANCE} in front of the camera: {}",
            model.translation.z
        );

        // And it is scaled to the stage's share of the window height, not to the window.
        let half_height = PREVIEW_DISTANCE * (std::f32::consts::FRAC_PI_4 / 2.0).tan();
        let expected = (360.0 / 900.0 * half_height * 2.0) / preview_frame().size.y;
        assert!(
            (model.scale.x - expected).abs() < 1e-4,
            "scaled to {} where the stage asks for {expected}",
            model.scale.x
        );
    }

    /// The stage and the model agree about where on the screen the figure is.
    ///
    /// **The coupling the issue named as the risk**, tested as arithmetic rather than
    /// through a window: a point at the centre of the view is straight ahead, and a point
    /// at the right edge is `aspect` half-heights to the right of that. Getting the aspect
    /// term wrong is a model that drifts out of its stage the moment somebody resizes.
    #[test]
    fn a_point_on_the_screen_lands_where_the_frustum_says() {
        let camera = GlobalTransform::from(Transform::default());
        let fov = std::f32::consts::FRAC_PI_4;
        let distance = 3.0;
        let half_height = distance * (fov / 2.0).tan();

        // Straight ahead is straight ahead, whatever the aspect.
        for aspect in [0.5, 1.0, 2.0] {
            let middle = world_point(&camera, fov, aspect, Vec2::ZERO, distance);
            assert!(
                middle.abs_diff_eq(Vec3::new(0.0, 0.0, -distance), 1e-5),
                "the middle of the view at aspect {aspect} is {middle:?}"
            );
        }

        // The top edge is one half-height up, and the aspect does not touch the vertical —
        // which is the whole reason the model is scaled off the vertical.
        let top = world_point(&camera, fov, 2.0, Vec2::new(0.0, 1.0), distance);
        assert!((top.y - half_height).abs() < 1e-5, "{top:?}");

        // The right edge is `aspect` half-heights across. A camera looks along -Z, so its
        // right is +X.
        let right = world_point(&camera, fov, 2.0, Vec2::new(1.0, 0.0), distance);
        assert!((right.x - half_height * 2.0).abs() < 1e-5, "{right:?}");
    }

    /// Every colour this screen offers is one the contract allows, which is what makes
    /// the offer an offer rather than a refusal waiting to happen.
    ///
    /// The compiler already checks each entry through [`worn`]; this checks the *rows*,
    /// so a palette added without going through that function is caught too.
    #[test]
    fn every_colour_this_screen_offers_is_one_the_contract_allows() {
        for palette in &PALETTES {
            assert!(
                !palette.colours.is_empty(),
                "{} offers nothing",
                palette.label
            );
            for colour in palette.colours {
                assert!(
                    Appearance::new(
                        *colour,
                        *colour,
                        *colour,
                        *colour,
                        HairModel::Shaved,
                        *colour
                    )
                    .is_ok(),
                    "{} offers {colour:#08x}, which the contract forbids",
                    palette.label
                );
            }
        }
    }

    /// Every hair model is offered, so a model the contract carries is not one no player
    /// can pick.
    #[test]
    fn every_hair_model_this_contract_carries_is_offered() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        let mut seen = Vec::new();
        for _ in 0..HairModel::ALL.len() {
            let model = HairModel::ALL[draft(&app).hair];
            assert!(!seen.contains(&model), "the ring repeats before it ends");
            seen.push(model);
            // Down to the hair row from the name, then along it.
            let steps = FIELDS
                .iter()
                .position(|field| *field == Field::Hair)
                .expect("the hair model is a field");
            let from = draft(&app).field;
            for _ in from..steps {
                press(&mut app, KeyCode::ArrowDown);
            }
            press(&mut app, KeyCode::ArrowRight);
        }
        assert_eq!(seen.len(), HairModel::ALL.len());
    }

    /// The draft a player starts on is the character the panel says it is: the indexes
    /// and [`STARTING_APPEARANCE`] describe one person, not two.
    #[test]
    fn the_starting_draft_is_the_starting_appearance() {
        assert_eq!(Draft::default().appearance(), STARTING_APPEARANCE);
    }

    /// The screen is up exactly while an exchange is live — and it is not a screen a
    /// player can dismiss, because a session that has been sent a character list is
    /// waiting for one.
    #[test]
    fn the_screen_is_up_exactly_while_a_choice_is_pending() {
        assert!(!character_is_up(None));
        assert!(character_is_up(Some(&CharacterChoice::for_a_test(
            Vec::new(),
            3
        ))));
    }

    // -----------------------------------------------------------------------
    // The launch that names somebody
    // -----------------------------------------------------------------------

    /// **`--name Eivor` plays Eivor**, which is the sentence that used to be the
    /// server's and is now a request like any other.
    #[test]
    fn a_launch_that_names_a_listed_character_plays_it() {
        let app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor"), character(7, "Sigrun")], 3),
            "Eivor",
        );

        assert_eq!(asked(&app), vec![ChooseCharacter::Play(900)]);
    }

    /// A name this account holds nobody under is a creation, wearing the starting
    /// appearance: a command line names a person, not a face.
    #[test]
    fn a_launch_naming_nobody_listed_creates_that_character() {
        let app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3),
            "Sigrun",
        );

        assert_eq!(
            asked(&app),
            vec![ChooseCharacter::Create {
                name: "Sigrun".to_owned(),
                appearance: STARTING_APPEARANCE,
            }]
        );
    }

    /// A full roster holding nobody by that name has nothing to ask for. The server
    /// would answer `CHARACTER_LIMIT_REACHED` and end the session, so the screen stays
    /// up and a person chooses — which is the whole of what this path degrades to.
    #[test]
    fn a_full_roster_with_no_such_name_leaves_the_screen_up() {
        let mut app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor"), character(7, "Sigrun")], 2),
            "Bjorn",
        );

        assert_eq!(asked(&app), vec![], "nothing to ask for");
        let labels = row_labels(&mut app);
        assert_eq!(
            labels,
            vec!["Eivor", "Sigrun"],
            "and the screen is up: {labels:?}"
        );
    }

    /// A launch that named nobody is every player's launch, and it waits.
    #[test]
    fn a_launch_that_names_nobody_waits_for_the_player() {
        let app = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor")],
            3,
        ));

        assert_eq!(asked(&app), vec![], "nothing was asked for");
    }

    /// **Once per exchange, however many frames it takes.**
    ///
    /// The screen is up for as long as the server takes to answer, and this system runs
    /// every frame of it. A second `SelectCharacterRequest` after a welcome is a protocol
    /// error that ends the session, so asking twice would be worse than not asking.
    #[test]
    fn the_launch_asks_once_however_long_the_server_takes() {
        let mut app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3),
            "Eivor",
        );

        assert_eq!(asked(&app), vec![ChooseCharacter::Play(900)]);
        app.world_mut()
            .resource_mut::<Messages<ChooseCharacter>>()
            .clear();

        for frame in 0..4 {
            app.update();
            assert_eq!(asked(&app), vec![], "asked again on frame {frame}");
        }
    }

    /// A choice the boundary has already sent is not asked for again either — the same
    /// rule from the other direction, and the one that holds when this system runs a
    /// frame after the send rather than before it.
    #[test]
    fn a_choice_already_on_the_wire_is_not_repeated() {
        let app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3).already_answered(),
            "Eivor",
        );

        assert_eq!(asked(&app), vec![]);
    }

    /// **A launch flag does not answer the screen a player asked to be on.**
    ///
    /// #184 made leaving a world land back on its character screen. `--name` answering
    /// that exchange too would send the player straight back in — a control that cannot be
    /// used. The line is a [`Session`] having existed, not the exchange number, which is
    /// why a pre-session exchange and a return from a world are not the same thing.
    #[test]
    fn the_launch_does_not_answer_the_screen_a_player_left_a_world_for() {
        let mut app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3),
            "Eivor",
        );
        assert_eq!(asked(&app), vec![ChooseCharacter::Play(900)]);

        // The world arrives, and then the player leaves it.
        app.insert_resource(a_session());
        app.update();
        app.world_mut().remove_resource::<Session>();
        app.world_mut().remove_resource::<CharacterChoice>();
        app.update();
        app.world_mut()
            .resource_mut::<Messages<ChooseCharacter>>()
            .clear();

        app.world_mut().insert_resource(CharacterChoice::for_a_test(
            vec![character(900, "Eivor")],
            3,
        ));
        app.update();

        assert_eq!(
            asked(&app),
            vec![],
            "the launch answered the screen the player had just asked to be on"
        );
    }

    /// A wholly new exchange gets its own launch answer. The automatic reconnect after
    /// a name refusal deliberately keeps `CharacterChoice` present, so it does not reset
    /// this local guard and cannot repeat the same refused creation in a loop.
    #[test]
    fn a_second_exchange_is_answered_like_the_first() {
        let mut app = headless_playing_as(
            CharacterChoice::for_a_test(vec![character(900, "Eivor")], 3),
            "Eivor",
        );
        assert_eq!(asked(&app), vec![ChooseCharacter::Play(900)]);

        // The exchange ends the way every one of them does: the resource goes.
        app.world_mut().remove_resource::<CharacterChoice>();
        app.update();
        app.world_mut()
            .resource_mut::<Messages<ChooseCharacter>>()
            .clear();

        app.world_mut().insert_resource(CharacterChoice::for_a_test(
            vec![character(900, "Eivor")],
            3,
        ));
        app.update();

        assert_eq!(asked(&app), vec![ChooseCharacter::Play(900)]);
    }

    // -----------------------------------------------------------------------
    // What the review of #161 found
    // -----------------------------------------------------------------------

    /// The `display` of the two halves, which is what decides whether a half occupies
    /// the panel. `Visibility` does not: `bevy_ui` lays a hidden node out exactly as it
    /// lays out a visible one.
    fn halves(app: &mut App) -> (Display, Display) {
        let world = app.world_mut();
        let mut choosing = world.query_filtered::<&Node, With<ChoosingPanel>>();
        let list = choosing
            .iter(world)
            .next()
            .expect("the choosing half exists")
            .display;
        let mut creating = world.query_filtered::<&Node, With<CreatingPanel>>();
        let form = creating
            .iter(world)
            .next()
            .expect("the creating half exists")
            .display;
        (list, form)
    }

    /// **The half that is down leaves the layout, not just the screen.**
    ///
    /// The two halves are flex siblings that both grow, so a half switched off with
    /// `Visibility` went on claiming its share of the row: the list drew into half the
    /// panel with an empty column beside it, and the form did the same in reverse. Only
    /// `Display::None` takes a node out of taffy. Asserted on the component because a
    /// headless app has no layout to measure — which is exactly why nothing caught it.
    #[test]
    fn the_half_that_is_down_is_out_of_the_layout() {
        let mut choosing = headless(CharacterChoice::for_a_test(
            vec![character(900, "Eivor")],
            3,
        ));
        assert_eq!(halves(&mut choosing), (Display::Flex, Display::None));

        // An account with nothing here opens on the form, which is the other half up.
        let mut creating = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        assert_eq!(halves(&mut creating), (Display::None, Display::Flex));
    }

    /// A retryable name refusal leaves the creation half present and draws the server's
    /// sentence beside the field that can remedy it.
    ///
    /// Both halves matter: a test that asserted only the text could pass with that text
    /// hidden in the list panel, and a test that asserted only the form could pass while
    /// leaving the player no explanation for why nothing was created.
    #[test]
    fn a_name_refusal_keeps_the_form_and_its_message_together() {
        let reason = "CHARACTER_NAME_TAKEN: a character on this world already has that name; \
                      choose another";
        let mut app =
            headless(CharacterChoice::for_a_test(Vec::new(), 3).after_creation_refusal(reason));

        assert_eq!(halves(&mut app), (Display::None, Display::Flex));
        assert_eq!(draft(&app).mode, Mode::Creating);
        assert_eq!(name_refusal(&mut app), reason);
    }

    /// **The rows belong to the exchange that drew them.**
    ///
    /// `Row::Play` carries an id and nothing else — not the name, not the face, not the
    /// limit — and character ids are minted per world, so the next server's list can be
    /// identical row for row and describe somebody else entirely. The screen used to
    /// compare the two and keep the first server's rows: you would stand at the gate of
    /// one world reading a name from another.
    #[test]
    fn the_rows_belong_to_the_exchange_that_drew_them() {
        let mut app = headless(CharacterChoice::for_a_test(vec![character(1, "Eivor")], 3));
        assert_eq!(row_labels(&mut app)[0], "Eivor");

        // The exchange ends the way all of them do, and the next one lists the same id.
        app.world_mut().remove_resource::<CharacterChoice>();
        app.update();
        app.world_mut()
            .insert_resource(CharacterChoice::for_a_test(vec![character(1, "Bjorn")], 3));
        app.update();

        assert_eq!(
            row_labels(&mut app)[0],
            "Bjorn",
            "the row is the previous server's"
        );
    }

    /// **The field stops at the byte the server stops at.**
    ///
    /// It counted characters, which is inside any limit you like and twice over the
    /// server's as soon as the characters are CJK or emoji — and the refusal that earns
    /// is a `ServerReject`, which by contract ends the session. A field that offers to
    /// hold a name the server cannot is a field that costs a player their connection.
    #[test]
    fn the_name_field_stops_at_the_bytes_the_server_accepts() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));

        // Three bytes each, forty of them: 120 bytes offered against a 64-byte limit.
        type_text(&mut app, &"名".repeat(40));

        let name = draft(&app).name;
        assert!(
            name.len() <= NAME_LIMIT_BYTES,
            "{} bytes reached the draft",
            name.len()
        );
        assert_eq!(
            name.chars().count(),
            NAME_LIMIT_BYTES / 3,
            "the character that would cross the limit is refused whole, never split"
        );
        assert!(
            name.chars().all(|c| c == '名'),
            "a character was split: {name}"
        );
    }
}
