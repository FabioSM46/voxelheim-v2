//! The client's on-screen surface and the input mode that owns the pointer.

mod character;
mod chat;
mod compass;
mod crosshair;
mod experience;
mod health;
mod hunger;

mod hotbar;
/// **`pub(crate)` since #418, and only so one test can reach it.** The four surfaces that
/// draw an item now agree by sharing a `Handle<Image>`, and the test that asserts that has to
/// read a real `ImageNode` off a real cell in the same app as the hand and the drop — which
/// puts it in `player`, on the other side of this boundary. Nothing outside the tests calls
/// into here; the drawing is still reached through `stack_style` and `refresh_cell_contents`.
pub(crate) mod icon;
mod inventory;
mod leaving;
mod login;
mod loot;
mod map;
mod menu;
mod party;
mod servers;
mod settings;
mod status;
mod storm;
mod text_input;
mod vendor;

use bevy::prelude::*;
use bevy::ui::FocusPolicy;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use icon::StackIcon;

/// The character a launch named, for `main.rs` to hand to the character screen.
pub use character::PlayAs;

use crate::net::{
    CharacterChoice, ChooseCharacter, ConnectRequest, ConnectionState, DisconnectRequest,
    InventoryStack, ReconnectRequest, RefreshServerList, ServerList, Session, SignInRequest,
    SignInState,
};

use crate::player::{
    ApplyInputMode, ApplySnapshots, CraftClick, InputMode, InventoryClick, Liveries, SelfVitals,
    ViewMode, item_linear_rgba, item_livery, item_shape,
};
use crate::settings::{Control, Settings};

#[cfg(test)]
use crate::world::palette;
use settings::SettingsScreen;

/// One square in either inventory view, in logical pixels.
pub(super) const CELL_SIZE: f32 = 52.0;

/// Border thickness shared by hotbar and inventory cells.
pub(super) const CELL_BORDER: f32 = 3.0;

/// Empty cells remain visible without pretending they contain a coloured item.
pub(super) const EMPTY_CELL: Color = Color::srgba(0.055, 0.065, 0.080, 0.94);

/// A cell with something in it: a plate for the picture to sit on, not the item's colour
/// spread flat across the square.
///
/// It used to *be* the item's colour, which is what made eleven items into eleven shades
/// of the same rectangle. Now the colour is in the icon, and this is a shade darker than
/// [`EMPTY_CELL`] so a full slot still reads as full at a glance — from the plate rather
/// than from the swatch.
pub(super) const FILLED_CELL: Color = Color::srgba(0.028, 0.034, 0.044, 0.96);

/// The plate the count sits on, so a two- or three-digit number stays readable over
/// whatever the icon happens to have drawn under it. It is sized by the text, so it
/// appears and grows with the number rather than being a fixed box.
pub(super) const COUNT_PLATE: Color = Color::srgba(0.0, 0.0, 0.0, 0.62);

/// The count's size, one number for both grids: it is a corner label over a picture now
/// rather than the only thing in the square, and two sizes would only be two numbers to
/// keep in step.
pub(super) const COUNT_FONT_SIZE: f32 = 16.0;

/// The ordinary cell border.
pub(super) const CELL_EDGE: Color = Color::srgb(0.30, 0.33, 0.38);

/// The selected hotbar cell, and the slot a picked-up stack came from.
///
/// Amber against a dark plate, which is a wider gap than it had when a filled cell *was*
/// the item's swatch: the selection is now the only saturated thing on a cell's edge, and
/// the picture inside it never reaches the border.
pub(super) const SELECTED_EDGE: Color = Color::srgb(1.0, 0.72, 0.25);

/// The rest colour of anything this client offers as pressable: menu entries, server
/// rows, character rows, the form's fields, a recipe.
///
/// One triple and one [`button_colour`] rather than a copy per screen. There were four
/// copies of these three values and five copies of the match below — the crafting rows
/// carried the fourth under names of their own, which is exactly the shape the problem
/// takes: a retheme edits what it can find, and the screen it misses is the one whose
/// buttons stop matching the others.
pub(super) const BUTTON: Color = Color::srgb(0.16, 0.18, 0.22);

/// Hovered: lighter, so the pointer says which row it is on before anything is pressed.
pub(super) const BUTTON_HOVERED: Color = Color::srgb(0.25, 0.29, 0.35);

/// Held down. Amber rather than lighter still, because a press should read as a
/// different thing from a hover rather than as more of one.
pub(super) const BUTTON_PRESSED: Color = Color::srgb(0.42, 0.31, 0.15);

/// The tab a strip currently has selected.
///
/// Its own state rather than one of the three [`button_colour`] answers: a selected tab is
/// not a hovered tab and not a pressed one, and a palette with three interactions has no arm
/// for it. It sits here rather than in the file that grew the first strip because there are
/// two strips now — `ui/inventory.rs`'s and `ui/settings.rs`'s — and a second copy is exactly
/// the shape the paragraph above describes.
pub(super) const TAB_SELECTED: Color = Color::srgb(0.20, 0.24, 0.30);

/// What a button wears for the interaction it is in.
///
/// Total over [`Interaction`] with no wildcard arm, so a fourth interaction state is a
/// build failure here rather than a screen quietly rendering it as at rest.
///
/// A control with a state of its own does not get an arm here — it decides that state
/// first and asks this only for the ordinary three. The unaffordable recipe row is the
/// one that does.
pub(super) const fn button_colour(interaction: &Interaction) -> Color {
    match interaction {
        Interaction::Pressed => BUTTON_PRESSED,
        Interaction::Hovered => BUTTON_HOVERED,
        Interaction::None => BUTTON,
    }
}

/// The complete player-facing UI.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // The producers also register these messages. `add_message` is idempotent, and
        // doing it here keeps every UI module headlessly testable on its own.
        app.init_resource::<InputMode>()
            // The player plugin owns these two in the game; initialising them here keeps
            // every UI module headlessly testable on its own. `ViewMode` is here because
            // `InputGate` reads it, and a `SystemParam` whose resource is missing takes
            // the app down rather than reading a default.
            .init_resource::<SelfVitals>()
            .init_resource::<ViewMode>()
            .add_message::<InventoryClick>()
            .add_message::<CraftClick>()
            .add_message::<crate::player::LootTakeClick>()
            .add_message::<crate::player::VendorTradeClick>()
            .add_message::<DisconnectRequest>()
            // Registered here as well as by `net::SignInPlugin`, which is not built
            // when no account service is configured. `add_message` is idempotent,
            // and this is what keeps the login screen headlessly testable on its
            // own — the same reason the four above are here.
            .add_message::<SignInRequest>()
            // The same reasoning for the server list's three: `net::NetPlugin` registers
            // `ConnectRequest` and `ReconnectRequest`, `net::ServerListPlugin` registers
            // `RefreshServerList`, and neither is built in a headless UI test.
            .add_message::<ConnectRequest>()
            .add_message::<ReconnectRequest>()
            .add_message::<RefreshServerList>()
            .add_message::<AppExit>()
            .add_message::<ChooseCharacter>()
            .add_plugins((
                character::CharacterUiPlugin,
                chat::ChatUiPlugin,
                // Nested rather than flat, because `add_plugins` accepts a tuple of at
                // most fifteen and this list is at that ceiling. A tuple of plugins is
                // itself `Plugins`, so the nesting changes nothing but the shape.
                (
                    storm::StormUiPlugin,
                    compass::CompassUiPlugin,
                    crosshair::CrosshairPlugin,
                ),
                // Nested for the reason the compass and the crosshair are: the tuple is
                // at `add_plugins`' fifteen-plugin ceiling. The leave countdown is beside
                // health because it is the same kind of surface -- permanent game UI a
                // release build keeps drawing -- and it borrows that module's z-ordering.
                (health::HealthUiPlugin, leaving::LeavingUiPlugin),
                hunger::HungerUiPlugin,
                experience::ExperienceUiPlugin,
                hotbar::HotbarPlugin,
                inventory::InventoryUiPlugin,
                login::LoginPlugin,
                // Nested for the reason the compass and the crosshair are, one entry
                // above: the tuple is at `add_plugins`' fifteen-plugin ceiling.
                (loot::LootUiPlugin, map::MapUiPlugin, vendor::VendorUiPlugin),
                menu::MenuPlugin,
                party::PartyUiPlugin,
                servers::ServerListUiPlugin,
                settings::SettingsScreenPlugin,
                status::StatusUiPlugin,
            ));

        add_input_mode_systems(app);
    }
}

