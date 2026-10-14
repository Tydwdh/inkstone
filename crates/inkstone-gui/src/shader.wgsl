// The one pipeline an inkstone window needs: axis-aligned quads, textured from
// an atlas that holds the glyphs. Everything on the screen — a background run, a
// glyph, an underline, the cursor block — is one of these, which is what makes
// the whole frame a single draw call.
//
// A quad arrives as an instance (see `render.rs`): where it goes in pixels, the
// rectangle of the atlas it samples in *texels*, its colour, and whether the
// atlas is a mask over that colour (a glyph) or not (a flat fill). The atlas
// rectangle is in texels rather than in normalized coordinates so that growing
// the atlas — which changes every `v` — cannot invalidate a quad that has
// already been built.

struct Uniforms {
    // The target's size in physical pixels.
    viewport: vec2<f32>,
    // The glyph atlas's size in texels.
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(2) var glyph_sampler: sampler;

// A flat rectangle of one colour.
const MODE_FILL: u32 = 0u;
// A glyph: the atlas's coverage, in the instance's colour.
const MODE_GLYPH: u32 = 1u;

struct Instance {
    // x, y, width, height in physical pixels.
    @location(0) rect: vec4<f32>,
    // x, y, width, height in atlas texels.
    @location(1) atlas_rect: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mode: u32,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) mode: u32,
};

@vertex
fn vs_main(instance: Instance, @builtin(vertex_index) index: u32) -> VertexOut {
    // Two triangles as offsets inside the quad's own unit square.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[index % 6u];

    let pixel = instance.rect.xy + corner * instance.rect.zw;
    // Pixels to clip space: x rightwards from -1, y downwards from +1.
    let clip = vec2<f32>(
        pixel.x / uniforms.viewport.x * 2.0 - 1.0,
        1.0 - pixel.y / uniforms.viewport.y * 2.0,
    );

    let uv = (instance.atlas_rect.xy + corner * instance.atlas_rect.zw) / uniforms.atlas;

    var out: VertexOut;
    out.clip = vec4<f32>(clip, 0.0, 1.0);
    out.uv = uv;
    out.color = instance.color;
    out.mode = instance.mode;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // A glyph's coverage is its mask; a flat fill is fully covered. The mask
    // only ever dims the colour, so one alpha-blended pipeline draws both.
    let mask = textureSample(glyph_atlas, glyph_sampler, in.uv).r;
    let coverage = select(1.0, mask, in.mode == MODE_GLYPH);
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
