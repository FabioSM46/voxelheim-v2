// The water surface: a procedural ripple that slides along the flow the mesher wrote
// into the vertex attributes.
//
// Shading only. The surface stays exactly where `mesher.rs` put it — the level the
// server sent — and nothing here displaces a vertex, so what moves on the screen is
// light and never geometry.
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
// The most the ripple may push the water's brightness, either way. 8% is the ceiling
// the issue sets: visible motion, and still the same colour as before it moved.
const RIPPLE_DEPTH: f32 = 0.08;
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
    var point = world.xz - flow * (FLOW_SPEED * flowing_water.time);
    if dot(flow, flow) == 0.0 {
        // Still water: a slow diagonal shimmer that belongs to no direction.
        point = world.xz + vec2<f32>(-STILL_DRIFT, STILL_DRIFT) * flowing_water.time;
    }
    if falling > 0.5 {
        // A waterfall is read down its own wall: one axis across the face, one down it.
        point = vec2<f32>(world.x + world.z, world.y + FALL_SPEED * flowing_water.time);
    }

    let brightness = 1.0 + RIPPLE_DEPTH * ripple(point);
    pbr_input.material.base_color = vec4<f32>(
        pbr_input.material.base_color.rgb * brightness,
        pbr_input.material.base_color.a,
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
