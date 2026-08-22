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
//! **A refused creation closes the connection, and that is the contract rather than a
//! shortcoming here.** `schemas/handshake.fbs` answers one with `ServerReject`, which
//! ends the session — so the sentence a player reads is on the screen they land on
//! afterwards, with the server's own words in it. What this screen keeps is the draft:
//! the name and the colours survive, so coming back is a click rather than a redo.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;

use super::{BUTTON, button_colour};
use crate::net::{Appearance, CharacterChoice, ChooseCharacter, HairModel};

use crate::player::{BodyPart, PlacedBox, body_boxes, body_envelope, body_slots, placed_box};

pub(super) struct CharacterUiPlugin;

impl Plugin for CharacterUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Draft>()
            .init_resource::<PlayAs>()
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
/// that earns is a `ServerReject` — which by contract ends the session, so the player is
/// dropped back to the server list for a name the field offered to hold. A name this
/// accepts can still be refused; that refusal is the server's to make, and it should not
/// be one this screen composed on purpose.
const NAME_LIMIT_BYTES: usize = 64;

/// What the player has chosen so far.
///
/// It deliberately **outlives one exchange**: a creation the server refuses closes the
/// connection, and a player who comes back to type the same six choices again would be
/// paying for a rule they have already been told about. Only the focus is re-derived,
/// and only when a new list arrives.
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

/// The panel the preview is drawn on.
///
/// Marked because the boxes being its **children** is load-bearing rather than
/// incidental: see [`a_box_behind_the_body_still_draws_over_the_panel`].
#[derive(Component)]
struct PreviewFrame;

/// One box of the drawn body: which part it belongs to, and which of that part's boxes
/// it is.
///
/// A pool rather than a node per box that exists: the hair is drawn from between one box
/// and four depending on the model, and a screen that spawned and despawned nodes as a
/// player cycled the choice would be rebuilding a layout on a key press. The spare nodes
/// of a smaller model draw nothing instead.
#[derive(Component, Debug, Clone, Copy)]
struct PreviewBox {
    part: BodyPart,
    index: usize,
}

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

/// How wide the preview panel is drawn, in logical pixels. Its height is not a constant:
/// it comes from the rig, through [`preview_frame`], so a notch is the same length across
/// the panel as it is up it and the proportions on screen are the body's.
const PREVIEW_WIDTH: f32 = 96.0;

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
type HairText = (With<HairLabel>, Without<NameField>);

