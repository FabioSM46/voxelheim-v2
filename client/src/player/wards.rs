//! The server-sent ward boundary, drawn as translucent world-space interface.
//!
//! A ward is a set of chunk columns, not a volume this client derives. The newest
//! [`WardsNearby`] replaces [`Wards`] whole, and every edge below is extracted from that
//! answer. Nothing in this module is read by input, targeting, movement or placement: the
//! server remains the only authority on whether an action is legal.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use super::camera::{AimCamera, WorldCamera};
use super::sky::submerged_at;
use crate::net::{DrainNetwork, Session, WardKind, WardedColumn, WardsInbox};
use crate::world::ChunkStore;

/// Half the vertical span of a wall, in blocks, around the camera's eye.
pub(super) const WARD_WALL_HALF_HEIGHT: f32 = 48.0;
/// How far the eye moves vertically before the wall mesh is rebuilt.
pub(super) const WARD_WALL_REBUILD_STEP: f32 = 8.0;
/// The ordinary alpha carried by every wall vertex.
pub(super) const WARD_WALL_ALPHA: f32 = 0.18;
/// The alpha of an edge while the eye is within two blocks on its warded side.
const WARD_WALL_NEAR_ALPHA: f32 = 0.35;
const WARD_WALL_NEAR_DISTANCE: f32 = 2.0;

const SETTLEMENT_TINT: [f32; 3] = [0.85, 0.65, 0.20];
const OTHER_RUNESTONE_TINT: [f32; 3] = [0.25, 0.45, 0.85];
const OWN_RUNESTONE_TINT: [f32; 3] = [0.30, 0.80, 0.40];

/// The last complete server-sent ward set for this session.
///
/// The coordinate is the lookup key the boundary extraction needs; the complete column
/// remains the value so no presentation fact decoded from the wire is lost. This is
/// cleared with the session and written only from [`WardsInbox`].
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Wards(HashMap<(i32, i32), WardedColumn>);

/// Installs the network mirror and the three transparent ward meshes.
pub(super) struct WardBoundaryPlugin;

impl Plugin for WardBoundaryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Wards>()
            // `NetPlugin` does the same. Whichever plugin is built first creates the
            // one-frame queue and the other finds it.
            .init_resource::<WardsInbox>()
            .init_resource::<WardMeshState>()
            .add_systems(Startup, spawn_walls)
            .add_systems(
                Update,
                (sync_wards, rebuild_walls, sync_visibility)
                    .chain()
                    .after(DrainNetwork)
                    .after(AimCamera),
            );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum WardClass {
    Settlement,
    OtherRunestone,
    OwnRunestone,
}

impl WardClass {
    const ALL: [Self; 3] = [Self::Settlement, Self::OtherRunestone, Self::OwnRunestone];

    fn of(column: WardedColumn) -> Self {
        match (column.kind, column.mine) {
            (WardKind::Settlement, _) => Self::Settlement,
            (WardKind::Runestone, false) => Self::OtherRunestone,
            (WardKind::Runestone, true) => Self::OwnRunestone,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Settlement => 0,
            Self::OtherRunestone => 1,
            Self::OwnRunestone => 2,
        }
    }

