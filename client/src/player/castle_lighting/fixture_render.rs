//! Candle mesh cache and attachment seam shared by production roots and the review scene.
use super::models::{self, Fixture};
use bevy::prelude::*;

struct FixtureMeshes {
    holder: Handle<Mesh>,
    wax: Handle<Mesh>,
    wicks: Vec<Vec3>,
}

#[derive(Resource)]
pub(super) struct FixtureAssets {
    fixtures: [FixtureMeshes; 3],
    flame: Handle<Mesh>,
    holder: Handle<StandardMaterial>,
    wax: Handle<StandardMaterial>,
    emission: Handle<StandardMaterial>,
}

#[derive(Component)]
pub(super) struct CandleFlame {
    phase: f32,
}

fn index(kind: Fixture) -> usize {
    match kind {
        Fixture::Wall => 0,
        Fixture::Floor => 1,
        Fixture::Table => 2,
    }
}

impl FixtureAssets {
    pub fn build(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> Self {
        let fixtures = [Fixture::Wall, Fixture::Floor, Fixture::Table].map(|kind| {
            let model = models::build(kind);
            FixtureMeshes {
                holder: meshes.add(model.holder),
                wax: meshes.add(model.wax),
                wicks: model.wicks,
            }
        });
        Self {
            fixtures,
            flame: meshes.add(models::flame()),
            holder: materials.add(StandardMaterial {
                base_color: Color::srgb(0.11, 0.10, 0.08),
                metallic: 0.65,
                perceptual_roughness: 0.65,
                ..default()
            }),
            wax: materials.add(Color::srgb(0.92, 0.82, 0.61)),
            emission: materials.add(StandardMaterial {
                base_color: Color::srgb(1.0, 0.62, 0.18),
                emissive: LinearRgba::new(7.0, 2.6, 0.5, 1.0),
                ..default()
            }),
        }
    }

    /// Root pose and lifetime belong to the static-prop owner. Every authored part is a child.
    pub fn attach(&self, commands: &mut Commands, root: Entity, kind: Fixture, prop_id: u64) {
        let fixture = &self.fixtures[index(kind)];
        commands.entity(root).with_children(|children| {
            children.spawn((
                Mesh3d(fixture.holder.clone()),
                MeshMaterial3d(self.holder.clone()),
                Transform::default(),
            ));
            children.spawn((
                Mesh3d(fixture.wax.clone()),
                MeshMaterial3d(self.wax.clone()),
                Transform::default(),
            ));
            for (i, wick) in fixture.wicks.iter().enumerate() {
                let phase =
                    ((prop_id.wrapping_mul(31).wrapping_add(i as u64 * 17)) % 10007) as f32 * 0.01;
                children.spawn((
                    Mesh3d(self.flame.clone()),
                    MeshMaterial3d(self.emission.clone()),
                    Transform::from_translation(*wick),
                    CandleFlame { phase },
                    bevy::light::NotShadowCaster,
                ));
            }
        });
    }

    /// One shadowed light per fixture, centered on its wick group; never one cubemap per arm.
    pub fn light_origin(&self, kind: Fixture) -> Vec3 {
        let wicks = &self.fixtures[index(kind)].wicks;
        wicks.iter().copied().sum::<Vec3>() / wicks.len() as f32 + Vec3::Y * 0.05
    }
}

pub(super) fn animate_flames(time: Res<Time>, mut flames: Query<(&CandleFlame, &mut Transform)>) {
    let seconds = time.elapsed_secs();
    for (flame, mut transform) in &mut flames {
        // The authored flame starts at the wick; scaling around that point never detaches it.
        transform.scale = Vec3::new(
            1.0 + 0.025 * (seconds * 2.1 + flame.phase).sin(),
            1.0 + 0.04 * (seconds * 2.7 + flame.phase).sin(),
            1.0,
        );
    }
}

/// Initial tuning only; final production-geometry night captures decide the accepted values.
pub(super) fn light_settings(kind: Fixture) -> (f32, f32) {
    match kind {
        Fixture::Wall => (8000.0, 9.0),
        Fixture::Floor => (16000.0, 11.0),
        Fixture::Table => (12000.0, 9.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::CommandQueue;

    #[test]
    fn meshes_are_shared_and_root_removal_owns_every_fixture_child() {
        let mut meshes = Assets::<Mesh>::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let assets = FixtureAssets::build(&mut meshes, &mut materials);
        assert_eq!(meshes.len(), 7);
        assert_eq!(materials.len(), 3);
        let mut world = World::new();
        let roots: Vec<_> = (0..96)
            .map(|_| world.spawn(Transform::default()).id())
            .collect();
        let mut queue = CommandQueue::default();
        {
            let mut commands = Commands::new(&mut queue, &world);
            for (i, root) in roots.iter().enumerate() {
                assets.attach(&mut commands, *root, Fixture::Floor, i as u64);
            }
        }
        queue.apply(&mut world);
        assert_eq!(world.query::<&CandleFlame>().iter(&world).count(), 288);
        assert_eq!(world.query::<&Mesh3d>().iter(&world).count(), 480);
        assert_eq!(meshes.len(), 7);
        for root in roots {
            world.despawn(root);
        }
        assert_eq!(world.query::<&Mesh3d>().iter(&world).count(), 0);
        assert_eq!(world.query::<&CandleFlame>().iter(&world).count(), 0);
    }
}
