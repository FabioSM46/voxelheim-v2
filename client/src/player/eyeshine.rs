//! Eyes that read in the dark: a pair of small emissive faces, and nothing else.
//!
//! ## Why this is a module rather than a few lines in `birds.rs`
//!
//! Every creature model in this client is built in code — there is no art pipeline to load
//! an asset with — and every one of them is **lit**: `player/birds.rs`'s plumage material
//! says so in as many words, and the reason is that a bird is an object in the world rather
//! than a light in it. That answer is right for a bird in daylight and wrong for the first
//! creature that is only ever seen after dark. An owl on a treetop at midnight is a dark
//! shape against a dark wood, and the one part of it a night eye actually finds is the pair
//! of eyes.
//!
//! Three creatures need exactly that presentation, and they live in three different modules:
//! the owl in `player/birds.rs` (#1191), and the mice and the distant wolves that
//! #1192 is about, which are ground creatures and not birds at all. So the pair of faces,
//! the material that makes them glow and the geometry that places them on a head are here,
//! parameterised by [`Eyeshine`], rather than authored once inside whichever module happened
//! to land first.
//!
//! ## Emissive, not unlit, and not a light
//!
//! `player/structures.rs`'s rune states the house answer for a thing that glows: a
//! `base_color` beside an `emissive`, which **reads as lit from its own material and costs
//! no light**. That is what is wanted here — an eye that catches the light is not a lamp, it
//! does not illuminate the branch it is sitting on, and adding a `PointLight` per creature
//! would be a per-entity cost for a quarter of a degree of screen.
//!
//! `unlit: true` — `player/precipitation.rs`'s answer for snow, and `player/saddle.rs`'s for
//! the world horse's eye — is the other candidate and is deliberately not used. It discards
//! the lighting entirely, so an unlit eye is exactly as bright at noon as at midnight, which
//! is the one thing this presentation must not be: the glint is a night-time reading and a
//! daylight owl should simply have dark eyes like everything else. Lit plus emissive gives
//! both — the base colour darkens with the sky and the emissive term is what survives it.
//!
//! ## The fade reaches the glow on its own, and must not be applied here
//!
//! A fading creature whose eyes kept their full glow would leave two bright dots hanging in
//! the air after the body had gone, so the glow has to follow the fade. **It already does, and
//! scaling it here as well is the bug that was nearly shipped.** Two places apply the alpha to
//! the emissive term before anything of ours could:
//!
//! - `bevy_pbr`'s own shader, in `pbr_functions.wgsl`: `var emissive_light = emissive.rgb *
//!   output_color.a;` — the emissive is multiplied by the base colour's alpha inside the
//!   fragment shader.
//! - The fixed-function blend. [`AlphaMode::Blend`] resolves to `MeshPipelineKey::BLEND_ALPHA`
//!   and then to `BlendState::ALPHA_BLENDING` (`bevy_pbr/src/render/mesh.rs`), whose colour
//!   factors are `SrcAlpha` / `OneMinusSrcAlpha` — so the whole fragment output, the emissive
//!   included, is multiplied by the alpha a second time.
//!
//! So the glow already reaches the screen at `glow × alpha²`. [`eyeshine_material`] therefore
//! passes `glow` through **unscaled**; multiplying it here would make it `glow × alpha³` and
//! the eyes would vanish long before the body they belong to — the opposite of the intent.
//!
//! `player/projectiles.rs` does scale its orb trail's emissive by alpha, and that is not a
//! precedent for this: it is `AlphaMode::Add`, a different blend with different factors.
//! Caught in review on #1216.

use bevy::prelude::*;