/// Registers the two systems that own the input mode, with the ordering they require.
///
/// Split out of `UiPlugin::build` so a test can install exactly these constraints on a
/// headless app without also building six UI panels. What is under test is *when*
/// `choose_input_mode` runs relative to the snapshots it reads, and a registration
/// copied into a test would not test that at all — it would test the copy.
fn add_input_mode_systems(app: &mut App) {
    app.add_systems(
        Update,
        (
            // After the snapshots, because this system reads the life state they
            // publish. Without it, the frame a player dies on can decide the input
            // mode from the vitals of the frame before — and leave the pack open on
            // top of a death overlay for one frame, or refuse `E` for one frame
            // after a respawn. Presentation either way, since the server refuses
            // what a dead player asks for, but a gate that reads yesterday's answer
            // is not the gate this module documents.
            choose_input_mode
                .in_set(ApplyInputMode)
                .after(ApplySnapshots),
            sync_cursor.after(ApplyInputMode),
        ),
    );
}

/// The two surfaces that own the keyboard without owning the mode.
///
/// The settings screen sits inside `Menu` and the map's note field sits inside `Map`, so
/// neither is a mode of its own -- and both need a key pressed over them to stop meaning what
/// it means everywhere else. Grouped for the reason [`Overlays`] is: the list only grows, and
/// the alternative is a signature that grows with it.
///
/// **They are not the same exception, and the difference is the whole of why there are two
/// methods.** The settings screen takes every key, because the press that rebinds a control
/// must not also fire the control it is taken from. The note field takes exactly one -- the
/// `Escape` that discards it -- and deliberately lets `Control::Map` through, so a window a
/// player has pressed `M` to leave still leaves.
#[derive(bevy::ecs::system::SystemParam)]
struct Typing<'w> {
    settings: Option<Res<'w, SettingsScreen>>,
    marker_form: Option<Res<'w, map::MarkerForm>>,
}

impl Typing<'_> {
    /// Whether the settings screen is up and every key belongs to it.
    fn settings_own_every_key(&self) -> bool {
        self.settings
            .as_deref()
            .is_some_and(SettingsScreen::is_open)
    }

    /// Whether a text field is up and `Escape` belongs to it.
    fn a_field_owns_escape(&self) -> bool {
        self.marker_form
            .as_deref()
            .is_some_and(map::MarkerForm::is_open)
    }
}

/// Every resource that decides which screen owns the pointer, as one parameter.
///
/// Grouped rather than listed, for the reason `net::Inboxes` is: there is one of these
/// per overlay and the list only grows. What it buys beyond a shorter signature is that
/// the two systems below ask the *same* question — a screen that took the pointer in one
/// and not the other would be a control nobody can press, or a click that also reached
/// the world.
#[derive(bevy::ecs::system::SystemParam)]
struct Overlays<'w> {
    sign_in: Option<Res<'w, SignInState>>,
    list: Option<Res<'w, ServerList>>,
    choice: Option<Res<'w, CharacterChoice>>,
    state: Option<Res<'w, ConnectionState>>,
}

impl Overlays<'_> {
    /// Whether any full-screen overlay is up. While one is, this frame's input is not
    /// for the world.
    fn any_is_up(&self) -> bool {
        login::login_is_up(self.sign_in.as_deref())
            || character::character_is_up(self.choice.as_deref())
            || servers::server_list_is_up(
                self.list.as_deref(),
                self.state.as_deref(),
                self.sign_in.as_deref(),
            )
    }

    /// Whether the sign-in or the connection moved this frame, which is what takes a
    /// player out of a pause menu they never opened.
    fn moved(&self) -> bool {
        self.sign_in
            .as_ref()
            .is_some_and(|sign_in| sign_in.is_changed())
            || self.state.as_ref().is_some_and(|state| state.is_changed())
    }

    /// Whether this is a live session.
    fn connected(&self) -> bool {
        self.state
            .as_deref()
            .is_some_and(|state| *state == ConnectionState::Connected)
    }
}

/// `E` owns the inventory toggle and `Esc` owns the pause menu.
///
/// **Death takes the inventory and leaves the menu.** Quitting and disconnecting must
/// never depend on being alive, so `Esc` keeps working exactly as it did; the pack is
/// game input the server would refuse anyway, and an open one on top of a death overlay is
/// a screen nobody can read. Closing one that is already open is the same rule rather than
/// a second one — and both are presentation, since the server owns every outcome a click
/// in there could ask for.
fn choose_input_mode(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    session: Option<Res<Session>>,
    overlays: Overlays<'_>,
    vitals: Res<SelfVitals>,
    settings: Option<Res<Settings>>,
    typing: Typing<'_>,
    mut mode: ResMut<InputMode>,
) {
    // **A full-screen overlay owns the input while one is up.** The game is running
    // behind them, so a click meant for a control would otherwise also reach the world as
    // a mining or attack intent. `Menu` is the mode that already means "this frame's
    // input is not for the world", so this reuses the gate rather than adding a second
    // one — and `Escape` cannot leave it, because none of the three is dismissible: the
    // login screen has no "not now", the server list is where a client with no session
    // belongs, and a session that has been sent a character list is waiting for one.
    if overlays.any_is_up() {
        set_mode(&mut mode, InputMode::Menu);
        return;
    }
    // The frame either comes down, the player is playing rather than paused: they
    // never opened the pause menu, and leaving them in it would be this client
    // inventing a press they did not make.
    if overlays.moved() {
        set_mode(&mut mode, InputMode::Playing);
    }

    let Some(_session) = session else {
        set_mode(&mut mode, InputMode::Playing);
        return;
    };

    if vitals.dead()
        && matches!(
            *mode,
            InputMode::Inventory | InputMode::Loot | InputMode::Vendor | InputMode::Map
        )
    {
        set_mode(&mut mode, InputMode::Playing);
    }

    let Some(keys) = keys else {
        return;
    };

    // **The settings screen owns the keyboard while it is up.** It sits inside `Menu`, so
    // the mode is already right; what it needs is for no key to mean two things at once —
    // the press that closes it must not also resume play, and the press that rebinds a
    // control must not also fire the control it is taken from. `ui/settings.rs` runs after
    // this system for the same reason.
    if typing.settings_own_every_key() {
        return;
    }

    // Text entry owns Enter, Escape and every printable key until it closes itself.
    // Returning here is what prevents the Escape used to discard a line from opening
    // the menu in the same frame.
    if *mode == InputMode::Chat {
        return;
    }

    // The bindings, or the defaults for an app built without them — which are `Escape` and
    // `E`, the two literals that stood here until this screen existed.
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());

    // **The map's note field owns `Escape` while it is up, and nothing else.** It is chat's
    // exception, narrowed: chat is a whole mode and this is one field inside one, so only the
    // one key it answers is taken. `Control::Map` deliberately still closes the map over an
    // open form, discarding the note -- a window a player has pressed `M` to leave should
    // leave, and the note was never sent anywhere.
    //
    // **`Escape` the key, and not `Control::Menu` the action.** The field answers the key --
    // `ui/text_input.rs` reads the logical `Escape` and nothing in it reads a binding -- so
    // the collision this guard exists to prevent is only ever there while the pause menu
    // sits on its default key. Swallowing the *action* instead would hand a player who
    // moved the menu to `F1` a key that does nothing whatever over an open form: the menu
    // declines to open, and the field, which was never listening for `F1`, does not cancel
    // either. `Control::Menu` on any other key is therefore let through, and takes the same
    // route `Control::Map` does -- the mode leaves `Map`, and `follow_input_mode` discards
    // the draft with the window it belonged to.
    if keys.just_pressed(bindings.key(Control::Menu)) {
        if typing.a_field_owns_escape() && bindings.key(Control::Menu) == KeyCode::Escape {
            return;
        }
        let next = match *mode {
            InputMode::Menu | InputMode::Loot | InputMode::Vendor | InputMode::Map => {
                InputMode::Playing
            }
            InputMode::Playing | InputMode::Chat | InputMode::Inventory => InputMode::Menu,
        };
        set_mode(&mut mode, next);
        return;
    }

    if keys.just_pressed(bindings.key(Control::Chat)) && *mode == InputMode::Playing {
        set_mode(&mut mode, InputMode::Chat);
        return;
    }

    if keys.just_pressed(bindings.key(Control::Inventory)) {
        if vitals.dead() {
            return;
        }
        let next = match *mode {
            InputMode::Playing => InputMode::Inventory,
            InputMode::Inventory => InputMode::Playing,
            InputMode::Loot => return,
            InputMode::Vendor => return,
            InputMode::Chat => return,
            InputMode::Menu => return,
            InputMode::Map => return,
        };
        set_mode(&mut mode, next);
        return;
    }

    // The map is the inventory's rule with a different key, deliberately and not by
    // coincidence: it is a full-screen overlay over a live session that the server would
    // refuse every action from while dead, so it opens from play, closes onto play, is
    // ignored while another screen owns the keyboard, and is forced shut by death. The
    // one thing it does not do is replace another overlay — pressing `M` over the pack
    // does nothing, exactly as pressing `E` over the loot window does.
    if keys.just_pressed(bindings.key(Control::Map)) {
        if vitals.dead() {
            return;
        }
        let next = match *mode {
            InputMode::Playing => InputMode::Map,
            InputMode::Map => InputMode::Playing,
            InputMode::Inventory => return,
            InputMode::Loot => return,
            InputMode::Vendor => return,
            InputMode::Chat => return,
            InputMode::Menu => return,
        };
        set_mode(&mut mode, next);
    }
}

