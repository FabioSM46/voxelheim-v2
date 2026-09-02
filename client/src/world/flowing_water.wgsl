// The water surface: a procedural ripple that slides along the flow the mesher wrote
// into the vertex attributes.
//
// Shading only. The surface stays exactly where `mesher.rs` put it — the level the
// server sent — and nothing here displaces a vertex. Colour and the lighting normal
// move; geometry never does.
//
// Three inputs, all of them per-vertex and all of them derived from block ids the
// server chose:
//   * `in.uv`   (ATTRIBUTE_UV_0) — the horizontal flow `(x, z)`, zero for still water
//   * `in.uv_b` (ATTRIBUTE_UV_1) — `x` is 1 where this water is falling
//   * `in.color`                — the blue and the alpha, from `palette.rs`
//
// The pattern is sampled at a point that MOVES BACKWARDS along the flow, which is what
// makes it appear to travel forwards: a pattern P displayed at `world - v*t` drifts with
// velocity `v`.

#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT,
}

struct FlowingWater {
    // Seconds, wrapped by the Rust side so an hour-long session keeps f32 precision.
    time: f32,
}

// Binding 100 by convention: the base StandardMaterial owns 0-99 of this group.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> flowing_water: FlowingWater;

// Blocks a ripple travels per second along a full-strength flow. At the wavelengths
// below the fine octave passes in about a second, while the coarse one takes just under
// three: motion is legible without the whole sheet sliding as one long wave.
const FLOW_SPEED: f32 = 5.0;
// Blocks per second the pattern drifts where there is no flow at all. Both components
// point the same way so the fine octave, which samples x + y, cannot cancel their time
// terms. It stays far slower than a current: a lake breathes without reading as one.
const STILL_DRIFT: f32 = 0.35;
// Blocks per second a falling column streaks along -Y. Faster than any horizontal
// flow, because a waterfall is the one place the eye expects speed.
const FALL_SPEED: f32 = 7.0;

// ── The three waters ──────────────────────────────────────────────────────────────
//
// **Until #655 there was one number here for all three, and it was 0.08.** Eight per
// cent of brightness, on a translucent blue surface, at play distance: a river was not
// distinguishable from a lake, which is what the issue reported. Worse, the falling
// branch had never once run — the server wrote only water *sources*, so no block in the
// world carried the falling bit until #653 gave the automaton a way to make one and
// #654 wrote falls into the terrain.
//
// **Three states have to differ in more than one thing or they read as the same water
// at three speeds.** So each differs in three: how deep the ripple cuts, how far its
// crest is pulled toward white, and — the one that actually carries it — the *shape* of
// the pattern. Still water is isotropic swell. Moving water is stretched along the
// direction it moves, because that is what a current looks like from above: streaks,
// not swell. A fall is stretched along the wall it runs down.
//
// Amplitudes. Still stays gentle and is if anything quieter than before, because it is
// the one that should not draw the eye; the other two are where the range went.
const STILL_DEPTH: f32 = 0.06;
const RUN_DEPTH: f32 = 0.20;
const FALL_DEPTH: f32 = 0.34;

// How far the crest of the pattern is pulled toward white. **This is the half that is
// not brightness**, and the acceptance criterion asks for it by name: brightness alone
// is a lit surface changing exposure, where a whitened crest is foam. Still water gets
// none — a lake has no foam — which is itself part of what separates the three.
const RUN_FOAM: f32 = 0.12;
const FALL_FOAM: f32 = 0.28;

// How much longer a streak is than it is wide. The ripple is sampled in a basis whose
// second axis lies across the direction of travel, and that axis is multiplied by this
// — so the pattern varies slowly along the flow and quickly across it, which is a streak.
// Keeping the animated first axis unscaled lets shape and crest rate be tuned apart.
// Both are kept modest: past about five the pattern stops varying along the flow at
// all, and a band that does not vary cannot be seen to move.
const RUN_STRETCH: f32 = 3.5;
const FALL_STRETCH: f32 = 3.0;