/// Where one creature's pair of eyes sits on its head, how big they are, and how they glow.
///
/// Every length is in the creature's **own model units**, so a model authored at a scale of
/// one — which is every model in this client — carries these as fractions of that scale and
/// the drawn size follows whatever the entity is scaled to. `player/birds.rs` authors its
/// bird at a wingspan of exactly one for the same reason.
///
/// **`-Z` is forward**, matching `player/birds.rs`'s body sections and the
/// `Transform::look_to` that aims them, so [`Eyeshine::forward`] is measured along `-Z` and
/// the faces are turned to look that way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Eyeshine {
    /// How far each eye's centre sits either side of the model's centre line.
    pub(super) spread: f32,
    /// How far in front of the model's origin the pair sits, along `-Z`.
    pub(super) forward: f32,
    /// How far above the model's origin the pair sits.
    pub(super) rise: f32,
    /// How wide and how tall one eye is — square, because a glint at a fifth of a degree has
    /// no shape to get wrong and a rectangle would only be two numbers to keep in step.
    pub(super) size: f32,
    /// The colour the eye is under the sky's own light.
    pub(super) colour: Color,
    /// What it emits on top of that, which is the whole of why it reads at night.
    ///
    /// A `LinearRgba` rather than a `Color` because that is what `StandardMaterial::emissive`
    /// is, and converting at the call site would hide the one thing about this number that
    /// matters: its components are **not bounded by one**. The rune's glow is `(0.5, 1.2,
    /// 3.0)`, and a value under one would be an eye dimmer than a white wall.
    pub(super) glow: LinearRgba,
}

/// The eyeshine a pooled material is minted with: dark, and invisible.
///
/// Every pool entry is overwritten with its creature's own row the moment one claims it, so
/// this is only ever what an unclaimed handle holds. It lives here rather than beside either
/// pool because there are two of them — `player/birds.rs`'s and `player/critters.rs`'s — and a
/// named constant rather than a literal because [`Eyeshine`] has six fields and a literal would
/// read as a species.
pub(super) const BLANK_EYES: Eyeshine = Eyeshine {
    spread: 0.0,
    forward: 0.0,
    rise: 0.0,
    size: 0.0,
    colour: Color::BLACK,
    glow: LinearRgba::BLACK,
};

/// The two faces, as one mesh and therefore one draw.
///
/// A pair of squares rather than anything rounder, for the reason `player/horse.rs`'s
/// `horse_eye_mesh` gives: at the angle an eye subtends the silhouette is a dot whatever its
/// outline is, and two quads is eight vertices against a sphere's hundreds.
///
/// **Both faces look along `-Z`**, which is where the creature is going and therefore where a
/// player standing in front of it is. Bevy's `Rectangle` is authored in the `XY` plane facing
/// `+Z`, so each is given a half turn about `Y`.
///
/// **That half turn does not reverse the winding, and an earlier version of this comment said
/// it did.** A rotation is orientation-preserving — determinant `+1` — so after it the quad's
/// stored normal is `-Z` *and* its winding is still counter-clockwise seen from `-Z`. The face
/// is therefore front-facing from exactly the direction it looks, which is why this material
/// keeps Bevy's default back-face culling: the eye is drawn from in front and culled from
/// behind, which is the picture this module wants. There is no separate "seen from behind"
/// case and deliberately none — a creature flying away from the player having no glint is
/// correct rather than a gap. `a_half_turn_leaves_the_eye_front_facing_from_the_front` is what
/// checks that rather than the prose. Caught in review on #1216.
pub(super) fn eye_pair_mesh(eyes: Eyeshine) -> Mesh {
    let [mut left, right] = [-1.0_f32, 1.0].map(|side| {
        Mesh::from(Rectangle::new(eyes.size, eyes.size)).transformed_by(
            Transform::from_translation(Vec3::new(side * eyes.spread, eyes.rise, -eyes.forward))
                .with_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
        )
    });
    super::merge_all(&mut left, [right], "creature eyeshine");
    left
}

