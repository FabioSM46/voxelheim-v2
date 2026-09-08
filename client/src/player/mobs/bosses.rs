//! Authored boss silhouettes. Only meshes and cosmetic poses live here.
use super::*;

pub(super) const IRON: Color = Color::srgb(0.27, 0.30, 0.32);
pub(super) const FROST: Color = Color::srgb(0.72, 0.81, 0.84);
pub(super) const BONE: Color = Color::srgb(0.72, 0.70, 0.63);
pub(super) const FUR: Color = Color::srgb(0.12, 0.14, 0.16);

pub(super) fn boxes(parts: &[(Vec3, Vec3, Color)]) -> Mesh {
    let mut mesh = draugr_box(parts[0].0, parts[0].1, parts[0].2);
    merge_all(
        &mut mesh,
        parts[1..]
            .iter()
            .map(|&(size, centre, colour)| draugr_box(size, centre, colour)),
        "boss segment",
    );
    mesh
}

#[cfg(test)]
pub(super) fn guardian_body() -> Mesh {
    let mut parts = vec![
        (Vec3::new(0.86, 0.48, 0.50), Vec3::new(0.0, 0.85, 0.38), FUR),
        (
            Vec3::new(1.20, 0.80, 0.70),
            Vec3::new(0.0, 1.18, -0.08),
            FUR,
        ),
        (
            Vec3::new(0.75, 0.20, 0.58),
            Vec3::new(0.0, 1.00, -0.25),
            IRON,
        ),
        (Vec3::new(0.18, 0.35, 0.20), Vec3::new(0.0, 0.67, 0.69), FUR),
    ];
    // Broken, uneven tufts form the shoulder silhouette; all merge into one draw.
    for (x, height) in [(-0.45, 0.25), (-0.2, 0.38), (0.08, 0.30), (0.34, 0.34)] {
        parts.push((
            Vec3::new(0.20, height - 0.045, 0.43),
            Vec3::new(x, 1.42 + (height - 0.045) / 2.0, -0.02),
            FUR,
        ));
        parts.push((
            Vec3::new(0.15, 0.045, 0.30),
            Vec3::new(x, 1.42 + height - 0.023, -0.02),
            FROST,
        ));
    }
    for x in [-0.35, 0.0, 0.35] {
        parts.push((Vec3::new(0.07, 0.30, 0.07), Vec3::new(x, 0.74, -0.50), IRON));
    }
    parts.push((
        Vec3::new(0.07, 0.28, 0.08),
        Vec3::new(-0.58, 1.37, -0.12),
        IRON,
    ));
    boxes(&parts)
}

#[cfg(test)]
pub(super) fn guardian_head() -> Mesh {
    boxes(&[
        (
            Vec3::new(0.62, 0.46, 0.40),
            Vec3::new(0.0, 0.97, -0.56),
            FUR,
        ),
        (
            Vec3::new(0.50, 0.13, 0.35),
            Vec3::new(0.0, 0.69, -0.60),
            FUR,
        ),
        (
            Vec3::new(0.05, 0.15, 0.06),
            Vec3::new(-0.20, 0.73, -0.765),
            BONE,
        ),
        (
            Vec3::new(0.06, 0.11, 0.06),
            Vec3::new(0.20, 0.75, -0.765),
            BONE,
        ),
        (
            Vec3::new(0.13, 0.19, 0.12),
            Vec3::new(-0.23, 1.24, -0.48),
            FUR,
        ),
        (
            Vec3::new(0.13, 0.10, 0.12),
            Vec3::new(0.23, 1.20, -0.48),
            FUR,
        ),
        (
            Vec3::new(0.07, 0.04, 0.025),
            Vec3::new(-0.17, 1.03, -0.775),
            Color::srgb(0.72, 0.43, 0.12),
        ),
        (
            Vec3::new(0.07, 0.04, 0.025),
            Vec3::new(0.17, 1.03, -0.775),
            Color::srgb(0.72, 0.43, 0.12),
        ),
    ])
}