/// Captures and hides the pointer only for a live playing session.
fn sync_cursor(
    mode: Res<InputMode>,
    overlays: Overlays<'_>,
    mut cursors: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    // The pointer belongs to whatever is on top, and while an overlay is up that is a
    // button. A locked, invisible cursor over a screen whose whole content is controls is
    // a screen nobody can press. The overlay test is redundant with the `Connected` one
    // beside it — none of the three is up on a live session — and it is asked anyway,
    // because "the pointer is released for every overlay" should be readable here rather
    // than inferred from a state machine somewhere else.
    let playing = matches!(*mode, InputMode::Playing | InputMode::Chat)
        && !overlays.any_is_up()
        && overlays.connected();
    let (grab_mode, visible) = if playing {
        // Bevy falls back to Confined on X11, where Locked is unsupported.
        (CursorGrabMode::Locked, false)
    } else {
        (CursorGrabMode::None, true)
    };

    for mut cursor in &mut cursors {
        if cursor.grab_mode != grab_mode {
            cursor.grab_mode = grab_mode;
        }
        if cursor.visible != visible {
            cursor.visible = visible;
        }
    }
}

pub(super) fn set_mode(mode: &mut ResMut<'_, InputMode>, next: InputMode) {
    if **mode != next {
        **mode = next;
    }
}

/// How one authoritative slot is drawn: the plate, the picture on it, and the count over
/// both.
///
/// The one description both grids go through, which is what keeps the pack and the hotbar
/// from being two opinions about the same slot.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StackStyle {
    /// The cell's own fill: [`EMPTY_CELL`] or [`FILLED_CELL`], never the item's colour.
    pub(super) background: Color,
    /// What to draw inside, or `None` for an empty cell.
    pub(super) icon: Option<StackIcon>,
    /// The count label. Empty for an empty cell, which is also what hides its plate.
    pub(super) count: String,
}

/// Everything a cell needs to know about one authoritative slot.
///
/// **Both halves of the picture come from the same registry row the hand is built from** —
/// [`item_shape`] and [`item_linear_rgba`] — rather than from a second opinion held on this
/// side. That is the rule the colour already followed, extended to the whole entry: a stack
/// cannot be a sword in the hand and a square in the pack, because there is one table and
/// it is read twice.
///
/// A cell used to be the swatch itself. Eight palette entries across eleven items meant
/// collisions by construction — the sharpening stone shares stone's swatch, the tent shares
/// snow's — and the shape is what tells those apart now.
pub(super) fn stack_style(stack: Option<InventoryStack>) -> StackStyle {
    let Some(stack) = stack.filter(|stack| stack.item_id != 0 && stack.count != 0) else {
        return StackStyle {
            background: EMPTY_CELL,
            icon: None,
            count: String::new(),
        };
    };
    let [r, g, b, a] = item_linear_rgba(stack.item_id);
    StackStyle {
        background: FILLED_CELL,
        icon: Some(StackIcon {
            shape: item_shape(stack.item_id),
            colour: Color::linear_rgba(r, g, b, a),
            livery: item_livery(stack.item_id),
        }),
        count: stack.count.to_string(),
    }
}

pub(super) fn cell_node() -> Node {
    Node {
        width: Val::Px(CELL_SIZE),
        height: Val::Px(CELL_SIZE),
        border: UiRect::all(Val::Px(CELL_BORDER)),
        border_radius: BorderRadius::all(Val::Px(4.0)),
        align_items: AlignItems::Center,
        justify_content: JustifyContent::Center,
        ..default()
    }
}

/// The two children every cell has, in the order they stack: the picture, then the count
/// over it.
///
/// Shared by both grids because a cell is one thing drawn in two places. It also moves the
/// count *off* the cell entity, where it used to live as a bare [`Text`]: a node that is
/// itself a text block is no place to hang a picture, and a corner label is what stays
/// readable over one anyway.
pub(super) fn spawn_cell_contents(cell: &mut ChildSpawnerCommands<'_>) {
    cell.spawn(icon::host_bundle());
    cell.spawn(count_bundle());
}

/// The count label: bottom-right, over the picture, on a plate of its own.
///
/// `FocusPolicy::Pass` for the same reason the icon host carries it — a node with no
/// policy blocks, and a label lying over its own cell would quietly take the pointer away
/// from it.
fn count_bundle() -> impl Bundle {
    (
        SlotCount,
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(2.0),
            bottom: Val::Px(1.0),
            padding: UiRect::axes(Val::Px(3.0), Val::Px(0.0)),
            border_radius: BorderRadius::all(Val::Px(3.0)),
            ..default()
        },
        // Transparent until there is a count to plate, so an empty cell shows no box.
        BackgroundColor(Color::NONE),
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(COUNT_FONT_SIZE),
            ..default()
        },
        TextColor(Color::WHITE),
        TextShadow::default(),
        FocusPolicy::Pass,
    )
}

/// The count label inside one cell. Found through the cell's own children, so the two
/// grids can share the marker without either one's refresh reaching into the other's.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlotCount;

/// Writes one slot's style into the children of its cell.
///
/// The two grids refresh on different triggers and draw different borders, but what goes
/// *inside* a cell is one job — so it is written once here rather than twice, which is
/// what stops the pack and the hotbar drifting into two answers for one stack.
pub(super) fn refresh_cell_contents(
    commands: &mut Commands<'_, '_>,
    children: &Children,
    style: &StackStyle,
    counts: &mut Query<(&mut Text, &mut BackgroundColor), With<SlotCount>>,
    icons: &mut Query<&mut icon::DrawnIcon>,
    liveries: Option<&Liveries>,
) {
    for child in children {
        if let Ok((mut text, mut plate)) = counts.get_mut(*child) {
            if text.0 != style.count {
                text.0.clone_from(&style.count);
            }
            let next = if style.count.is_empty() {
                Color::NONE
            } else {
                COUNT_PLATE
            };
            if plate.0 != next {
                plate.0 = next;
            }
        }
        if let Ok(drawn) = icons.get_mut(*child) {
            icon::redraw(commands, *child, drawn, style.icon, liveries);
        }
    }
}