/// The material one pair of eyes wears, at `alpha` of the creature's fade.
///
/// **Only the base colour carries the fade.** The glow is passed through unscaled because the
/// renderer applies the alpha to it twice already — see the note on the fade at the head of
/// this module for the two lines that do it.
///
/// **Back-face culling is Bevy's default and is deliberately left alone.** `player/birds.rs`'s
/// plumage material sets `cull_mode: None`, but its reason does not transfer: that is a closed
/// shell lofted through hand-authored rings, where drawing both faces turns a ring wound the
/// wrong way into a shading mistake rather than a hole at forty blocks. A single quad has no
/// rings to get wrong, and culling here is what makes "an eye is not visible from the back of
/// a head" true rather than merely stated.
pub(super) fn eyeshine_material(eyes: Eyeshine, alpha: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: eyes.colour.with_alpha(alpha),
        emissive: eyes.glow,
        alpha_mode: AlphaMode::Blend,
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An owl-sized pair, so the assertions below are about a shape somebody ships rather
    /// than about round numbers.
    const SAMPLE: Eyeshine = Eyeshine {
        spread: 0.036,
        forward: 0.232,
        rise: 0.02,
        size: 0.048,
        colour: Color::srgb(0.98, 0.86, 0.45),
        glow: LinearRgba::rgb(3.4, 2.6, 0.9),
    };

    fn points(mesh: &Mesh) -> Vec<Vec3> {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION.id)
            .and_then(|values| values.as_float3())
            .expect("an eyeshine mesh carries positions")
            .iter()
            .map(|value| Vec3::from_array(*value))
            .collect()
    }

    fn normals(mesh: &Mesh) -> Vec<Vec3> {
        mesh.attribute(Mesh::ATTRIBUTE_NORMAL.id)
            .and_then(|values| values.as_float3())
            .expect("an eyeshine mesh carries normals")
            .iter()
            .map(|value| Vec3::from_array(*value))
            .collect()
    }

    #[test]
    fn a_pair_of_eyes_is_two_mirrored_faces_that_both_look_forward() {
        let mesh = eye_pair_mesh(SAMPLE);
        let points = points(&mesh);
        assert_eq!(points.len(), 8, "two quads, four corners each");

        // Mirrored across the centre line: summing every x cancels exactly, and each eye is
        // genuinely off-centre rather than both sitting on zero.
        let sum: f32 = points.iter().map(|at| at.x).sum();
        assert!(
            sum.abs() < 1e-6,
            "the pair is not mirrored: x sums to {sum}"
        );
        assert!(
            points.iter().all(|at| at.x.abs() > 1e-6),
            "an eye sits on the centre line"
        );

        // In front of the origin and above it, which is where a head is.
        assert!(
            points.iter().all(|at| at.z < 0.0),
            "an eye is not in front of the origin, so -Z is not forward here"
        );
        // Centred above the origin, not wholly above it: an owl's eye is 0.048 across on a
        // 0.02 rise, so the lower corner sits a few thousandths under the centre line and
        // still well inside the head section's own half-height. The centre is the claim.
        let mean = points.iter().map(|at| at.y).sum::<f32>() / points.len() as f32;
        assert!(
            mean > 0.0,
            "the pair is centred at {mean}, not above the origin"
        );

        // **The property, not the winding**: every corner's stored normal points along -Z,
        // so the glint faces whoever the creature is facing.
        for normal in normals(&mesh) {
            assert!(
                normal.dot(Vec3::NEG_Z) > 0.99,
                "an eye faces {normal} rather than forward"
            );
        }
    }

    #[test]
    fn an_eye_is_authored_at_the_size_and_the_offsets_it_was_given() {
        // The numbers are fractions of a creature's own scale, so a model that reads them
        // wrong is a creature with eyes in its chest and nothing would say so.
        let points = points(&eye_pair_mesh(SAMPLE));
        let reach = |pick: fn(&Vec3) -> f32| {
            let low = points.iter().map(pick).fold(f32::INFINITY, f32::min);
            let high = points.iter().map(pick).fold(f32::NEG_INFINITY, f32::max);
            (low, high)
        };
        let (left, right) = reach(|at| at.x);
        assert!((right - (SAMPLE.spread + SAMPLE.size / 2.0)).abs() < 1e-6);
        assert!((left + (SAMPLE.spread + SAMPLE.size / 2.0)).abs() < 1e-6);

        let (low, high) = reach(|at| at.y);
        assert!(
            (high - low - SAMPLE.size).abs() < 1e-6,
            "an eye is not square"
        );
        assert!(((low + high) / 2.0 - SAMPLE.rise).abs() < 1e-6);

        let (near, far) = reach(|at| at.z);
        assert!((near + SAMPLE.forward).abs() < 1e-6 && (far + SAMPLE.forward).abs() < 1e-6);
    }

    #[test]
    fn the_glow_is_brighter_than_white_and_the_renderer_applies_the_fade_to_it() {
        // The one number that makes an eye read at night: an emissive term that is not
        // bounded by one, which is what `structures.rs`'s rune says a glow has to be.
        let whole = eyeshine_material(SAMPLE, 1.0);
        assert_eq!(whole.emissive, SAMPLE.glow);
        assert!(
            whole.emissive.red.max(whole.emissive.green) > 1.0,
            "a glow no brighter than white is not a glint: {:?}",
            whole.emissive
        );
        // Lit, deliberately: an unlit eye is as bright at noon as at midnight.
        assert!(!whole.unlit, "eyeshine must still be darkened by the sky");
        assert_eq!(whole.alpha_mode, AlphaMode::Blend);
        // Bevy's default culling, deliberately kept — see `eyeshine_material`.
        assert_eq!(
            whole.cull_mode,
            Some(bevy::render::render_resource::Face::Back)
        );

        // **The fade lives on the base colour alone**, and this asserts the emissive does
        // *not* move with it. `pbr_functions.wgsl` multiplies the emissive by
        // `output_color.a` inside the shader and `BlendState::ALPHA_BLENDING` multiplies the
        // whole output by the alpha again, so a glow scaled here too would reach the screen
        // at `glow * alpha^3` and the eyes would be gone long before the body. This was
        // scaled, and was caught in review on #1216.
        for alpha in [0.0, 0.25, 0.5, 1.0] {
            let material = eyeshine_material(SAMPLE, alpha);
            assert_eq!(
                material.emissive, SAMPLE.glow,
                "the glow moved with the fade at alpha {alpha}, which the renderer already does"
            );
            assert_eq!(material.base_color.alpha(), alpha);
        }
    }

    #[test]
    fn a_half_turn_leaves_the_eye_front_facing_from_the_front() {
        // The claim `eye_pair_mesh` used to get wrong: the half turn about `Y` that aims the
        // quad at `-Z` is a rotation, so it preserves orientation and cannot reverse the
        // winding. That is what lets this material keep back-face culling and still be seen —
        // and it is why "an eye is not visible from the back of a head" is true rather than
        // merely written down.
        let mesh = eye_pair_mesh(SAMPLE);
        let points = points(&mesh);
        let normals = normals(&mesh);
        let Some(bevy::mesh::Indices::U32(indices)) = mesh.indices() else {
            panic!("an eyeshine mesh is a U32 triangle list")
        };
        let mut faces = 0usize;
        for triangle in indices.chunks_exact(3) {
            let [a, b, c] = [0, 1, 2].map(|at| points[triangle[at] as usize]);
            let wound = (b - a).cross(c - a).normalize_or_zero();
            assert!(
                wound.dot(Vec3::NEG_Z) > 0.99,
                "a face is wound {wound}, so culling would hide it from the front"
            );
            // And the stored normal agrees with the winding, which is the other half: a face
            // lit from one side and culled from the other is the failure this pair catches.
            for corner in triangle {
                assert!(wound.dot(normals[*corner as usize]) > 0.99);
            }
            faces += 1;
        }
        assert_eq!(faces, 4, "two quads, two triangles each");
    }
}