    const fn tint(self) -> [f32; 3] {
        match self {
            Self::Settlement => SETTLEMENT_TINT,
            Self::OtherRunestone => OTHER_RUNESTONE_TINT,
            Self::OwnRunestone => OWN_RUNESTONE_TINT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Side {
    West,
    East,
    North,
    South,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Edge {
    cx: i32,
    cz: i32,
    side: Side,
    class: WardClass,
}

/// The only edges whose alpha can be raised on one frame.
///
/// An eye can be on the warded side of at most the four faces of the column that
/// contains it. Keeping that fixed-size answer avoids allocating and scanning the
/// complete ward boundary while the player moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct HighlightedEdges([Option<Edge>; 4]);

impl HighlightedEdges {
    fn contains(self, edge: Edge) -> bool {
        self.0
            .into_iter()
            .flatten()
            .any(|candidate| candidate == edge)
    }
}

/// Marks one of the three independently sorted transparent entities.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
struct WardWall(WardClass);

#[derive(Resource)]
struct WardVisuals {
    meshes: [Handle<Mesh>; 3],
}

#[derive(Resource, Debug, Default)]
struct WardMeshState {
    eye_step: Option<i64>,
    chunk_size: Option<u16>,
    edges: Vec<Edge>,
    highlighted: HighlightedEdges,
    drawn: [bool; 3],
    active: bool,
    revision: u64,
    edge_revision: u64,
}

fn spawn_walls(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let handles = WardClass::ALL.map(|class| {
        let mesh = meshes.add(empty_mesh());
        let material = materials.add(StandardMaterial {
            // White is load-bearing: tint and alpha live in the vertex colour.
            base_color: Color::WHITE,
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            cull_mode: None,
            double_sided: true,
            ..default()
        });
        commands.spawn((
            WardWall(class),
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden,
        ));
        mesh
    });
    commands.insert_resource(WardVisuals { meshes: handles });
}

fn sync_wards(
    session: Option<Res<Session>>,
    mut inbox: ResMut<WardsInbox>,
    mut wards: ResMut<Wards>,
) {
    if session.is_none() {
        // The inbox belongs to the network plugin and therefore outlives a socket.
        // Discard a late answer instead of briefly publishing it as this session's
        // state; an answer for a future session can only arrive after its Session.
        if !inbox.is_empty() {
            inbox.take();
        }
        if !wards.0.is_empty() {
            wards.0.clear();
        }
        return;
    }
    if inbox.is_empty() {
        return;
    }
    let Some(last) = inbox.take().pop() else {
        return;
    };
    let next = Wards(
        last.columns
            .into_iter()
            .map(|column| ((column.cx, column.cz), column))
            .collect(),
    );
    if *wards != next {
        *wards = next;
    }
}

fn rebuild_walls(
    wards: Res<Wards>,
    session: Option<Res<Session>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    visuals: Option<Res<WardVisuals>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut state: ResMut<WardMeshState>,
) {
    let Some(visuals) = visuals else {
        return;
    };
    let Some(params) = session.as_deref().map(|session| session.0) else {
        clear_geometry_if_needed(&visuals, &mut meshes, &mut state);
        return;
    };
    if !params.clock.declared() || wards.0.is_empty() {
        clear_geometry_if_needed(&visuals, &mut meshes, &mut state);
        return;
    }
    let Some(eye) = eyes.iter().next().map(|eye| eye.translation) else {
        clear_geometry_if_needed(&visuals, &mut meshes, &mut state);
        return;
    };
    let Some(eye_step) = height_step(eye.y) else {
        clear_geometry_if_needed(&visuals, &mut meshes, &mut state);
        return;
    };

    let chunk_size = f32::from(params.chunk_size);
    let wards_changed = wards.is_changed();
    if wards_changed || state.edges.is_empty() {
        state.edges = boundary_edges(&wards);
        state.edge_revision = state.edge_revision.wrapping_add(1);
    }
    let highlighted = highlighted_edges(&wards, eye, chunk_size);
    if state.active
        && !wards_changed
        && state.eye_step == Some(eye_step)
        && state.chunk_size == Some(params.chunk_size)
        && state.highlighted == highlighted
    {
        return;
    }

    let centre_y = eye_step as f32 * WARD_WALL_REBUILD_STEP;
    let mut drawn = [false; 3];
    for class in WardClass::ALL {
        let class_edges: Vec<_> = state
            .edges
            .iter()
            .copied()
            .filter(|edge| edge.class == class)
            .collect();
        drawn[class.index()] = !class_edges.is_empty();
        let mesh = wall_mesh(&class_edges, highlighted, class, chunk_size, centre_y);
        replace_mesh(&mut meshes, &visuals.meshes[class.index()], mesh);
    }
    state.eye_step = Some(eye_step);
    state.chunk_size = Some(params.chunk_size);
    state.highlighted = highlighted;
    state.drawn = drawn;
    state.active = true;
    state.revision = state.revision.wrapping_add(1);
}

fn clear_geometry_if_needed(
    visuals: &WardVisuals,
    meshes: &mut Assets<Mesh>,
    state: &mut WardMeshState,
) {
    if !state.active && !state.drawn.into_iter().any(|drawn| drawn) {
        return;
    }
    for handle in &visuals.meshes {
        replace_mesh(meshes, handle, empty_mesh());
    }
    state.eye_step = None;
    state.chunk_size = None;
    state.edges.clear();
    state.highlighted = HighlightedEdges::default();
    state.drawn = [false; 3];
    state.active = false;
    state.revision = state.revision.wrapping_add(1);
}

fn replace_mesh(meshes: &mut Assets<Mesh>, handle: &Handle<Mesh>, mesh: Mesh) {
    meshes
        .insert(handle.id(), mesh)
        .expect("a live ward mesh handle keeps its asset generation valid");
}

fn sync_visibility(
    session: Option<Res<Session>>,
    store: Option<Res<ChunkStore>>,
    eyes: Query<&Transform, With<WorldCamera>>,
    state: Res<WardMeshState>,
    mut walls: Query<(&WardWall, &mut Visibility)>,
) {
    let drawable = session.as_deref().is_some_and(|session| {
        session.0.clock.declared()
            && eyes.iter().next().is_some_and(|eye| {
                !submerged_at(
                    store.as_deref(),
                    eye.translation,
                    usize::from(session.0.chunk_size),
                )
            })
    });
    for (wall, mut visibility) in &mut walls {
        let wanted = if drawable && state.drawn[wall.0.index()] {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

fn height_step(y: f32) -> Option<i64> {
    y.is_finite()
        .then_some((y / WARD_WALL_REBUILD_STEP).floor() as i64)
}

fn column_at(value: f32, chunk_size: f32) -> Option<i32> {
    if !value.is_finite() || !chunk_size.is_finite() || chunk_size <= 0.0 {
        return None;
    }
    let column = (f64::from(value) / f64::from(chunk_size)).floor();
    (column >= f64::from(i32::MIN) && column <= f64::from(i32::MAX)).then_some(column as i32)
}

fn neighbour_class(wards: &Wards, cx: i32, cz: i32, dx: i32, dz: i32) -> Option<WardClass> {
    cx.checked_add(dx)
        .zip(cz.checked_add(dz))
        .and_then(|coord| wards.0.get(&coord))
        .copied()
        .map(WardClass::of)
}

fn highlighted_edges(wards: &Wards, eye: Vec3, chunk_size: f32) -> HighlightedEdges {
    if !eye.is_finite() {
        return HighlightedEdges::default();
    }
    let Some((cx, cz)) = column_at(eye.x, chunk_size).zip(column_at(eye.z, chunk_size)) else {
        return HighlightedEdges::default();
    };
    let Some(&column) = wards.0.get(&(cx, cz)) else {
        return HighlightedEdges::default();
    };
    let class = WardClass::of(column);
    let mut highlighted = HighlightedEdges::default();
    for (index, (side, dx, dz)) in [
        (Side::West, -1, 0),
        (Side::East, 1, 0),
        (Side::North, 0, -1),
        (Side::South, 0, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let edge = Edge {
            cx,
            cz,
            side,
            class,
        };
        if neighbour_class(wards, cx, cz, dx, dz) != Some(class)
            && near_warded_side(edge, eye, chunk_size)
        {
            highlighted.0[index] = Some(edge);
        }
    }
    highlighted
}

/// Set difference over the complete map. A different class deliberately counts as
/// absent: each owner emits its own face on a shared boundary, so both colours are drawn.
fn boundary_edges(wards: &Wards) -> Vec<Edge> {
    let mut edges = Vec::with_capacity(wards.0.len() * 4);
    for (&(cx, cz), &column) in &wards.0 {
        let class = WardClass::of(column);
        for (side, dx, dz) in [
            (Side::West, -1, 0),
            (Side::East, 1, 0),
            (Side::North, 0, -1),
            (Side::South, 0, 1),
        ] {
            if neighbour_class(wards, cx, cz, dx, dz) != Some(class) {
                edges.push(Edge {
                    cx,
                    cz,
                    side,
                    class,
                });
            }
        }
    }
    edges.sort_unstable();
    edges
}

fn near_warded_side(edge: Edge, eye: Vec3, chunk_size: f32) -> bool {
    if !eye.is_finite() {
        return false;
    }
    let (x0, x1, z0, z1) = column_bounds(edge.cx, edge.cz, chunk_size);
    match edge.side {
        Side::West => {
            (x0..=x0 + WARD_WALL_NEAR_DISTANCE).contains(&eye.x) && (z0..=z1).contains(&eye.z)
        }
        Side::East => {
            (x1 - WARD_WALL_NEAR_DISTANCE..=x1).contains(&eye.x) && (z0..=z1).contains(&eye.z)
        }
        Side::North => {
            (z0..=z0 + WARD_WALL_NEAR_DISTANCE).contains(&eye.z) && (x0..=x1).contains(&eye.x)
        }
        Side::South => {
            (z1 - WARD_WALL_NEAR_DISTANCE..=z1).contains(&eye.z) && (x0..=x1).contains(&eye.x)
        }
    }
}

fn column_bounds(cx: i32, cz: i32, chunk_size: f32) -> (f32, f32, f32, f32) {
    let x0 = i64::from(cx) as f32 * chunk_size;
    let z0 = i64::from(cz) as f32 * chunk_size;
    (x0, x0 + chunk_size, z0, z0 + chunk_size)
}

fn wall_mesh(
    edges: &[Edge],
    highlighted: HighlightedEdges,
    class: WardClass,
    chunk_size: f32,
    centre_y: f32,
) -> Mesh {
    let mut positions = Vec::with_capacity(edges.len() * 4);
    let mut normals = Vec::with_capacity(edges.len() * 4);
    let mut colours = Vec::with_capacity(edges.len() * 4);
    let mut indices = Vec::with_capacity(edges.len() * 6);
    let tint = class.tint();
    let bottom = centre_y - WARD_WALL_HALF_HEIGHT;
    let top = centre_y + WARD_WALL_HALF_HEIGHT;

    for edge in edges {
        let (x0, x1, z0, z1) = column_bounds(edge.cx, edge.cz, chunk_size);
        let (a, b, normal) = match edge.side {
            Side::West => ([x0, z1], [x0, z0], [-1.0, 0.0, 0.0]),
            Side::East => ([x1, z0], [x1, z1], [1.0, 0.0, 0.0]),
            Side::North => ([x0, z0], [x1, z0], [0.0, 0.0, -1.0]),
            Side::South => ([x1, z1], [x0, z1], [0.0, 0.0, 1.0]),
        };
        let first = positions.len() as u32;
        positions.extend_from_slice(&[
            [a[0], bottom, a[1]],
            [b[0], bottom, b[1]],
            [b[0], top, b[1]],
            [a[0], top, a[1]],
        ]);
        normals.extend_from_slice(&[normal; 4]);
        let alpha = if highlighted.contains(*edge) {
            WARD_WALL_NEAR_ALPHA
        } else {
            WARD_WALL_ALPHA
        };
        colours.extend_from_slice(&[[tint[0], tint[1], tint[2], alpha]; 4]);
        indices.extend_from_slice(&[first, first + 1, first + 2, first, first + 2, first + 3]);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
    .with_inserted_indices(Indices::U32(indices))
}

fn empty_mesh() -> Mesh {
    wall_mesh(
        &[],
        HighlightedEdges::default(),
        WardClass::Settlement,
        1.0,
        0.0,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use bevy::asset::AssetPlugin;
    use bevy::mesh::VertexAttributeValues;

    use super::*;
    use crate::net::{ANY_TOKEN, ChunkCoord, SessionParams, WardsNearby, WorldClock};
    use crate::world::{VoxelChunk, palette};

    const SIZE: u16 = 32;

    fn column(cx: i32, cz: i32, kind: WardKind, mine: bool) -> WardedColumn {
        WardedColumn { cx, cz, kind, mine }
    }

    fn wards(columns: impl IntoIterator<Item = WardedColumn>) -> Wards {
        Wards(
            columns
                .into_iter()
                .map(|column| ((column.cx, column.cz), column))
                .collect(),
        )
    }

    fn session(clock: bool) -> Session {
        Session(SessionParams {
            entity_id: 1,
            spawn: [0.5, 80.0, 0.5],
            world_seed: 1,
            tick_rate: 20,
            chunk_size: SIZE,
            view_distance: 8,
            inventory_slots: 37,
            hotbar_slots: 9,
            equipment_slots: 4,
            player_token: ANY_TOKEN,
            clock: if clock {
                WorldClock {
                    day_length_ticks: 24_000,
                    night_start_ticks: 14_000,
                    night_end_ticks: 22_000,
                }
            } else {
                WorldClock::default()
            },
        })
    }

    fn app(clock: bool) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .insert_resource(session(clock))
            .add_plugins(WardBoundaryPlugin);
        app.world_mut()
            .spawn((WorldCamera, Transform::from_xyz(16.0, 80.0, 16.0)));
        app
    }

    fn deliver(app: &mut App, columns: Vec<WardedColumn>) {
        app.world_mut()
            .resource_mut::<WardsInbox>()
            .push(WardsNearby { columns });
        app.update();
    }

    fn quad_count(mesh: &Mesh) -> usize {
        mesh.indices().map_or(0, |indices| indices.len() / 6)
    }

    fn wall_quads(app: &mut App) -> usize {
        let world = app.world_mut();
        let handles: Vec<_> = world
            .query_filtered::<&Mesh3d, With<WardWall>>()
            .iter(world)
            .map(|mesh| mesh.0.clone())
            .collect();
        let meshes = world.resource::<Assets<Mesh>>();
        handles
            .iter()
            .map(|handle| quad_count(meshes.get(handle).expect("ward mesh exists")))
            .sum()
    }

    fn all_hidden(app: &mut App) -> bool {
        let world = app.world_mut();
        world
            .query_filtered::<&Visibility, With<WardWall>>()
            .iter(world)
            .all(|visibility| *visibility == Visibility::Hidden)
    }

    fn wall_materials(app: &mut App) -> Vec<Handle<StandardMaterial>> {
        let world = app.world_mut();
        world
            .query_filtered::<&MeshMaterial3d<StandardMaterial>, With<WardWall>>()
            .iter(world)
            .map(|material| material.0.clone())
            .collect()
    }

    #[test]
    fn a_three_by_three_ward_has_only_twelve_outer_edges() {
        let mut columns = Vec::new();
        for cz in -1..=1 {
            for cx in -1..=1 {
                columns.push(column(cx, cz, WardKind::Runestone, true));
            }
        }
        assert_eq!(boundary_edges(&wards(columns)).len(), 12);
    }

    #[test]
    fn adjacent_columns_of_one_class_do_not_draw_the_shared_edge() {
        let edges = boundary_edges(&wards([
            column(0, 0, WardKind::Runestone, false),
            column(1, 0, WardKind::Runestone, false),
        ]));
        assert_eq!(edges.len(), 6);
        assert!(!edges.iter().any(|edge| {
            (edge.cx, edge.side) == (0, Side::East) || (edge.cx, edge.side) == (1, Side::West)
        }));
    }

    #[test]
    fn adjacent_different_wards_draw_both_faces_of_the_shared_edge() {
        let edges = boundary_edges(&wards([
            column(0, 0, WardKind::Settlement, false),
            column(1, 0, WardKind::Runestone, true),
        ]));
        assert_eq!(edges.len(), 8);
        assert!(edges.iter().any(|edge| {
            edge.class == WardClass::Settlement && edge.cx == 0 && edge.side == Side::East
        }));
        assert!(edges.iter().any(|edge| {
            edge.class == WardClass::OwnRunestone && edge.cx == 1 && edge.side == Side::West
        }));
    }

    #[test]
    fn a_complete_message_replaces_the_mesh_and_an_empty_one_clears_it() {
        let mut app = app(true);
        deliver(&mut app, vec![column(0, 0, WardKind::Runestone, true)]);
        assert_eq!(wall_quads(&mut app), 4);
        assert!(!all_hidden(&mut app));

        deliver(&mut app, Vec::new());
        assert_eq!(wall_quads(&mut app), 0);
        assert!(all_hidden(&mut app));
        assert!(app.world().resource::<Wards>().0.is_empty());
    }

    #[test]
    fn only_the_newest_complete_message_in_one_frame_becomes_the_server_copy() {
        let mut app = app(true);
        {
            let mut inbox = app.world_mut().resource_mut::<WardsInbox>();
            inbox.push(WardsNearby {
                columns: vec![column(0, 0, WardKind::Settlement, false)],
            });
            inbox.push(WardsNearby {
                columns: vec![column(2, -3, WardKind::Runestone, true)],
            });
        }
        app.update();

        assert_eq!(
            app.world().resource::<Wards>().0,
            HashMap::from([((2, -3), column(2, -3, WardKind::Runestone, true))])
        );
    }

    #[test]
    fn the_mesh_rebuilds_on_a_list_and_an_eye_step_but_not_idle() {
        let mut app = app(true);
        deliver(&mut app, vec![column(0, 0, WardKind::Settlement, false)]);
        let first = app.world().resource::<WardMeshState>();
        let first_revision = first.revision;
        let first_edge_revision = first.edge_revision;
        app.update();
        let idle = app.world().resource::<WardMeshState>();
        assert_eq!(idle.revision, first_revision);
        assert_eq!(idle.edge_revision, first_edge_revision);

        let world = app.world_mut();
        let mut query = world.query_filtered::<&mut Transform, With<WorldCamera>>();
        query.single_mut(world).expect("one camera").translation.y += WARD_WALL_REBUILD_STEP;
        app.update();
        let raised = app.world().resource::<WardMeshState>();
        assert_eq!(raised.revision, first_revision + 1);
        assert_eq!(raised.edge_revision, first_edge_revision);
    }

    #[test]
    fn crossing_the_near_threshold_rebuilds_only_the_mesh_not_the_edge_cache() {
        let mut app = app(true);
        deliver(&mut app, vec![column(0, 0, WardKind::Settlement, false)]);
        let first = app.world().resource::<WardMeshState>();
        let first_revision = first.revision;
        let first_edge_revision = first.edge_revision;

        let world = app.world_mut();
        let mut query = world.query_filtered::<&mut Transform, With<WorldCamera>>();
        query.single_mut(world).expect("one camera").translation.x = 31.0;
        app.update();

        let near = app.world().resource::<WardMeshState>();
        assert_eq!(near.revision, first_revision + 1);
        assert_eq!(near.edge_revision, first_edge_revision);
        assert!(near.highlighted.contains(Edge {
            cx: 0,
            cz: 0,
            side: Side::East,
            class: WardClass::Settlement,
        }));

        app.update();
        assert_eq!(
            app.world().resource::<WardMeshState>().revision,
            first_revision + 1
        );
    }

    #[test]
    fn no_session_clears_the_server_copy_and_hides_every_wall() {
        let mut app = app(true);
        deliver(&mut app, vec![column(0, 0, WardKind::Runestone, false)]);
        app.world_mut().remove_resource::<Session>();
        app.world_mut()
            .resource_mut::<WardsInbox>()
            .push(WardsNearby {
                columns: vec![column(8, 9, WardKind::Settlement, false)],
            });
        app.update();

        assert!(app.world().resource::<Wards>().0.is_empty());
        assert!(app.world().resource::<WardsInbox>().is_empty());
        assert_eq!(wall_quads(&mut app), 0);
        assert!(all_hidden(&mut app));
    }

    #[test]
    fn clockless_and_submerged_worlds_hide_the_walls() {
        let mut clockless = app(false);
        deliver(
            &mut clockless,
            vec![column(0, 0, WardKind::Settlement, false)],
        );
        assert!(all_hidden(&mut clockless));

        let mut submerged = app(true);
        let mut chunk = VoxelChunk::all_air(usize::from(SIZE));
        chunk.set(16, 16, 16, palette::WATER);
        let mut store = ChunkStore::default();
        store.insert(
            ChunkCoord {
                cx: 0,
                cy: 2,
                cz: 0,
            },
            chunk,
        );
        submerged.insert_resource(store);
        deliver(
            &mut submerged,
            vec![column(0, 0, WardKind::Settlement, false)],
        );
        assert!(all_hidden(&mut submerged));
    }

    #[test]
    fn tint_and_alpha_are_carried_by_vertices() {
        let edge = Edge {
            cx: 0,
            cz: 0,
            side: Side::West,
            class: WardClass::OwnRunestone,
        };
        let highlighted = HighlightedEdges([Some(edge), None, None, None]);
        let near_mesh = wall_mesh(&[edge], highlighted, WardClass::OwnRunestone, 32.0, 80.0);
        let Some(VertexAttributeValues::Float32x4(near_colours)) =
            near_mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("wall colours are float RGBA");
        };
        assert_eq!(
            near_colours,
            &vec![[0.30, 0.80, 0.40, WARD_WALL_NEAR_ALPHA]; 4]
        );

        let far_mesh = wall_mesh(
            &[edge],
            HighlightedEdges::default(),
            WardClass::OwnRunestone,
            32.0,
            80.0,
        );
        let Some(VertexAttributeValues::Float32x4(far_colours)) =
            far_mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("wall colours are float RGBA");
        };
        assert_eq!(far_colours, &vec![[0.30, 0.80, 0.40, WARD_WALL_ALPHA]; 4]);
    }

    #[test]
    fn every_ward_class_has_its_own_translucent_unlit_double_sided_material() {
        let mut app = app(true);
        app.update();

        let handles = wall_materials(&mut app);
        assert_eq!(handles.len(), 3);
        assert_eq!(handles.iter().collect::<HashSet<_>>().len(), 3);
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        for handle in handles {
            let material = materials.get(&handle).expect("ward material exists");
            assert_eq!(material.base_color, Color::WHITE);
            assert_eq!(material.alpha_mode, AlphaMode::Blend);
            assert!(material.unlit);
            assert!(material.double_sided);
            assert_eq!(material.cull_mode, None);
        }
    }
}