type StatusText = (
    With<CharacterStatus>,
    Without<NameField>,
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
                ..default()
            },
            BackgroundColor(Color::srgba(0.012, 0.016, 0.024, 0.98)),
            Visibility::Hidden,
            GlobalZIndex(CHARACTER_LAYER),
        ))
        .with_children(|overlay| {
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
                            spawn_preview(body);
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

/// The body, drawn from the same boxes the world's own body is built from.
///
/// Flat nodes rather than a mesh, which is the choice `ui/icon.rs` already made for
/// items: a second camera and a render target would cost a texture per frame and put the
/// result out of reach of a headless test, where a handful of nodes is components a test
/// can read. What keeps it honest is that the *boxes* come from `player::appearance` —
/// the same table `player::part_mesh` builds meshes from, seen head-on with the depth
/// thrown away.
fn spawn_preview(parent: &mut ChildSpawnerCommands<'_>) {
    let frame = preview_frame();
    // Square notches: the panel is as much taller than it is wide as the rig is.
    let height = PREVIEW_WIDTH * frame.size.y / frame.size.x;

    parent
        .spawn(Node {
            width: Val::Px(PREVIEW_WIDTH + 24.0),
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|column| {
            column
                .spawn((
                    PreviewFrame,
                    Node {
                        width: Val::Px(PREVIEW_WIDTH),
                        height: Val::Px(height),
                        position_type: PositionType::Relative,
                        border_radius: BorderRadius::all(Val::Px(4.0)),
                        overflow: Overflow::clip(),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.03, 0.035, 0.045)),
                ))
                .with_children(|box_| {
                    // A fixed pool, sized by the widest model each part has. Which node
                    // covers which is decided per refresh by `ZIndex`, not by the order
                    // they are spawned in: a curtain of hair falls *behind* the shoulders
                    // and a cap sits over the crown, and the box's own depth is the only
                    // thing that knows the difference.
                    for part in BodyPart::IN_DRAWING_ORDER {
                        for index in 0..body_slots(part) {
                            box_.spawn((
                                PreviewBox { part, index },
                                Node {
                                    position_type: PositionType::Absolute,
                                    ..default()
                                },
                                BackgroundColor(Color::NONE),
                                ZIndex(0),
                            ));
                        }
                    }
                });
        });
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
fn answer_from_the_launch(
    choice: Option<Res<CharacterChoice>>,
    play_as: Res<PlayAs>,
    mut chosen: MessageWriter<ChooseCharacter>,
    mut asked: Local<bool>,
) {
    let Some(choice) = choice else {
        // The exchange is over — established, refused or disconnected. The next one is a
        // new question and gets a new answer.
        *asked = false;
        return;
    };
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

    mut previews: Query<(&PreviewBox, &mut Node, &mut BackgroundColor, &mut ZIndex)>,
    mut name: Query<&mut Text, With<NameField>>,
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

    // The body, drawn from the draft while one is being made and from the focused
    // character while one is being chosen — so the preview is always of whoever is about
    // to go in.
    let worn = match (draft.mode, focused_row) {
        (Mode::Choosing, Some(Row::Play(id))) => choice
            .characters()
            .iter()
            .find(|character| character.character_id == id)
            .map_or_else(|| draft.appearance(), |character| character.appearance),
        _ => draft.appearance(),
    };
    // One frame and one hair model for the whole pass, rather than one of each per node.
    let frame = preview_frame();
    let low = frame.centre - frame.size / 2.0;
    let model = worn.hair_model();

    for (slot, mut node, mut colour, mut depth) in &mut previews {
        let Some(cell) = body_boxes(slot.part, model).get(slot.index) else {
            // A model drawn from fewer boxes than the pool holds. The spare nodes keep
            // whatever layout they had and simply draw nothing.
            if colour.0 != Color::NONE {
                colour.0 = Color::NONE;
            }
            continue;
        };

        let box_ = placed_box(slot.part, *cell);
        // The four fields written as one value and compared as one, because `Mut<Node>`
        // marks the component changed on the *first* `DerefMut` and Bevy lays a changed
        // node's subtree out again. This system runs on every key press while somebody is
        // typing a name, and none of those move a box.
        let next = Node {
            position_type: PositionType::Absolute,
            left: Val::Percent((box_.centre.x - box_.size.x / 2.0 - low.x) / frame.size.x * 100.0),
            bottom: Val::Percent(
                (box_.centre.y - box_.size.y / 2.0 - low.y) / frame.size.y * 100.0,
            ),
            width: Val::Percent(box_.size.x / frame.size.x * 100.0),
            height: Val::Percent(box_.size.y / frame.size.y * 100.0),
            ..default()
        };
        if *node != next {
            *node = next;
        }

        let worn_colour = swatch_colour(slot.part.colour(worn));
        if colour.0 != worn_colour {
            colour.0 = worn_colour;
        }

        // The painter's algorithm a flat projection needs and a depth buffer would not:
        // what faces the viewer is drawn over what is behind it. Millimetres, because
        // `ZIndex` is an integer and the smallest gap in the rig is half a notch.
        //
        // **A box behind the body gets a negative rank, and that does not hide it behind
        // the panel.** `ZIndex` sorts a node among its *siblings* only: Bevy pushes a
        // parent onto the UI stack before any of its children, so every box draws over the
        // frame whatever its rank. That is a property of the tree rather than of the
        // number, and it is pinned as one — see
        // [`a_box_behind_the_body_still_draws_over_the_panel`].
        let rank = ZIndex((box_.nearness() * 1000.0).round() as i32);
        if *depth != rank {
            *depth = rank;
        }
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
/// **It does not read [`ConnectionState`], and used to.** An arm returned the refusal
/// reason for `Rejected`, under a comment conceding it was reached "for a refusal that
/// did not end this exchange, which today means none". There is no such refusal: every
/// reject on this screen closes the connection, `ui::status` renders the reason on the
/// screen that follows, and `a_rejection_shows_the_servers_reason_verbatim` is what
/// holds that promise. An arm that cannot run is not a defence — it is a second place a
/// refusal could be rendered, quietly disagreeing with the first.
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
    use super::*;
    // The screen no longer reads the connection state — see `describe`. The tests still
    // insert it, because the plugin is built under the state the game builds it under.
    use crate::net::{CharacterSummary, ConnectionState};

    /// Builds the screen headlessly. `MinimalPlugins` has no renderer, so the nodes are
    /// spawned and updated but never drawn — which is exactly the part worth asserting,
    /// and it needs no display.
    fn headless(choice: CharacterChoice) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ChooseCharacter>()
            .init_resource::<ButtonInput<KeyCode>>()
            .insert_resource(ConnectionState::Choosing)
            .insert_resource(choice)
            .add_plugins(CharacterUiPlugin);
        app.update();
        app
    }

    /// The same, launched with `--name` naming somebody.
    fn headless_playing_as(choice: CharacterChoice, name: &str) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
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

    /// A box behind the body still draws over the panel it is drawn on.
    ///
    /// **The review of this pull request read a negative `ZIndex` as "behind the panel
    /// background", and it is not.** The geometry half of that reading was right — the
    /// Loose model's curtain is the box furthest back, it ranks at -175, and it genuinely
    /// does peek out beside the neck between the shoulders and the jaw, so a bug there
    /// would be visible. What makes it safe is structural: `ZIndex` sorts a node among its
    /// **siblings**, and `bevy_ui::stack::update_uistack_recursive` pushes a parent onto
    /// the UI stack before any of its children, so a child is drawn after its parent
    /// whatever its rank.
    ///
    /// So this test does not assert a number. It asserts the two facts the safety rests
    /// on — that a box really does sit behind the body, and that every box is a child of
    /// the node carrying the panel's background — because the numeric reading is the one
    /// somebody will arrive at again, and a tree that stopped being that shape would break
    /// the preview silently.
    #[test]
    fn a_box_behind_the_body_still_draws_over_the_panel() {
        let behind = body_boxes(BodyPart::Hair, HairModel::Loose)
            .iter()
            .map(|cell| placed_box(BodyPart::Hair, *cell))
            .filter(|box_| box_.nearness() < 0.0)
            .count();
        assert!(
            behind > 0,
            "the Loose model is the one whose curtain falls behind the body"
        );

        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        let world = app.world_mut();

        let mut frames = world.query_filtered::<Entity, With<PreviewFrame>>();
        let panels: Vec<Entity> = frames.iter(world).collect();
        assert_eq!(panels.len(), 1, "one panel holds the preview");

        let mut drawn = world.query_filtered::<Entity, With<PreviewBox>>();
        let boxes: Vec<Entity> = drawn.iter(world).collect();
        assert!(!boxes.is_empty(), "the preview draws boxes");

        let mut children = world.query::<&Children>();
        let held: Vec<Entity> = children
            .get(world, panels[0])
            .expect("the panel has children")
            .iter()
            .collect();
        for box_ in boxes {
            assert!(
                held.contains(&box_),
                "a preview box is not a child of the panel it draws on, so its ZIndex is                  no longer sorted against its siblings alone"
            );
        }
    }

    /// **The preview wears what is being chosen**, part by part — which is what makes it
    /// worth having at all: a player picking a shirt colour sees the shirt change.
    #[test]
    fn the_preview_wears_what_is_chosen() {
        let mut app = headless(CharacterChoice::for_a_test(Vec::new(), 3));
        // Down to the shirt row and along it, so the colour under test is not the one a
        // fresh draft starts with.
        press(&mut app, KeyCode::ArrowDown);
        press(&mut app, KeyCode::ArrowDown);
        press(&mut app, KeyCode::ArrowRight);
        let expected = swatch_colour(SHIRT[draft(&app).colour[1]]);

        let world = app.world_mut();
        let mut parts = world.query::<(&PreviewBox, &BackgroundColor)>();
        let shirt = parts
            .iter(world)
            .find(|(slot, _)| slot.part == BodyPart::Shirt)
            .map(|(_, colour)| colour.0)
            .expect("the body is drawn from boxes");
        assert_eq!(shirt, expected);
    }

    /// While a character is being *chosen* the preview is that character's, so the panel
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

        let world = app.world_mut();
        let mut parts = world.query::<(&PreviewBox, &BackgroundColor)>();
        let skin = parts
            .iter(world)
            .find(|(slot, _)| slot.part == BodyPart::Skin)
            .map(|(_, colour)| colour.0)
            .expect("the body is drawn from boxes");
        assert_eq!(skin, swatch_colour(SKIN[5]));
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

    /// The next exchange gets its own answer: a refused creation ends the session, and
    /// reconnecting asks the same question again.
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