/// What one cell ended up drawing, for the tests in both grids.
///
/// It walks the two children a cell has rather than reading components off the cell
/// itself, which is the point: the count and the picture live *inside* a cell now, and a
/// test that still read the cell entity would pass while the screen showed nothing.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DrawnCell {
    /// The label on the count's own node.
    pub(crate) count: String,
    /// The count's plate, which is transparent when there is no count to plate.
    pub(crate) plate: Color,
    /// The rectangles the picture is made of, each with the shade it was drawn in and the
    /// turn it was drawn at. Empty for an empty cell.
    pub(crate) rectangles: Vec<(Node, UiTransform, Color)>,
}

#[cfg(test)]
pub(crate) fn drawn_cell(world: &World, cell: Entity) -> DrawnCell {
    let mut drawn = DrawnCell {
        count: String::new(),
        plate: Color::NONE,
        rectangles: Vec::new(),
    };
    let Some(children) = world.get::<Children>(cell) else {
        return drawn;
    };
    for child in children.iter() {
        if world.get::<SlotCount>(child).is_some() {
            if let Some(text) = world.get::<Text>(child) {
                drawn.count.clone_from(&text.0);
            }
            if let Some(plate) = world.get::<BackgroundColor>(child) {
                drawn.plate = plate.0;
            }
        }
        if world.get::<icon::IconHost>(child).is_none() {
            continue;
        }
        let parts: Vec<Entity> = world
            .get::<Children>(child)
            .map(|parts| parts.iter().collect())
            .unwrap_or_default();
        for part in parts {
            let (Some(node), Some(transform), Some(colour)) = (
                world.get::<Node>(part),
                world.get::<UiTransform>(part),
                world.get::<BackgroundColor>(part),
            ) else {
                continue;
            };
            drawn.rectangles.push((node.clone(), *transform, colour.0));
        }
    }
    drawn
}

/// The tooltip's surface and text. Darker than the panel it floats over, inside the same
/// grey the cells are bordered with, so it reads as a label on top rather than a third
/// panel.
pub(super) const TOOLTIP_BACKGROUND: Color = Color::srgba(0.020, 0.026, 0.036, 0.97);
pub(super) const TOOLTIP_TEXT: Color = Color::srgb(0.92, 0.94, 0.97);

/// How far from the pointer a tooltip sits, in logical pixels. Enough that the cursor
/// glyph never covers the first letter.
pub(super) const TOOLTIP_GAP: f32 = 14.0;

/// The one tooltip node a screen hangs off its own root, tagged with whatever component
/// that screen finds it by.
///
/// **One node per screen, never one per hovered thing.** Accumulation is then not a rule
/// anybody has to remember: the system that shows a tooltip rewrites this node's text and
/// moves it, and there is nothing to despawn.
///
/// `GlobalZIndex(31)` puts it over the overlays this client draws, which are all at 30 --
/// the whole point of a tooltip. `FocusPolicy::Pass` is the trap that comes with that: a
/// node with no policy **blocks**, so a tooltip the pointer ever landed inside would
/// capture the interaction, whatever is under it would fall to `Interaction::None`, and the
/// tooltip would hide and reappear every other frame. [`TOOLTIP_GAP`] keeps the pointer
/// outside it today; `Pass` is what stops that being load-bearing.
///
/// `Visibility::Hidden` to start, and the shower puts it back to `Inherited` rather than
/// `Visible`: the overlay above it owns whether the screen is on at all, and a tooltip must
/// not survive it being closed.
pub(super) fn tooltip_bundle(tag: impl Component) -> impl Bundle {
    (
        tag,
        Node {
            position_type: PositionType::Absolute,
            padding: UiRect::axes(Val::Px(9.0), Val::Px(5.0)),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(TOOLTIP_BACKGROUND),
        BorderColor::all(CELL_EDGE),
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..default()
        },
        TextColor(TOOLTIP_TEXT),
        TextShadow::default(),
        Visibility::Hidden,
        GlobalZIndex(31),
        FocusPolicy::Pass,
    )
}

/// Where the absolutely positioned tooltip is pinned, for one pointer position.
///
/// Two of the four are `Auto`: an absolutely positioned node is anchored by the edges that
/// are not.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct TooltipAnchor {
    pub(super) left: Val,
    pub(super) right: Val,
    pub(super) top: Val,
    pub(super) bottom: Val,
}

impl TooltipAnchor {
    /// Writes this anchor onto a node, and answers whether anything moved.
    ///
    /// The write is guarded because a tooltip that reassigns four `Val`s every frame marks
    /// its `Node` as changed every frame, which is a layout pass for a label nobody moved.
    pub(super) fn apply_to(self, node: &mut Node) -> bool {
        if node.left == self.left
            && node.right == self.right
            && node.top == self.top
            && node.bottom == self.bottom
        {
            return false;
        }
        (node.left, node.right, node.top, node.bottom) =
            (self.left, self.right, self.top, self.bottom);
        true
    }
}

/// Anchors the tooltip to the pointer, away from whichever window edge is nearer.
///
/// **Anchored rather than clamped, because the width is not known here.** A node's size is
/// decided by layout, one frame after this runs, so a clamp against the right edge would
/// have to guess how wide the word is and would clip whenever it guessed low. Pinning the
/// *right* edge of the tooltip instead makes it grow leftwards, away from the edge it is
/// near, and the same argument in the other axis keeps it off the bottom of the window. No
/// measurement, and no way to be clipped.
pub(super) fn anchor_for(cursor: Vec2, window: Vec2) -> TooltipAnchor {
    let (left, right) = if cursor.x * 2.0 <= window.x {
        (Val::Px(cursor.x + TOOLTIP_GAP), Val::Auto)
    } else {
        (
            Val::Auto,
            Val::Px((window.x - cursor.x).max(0.0) + TOOLTIP_GAP),
        )
    };
    let (top, bottom) = if cursor.y * 2.0 <= window.y {
        (Val::Px(cursor.y + TOOLTIP_GAP), Val::Auto)
    } else {
        (
            Val::Auto,
            Val::Px((window.y - cursor.y).max(0.0) + TOOLTIP_GAP),
        )
    };
    TooltipAnchor {
        left,
        right,
        top,
        bottom,
    }
}

/// Where the pointer is and how big the window is, or `None` when there is neither.
///
/// `None` while the pointer is outside the window, and no window at all in a headless test:
/// in both a tooltip keeps the position it had, because there is nothing newer to move it
/// to.
pub(super) fn pointer_in_window(
    windows: &Query<'_, '_, &Window, With<PrimaryWindow>>,
) -> Option<(Vec2, Vec2)> {
    let window = windows.iter().next()?;
    let cursor = window.cursor_position()?;
    Some((cursor, Vec2::new(window.width(), window.height())))
}

/// The one rule that keeps every string this client composes drawable.
///
/// Bevy's `default_font` is the whole font stack here: `FiraMono-subset.ttf`, embedded in
/// `bevy_text`, whose `cmap` holds exactly 95 glyphs — every printable ASCII codepoint and
/// nothing else. A codepoint the font does not have is not drawn as a box and not logged as
/// a warning; it is laid out with **zero advance**, so the string on the screen is simply
/// shorter than the string in the source and nothing anywhere says so. That is how six
/// characters — `°` `·` `—` `…` `♛` `⚔` — reached twenty-one on-screen sites across eleven
/// modules without anybody seeing a hole, from the compass onward (#481): every test
/// compared a formatted string against the same literal, so the tests agreed with the
/// source, and neither knew what the font could draw.
#[cfg(test)]
mod ascii_guard {
    use std::path::{Path, PathBuf};