#[cfg(test)]
pub(super) fn guardian_legs() -> Mesh {
    let mut parts = Vec::new();
    for x in [-0.58, 0.58] {
        for z in [-0.34, 0.39] {
            parts.push((Vec3::new(0.30, 0.70, 0.30), Vec3::new(x, 0.58, z), FUR));
            parts.push((Vec3::new(0.34, 0.30, 0.36), Vec3::new(x, 0.15, z), FUR));
            parts.push((
                Vec3::new(0.28, 0.06, 0.08),
                Vec3::new(x, 0.03, z - 0.15),
                BONE,
            ));
        }
    }
    boxes(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posed_meshes(kind: MobKind, action: MobAction) -> Vec<Mesh> {
        let mut meshes = if kind == MobKind::VargrGuardian {
            let down = if action == MobAction::Corpse {
                1.0
            } else {
                0.0
            };
            guardian::posed_meshes(action, Duration::from_secs(1), down)
        } else {
            king::posed_meshes(action, Duration::from_secs(1))
        };
        let rotation = if kind == MobKind::VargrGuardian {
            Quat::IDENTITY
        } else if action == MobAction::Corpse {
            collapse(kind, 1.0)
        } else {
            Quat::from_rotation_x(lean_for(kind, action))
        };
        for mesh in &mut meshes {
            *mesh = mesh.clone().rotated_by(rotation);
        }
        meshes
    }

    /// An opt-in review artifact made from the actual mesh vertices, not a second
    /// model. It needs no GPU/display; the reviewer opens the resulting SVG.
    #[test]
    #[ignore = "manual visual review; set VOXELHEIM_BOSS_REVIEW_PATH to an SVG destination"]
    fn export_boss_review_sheet() {
        use bevy::mesh::VertexAttributeValues;
        let mut svg = String::from(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1800 1000\"><rect width=\"1800\" height=\"1000\" fill=\"#d7d8d6\"/><g font-family=\"sans-serif\" font-size=\"18\" fill=\"#20272b\"><text x=\"25\" y=\"30\">Actual boss meshes: rest views, snapshot poses and gameplay angular size</text>",
        );
        for (row, kind) in [MobKind::VargrGuardian, MobKind::DraugrKing]
            .into_iter()
            .enumerate()
        {
            let measured = posed_meshes(kind, MobAction::Idle);
            let triangles: usize = measured
                .iter()
                .map(|m| m.indices().unwrap().len() / 3)
                .sum();
            let vertices: usize = measured.iter().map(Mesh::count_vertices).sum();
            println!(
                "{kind:?}: {} segments, {triangles} triangles, {vertices} vertices, 1 material, 0 effects",
                measured.len()
            );

            for (col, (label, yaw, action, distance)) in [
                ("front", 0.0, MobAction::Idle, 0.0),
                ("side", FRAC_PI_2, MobAction::Idle, 0.0),
                ("rear", std::f32::consts::PI, MobAction::Idle, 0.0),
                ("windup", 0.5, MobAction::Windup, 0.0),
                ("recovery", 0.5, MobAction::Recovery, 0.0),
                ("corpse", 0.5, MobAction::Corpse, 0.0),
                ("13 blocks", 0.0, MobAction::Idle, 13.0),
                ("25 blocks", 0.0, MobAction::Idle, 25.0),
                ("cast key", 0.5, MobAction::Idle, -1.0),
            ]
            .into_iter()
            .enumerate()
            {
                let scale = if distance > 0.0 {
                    1080.0
                        / (2.0
                            * (crate::settings::DEFAULT_FIELD_OF_VIEW.to_radians() / 2.0).tan()
                            * distance)
                } else {
                    75.0
                };
                let cx = 100.0 + col as f32 * 200.0;
                let base = 400.0 + row as f32 * 470.0;
                let rotation = Quat::from_rotation_y(yaw);
                let mut faces = Vec::new();
                let meshes = if distance < 0.0 {
                    if kind == MobKind::DraugrKing {
                        king::cast_meshes()
                    } else {
                        Vec::new()
                    }
                } else {
                    posed_meshes(kind, action)
                };
                for mesh in meshes {
                    let VertexAttributeValues::Float32x3(positions) =
                        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
                    else {
                        panic!("positions");
                    };
                    let VertexAttributeValues::Float32x4(colours) =
                        mesh.attribute(Mesh::ATTRIBUTE_COLOR).unwrap()
                    else {
                        panic!("colours");
                    };
                    let indices: Vec<_> = mesh.indices().unwrap().iter().collect();
                    for triangle in indices.chunks_exact(3) {
                        let points: Vec<_> = triangle
                            .iter()
                            .map(|&i| rotation * Vec3::from_array(positions[i]))
                            .collect();
                        let normal = (points[1] - points[0]).cross(points[2] - points[0]);
                        if normal.z >= 0.0 {
                            continue;
                        }
                        let rgba = colours[triangle[0]];
                        let c = Color::linear_rgba(rgba[0], rgba[1], rgba[2], rgba[3]).to_srgba();
                        let colour = format!(
                            "#{:02x}{:02x}{:02x}",
                            (c.red * 255.0) as u8,
                            (c.green * 255.0) as u8,
                            (c.blue * 255.0) as u8
                        );
                        let polygon = points
                            .iter()
                            .map(|p| format!("{:.2},{:.2}", cx + p.x * scale, base - p.y * scale))
                            .collect::<Vec<_>>()
                            .join(" ");
                        faces.push((
                            points.iter().map(|p| p.z).sum::<f32>(),
                            format!("<polygon points=\"{polygon}\" fill=\"{colour}\"/>"),
                        ));
                    }
                }
                faces.sort_by(|a, b| b.0.total_cmp(&a.0));
                for (_, face) in faces {
                    svg.push_str(&face);
                }
                svg.push_str(&format!(
                    "<text x=\"{cx}\" y=\"{}\" text-anchor=\"middle\">{label}</text>",
                    base + 95.0
                ));
            }
        }
        svg.push_str("<text x=\"25\" y=\"995\">Gameplay columns: 1080 vertical pixels, default vertical FOV; projected mesh study, not a GPU frame.</text></g></svg>");
        std::fs::write(
            std::env::var("VOXELHEIM_BOSS_REVIEW_PATH").expect("set review destination"),
            svg,
        )
        .unwrap();
    }

    // The authoring primitive has 24 vertices per opaque cuboid. Cast a ray
    // outward from each decorative face centre and reject it if another box
    // covers that sightline. Bounds alone cannot detect buried ornament.
    fn visible_caps(mesh: Mesh, colour: Color, axis: usize, positive: bool) -> usize {
        use bevy::mesh::VertexAttributeValues;
        let VertexAttributeValues::Float32x3(positions) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
        else {
            panic!("positions")
        };
        let VertexAttributeValues::Float32x4(colours) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR).unwrap()
        else {
            panic!("colours")
        };
        let boxes: Vec<_> = positions
            .chunks_exact(24)
            .enumerate()
            .map(|(i, vertices)| {
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = Vec3::splat(f32::NEG_INFINITY);
                for &v in vertices {
                    lo = lo.min(Vec3::from_array(v));
                    hi = hi.max(Vec3::from_array(v));
                }
                (lo, hi, colours[i * 24])
            })
            .collect();
        let wanted = colour.to_linear().to_f32_array();
        boxes
            .iter()
            .enumerate()
            .filter(|(i, (lo, hi, c))| {
                if *c != wanted {
                    return false;
                }
                let mut face = (*lo + *hi) / 2.0;
                face[axis] = if positive { hi[axis] } else { lo[axis] };
                !boxes
                    .iter()
                    .enumerate()
                    .any(|(j, (other_lo, other_hi, _))| {
                        j != *i
                            && (0..3)
                                .filter(|&a| a != axis)
                                .all(|a| face[a] > other_lo[a] && face[a] < other_hi[a])
                            && if positive {
                                other_hi[axis] > face[axis] + 1e-5
                            } else {
                                other_lo[axis] < face[axis] - 1e-5
                            }
                    })
            })
            .count()
    }

    #[test]
    fn guardian_frost_and_fangs_have_unoccluded_outer_faces() {
        let all = guardian::meshes();
        let mut mesh = all[0].1.clone();
        merge_all(
            &mut mesh,
            all.into_iter().skip(1).map(|(_, m)| m),
            "actual guardian",
        );
        assert_eq!(visible_caps(mesh.clone(), FROST, 1, true), 4);
        let head = guardian::meshes()
            .into_iter()
            .find(|(s, _)| *s == guardian::Segment::Head)
            .unwrap()
            .1;
        assert_eq!(visible_caps(head, BONE, 2, false), 2);
    }

    #[test]
    fn authored_boss_rest_meshes_fit_the_existing_body_boxes() {
        for (kind, meshes) in [
            (
                MobKind::VargrGuardian,
                guardian::meshes()
                    .into_iter()
                    .map(|(_, mesh)| mesh)
                    .collect::<Vec<_>>(),
            ),
            (
                MobKind::DraugrKing,
                king::meshes().into_iter().map(|(_, mesh)| mesh).collect(),
            ),
        ] {
            let envelope = body(kind);
            for mesh in meshes {
                let bevy::mesh::VertexAttributeValues::Float32x3(positions) =
                    mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
                else {
                    panic!("positions");
                };
                for &[x, y, z] in positions {
                    assert!(
                        x.abs() <= envelope.width / 2.0 + 1e-5
                            && z.abs() <= envelope.width / 2.0 + 1e-5
                            && y >= -1e-5
                            && y <= envelope.height + 1e-5,
                        "{kind:?}: {x},{y},{z}"
                    );
                }
            }
            assert_ne!(collapse(kind, 1.0), Quat::IDENTITY);
            assert_eq!(collapse(kind, 0.0), Quat::IDENTITY);
            assert_eq!(lean_for(kind, MobAction::Corpse), 0.0);
        }
    }
}
