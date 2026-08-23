//! The client's on-screen surface and the input mode that owns the pointer.

mod character;
mod crosshair;
mod health;

mod hotbar;
mod icon;
mod inventory;
mod login;
mod menu;
mod servers;
mod settings;
mod status;

use bevy::prelude::*;
use bevy::ui::FocusPolicy;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use icon::StackIcon;

/// The character a launch named, for `main.rs` to hand to the character screen.
pub use character::PlayAs;

use crate::net::{
    CharacterChoice, ChooseCharacter, ConnectRequest, ConnectionState, DisconnectRequest,
    InventoryStack, RefreshServerList, ServerList, Session, SignInRequest, SignInState,
};

use crate::player::{
    ApplyInputMode, ApplySnapshots, CraftClick, InputMode, InventoryClick, SelfVitals, ViewMode,
    item_linear_rgba, item_shape,
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
            .add_message::<DisconnectRequest>()
            // Registered here as well as by `net::SignInPlugin`, which is not built
            // when no account service is configured. `add_message` is idempotent,
            // and this is what keeps the login screen headlessly testable on its
            // own — the same reason the four above are here.
            .add_message::<SignInRequest>()
            // The same reasoning for the server list's two: `net::NetPlugin` registers
            // `ConnectRequest` and `net::ServerListPlugin` registers
            // `RefreshServerList`, and neither is built in a headless UI test.
            .add_message::<ConnectRequest>()
            .add_message::<RefreshServerList>()
            .add_message::<AppExit>()
            .add_message::<ChooseCharacter>()
            .add_plugins((
                character::CharacterUiPlugin,
                crosshair::CrosshairPlugin,
                health::HealthUiPlugin,
                hotbar::HotbarPlugin,
                inventory::InventoryUiPlugin,
                login::LoginPlugin,
                menu::MenuPlugin,
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
    screen: Option<Res<SettingsScreen>>,
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

    if vitals.dead() && *mode == InputMode::Inventory {
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
    if screen.is_some_and(|screen| screen.is_open()) {
        return;
    }

    // The bindings, or the defaults for an app built without them — which are `Escape` and
    // `E`, the two literals that stood here until this screen existed.
    let bindings = settings
        .as_deref()
        .map_or_else(Default::default, |settings| *settings.bindings());

    if keys.just_pressed(bindings.key(Control::Menu)) {
        let next = if *mode == InputMode::Menu {
            InputMode::Playing
        } else {
            InputMode::Menu
        };
        set_mode(&mut mode, next);
        return;
    }

    if keys.just_pressed(bindings.key(Control::Inventory)) {
        if vitals.dead() {
            return;
        }
        let next = match *mode {
            InputMode::Playing => InputMode::Inventory,
            InputMode::Inventory => InputMode::Playing,
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
    let playing = *mode == InputMode::Playing && !overlays.any_is_up() && overlays.connected();
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
            icon::redraw(commands, *child, drawn, style.icon);
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::net::{
        LifeState, PlayerVitals, ServerAddress, SessionParams, Snapshot, SnapshotInbox,
    };
    use crate::player::{ItemShape, PlayerPlugin, known_item_ids};

    fn session() -> Session {
        Session(SessionParams {
            clock: Default::default(),
            entity_id: 1,
            spawn: [0.0; 3],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: 32,
            view_distance: 3,
            inventory_slots: 36,
            hotbar_slots: 9,
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
            life_state,
            respawn_ticks: if life_state == LifeState::Dead { 40 } else { 0 },
            invulnerable: false,
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
    fn e_and_escape_own_the_three_mode_transitions() {
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

    /// `Escape` and `E` are bindings now rather than literals, and this is what says so:
    /// move them, and the mode follows the keys the settings name rather than the keys
    /// this file used to spell.
    #[test]
    fn the_two_mode_keys_are_the_ones_the_settings_name() {
        let mut settings = Settings::default();
        settings
            .rebind(Control::Menu, KeyCode::KeyG)
            .expect("g is bindable and free");
        settings
            .rebind(Control::Inventory, KeyCode::KeyH)
            .expect("h is bindable and free");
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
            mode_after_key_with(InputMode::Playing, KeyCode::KeyE, settings, screen),
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
        for key in [KeyCode::Escape, KeyCode::KeyE] {
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