    /// **Every string and character literal the production build compiles is ASCII.**
    ///
    /// The scan is the whole crate rather than a list of the modules that draw, for the
    /// reason a list kept in step with a directory by hand falls behind it. Text reaches
    /// the screen from further away than `ui/`: the level on a name plate is composed in
    /// `player/`, the field of view in `settings/`, and the line under the login control is
    /// a `tls::ConnectError` message written in `net/`. A rule with no exceptions needs
    /// nothing kept current.
    ///
    /// It reads literals rather than rendered text on purpose. What a screen shows depends
    /// on the state a test drives it into, and several of the twenty-one were reachable only
    /// from a state no test visited — a party leader who is offline, a corpse window, a
    /// certificate that was substituted.
    ///
    /// It reads the character a literal *produces*, not the bytes it is written with, so
    /// `"\u{265b}"` fails exactly as a pasted `♛` does. Both compile to the same codepoint
    /// and the font lays both out with zero advance; a guard that could be stepped around
    /// by spelling the crown differently would be a habit rather than a rule.
    ///
    /// Excluded: `src/gen/` (flatc output, which is never hand-edited) and `tests.rs`
    /// (test code, which may legitimately spell a hostile name in a script this font
    /// cannot draw, and does).
    #[test]
    fn every_string_the_client_composes_is_ascii() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        collect_sources(&root, &mut sources);
        assert!(
            sources.len() > 20,
            "the walk found {} files under {}, which is not this crate",
            sources.len(),
            root.display()
        );

