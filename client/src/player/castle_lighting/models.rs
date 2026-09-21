//! Shared authored candle fixtures in their support frame: floor/table at Y=0,
//! wall at Z=+0.5, front toward -Z. The static-prop root rotates this whole frame.
use bevy::prelude::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum Fixture {
    Wall,
    Floor,
    Table,
}

pub(super) struct Model {
    pub holder: Mesh,
    pub wax: Mesh,
    pub wicks: Vec<Vec3>,
}

fn cuboid(size: Vec3, centre: Vec3) -> Mesh {
    Mesh::from(Cuboid::from_size(size)).translated_by(centre)
}

pub(super) fn build(kind: Fixture) -> Model {
    let (mut holder, candle_base, xs, z) = match kind {
        Fixture::Wall => {
            // The back of the plate touches the wall; the bracket reaches beneath the candle.
            let mut mesh = cuboid(Vec3::new(0.20, 0.50, 0.06), Vec3::new(0.0, 0.35, 0.47));
            mesh.merge(&cuboid(
                Vec3::new(0.06, 0.06, 0.46),
                Vec3::new(0.0, 0.16, 0.24),
            ))
            .expect("cuboid layout");
            mesh.merge(&cuboid(
                Vec3::new(0.22, 0.06, 0.22),
                Vec3::new(0.0, 0.20, 0.04),
            ))
            .expect("cuboid layout");
            (mesh, 0.23, vec![0.0], 0.04)
        }
        Fixture::Floor => {
            let mut mesh = cuboid(Vec3::new(0.48, 0.10, 0.48), Vec3::new(0.0, 0.05, 0.0));
            mesh.merge(&cuboid(
                Vec3::new(0.08, 1.20, 0.08),
                Vec3::new(0.0, 0.70, 0.0),
            ))
            .expect("cuboid layout");
            mesh.merge(&cuboid(
                Vec3::new(0.80, 0.07, 0.07),
                Vec3::new(0.0, 1.30, 0.0),
            ))
            .expect("cuboid layout");
            (mesh, 1.40, vec![-0.34, 0.0, 0.34], 0.0)
        }
        Fixture::Table => {
            let mut mesh = cuboid(Vec3::new(0.40, 0.06, 0.28), Vec3::new(0.0, 0.03, 0.0));
            mesh.merge(&cuboid(
                Vec3::new(0.06, 0.24, 0.06),
                Vec3::new(0.0, 0.18, 0.0),
            ))
            .expect("cuboid layout");
            mesh.merge(&cuboid(
                Vec3::new(0.64, 0.06, 0.06),
                Vec3::new(0.0, 0.30, 0.0),
            ))
            .expect("cuboid layout");
            (mesh, 0.40, vec![-0.27, 0.0, 0.27], 0.0)
        }
    };
    let mut wax: Option<Mesh> = None;
    let mut wicks = Vec::new();
    for x in xs {
        // Each arm ends in a cup. Its stem closes the gap from the horizontal arm.
        if !matches!(kind, Fixture::Wall) {
            holder
                .merge(&cuboid(
                    Vec3::new(0.05, 0.10, 0.05),
                    Vec3::new(x, candle_base - 0.08, z),
                ))
                .expect("cuboid layout");
            holder
                .merge(&cuboid(
                    Vec3::new(0.17, 0.05, 0.17),
                    Vec3::new(x, candle_base - 0.025, z),
                ))
                .expect("cuboid layout");
        }
        let candle = cuboid(
            Vec3::new(0.09, 0.24, 0.09),
            Vec3::new(x, candle_base + 0.12, z),
        );
        if let Some(wax) = wax.as_mut() {
            wax.merge(&candle).expect("cuboid layout");
        } else {
            wax = Some(candle);
        }
        let wick = Vec3::new(x, candle_base + 0.265, z);
        holder
            .merge(&cuboid(
                Vec3::new(0.018, 0.025, 0.018),
                wick - Vec3::Y * 0.0125,
            ))
            .expect("cuboid layout");
        wicks.push(wick);
    }
    Model {
        holder,
        wax: wax.expect("each fixture has candles"),
        wicks,
    }
}

/// The flame's bottom touches the top of its wick. Motion scales around this origin.
pub(super) fn flame() -> Mesh {
    Mesh::from(Cone::new(0.045, 0.16)).translated_by(Vec3::Y * 0.08)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    fn positions(mesh: &Mesh) -> &[[f32; 3]] {
        let Some(VertexAttributeValues::Float32x3(values)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("fixture positions")
        };
        values
    }

    #[test]
    fn candles_meet_wicks_and_fixture_bases_meet_their_supports() {
        for kind in [Fixture::Wall, Fixture::Floor, Fixture::Table] {
            let model = build(kind);
            assert_eq!(
                model.wicks.len(),
                if matches!(kind, Fixture::Wall) { 1 } else { 3 }
            );
            for wick in model.wicks {
                let wax_top = positions(&model.wax)
                    .iter()
                    .filter(|p| (p[0] - wick.x).abs() < 0.05 && (p[2] - wick.z).abs() < 0.05)
                    .map(|p| p[1])
                    .fold(f32::NEG_INFINITY, f32::max);
                assert!(
                    (wax_top + 0.025 - wick.y).abs() < 1e-5,
                    "wick floats above wax"
                );
                assert!(
                    positions(&model.holder)
                        .iter()
                        .any(|p| (p[0] - wick.x).abs() < 0.01
                            && (p[2] - wick.z).abs() < 0.01
                            && (p[1] - wick.y).abs() < 1e-5),
                    "flame lacks wick"
                );
            }
            if matches!(kind, Fixture::Wall) {
                assert!(
                    (positions(&model.holder)
                        .iter()
                        .map(|p| p[2])
                        .fold(f32::NEG_INFINITY, f32::max)
                        - 0.5)
                        .abs()
                        < 1e-5
                );
            } else {
                assert!(
                    positions(&model.holder)
                        .iter()
                        .map(|p| p[1])
                        .fold(f32::INFINITY, f32::min)
                        .abs()
                        < 1e-5
                );
            }
        }
        assert!(
            positions(&flame())
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .abs()
                < 1e-5
        );
    }
}
