//! Shared, root-local Norse furnishing meshes. Fixtures are owned by castle lighting.
use crate::net::StaticPropKind;
use bevy::prelude::*;

pub struct PropPart {
    pub mesh: Handle<Mesh>,
    pub material: Handle<StandardMaterial>,
}

pub struct PropModels {
    rows: Vec<(StaticPropKind, [Vec<PropPart>; 4])>,
}

const WOOD: usize = 0;
const IRON: usize = 1;
const CLOTH: usize = 2;
const BRASS: usize = 3;
const DETAIL: usize = 4;
const ROLES: usize = 5;
const KINDS: [StaticPropKind; 16] = {
    use StaticPropKind::*;
    [
        BanquetTable,
        Chair,
        Bench,
        Throne,
        Bookcase,
        Desk,
        Counter,
        Barrel,
        EquipmentRack,
        CouncilTable,
        Rug,
        Runner,
        Banner,
        Shield,
        Trophy,
        FeastSetting,
    ]
};

impl PropModels {
    pub fn build(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> Self {
        let mut invariant_materials: [Option<Handle<StandardMaterial>>; ROLES] =
            std::array::from_fn(|_| None);
        let palettes: [[Handle<StandardMaterial>; ROLES]; 4] = std::array::from_fn(|v| {
            let wood = [
                [0.25, 0.105, 0.043],
                [0.34, 0.18, 0.075],
                [0.18, 0.105, 0.072],
                [0.39, 0.225, 0.105],
            ][v];
            let cloth = [
                [0.40, 0.035, 0.055],
                [0.035, 0.12, 0.28],
                [0.07, 0.24, 0.16],
                [0.45, 0.26, 0.055],
            ][v];
            std::array::from_fn(|role| {
                if let Some(shared) = &invariant_materials[role] {
                    return shared.clone();
                }
                let rgb = match role {
                    WOOD => wood,
                    IRON => [0.075, 0.085, 0.10],
                    CLOTH => cloth,
                    BRASS => [0.56, 0.34, 0.10],
                    _ => [1.0; 3],
                };
                let material = materials.add(StandardMaterial {
                    base_color: Color::srgb(rgb[0], rgb[1], rgb[2]),
                    perceptual_roughness: if role == BRASS { 0.4 } else { 0.8 },
                    metallic: if role == IRON || role == BRASS {
                        0.7
                    } else {
                        0.0
                    },
                    ..default()
                });
                if role != WOOD && role != CLOTH {
                    invariant_materials[role] = Some(material.clone());
                }
                material
            })
        });
        let rows = KINDS
            .into_iter()
            .map(|kind| {
                let geometry = geometry(kind);
                let handles: Vec<_> = geometry
                    .meshes
                    .into_iter()
                    .enumerate()
                    .filter_map(|(role, mesh)| mesh.map(|mesh| (role, meshes.add(mesh))))
                    .collect();
                let variants = std::array::from_fn(|variant| {
                    handles
                        .iter()
                        .map(|(role, mesh)| PropPart {
                            mesh: mesh.clone(),
                            material: palettes[variant][*role].clone(),
                        })
                        .collect()
                });
                (kind, variants)
            })
            .collect();
        Self { rows }
    }

    pub fn parts(&self, kind: StaticPropKind, variant: u8) -> &[PropPart] {
        self.rows
            .iter()
            .find(|row| row.0 == kind)
            .and_then(|row| row.1.get(usize::from(variant)))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[derive(Default)]
struct Assembly {
    meshes: [Option<Mesh>; ROLES],
}
impl Assembly {
    fn add(&mut self, role: usize, mut mesh: Mesh, color: [f32; 4]) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![color; mesh.count_vertices()]);
        if let Some(existing) = &mut self.meshes[role] {
            existing
                .merge(&mesh)
                .expect("uniform furnishing mesh attributes");
        } else {
            self.meshes[role] = Some(mesh);
        }
    }
    fn block(&mut self, role: usize, lo: [f32; 3], hi: [f32; 3]) {
        self.tint_box(role, lo, hi, [1.0; 4]);
    }
    fn tint_box(&mut self, role: usize, lo: [f32; 3], hi: [f32; 3], color: [f32; 4]) {
        let lo = Vec3::from_array(lo);
        let hi = Vec3::from_array(hi);
        self.add(
            role,
            Mesh::from(Cuboid::from_size(hi - lo)).translated_by((lo + hi) * 0.5),
            color,
        );
    }
    fn beam(&mut self, role: usize, a: Vec3, b: Vec3, width: f32) {
        let delta = b - a;
        self.add(
            role,
            Mesh::from(Cuboid::new(width, delta.length(), width)).transformed_by(
                Transform::from_translation((a + b) * 0.5)
                    .with_rotation(Quat::from_rotation_arc(Vec3::Y, delta.normalize())),
            ),
            [1.0; 4],
        );
    }
}

fn geometry(kind: StaticPropKind) -> Assembly {
    use StaticPropKind::*;
    let mut a = Assembly::default();
    // Major visible members use the same extents as the authoritative solid catalogue.
    // Bookcases use a filled shelving envelope; books and shelves fill its front.
    if kind != Bookcase {
        for &(lo, hi) in solid_members(kind) {
            a.block(WOOD, lo, hi);
        }
    }
    match kind {
        BanquetTable => {
            for z in [-2.0, -1.0, 1.0, 2.0] {
                for x in [-0.48, 0.48] {
                    setting(&mut a, x, 1.0, z);
                }
            }
            a.block(CLOTH, [-0.16, 1.001, -2.32], [0.16, 1.012, 2.32]);
            for z in [-1.5, 1.5] {
                platter(&mut a, 0.0, 1.016, z);
            }
            table_trim(&mut a, 0.7, 2.4);
        }
        Chair => {
            a.block(CLOTH, [-0.29, 0.551, -0.30], [0.29, 0.595, 0.27]);
            a.block(CLOTH, [-0.25, 0.69, 0.262], [0.25, 1.10, 0.279]);
            rune(&mut a, 0.0, 1.03, 0.25, 0.11);
            for x in [-0.29, 0.29] {
                a.block(BRASS, [x - 0.025, 1.17, 0.266], [x + 0.025, 1.23, 0.281]);
            }
        }
        Bench => {
            a.block(CLOTH, [-0.28, 0.561, -1.67], [0.28, 0.585, 1.67]);
            for z in [-1.25, 1.25] {
                a.block(IRON, [-0.351, 0.45, z - 0.03], [0.351, 0.555, z + 0.03]);
            }
        }
        Throne => {
            a.block(CLOTH, [-0.45, 0.651, -0.39], [0.45, 0.71, 0.37]);
            a.block(CLOTH, [-0.42, 0.75, 0.353], [0.42, 1.91, 0.379]);
            for x in [-0.49, 0.49] {
                a.block(BRASS, [x - 0.025, 0.70, 0.345], [x + 0.025, 2.04, 0.381]);
            }
            for y in [0.79, 1.9] {
                a.block(BRASS, [-0.45, y, 0.34], [0.45, y + 0.028, 0.38]);
            }
            rune(&mut a, 0.0, 1.55, 0.326, 0.30);
            // Two faceted raven heads rise from the arm ends, inside the arm envelope.
            for x in [-0.575, 0.575] {
                a.block(BRASS, [x - 0.06, 0.91, -0.36], [x + 0.06, 0.98, -0.22]);
                a.block(IRON, [x - 0.037, 0.93, -0.39], [x + 0.037, 0.963, -0.34]);
            }
        }
        Bookcase => {
            a.block(WOOD, [-0.9, 0.0, 0.20], [0.9, 2.3, 0.28]);
            for x in [-0.9, 0.80] {
                a.block(WOOD, [x, 0.0, -0.28], [x + 0.1, 2.3, 0.2]);
            }
            for y in [0.0, 0.55, 1.1, 1.65, 2.22] {
                a.block(WOOD, [-0.8, y, -0.28], [0.8, y + 0.08, 0.2]);
            }
            for row in 0..4 {
                for col in 0..11 {
                    let x = -0.76 + col as f32 * 0.139;
                    let y = row as f32 * 0.55 + 0.08;
                    let h = 0.32 + ((row * 7 + col * 3) % 4) as f32 * 0.035;
                    let colors = [
                        [0.45, 0.08, 0.055, 1.0],
                        [0.06, 0.19, 0.23, 1.0],
                        [0.39, 0.29, 0.12, 1.0],
                        [0.15, 0.23, 0.11, 1.0],
                    ];
                    a.tint_box(
                        DETAIL,
                        [x, y, -0.24],
                        [x + 0.116, y + h, 0.17],
                        colors[(row + col) % 4],
                    );
                    for dy in [0.06, h - 0.05] {
                        a.block(
                            BRASS,
                            [x + 0.006, y + dy, -0.249],
                            [x + 0.110, y + dy + 0.012, -0.241],
                        );
                    }
                }
            }
        }
        Desk => {
            parchment(&mut a, 0.0, 1.007, 0.0, 0.56, 0.28);
            cup(&mut a, 0.66, 1.0, 0.15, 0.06);
            table_trim(&mut a, 0.9, 0.45);
        }
        Counter => {
            for z in [-1.10, -0.70, -0.30, 0.10, 0.50, 0.90] {
                a.block(IRON, [-0.656, 0.10, z], [-0.65, 0.79, z + 0.018]);
                a.block(IRON, [0.65, 0.10, z], [0.656, 0.79, z + 0.018]);
            }
            platter(&mut a, 0.0, 1.0, -0.6);
            cup(&mut a, 0.0, 1.0, 0.65, 0.13);
        }
        Barrel => {
            for y in [0.23, 0.78] {
                a.block(IRON, [-0.405, y, -0.405], [0.405, y + 0.07, -0.395]);
                a.block(IRON, [-0.405, y, 0.395], [0.405, y + 0.07, 0.405]);
                for x in [-0.405, 0.395] {
                    a.block(IRON, [x, y, -0.395], [x + 0.01, y + 0.07, 0.395]);
                }
            }
            for x in [-0.20, 0.0, 0.20] {
                a.block(IRON, [x - 0.006, 1.100, -0.31], [x + 0.006, 1.104, 0.31]);
            }
        }
        EquipmentRack => {
            for x in [-0.60, 0.0, 0.60] {
                a.block(WOOD, [x - 0.026, 0.36, -0.18], [x + 0.026, 1.85, -0.13]);
                a.block(IRON, [x - 0.09, 1.26, -0.21], [x + 0.09, 1.80, -0.19]);
                a.block(BRASS, [x - 0.16, 1.21, -0.23], [x + 0.16, 1.25, -0.16]);
            }
        }
        CouncilTable => {
            table_trim(&mut a, 1.2, 1.5);
            parchment(&mut a, 0.0, 1.006, 0.0, 0.9, 1.10);
            for (x, z) in [(-0.4, -0.7), (0.3, 0.4), (-0.2, 0.8)] {
                a.block(
                    IRON,
                    [x - 0.035, 1.022, z - 0.035],
                    [x + 0.035, 1.13, z + 0.035],
                );
            }
        }
        Rug => carpet(&mut a, 1.5, 2.0, 0.015),
        Runner => carpet(&mut a, 0.7, 2.5, 0.018),
        Banner => {
            a.block(CLOTH, [-0.6, 0.3, -0.012], [0.6, 2.3, 0.012]);
            for x in [-0.56, 0.52] {
                a.block(BRASS, [x, 0.32, -0.02], [x + 0.04, 2.26, -0.012]);
            }
            a.block(IRON, [-0.65, 2.29, -0.027], [0.65, 2.34, 0.027]);
            rune(&mut a, 0.0, 1.43, -0.024, 0.34);
        }
        Shield => shield(&mut a, 0.0, 0.5, 0.0, 0.46),
        Trophy => {
            shield(&mut a, 0.0, 0.5, 0.18, 0.32);
            a.tint_box(
                DETAIL,
                [-0.14, 0.30, -0.10],
                [0.14, 0.70, 0.13],
                [0.77, 0.71, 0.53, 1.0],
            );
            for sign in [-1.0, 1.0] {
                a.beam(
                    BRASS,
                    Vec3::new(sign * 0.10, 0.62, 0.0),
                    Vec3::new(sign * 0.39, 0.87, 0.03),
                    0.07,
                );
                a.beam(
                    BRASS,
                    Vec3::new(sign * 0.39, 0.87, 0.03),
                    Vec3::new(sign * 0.43, 0.99, 0.06),
                    0.035,
                );
            }
        }
        FeastSetting => setting(&mut a, 0.0, 0.0, 0.0),
        WallSconce | FloorCandelabrum | TableCandelabrum => {}
    }
    a
}

fn table_trim(a: &mut Assembly, x: f32, z: f32) {
    for sign in [-1.0, 1.0] {
        let edge = sign * (x - 0.016);
        a.block(IRON, [edge - 0.014, 0.88, -z], [edge + 0.014, 0.96, z]);
        for along in [-z + 0.15, 0.0, z - 0.15] {
            a.block(
                BRASS,
                [edge - 0.02, 0.899, along - 0.024],
                [edge + 0.02, 0.941, along + 0.024],
            );
        }
    }
}
fn rune(a: &mut Assembly, x: f32, y: f32, z: f32, s: f32) {
    let p = |dx, dy| Vec3::new(x + dx * s, y + dy * s, z);
    a.beam(BRASS, p(0.0, -1.0), p(0.0, 1.0), s * 0.13);
    a.beam(BRASS, p(0.0, 0.15), p(-0.65, 0.75), s * 0.13);
    a.beam(BRASS, p(0.0, -0.25), p(0.65, 0.4), s * 0.13);
}
fn carpet(a: &mut Assembly, x: f32, z: f32, y: f32) {
    a.block(CLOTH, [-x, 0.002, -z], [x, y, z]);
    for side in [-1.0, 1.0] {
        let edge = side * (x - 0.10);
        a.block(
            BRASS,
            [edge - 0.025, y, -z + 0.08],
            [edge + 0.025, y + 0.003, z - 0.08],
        );
        let end = side * (z - 0.10);
        a.block(
            BRASS,
            [-x + 0.08, y, end - 0.025],
            [x - 0.08, y + 0.003, end + 0.025],
        );
        for i in 0..9 {
            let t = -x + 0.12 + i as f32 * (2.0 * x - 0.24) / 8.0;
            a.tint_box(
                DETAIL,
                [t - 0.014, 0.003, side * z - 0.04],
                [t + 0.014, y, side * z + 0.04],
                [0.64, 0.51, 0.31, 1.0],
            );
        }
    }
    for sign in [-1.0, 1.0] {
        a.beam(
            BRASS,
            Vec3::new(0.0, y + 0.006, sign * 0.48),
            Vec3::new(x * 0.5, y + 0.006, 0.0),
            0.025,
        );
        a.beam(
            BRASS,
            Vec3::new(0.0, y + 0.006, sign * 0.48),
            Vec3::new(-x * 0.5, y + 0.006, 0.0),
            0.025,
        );
    }
}
fn setting(a: &mut Assembly, x: f32, y: f32, z: f32) {
    // Shallow pewter plate, bread and roast portions; all stay within one place setting.
    a.tint_box(
        DETAIL,
        [x - 0.15, y + 0.006, z - 0.18],
        [x + 0.15, y + 0.026, z + 0.18],
        [0.44, 0.48, 0.49, 1.0],
    );
    a.tint_box(
        DETAIL,
        [x - 0.11, y + 0.026, z - 0.10],
        [x + 0.07, y + 0.083, z + 0.03],
        [0.38, 0.16, 0.07, 1.0],
    );
    a.tint_box(
        DETAIL,
        [x - 0.09, y + 0.026, z + 0.05],
        [x + 0.09, y + 0.091, z + 0.14],
        [0.70, 0.41, 0.14, 1.0],
    );
    cup(a, x, y, z - 0.31, 0.060);
    a.block(
        IRON,
        [x - 0.205, y + 0.012, z - 0.11],
        [x - 0.185, y + 0.028, z + 0.13],
    );
}
fn cup(a: &mut Assembly, x: f32, y: f32, z: f32, r: f32) {
    // Hollow square-cut cup: four thin walls, a closed bottom and a dark liquid surface.
    let t = r * 0.23;
    let h = r * 2.1;
    a.block(BRASS, [x - r, y, z - r], [x + r, y + t, z + r]);
    for s in [-1.0, 1.0] {
        let ex = x + s * (r - t * 0.5);
        let ez = z + s * (r - t * 0.5);
        a.block(
            BRASS,
            [ex - t * 0.5, y + t, z - r],
            [ex + t * 0.5, y + h, z + r],
        );
        a.block(
            BRASS,
            [x - r + t, y + t, ez - t * 0.5],
            [x + r - t, y + h, ez + t * 0.5],
        );
    }
    a.tint_box(
        DETAIL,
        [x - r + t, y + h - t * 2.0, z - r + t],
        [x + r - t, y + h - t * 1.8, z + r - t],
        [0.15, 0.04, 0.02, 1.0],
    );
}
fn platter(a: &mut Assembly, x: f32, y: f32, z: f32) {
    a.block(
        BRASS,
        [x - 0.24, y, z - 0.31],
        [x + 0.24, y + 0.025, z + 0.31],
    );
    let colors = if z < 0.0 {
        [
            [0.24, 0.065, 0.022, 1.0],
            [0.53, 0.26, 0.075, 1.0],
            [0.31, 0.10, 0.03, 1.0],
        ]
    } else {
        [
            [0.44, 0.025, 0.018, 1.0],
            [0.13, 0.30, 0.035, 1.0],
            [0.64, 0.34, 0.055, 1.0],
        ]
    };
    for ((dx, dz), color) in [(-0.11, -0.17), (0.08, 0.0), (-0.08, 0.16)]
        .into_iter()
        .zip(colors)
    {
        a.tint_box(
            DETAIL,
            [x + dx - 0.075, y + 0.025, z + dz - 0.09],
            [x + dx + 0.075, y + 0.15, z + dz + 0.09],
            color,
        );
    }
    a.tint_box(
        DETAIL,
        [x - 0.19, y + 0.026, z + 0.04],
        [x - 0.10, y + 0.042, z + 0.23],
        [0.075, 0.23, 0.035, 1.0],
    );
}
fn parchment(a: &mut Assembly, x: f32, y: f32, z: f32, hx: f32, hz: f32) {
    a.tint_box(
        DETAIL,
        [x - hx, y, z - hz],
        [x + hx, y + 0.008, z + hz],
        [0.73, 0.65, 0.42, 1.0],
    );
    for sign in [-1.0, 1.0] {
        a.tint_box(
            DETAIL,
            [x + sign * hx - 0.025, y, z - hz],
            [x + sign * hx + 0.025, y + 0.045, z + hz],
            [0.63, 0.53, 0.30, 1.0],
        );
    }
    // Deliberately angular coastline and roads, visible without a new texture asset.
    for i in 0..7 {
        let t = i as f32 / 7.0;
        let px = x - hx * 0.7 + t * hx * 1.3;
        let pz = z - hz * 0.65 + t * hz * 1.2;
        a.tint_box(
            DETAIL,
            [px, y + 0.009, pz],
            [px + hx * 0.18, y + 0.012, pz + 0.015],
            [0.15, 0.25, 0.23, 1.0],
        );
    }
}
fn shield(a: &mut Assembly, x: f32, y: f32, z: f32, r: f32) {
    // Crossed boards form the stepped silhouette of a ceremonial shield.
    a.block(
        CLOTH,
        [x - r, y - r * 0.55, z - 0.04],
        [x + r, y + r * 0.55, z + 0.04],
    );
    a.block(
        CLOTH,
        [x - r * 0.55, y - r, z - 0.04],
        [x + r * 0.55, y + r, z + 0.04],
    );
    a.block(
        IRON,
        [x - 0.06, y - r, z - 0.055],
        [x + 0.06, y + r, z - 0.04],
    );
    a.block(
        BRASS,
        [x - 0.10, y - 0.10, z - 0.11],
        [x + 0.10, y + 0.10, z - 0.055],
    );
}

/// Physical member extents, mirrored from the world catalogue; cosmetic additions are thin.
pub(super) fn solid_members(kind: StaticPropKind) -> &'static [([f32; 3], [f32; 3])] {
    use StaticPropKind::*;
    match kind {
        BanquetTable => &[
            ([-0.700, 0.860, -2.400], [0.700, 1.000, 2.400]),
            ([-0.620, 0.000, -2.240], [-0.440, 0.860, -2.060]),
            ([-0.620, 0.000, 2.060], [-0.440, 0.860, 2.240]),
            ([0.440, 0.000, -2.240], [0.620, 0.860, -2.060]),
            ([0.440, 0.000, 2.060], [0.620, 0.860, 2.240]),
        ],
        Chair => &[
            ([-0.350, 0.420, -0.380], [0.350, 0.550, 0.380]),
            ([-0.350, 0.550, 0.280], [0.350, 1.250, 0.400]),
            ([-0.305, 0.000, -0.325], [-0.175, 0.420, -0.195]),
            ([-0.305, 0.000, 0.195], [-0.175, 0.420, 0.325]),
            ([0.175, 0.000, -0.325], [0.305, 0.420, -0.195]),
            ([0.175, 0.000, 0.195], [0.305, 0.420, 0.325]),
        ],
        Bench => &[
            ([-0.350, 0.430, -1.800], [0.350, 0.560, 1.800]),
            ([-0.280, 0.000, -1.400], [0.280, 0.430, -1.200]),
            ([-0.280, 0.000, 1.200], [0.280, 0.430, 1.400]),
        ],
        Throne => &[
            ([-0.480, 0.000, -0.400], [0.480, 0.500, 0.400]),
            ([-0.550, 0.500, -0.480], [0.550, 0.650, 0.480]),
            ([-0.550, 0.650, 0.380], [0.550, 2.100, 0.520]),
            ([-0.650, 0.650, -0.380], [-0.500, 0.950, 0.500]),
            ([0.500, 0.650, -0.380], [0.650, 0.950, 0.500]),
        ],
        Bookcase => &[([-0.900, 0.000, -0.280], [0.900, 2.300, 0.280])],
        Desk => &[
            ([-0.900, 0.860, -0.450], [0.900, 1.000, 0.450]),
            ([-0.800, 0.000, -0.360], [-0.640, 0.860, -0.200]),
            ([-0.800, 0.000, 0.200], [-0.640, 0.860, 0.360]),
            ([0.640, 0.000, -0.360], [0.800, 0.860, -0.200]),
            ([0.640, 0.000, 0.200], [0.800, 0.860, 0.360]),
        ],
        Counter => &[
            ([-0.650, 0.000, -1.300], [0.650, 0.880, 1.300]),
            ([-0.700, 0.880, -1.400], [0.700, 1.000, 1.400]),
        ],
        Barrel => &[
            ([-0.320, 0.000, -0.320], [0.320, 0.200, 0.320]),
            ([-0.400, 0.200, -0.400], [0.400, 0.900, 0.400]),
            ([-0.320, 0.900, -0.320], [0.320, 1.100, 0.320]),
        ],
        EquipmentRack => &[
            ([-0.950, 0.000, -0.250], [-0.800, 1.950, 0.250]),
            ([0.800, 0.000, -0.250], [0.950, 1.950, 0.250]),
            ([-0.950, 0.500, -0.180], [0.950, 0.650, 0.180]),
            ([-0.950, 1.650, -0.180], [0.950, 1.800, 0.180]),
        ],
        CouncilTable => &[
            ([-1.200, 0.860, -1.500], [1.200, 1.000, 1.500]),
            ([-1.060, 0.000, -1.340], [-0.860, 0.860, -1.140]),
            ([-1.060, 0.000, 1.140], [-0.860, 0.860, 1.340]),
            ([0.860, 0.000, -1.340], [1.060, 0.860, -1.140]),
            ([0.860, 0.000, 1.140], [1.060, 0.860, 1.340]),
        ],
        Rug | Runner | Banner | Shield | Trophy | FeastSetting | WallSconce | FloorCandelabrum
        | TableCandelabrum => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    #[test]
    fn palette_variants_share_meshes_and_fixture_roots_remain_empty() {
        let mut meshes = Assets::<Mesh>::default();
        let mut materials = Assets::<StandardMaterial>::default();
        let models = PropModels::build(&mut meshes, &mut materials);
        let mut unique = std::collections::HashSet::new();
        for kind in KINDS {
            let canonical = models.parts(kind, 0);
            assert!(!canonical.is_empty(), "missing {kind:?}");
            assert!(canonical.len() <= ROLES);
            for part in canonical {
                unique.insert(part.mesh.id());
            }
            for variant in 1..4 {
                let parts = models.parts(kind, variant);
                assert_eq!(parts.len(), canonical.len());
                for (part, base) in parts.iter().zip(canonical) {
                    assert_eq!(
                        part.mesh.id(),
                        base.mesh.id(),
                        "duplicate geometry {kind:?}"
                    );
                }
            }
            assert!(models.parts(kind, 4).is_empty());
        }
        assert_eq!(meshes.len(), unique.len());
        assert_eq!(materials.len(), 11);
        for kind in [
            StaticPropKind::WallSconce,
            StaticPropKind::FloorCandelabrum,
            StaticPropKind::TableCandelabrum,
        ] {
            for variant in 0..4 {
                assert!(models.parts(kind, variant).is_empty());
            }
        }
    }

    #[test]
    fn authored_triangles_have_finite_unit_normals_and_consistent_winding() {
        for kind in KINDS {
            for mesh in geometry(kind).meshes.into_iter().flatten() {
                let Some(VertexAttributeValues::Float32x3(positions)) =
                    mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                else {
                    panic!("positions");
                };
                let Some(VertexAttributeValues::Float32x3(normals)) =
                    mesh.attribute(Mesh::ATTRIBUTE_NORMAL)
                else {
                    panic!("normals");
                };
                let Some(VertexAttributeValues::Float32x4(colors)) =
                    mesh.attribute(Mesh::ATTRIBUTE_COLOR)
                else {
                    panic!("colors");
                };
                assert_eq!(positions.len(), normals.len());
                assert_eq!(positions.len(), colors.len());
                for (p, n) in positions.iter().zip(normals) {
                    assert!(Vec3::from_array(*p).is_finite(), "{kind:?}");
                    let n = Vec3::from_array(*n);
                    assert!(
                        n.is_finite() && (n.length() - 1.0).abs() < 0.001,
                        "{kind:?}"
                    );
                    assert!(p[1] >= -0.001, "model below floor: {kind:?}: {p:?}");
                }
                let indices: Vec<_> = mesh.indices().expect("indexed cuboids").iter().collect();
                for face in indices.chunks_exact(3) {
                    let [a, b, c] =
                        [face[0], face[1], face[2]].map(|i| Vec3::from_array(positions[i]));
                    let normal = Vec3::from_array(normals[face[0]]);
                    assert!(
                        (b - a).cross(c - a).dot(normal) > 0.0,
                        "inverted/degenerate {kind:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn major_members_match_authoritative_fixture_and_visual_extrema() {
        let fixture =
            include_str!("../../../../server/internal/world/testdata/static_prop_bounds.tsv");
        let mut checked = 0;
        for kind in KINDS {
            let name = format!("{kind:?}");
            let expected: Vec<_> = fixture
                .lines()
                .filter_map(|line| {
                    let mut cols = line.split_whitespace();
                    if cols.next() != Some(name.as_str()) {
                        return None;
                    }
                    let values: Vec<f32> = cols
                        .map(|v| v.parse().expect("fixture coordinate"))
                        .collect();
                    assert_eq!(values.len(), 6);
                    Some((
                        [values[0], values[1], values[2]],
                        [values[3], values[4], values[5]],
                    ))
                })
                .collect();
            assert_eq!(solid_members(kind), expected.as_slice(), "{kind:?}");
            checked += expected.len();
            if expected.is_empty() {
                continue;
            }
            let assembly = geometry(kind);
            let positions: Vec<_> = assembly
                .meshes
                .iter()
                .flatten()
                .flat_map(|mesh| {
                    let Some(VertexAttributeValues::Float32x3(p)) =
                        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
                    else {
                        panic!("positions");
                    };
                    p.iter().copied()
                })
                .collect();
            // Every member's six extremal planes have visible vertices. Bookcases are a
            // filled shelving envelope; internal gaps do not become a movement promise.
            for (lo, hi) in expected {
                for axis in 0..3 {
                    for bound in [lo[axis], hi[axis]] {
                        assert!(
                            positions.iter().any(|p| (p[axis] - bound).abs() < 0.0001),
                            "missing physical plane {kind:?}/{axis}/{bound}"
                        );
                    }
                }
            }
        }
        assert_eq!(checked, 39);
    }
}