// Radians of the coarse octave per block. About fourteen blocks a wave, which is a
// lake-sized ripple rather than a texture.
const RIPPLE_SCALE: f32 = 0.45;
// How much faster the second octave is than the first. Irrational-ish on purpose, so
// the two do not line up into a visible tile.
const OCTAVE_RATIO: f32 = 2.7;
// How much of the pattern the second octave contributes.
const OCTAVE_WEIGHT: f32 = 0.35;

// How far the ripple's analytic slope tilts the lighting normal. This is deliberately
// small: the surface is one flat quad over many blocks, and a large value would turn it
// into a warped mirror. It changes only the normal handed to PBR, never the geometry.
const NORMAL_STRENGTH: f32 = 0.08;

// Two octaves of sine. The returned x component is the value in [-1, 1]; y and z are
// its analytic derivatives. No texture, no hash table, no asset — the whole pattern is
// four sines of the sample point.
//
// **The range is exact, it is proved by the last line, and every depth constant above
// depends on it.** `coarse` is a product of two sines, so |coarse| <= 1; `fine` is one
// sine, so |fine| <= 1; therefore |coarse + fine * OCTAVE_WEIGHT| <= 1 + OCTAVE_WEIGHT,
// and dividing by exactly that is what closes it at one. Nothing here clamps, and that
// is deliberate: with |ripple.x| <= 1 the fragment below cannot leave its range —
// brightness stays in [1 - FALL_DEPTH, 1 + FALL_DEPTH] and the foam crest in
// [0, FALL_FOAM] — so a clamp would protect nothing that holds and would hide the one
// thing that could break it.
//
// **What could break it is adding an octave.** A third term without a matching divisor
// makes this return more than one, and the depths above silently stop being the bound
// they are documented as: at |ripple| = 3, `FALL_DEPTH` alone would drive brightness
// negative. So the divisor is not decoration and it is not optional —
// `the_ripple_is_normalised_and_the_depths_depend_on_it` in `water_material.rs` fails if
// this line stops naming every octave weight in the sum above it.
fn ripple(point: vec2<f32>) -> vec3<f32> {
    let coarse_x = point.x * RIPPLE_SCALE;
    let coarse_y = point.y * RIPPLE_SCALE * 1.3 + point.x * 0.21;
    let fine_phase = (point.x + point.y) * RIPPLE_SCALE * OCTAVE_RATIO + 1.7;

    let coarse = sin(coarse_x) * sin(coarse_y);
    let fine = sin(fine_phase);
    let divisor = 1.0 + OCTAVE_WEIGHT;

    // Analytic derivatives in the two coordinates of `point`. Returning them with the
    // value keeps the normal and colour on exactly the same procedural wave.
    let gradient_x = (
        cos(coarse_x) * RIPPLE_SCALE * sin(coarse_y)
        + sin(coarse_x) * cos(coarse_y) * 0.21
        + cos(fine_phase) * RIPPLE_SCALE * OCTAVE_RATIO * OCTAVE_WEIGHT
    ) / divisor;
    let gradient_y = (
        sin(coarse_x) * cos(coarse_y) * RIPPLE_SCALE * 1.3
        + cos(fine_phase) * RIPPLE_SCALE * OCTAVE_RATIO * OCTAVE_WEIGHT
    ) / divisor;

    return vec3<f32>((coarse + fine * OCTAVE_WEIGHT) / divisor, gradient_x, gradient_y);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    // Guarded, because the shader defs follow the mesh layout: a water mesh always
    // carries both attributes, and a fragment with neither must still compile.
    var flow = vec2<f32>(0.0, 0.0);
    var falling = 0.0;
#ifdef VERTEX_UVS_A
    flow = in.uv;
#endif
#ifdef VERTEX_UVS_B
    falling = in.uv_b.x;
#endif

    let world = in.world_position.xyz;
    let time = flowing_water.time;

    // Which of the three this fragment is, and the sample point, the depth and the foam
    // that go with it. One branch chain and no second material: the state is per-vertex
    // data the mesher wrote from block ids the server chose.
    var point: vec2<f32>;
    // World-space gradients of point.x and point.y. They turn `ripple`'s two analytic
    // derivatives back into a slope on whichever plane this water face occupies.
    var point_x_gradient: vec3<f32>;
    var point_y_gradient: vec3<f32>;
    var depth: f32;
    var foam: f32;

    if falling > 0.5 {
        // A fall is read down its own wall: the transverse axis is compressed, so
        // the water breaks into columns rather than into ripples, and the whole pattern
        // travels down an unscaled animated axis.
        point = vec2<f32>(world.y + FALL_SPEED * time, (world.x + world.z) * FALL_STRETCH);
        point_x_gradient = vec3<f32>(0.0, 1.0, 0.0);
        point_y_gradient = vec3<f32>(FALL_STRETCH, 0.0, FALL_STRETCH);
        depth = FALL_DEPTH;
        foam = FALL_FOAM;
    } else if dot(flow, flow) > 0.0 {
        // A current, in the basis it runs in: `along` down the flow, `across` at right
        // angles to it. Subtracting the travelled distance from `along` is what makes
        // the pattern appear to move forwards; multiplying `across` by the stretch is
        // what makes it a streak without also slowing that motion.
        let direction = normalize(flow);
        let perpendicular = vec2<f32>(-direction.y, direction.x);
        let speed = FLOW_SPEED * length(flow);
        let along = dot(world.xz, direction) - speed * time;
        let across = dot(world.xz, perpendicular);
        point = vec2<f32>(along, across * RUN_STRETCH);
        point_x_gradient = vec3<f32>(direction.x, 0.0, direction.y);
        point_y_gradient = vec3<f32>(
            perpendicular.x * RUN_STRETCH,
            0.0,
            perpendicular.y * RUN_STRETCH,
        );
        depth = RUN_DEPTH;
        foam = RUN_FOAM;
    } else {
        // Still water: a slow diagonal shimmer that belongs to no direction, and no
        // foam, because a lake has none.
        point = world.xz + vec2<f32>(STILL_DRIFT * 0.7, STILL_DRIFT) * time;
        point_x_gradient = vec3<f32>(1.0, 0.0, 0.0);
        point_y_gradient = vec3<f32>(0.0, 0.0, 1.0);
        depth = STILL_DEPTH;
        foam = 0.0;
    }

    let sample = ripple(point);
    let wave = sample.x;
    let brightness = 1.0 + depth * wave;
    // Only the crest foams. `max(wave, 0.0)` rather than `abs`, so a trough stays water
    // instead of turning the pattern into a row of white bars at twice the frequency.
    let crest = foam * max(wave, 0.0);

    // Depth still acts on the diffuse base colour, where it cannot scale the specular
    // highlight the standard material computes. Foam takes the other path: emissive is
    // added after direct and ambient lighting inside `apply_pbr_lighting`, so a small
    // crest survives night without turning the whole lit result — specular included —
    // up and down. At noon this term is small beside the sun; in darkness it is the
    // scattering cue that keeps moving water from becoming a flat pane. This emits no
    // light into the world; it changes only the colour of this surface.
    let colour = pbr_input.material.base_color.rgb * brightness;
    pbr_input.material.base_color = vec4<f32>(colour, pbr_input.material.base_color.a);
    pbr_input.material.emissive = vec4<f32>(
        pbr_input.material.emissive.rgb + vec3<f32>(1.0, 1.0, 1.0) * crest,
        pbr_input.material.emissive.a,
    );

    // PBR reads `N` for direct and specular lighting. Projecting the analytic gradient
    // into the actual face plane keeps the same arithmetic valid for a horizontal
    // surface and for either orientation of a falling wall. `world_normal` stays flat:
    // shadows and geometry still describe the server-sent surface.
    let world_gradient = sample.y * point_x_gradient + sample.z * point_y_gradient;
    let surface_gradient = world_gradient
        - pbr_input.N * dot(world_gradient, pbr_input.N);
    pbr_input.N = normalize(pbr_input.N - NORMAL_STRENGTH * surface_gradient);

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