        let mut holes = Vec::new();
        for path in &sources {
            let source = std::fs::read_to_string(path).expect("a source file this crate compiles");
            for (line, character) in non_ascii_in_literals(&source) {
                holes.push(format!(
                    "{}:{line}: U+{:04X} `{character}`",
                    path.strip_prefix(&root).unwrap_or(path).display(),
                    character as u32,
                ));
            }
        }
        assert!(
            holes.is_empty(),
            "these characters are absent from the only font this client has, so each would \
             be laid out with zero advance and drawn as nothing:\n  {}\n\
             Spell them in ASCII, or draw them as `bevy_ui` nodes the way `ui/party.rs` \
             draws the crown and the crossed swords.",
            holes.join("\n  ")
        );
    }

    /// Every `.rs` file the guard reads: this crate's own source, minus generated code and
    /// minus whole test files.
    fn collect_sources(directory: &Path, into: &mut Vec<PathBuf>) {
        let entries = std::fs::read_dir(directory).expect("a readable source directory");
        for entry in entries {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "gen") {
                    continue;
                }
                collect_sources(&path, into);
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.ends_with(".rs") && name != "tests.rs" {
                into.push(path);
            }
        }
        into.sort();
    }

    /// Which part of the source one character belongs to.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Region {
        Code,
        Comment,
        Literal,
        /// The backslash that opens an escape sequence inside a literal.
        ///
        /// It is told apart from the rest of the literal for one reason: `"\u{265b}"` is
        /// pure ASCII in the source and a crown in the compiled string, so a scan that
        /// only reads source characters would pass it — and the character it produces is
        /// laid out with zero advance exactly as a pasted `♛` would be. Knowing where an
        /// escape genuinely *starts* is also the only way to tell that apart from
        /// `"\\u{:04x}"`, the format string `net/json.rs` builds a JSON escape with,
        /// where the `u{` follows a doubled backslash and opens nothing.
        Escape,
    }

    /// The scalar a `\u{…}` escape beginning at `start` produces, if one begins there.
    ///
    /// `None` covers every other escape (`\n`, `\\`, `\x41`, `\"`), all of which are ASCII
    /// by Rust's own rules — `\x` is refused above `0x7F` in a string literal — and covers
    /// an escape the compiler would reject too, which is not this test's to report.
    fn unicode_escape(text: &[char], start: usize) -> Option<char> {
        if text.get(start + 1).copied()? != 'u' || text.get(start + 2).copied()? != '{' {
            return None;
        }
        let mut digits = String::new();
        for character in text.iter().copied().skip(start + 3) {
            match character {
                '}' => {
                    return u32::from_str_radix(&digits, 16)
                        .ok()
                        .and_then(char::from_u32);
                }
                // Rust allows `\u{2_6_5_b}`, and it means the same thing.
                '_' => {}
                _ if character.is_ascii_hexdigit() => digits.push(character),
                _ => return None,
            }
        }
        None
    }

    /// Where `source` puts a non-ASCII character inside a string or character literal the
    /// production build compiles, as `(line number, character)`.
    ///
    /// Two spellings reach the same place and both are read: the character written as
    /// itself, and the character written as a `\u{…}` escape. The escape is the one a
    /// source-only scan would miss, and missing it would make this guard exactly as good
    /// as the habit it exists to replace.
    ///
    /// Comments are skipped, because prose about the code is not drawn by it — this module
    /// is full of em dashes and every one of them is fine. `#[cfg(test)]` items are skipped
    /// for the same reason: a test may name a hostile string deliberately, and several do.
    fn non_ascii_in_literals(source: &str) -> Vec<(usize, char)> {
        let text: Vec<char> = source.chars().collect();
        let region = classify(&text);
        let excluded = test_only(&text, &region);

        let mut line = 1;
        let mut found = Vec::new();
        for (index, character) in text.iter().copied().enumerate() {
            if character == '\n' {
                line += 1;
                continue;
            }
            if excluded[index] {
                continue;
            }
            match region[index] {
                Region::Literal if !character.is_ascii() => found.push((line, character)),
                Region::Escape => {
                    if let Some(escaped) = unicode_escape(&text, index)
                        && !escaped.is_ascii()
                    {
                        found.push((line, escaped));
                    }
                }
                _ => {}
            }
        }
        found
    }

    /// One pass over the source deciding, for each character, whether it is code, comment
    /// or the inside of a literal.
    ///
    /// The three kinds of literal Rust spells differently all have to be here rather than
    /// in a regular expression over lines: a `"` inside a comment opens nothing, a `'`
    /// inside a string is not a character literal, and `r#"…"#` ends only at its own
    /// hash count.
    ///
    /// Escaped literals get [`Region::Escape`] on the opening backslash, which is what
    /// lets a later pass read `\u{…}`. A raw string never gets one, because a raw string
    /// processes no escapes: `r"\u{265b}"` really is nine ASCII characters.
    fn classify(text: &[char]) -> Vec<Region> {
        let mut region = vec![Region::Code; text.len()];
        let at = |index: usize| text.get(index).copied().unwrap_or('\0');
        /// Classifies one character, ignoring an index one past the end — which an
        /// escape at the very end of a truncated file would reach.
        fn mark(region: &mut [Region], index: usize, kind: Region) {
            if let Some(slot) = region.get_mut(index) {
                *slot = kind;
            }
        }
        let mut index = 0;
        while index < text.len() {
            match at(index) {
                '/' if at(index + 1) == '/' => {
                    while index < text.len() && at(index) != '\n' {
                        mark(&mut region, index, Region::Comment);
                        index += 1;
                    }
                }
                '/' if at(index + 1) == '*' => {
                    let mut depth = 0usize;
                    while index < text.len() {
                        if at(index) == '/' && at(index + 1) == '*' {
                            depth += 1;
                        } else if at(index) == '*' && at(index + 1) == '/' {
                            depth -= 1;
                            mark(&mut region, index, Region::Comment);
                            mark(&mut region, index + 1, Region::Comment);
                            index += 2;
                            if depth == 0 {
                                break;
                            }
                            continue;
                        }
                        mark(&mut region, index, Region::Comment);
                        index += 1;
                    }
                }
                'r' if at(index + 1) == '"' || at(index + 1) == '#' => {
                    // A raw string, or an identifier that happens to start with `r`.
                    let hashes = (1..).take_while(|step| at(index + step) == '#').count();
                    if at(index + 1 + hashes) != '"' {
                        index += 1;
                        continue;
                    }
                    index += hashes + 2;
                    while index < text.len() {
                        if at(index) == '"' && (1..=hashes).all(|step| at(index + step) == '#') {
                            index += hashes + 1;
                            break;
                        }
                        mark(&mut region, index, Region::Literal);
                        index += 1;
                    }
                }
                quote @ ('"' | '\'') => {
                    // A single quote is a character literal only when a close quote follows
                    // one character or one escape. `'a` in `&'a str` and `'_` in
                    // `Mut<'_, T>` are lifetimes, and neither opens anything.
                    if quote == '\'' && at(index + 1) != '\\' && at(index + 2) != '\'' {
                        index += 1;
                        continue;
                    }
                    index += 1;
                    while index < text.len() {
                        match at(index) {
                            '\\' => {
                                mark(&mut region, index, Region::Escape);
                                mark(&mut region, index + 1, Region::Literal);
                                index += 2;
                            }
                            character if character == quote => {
                                index += 1;
                                break;
                            }
                            _ => {
                                mark(&mut region, index, Region::Literal);
                                index += 1;
                            }
                        }
                    }
                }
                _ => index += 1,
            }
        }
        region
    }

    /// Which characters belong to a `#[cfg(test)]` item.
    ///
    /// The attribute runs either to the `;` that ends a `use` or a `mod tests;`, or to the
    /// `}` that closes a block. It reads the classification rather than the raw text
    /// because a brace inside a string or a comment is not a brace.
    fn test_only(text: &[char], region: &[Region]) -> Vec<bool> {
        let attribute: Vec<char> = "#[cfg(test)]".chars().collect();
        let at = |index: usize| text.get(index).copied().unwrap_or('\0');
        let mut excluded = vec![false; text.len()];
        let mut index = 0;
        while index < text.len() {
            let here = region[index] == Region::Code
                && attribute
                    .iter()
                    .enumerate()
                    .all(|(step, wanted)| at(index + step) == *wanted);
            if !here {
                index += 1;
                continue;
            }
            let mut cursor = index + attribute.len();
            let mut depth = 0usize;
            while cursor < text.len() {
                excluded[cursor] = true;
                if region[cursor] == Region::Code {
                    match at(cursor) {
                        '{' => depth += 1,
                        '}' if depth > 0 => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        ';' if depth == 0 => break,
                        _ => {}
                    }
                }
                cursor += 1;
            }
            index = cursor.max(index + 1);
        }
        excluded
    }

    #[test]
    fn the_scan_reads_literals_and_nothing_else() {
        // Two hashes on the outside, because the fixture below contains a raw string
        // of its own and `"#` would close this one at it.
        let source = r##"
//! A doc comment with — an em dash in it.
/* a block /* nested */ comment with … in it */
const A: &str = "plain";
const B: &str = "hole·here";
const C: char = '…';
fn f<'a>(x: &'a str) -> &'a str { x }
const D: &str = "a quote ' and a slash \\ inside";
const E: &str = r#"raw " with ° in it"#;
#[cfg(test)]
const F: &str = "tests may say ♛";
#[cfg(test)]
mod tests {
    const G: &str = "and ⚔ in here too";
}
const H: &str = "back in production ‽";
const I: &str = "an escape \u{265b} is ASCII in the source and a crown on the screen";
const J: &str = "\\u{265b} follows a doubled backslash and escapes nothing";
const K: &str = "\u{41} is just an A";
const L: &str = r"\u{265b} in a raw string is nine plain characters";
#[cfg(test)]
const M: char = '\u{2026}';
"##;
        assert_eq!(
            non_ascii_in_literals(source),
            vec![(5, '·'), (6, '…'), (9, '°'), (16, '‽'), (17, '♛')],
        );
    }

    /// The spelling a scan over source characters alone would have let through.
    ///
    /// `"\u{265b}"` is twelve ASCII characters in the file and one crown in the compiled
    /// string, and the font draws that crown with zero advance exactly as it draws a pasted
    /// one. The three cases below are the ones that make the difference readable: an escape
    /// is decoded, a doubled backslash opens no escape, and a raw string has no escapes at
    /// all.
    #[test]
    fn a_unicode_escape_is_read_as_the_character_it_produces() {
        assert_eq!(
            non_ascii_in_literals(r#"const A: &str = "\u{2014}";"#),
            vec![(1, '—')],
        );
        assert_eq!(
            non_ascii_in_literals(r#"const B: char = '\u{2_6_5_b}';"#),
            vec![(1, '♛')],
        );
        // An escape that produces ASCII is not a hole, and neither is one that is not an
        // escape at all.
        assert_eq!(
            non_ascii_in_literals(r#"const C: &str = "\u{7f}";"#),
            vec![]
        );
        assert_eq!(
            non_ascii_in_literals(r#"const D: &str = "\\u{2014}";"#),
            vec![],
        );
        assert_eq!(
            non_ascii_in_literals(r##"const E: &str = r"\u{2014}";"##),
            vec![],
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bevy::input::ButtonState;
    use bevy::input::InputPlugin;
    use bevy::input::keyboard::{Key, KeyboardInput};
    use bevy::input::mouse::MouseMotion;
    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::net::{
        LifeState, Outbound, PlayerVitals, ServerAddress, SessionParams, Snapshot, SnapshotInbox,
    };
    use crate::player::{ItemShape, LookState, MoveIntent, PlayerPlugin, known_item_ids};
    use crate::wire::voxelheim::net as fb;

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: crate::net::ANY_TOKEN,
        })
    }

    /// One stack, as the server would have sent it.
    fn stack(item_id: u16, count: u16) -> Option<InventoryStack> {
        Some(InventoryStack {
            item_id,
            count,
            ..Default::default()
        })
    }

    #[test]
    fn unknown_items_use_the_palette_placeholder() {
        let style = stack_style(stack(u16::MAX, 7));
        let icon = style.icon.expect("a stack of something is drawn");
        let [r, g, b, a] = palette::linear_rgba(u16::MAX);
        assert_eq!(
            icon.colour,
            Color::linear_rgba(r, g, b, a),
            "an id from a newer contract drew a plausible shade instead of the placeholder"
        );
        // A stub of *something* carryable, which is the registry's own fallback shape.
        assert_eq!(icon.shape, ItemShape::Material);
        assert_eq!(style.count, "7");
    }

    /// The three shapes an empty cell can arrive in, and none of them draws a picture.
    #[test]
    fn an_empty_slot_asks_for_no_picture() {
        for empty in [None, stack(0, 0), stack(0, 4), stack(1, 0)] {
            let style = stack_style(empty);
            assert_eq!(style.icon, None, "{empty:?} drew a picture");
            assert_eq!(style.count, "", "{empty:?} drew a count");
            assert_eq!(
                style.background, EMPTY_CELL,
                "{empty:?} did not keep the empty-cell treatment"
            );
        }
    }

    /// The cell an item draws is built from the row the hand is built from — the whole
    /// row, not only its colour.
    ///
    /// `hands::the_swatch_a_panel_draws_is_the_one_the_hand_is_built_from` asserts the
    /// colour half from the other side. This is the shape half, swept over every id the
    /// registry knows rather than a list kept beside it, so a twelfth item is covered by
    /// being registered at all.
    #[test]
    fn every_item_draws_the_shape_and_the_colour_its_row_names() {
        for item_id in known_item_ids() {
            let style = stack_style(stack(item_id, 1));
            let icon = style
                .icon
                .unwrap_or_else(|| panic!("item {item_id} draws no picture"));
            assert_eq!(icon.shape, item_shape(item_id), "item {item_id}");
            let [r, g, b, a] = item_linear_rgba(item_id);
            assert_eq!(
                icon.colour,
                Color::linear_rgba(r, g, b, a),
                "item {item_id}"
            );
            assert!(
                !icon::parts(icon.shape).is_empty(),
                "item {item_id} draws a shape with no rectangles"
            );
            assert_eq!(
                style.background, FILLED_CELL,
                "item {item_id} spread its colour over the cell instead of drawing on it"
            );
        }
    }

    /// The case the feature exists for: eight palette entries across eleven items means
    /// collisions, and the shape is what tells the colliding pairs apart.
    #[test]
    fn two_items_that_share_a_swatch_draw_different_cells() {
        // From the registry: stone and the sharpening stone both present as `palette::STONE`,
        // snow and the tent both as `palette::SNOW`.
        for (left, right) in [(1u16, 11u16), (3, 9)] {
            let one = stack_style(stack(left, 1));
            let other = stack_style(stack(right, 1));
            let (one, other) = (
                one.icon.expect("a known item is drawn"),
                other.icon.expect("a known item is drawn"),
            );
            assert_eq!(
                one.colour, other.colour,
                "items {left} and {right} no longer share a swatch — pick another pair"
            );
            assert_ne!(
                one.shape, other.shape,
                "items {left} and {right} share a swatch and draw the same picture"
            );
            assert_ne!(icon::parts(one.shape), icon::parts(other.shape));
        }
    }

    /// Vitals the server could have sent, in the life state asked for.
    fn vitals(life_state: LifeState) -> PlayerVitals {
        PlayerVitals {
            health: if life_state == LifeState::Dead {
                0
            } else {
                100
            },
            max_health: 100,
            hunger: 100,
            max_hunger: 100,
            level: 1,
            experience: 0,
            experience_to_next: 50,
            life_state,
            respawn_ticks: if life_state == LifeState::Dead { 40 } else { 0 },
            invulnerable: false,
            blocking: false,
        }
    }

    /// The ordering constraint on `choose_input_mode`, exercised rather than assumed.
    ///
    /// One frame, in which a snapshot says this player has died while the pack is open.
    /// The mode has to be `Playing` by the end of that frame. Without
    /// `.after(ApplySnapshots)` the system is free to run before the vitals it reads are
    /// published, and the pack survives on top of the death overlay for a frame.
    ///
    /// Every other test in this module inserts `SelfVitals` directly and runs the system
    /// alone, which is why none of them can see this: the bug is in when the system runs,
    /// not in what it decides.
    #[test]
    fn dying_closes_the_pack_on_the_frame_the_snapshot_arrives() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .add_plugins(PlayerPlugin);
        // The plugin's own registration rather than a copy of it: this test exists to
        // pin the ordering that registration carries.
        add_input_mode_systems(&mut app);
        app.insert_resource(InputMode::Inventory);

        app.world_mut().resource_mut::<SnapshotInbox>().push(
            Snapshot {
                server_tick: 1,
                self_vitals: vitals(LifeState::Dead),
                ..Default::default()
            },
            Instant::now(),
        );

        app.update();

        assert_eq!(
            *app.world().resource::<InputMode>(),
            InputMode::Playing,
            "the pack was still open a frame after the server said the player was dead"
        );
    }

    /// Drives a keyboard edge through `InputPlugin` so `just_pressed` reaches `Update`.
    fn keyboard_edge(app: &mut App, key_code: KeyCode, state: ButtonState) {
        app.world_mut().write_message(KeyboardInput {
            key_code,
            logical_key: Key::Character("test".into()),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
    }

    /// The movement fields the server observes in one outbound `PlayerInput`.
    fn outbound_movement(frame: &[u8]) -> (f32, f32, bool) {
        let envelope = fb::root_as_envelope(frame).expect("the client encoded a valid envelope");
        let input = envelope
            .payload_as_player_input()
            .expect("the outbound frame is a PlayerInput");
        (input.move_x(), input.move_z(), input.jump())
    }

    #[test]
    fn the_inventory_transition_keeps_horizontal_movement_and_closes_the_rest() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session())
            .insert_resource(ConnectionState::Connected)
            // One frame is one announced input tick, so every state below is observed on
            // the wire without sleeping or relying on wall-clock scheduling.
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                50,
            )))
            // The production registration, in production order: the player samples and
            // sends while the UI owns the E transition and ApplyInputMode ordering.
            .add_plugins((PlayerPlugin, UiPlugin));
        let (outbound, frames) = Outbound::to_a_test(8);
        app.insert_resource(outbound);

        // The first Time update establishes its clock; the second advances one manual
        // interval. Settle both and discard that first neutral input tick.
        app.update();
        app.update();
        let _ = frames.try_recv().expect("the first announced tick is sent");
        assert!(
            frames.try_recv().is_err(),
            "the clock-establishing update sent an input before any time elapsed"
        );

        keyboard_edge(&mut app, KeyCode::KeyW, ButtonState::Pressed);
        keyboard_edge(&mut app, KeyCode::Space, ButtonState::Pressed);
        app.update();
        assert_eq!(
            outbound_movement(&frames.try_recv().expect("held movement reaches the wire")),
            (0.0, 1.0, true)
        );

        let look_before = *app.world().resource::<LookState>();
        keyboard_edge(&mut app, KeyCode::KeyE, ButtonState::Pressed);
        // The same frame also carries world-facing input. The pack deliberately leaves
        // horizontal movement live, while jump and mouse motion still belong to gameplay.
        app.world_mut().write_message(MouseMotion {
            delta: Vec2::new(80.0, -40.0),
        });
        app.update();

        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Inventory);
        assert_eq!(
            *app.world().resource::<MoveIntent>(),
            MoveIntent {
                x: 0.0,
                z: 1.0,
                jump: false,
            }
        );
        assert_eq!(*app.world().resource::<LookState>(), look_before);
        assert_eq!(
            outbound_movement(
                &frames
                    .try_recv()
                    .expect("the inventory-opening tick keeps walking")
            ),
            (0.0, 1.0, false),
            "E stopped held movement or leaked jump into the inventory"
        );

        // Held and newly pressed movement both remain live while the pack owns the input.
        keyboard_edge(&mut app, KeyCode::KeyD, ButtonState::Pressed);
        app.update();
        assert_eq!(
            *app.world().resource::<MoveIntent>(),
            MoveIntent {
                x: 1.0,
                z: 1.0,
                jump: false,
            }
        );
        assert_eq!(
            outbound_movement(&frames.try_recv().expect("the next input tick is sent")),
            (1.0, 1.0, false)
        );

        // Releasing the stale controls before closing proves the transition samples no
        // remembered direction. A later press, sampled after Playing returned, may move.
        for key in [KeyCode::KeyW, KeyCode::KeyD, KeyCode::Space] {
            keyboard_edge(&mut app, key, ButtonState::Released);
        }
        keyboard_edge(&mut app, KeyCode::KeyE, ButtonState::Released);
        app.update();
        let _ = frames.try_recv().expect("the release tick is sent");
        keyboard_edge(&mut app, KeyCode::KeyE, ButtonState::Pressed);
        app.update();
        assert_eq!(*app.world().resource::<InputMode>(), InputMode::Playing);
        assert_eq!(
            outbound_movement(&frames.try_recv().expect("the closing tick is sent")),
            (0.0, 0.0, false)
        );

        keyboard_edge(&mut app, KeyCode::KeyA, ButtonState::Pressed);
        app.update();
        assert_eq!(
            outbound_movement(&frames.try_recv().expect("fresh movement reaches the wire")),
            (-1.0, 0.0, false)
        );
    }

    fn mode_after_key(initial: InputMode, key: KeyCode) -> InputMode {
        mode_after_key_while(initial, key, LifeState::Alive)
    }

    fn mode_after_key_while(initial: InputMode, key: KeyCode, life_state: LifeState) -> InputMode {
        let mut keys = ButtonInput::default();
        keys.press(key);

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            // The character screen's preview is a real body, so the assets its meshes and
            // materials live in have to exist. `Assets<T>` is an ordinary resource, which
            // is what keeps this headless.
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(keys)
            .insert_resource(initial)
            .insert_resource(session())
            .insert_resource(SelfVitals::from_server(vitals(life_state)))
            .add_systems(Update, choose_input_mode);
        app.update();
        *app.world().resource::<InputMode>()
    }

    #[test]
    fn chat_inventory_and_menu_keys_own_the_mode_transitions() {
        assert_eq!(
            mode_after_key(InputMode::Playing, KeyCode::KeyT),
            InputMode::Chat
        );
        assert_eq!(
            mode_after_key(InputMode::Chat, KeyCode::Escape),
            InputMode::Chat,
            "chat owns its own close keys"
        );
        assert_eq!(
            mode_after_key(InputMode::Playing, KeyCode::KeyE),
            InputMode::Inventory
        );
        assert_eq!(
            mode_after_key(InputMode::Inventory, KeyCode::KeyE),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key(InputMode::Playing, KeyCode::Escape),
            InputMode::Menu
        );
        assert_eq!(
            mode_after_key(InputMode::Inventory, KeyCode::Escape),
            InputMode::Menu
        );
        assert_eq!(
            mode_after_key(InputMode::Menu, KeyCode::Escape),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key(InputMode::Menu, KeyCode::KeyE),
            InputMode::Menu,
            "inventory cannot replace an open pause menu"
        );
    }

    /// `M` opens and closes the map, and every screen that already owns the keyboard
    /// keeps it.
    #[test]
    fn the_map_key_toggles_the_map_and_is_ignored_by_every_other_screen() {
        assert_eq!(
            mode_after_key(InputMode::Playing, KeyCode::KeyM),
            InputMode::Map
        );
        assert_eq!(
            mode_after_key(InputMode::Map, KeyCode::KeyM),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key(InputMode::Map, KeyCode::Escape),
            InputMode::Playing,
            "escape closes the map onto play rather than opening the pause menu over it"
        );
        for mode in [
            InputMode::Chat,
            InputMode::Loot,
            InputMode::Menu,
            InputMode::Inventory,
        ] {
            assert_eq!(
                mode_after_key(mode, KeyCode::KeyM),
                mode,
                "{mode:?} does not give the keyboard up to the map"
            );
        }
        assert_eq!(
            mode_after_key(InputMode::Map, KeyCode::KeyE),
            InputMode::Map,
            "the pack does not replace an open map either"
        );
    }

    /// Death takes the map, exactly as it takes the pack.
    #[test]
    fn a_dead_player_cannot_open_the_map_and_does_not_keep_one() {
        assert_eq!(
            mode_after_key_while(InputMode::Playing, KeyCode::KeyM, LifeState::Dead),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key_while(InputMode::Map, KeyCode::KeyM, LifeState::Dead),
            InputMode::Playing
        );
    }

    /// The same app as [`mode_after_key_while`], with the two resources this screen added.
    fn mode_after_key_with(
        initial: InputMode,
        key: KeyCode,
        settings: Settings,
        screen: SettingsScreen,
    ) -> InputMode {
        let mut keys = ButtonInput::default();
        keys.press(key);

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(keys)
            .insert_resource(initial)
            .insert_resource(session())
            .insert_resource(settings)
            .insert_resource(screen)
            .insert_resource(SelfVitals::from_server(vitals(LifeState::Alive)))
            .add_systems(Update, choose_input_mode);
        app.update();
        *app.world().resource::<InputMode>()
    }

    /// `Escape`, `E` and `T` are bindings now rather than literals, and this is what says so:
    /// move them, and the mode follows the keys the settings name rather than the keys
    /// this file used to spell.
    #[test]
    fn the_three_mode_keys_are_the_ones_the_settings_name() {
        let mut settings = Settings::default();
        settings
            .rebind(Control::Menu, KeyCode::KeyG)
            .expect("g is bindable and free");
        settings
            .rebind(Control::Inventory, KeyCode::KeyH)
            .expect("h is bindable and free");
        settings
            .rebind(Control::Chat, KeyCode::KeyJ)
            .expect("j is bindable and free");
        let screen = SettingsScreen::default();

        assert_eq!(
            mode_after_key_with(
                InputMode::Playing,
                KeyCode::KeyG,
                settings.clone(),
                screen.clone()
            ),
            InputMode::Menu
        );
        assert_eq!(
            mode_after_key_with(
                InputMode::Playing,
                KeyCode::KeyH,
                settings.clone(),
                screen.clone()
            ),
            InputMode::Inventory
        );
        assert_eq!(
            mode_after_key_with(
                InputMode::Playing,
                KeyCode::KeyJ,
                settings.clone(),
                screen.clone()
            ),
            InputMode::Chat
        );
        // And the keys they used to be belong to nobody.
        assert_eq!(
            mode_after_key_with(
                InputMode::Playing,
                KeyCode::Escape,
                settings.clone(),
                screen.clone()
            ),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key_with(
                InputMode::Playing,
                KeyCode::KeyE,
                settings.clone(),
                screen.clone()
            ),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key_with(InputMode::Playing, KeyCode::KeyT, settings, screen),
            InputMode::Playing
        );
    }

    /// While the settings screen is up this system reads no key at all. Without that, the
    /// press that closes the screen would resume play in the same frame, and the key a
    /// player pressed to rebind a control would also fire the control it is being taken
    /// from.
    #[test]
    fn the_settings_screen_keeps_the_keyboard_while_it_is_up() {
        let mut screen = SettingsScreen::default();
        screen.open();
        for key in [KeyCode::Escape, KeyCode::KeyE, KeyCode::KeyT] {
            assert_eq!(
                mode_after_key_with(InputMode::Menu, key, Settings::default(), screen.clone()),
                InputMode::Menu,
                "{key:?} reached the mode through an open settings screen"
            );
        }
    }

    #[test]
    fn death_takes_the_inventory_and_leaves_the_pause_menu() {
        // The pack is gameplay input the server would refuse anyway, so `E` does nothing
        // and an already-open one closes. Nothing about this is a decision: the server's
        // life state is read, never written.
        assert_eq!(
            mode_after_key_while(InputMode::Playing, KeyCode::KeyE, LifeState::Dead),
            InputMode::Playing
        );
        assert_eq!(
            mode_after_key_while(InputMode::Inventory, KeyCode::KeyE, LifeState::Dead),
            InputMode::Playing,
            "an inventory opened before the death does not survive it"
        );

        // Quitting and disconnecting must never depend on being alive.
        assert_eq!(
            mode_after_key_while(InputMode::Playing, KeyCode::Escape, LifeState::Dead),
            InputMode::Menu
        );
        assert_eq!(
            mode_after_key_while(InputMode::Menu, KeyCode::Escape, LifeState::Dead),
            InputMode::Playing
        );
    }

    #[test]
    fn cursor_capture_follows_the_live_playing_mode() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            // The character screen's preview is a real body, so the assets its meshes and
            // materials live in have to exist. `Assets<T>` is an ordinary resource, which
            // is what keeps this headless.
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(ConnectionState::Connected)
            .insert_resource(ServerAddress("ws://127.0.0.1:7777".to_owned()))
            .insert_resource(session())
            .add_plugins(UiPlugin)
            .world_mut()
            .spawn((PrimaryWindow, CursorOptions::default()));

        app.update();
        let cursor = app
            .world_mut()
            .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
            .single(app.world())
            .expect("one primary cursor");
        assert_eq!(cursor.grab_mode, CursorGrabMode::Locked);
        assert!(!cursor.visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Chat;
        app.update();
        let cursor = app
            .world_mut()
            .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
            .single(app.world())
            .expect("one primary cursor");
        assert_eq!(cursor.grab_mode, CursorGrabMode::Locked);
        assert!(!cursor.visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Inventory;
        app.update();
        let cursor = app
            .world_mut()
            .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
            .single(app.world())
            .expect("one primary cursor");
        assert_eq!(cursor.grab_mode, CursorGrabMode::None);
        assert!(cursor.visible);

        *app.world_mut().resource_mut::<InputMode>() = InputMode::Menu;
        app.update();
        let cursor = app
            .world_mut()
            .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
            .single(app.world())
            .expect("one primary cursor");
        assert_eq!(cursor.grab_mode, CursorGrabMode::None);
        assert!(cursor.visible);
    }
}
