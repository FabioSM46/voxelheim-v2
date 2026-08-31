// The water surface: a procedural ripple that slides along the flow the mesher wrote
// into the vertex attributes.
//
// Shading only. The surface stays exactly where `mesher.rs` put it — the level the
// server sent — and nothing here displaces a vertex, so what moves on the screen is
// colour and never geometry.
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

// Blocks a ripple travels per second along a full-strength flow. A little under
// walking pace, so a river reads as moving without the surface looking blown along.
const FLOW_SPEED: f32 = 1.4;
// Blocks per second the pattern drifts where there is no flow at all. Slow enough to
// read as a lake breathing rather than as a current nobody can swim against.
const STILL_DRIFT: f32 = 0.06;
// Blocks per second a falling column streaks along -Y. Faster than any horizontal
// flow, because a waterfall is the one place the eye expects speed.
const FALL_SPEED: f32 = 5.0;

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
// first axis lies along the direction of travel, and that axis is divided by this — so
// the pattern varies slowly along the flow and quickly across it, which is a streak.
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

// Two octaves of sine, in [-1, 1]. No texture, no hash table, no asset — the whole
// pattern is four sines of the sample point.
//
// **The range is exact, it is proved by the last line, and every depth constant above
// depends on it.** `coarse` is a product of two sines, so |coarse| <= 1; `fine` is one
// sine, so |fine| <= 1; therefore |coarse + fine * OCTAVE_WEIGHT| <= 1 + OCTAVE_WEIGHT,
// and dividing by exactly that is what closes it at one. Nothing here clamps, and that
// is deliberate: with |ripple| <= 1 the fragment below cannot leave its range —
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
fn ripple(point: vec2<f32>) -> f32 {
    let coarse = sin(point.x * RIPPLE_SCALE)
        * sin(point.y * RIPPLE_SCALE * 1.3 + point.x * 0.21);
    let fine = sin((point.x + point.y) * RIPPLE_SCALE * OCTAVE_RATIO + 1.7);
    return (coarse + fine * OCTAVE_WEIGHT) / (1.0 + OCTAVE_WEIGHT);
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
    var depth: f32;
    var foam: f32;

    if falling > 0.5 {
        // A fall is read down its own wall: the vertical axis is the stretched one, so
        // the water breaks into columns rather than into ripples, and the whole pattern
        // travels down it.
        point = vec2<f32>((world.y + FALL_SPEED * time) / FALL_STRETCH, world.x + world.z);
        depth = FALL_DEPTH;
        foam = FALL_FOAM;
    } else if dot(flow, flow) > 0.0 {
        // A current, in the basis it runs in: `along` down the flow, `across` at right
        // angles to it. Subtracting the travelled distance from `along` is what makes
        // the pattern appear to move forwards; dividing it by the stretch is what makes
        // it a streak rather than a swell.
        let direction = normalize(flow);
        let speed = FLOW_SPEED * length(flow);
        let along = dot(world.xz, direction) - speed * time;
        let across = dot(world.xz, vec2<f32>(-direction.y, direction.x));
        point = vec2<f32>(along / RUN_STRETCH, across);
        depth = RUN_DEPTH;
        foam = RUN_FOAM;
    } else {
        // Still water: a slow diagonal shimmer that belongs to no direction, and no
        // foam, because a lake has none.
        point = world.xz + vec2<f32>(-STILL_DRIFT, STILL_DRIFT) * time;
        depth = STILL_DEPTH;
        foam = 0.0;
    }

    let wave = ripple(point);
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

    var out: FragmentOutput;
    if (pbr_input.material.flags & STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
