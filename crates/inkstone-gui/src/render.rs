//! The GPU renderer: the grid the daemon sent, drawn as textured quads.
//!
//! One pipeline, one draw call per frame. Every visible thing — a run of
//! background, a glyph, an underline, the cursor block — is the same kind of
//! object: an axis-aligned quad with a colour sampled from a one-channel atlas
//! (or not sampled at all, for a flat fill). The vertex shader places it from
//! the instance's pixel rectangle and the viewport uniform, so nothing has to be
//! rebuilt when the window is resized.
//!
//! The instance buffer is laid out **per row**, with a fixed number of quads
//! reserved for each: `4 * width` for that row's runs, glyphs and rules, plus a
//! few for the row's own background and the cursor. That is what makes the delta
//! protocol cheap here — a frame that changes one row rebuilds one row, and
//! `queue.write_buffer` uploads exactly that row's slice of the buffer.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use inkstone_rpc::{Cell, Grid};
use winit::window::Window;

use crate::glyphs::{AtlasRect, Glyph, Glyphs};
use crate::theme::{self, Rgba, Style};

/// How many quads are reserved for each column of a row.
///
/// A worst-case row has one background run and one glyph per column, plus a
/// merged underline run and a merged strikethrough run — at most three quads per
/// column, so four is room to spare.
const QUADS_PER_COLUMN: usize = 4;

/// A flat rectangle of one colour.
const MODE_FILL: u32 = 0;

/// A glyph: the atlas's coverage, painted in the quad's colour.
const MODE_GLYPH: u32 = 1;

/// The size of one [`Quad`] as the vertex layout packs it.
const QUAD_SIZE: usize = 52;

/// The two triangles of a quad, as vertex indices.
const QUAD_VERTICES: u32 = 6;

/// The atlas upload's rows must start on this boundary.
const COPY_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// How thick a rule is, as a fraction of the cell height (at least a pixel).
const RULE_THICKNESS: f32 = 0.07;

/// Where an underline sits, as a fraction of the cell height from its top.
const UNDERLINE_POSITION: f32 = 0.88;

/// Where a strikethrough sits, as a fraction of the cell height from its top.
const STRIKETHROUGH_POSITION: f32 = 0.5;

/// Where the shader expects each instance's fields, in bytes.
const QUAD_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Uint32];

/// One instance of the quad pipeline, as the shader reads it.
#[derive(Clone, Copy, Debug, Default)]
struct Quad {
    /// The quad, in physical pixels: x, y, width, height.
    rect: [f32; 4],
    /// The atlas region to sample, in texels: x, y, width, height.
    atlas: [f32; 4],
    /// The colour to paint.
    color: Rgba,
    /// [`MODE_FILL`] or [`MODE_GLYPH`].
    mode: u32,
}

/// The window, the swap chain and the one pipeline that draws a screen.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    atlas: wgpu::Texture,
    atlas_size: (u32, u32),
    glyphs: Glyphs,
    /// The cell a glyph is drawn in, in physical pixels.
    cell: (f32, f32),
    /// The grid the instance buffer is laid out for.
    grid: (u16, u16),
    /// Quads reserved per row: one per column for four purposes, plus the row's
    /// background, the cursor block and its glyph, and two unused.
    quads_per_row: usize,
    instances: wgpu::Buffer,
    /// Reused while one row is built, so a frame does not allocate per row.
    quads: Vec<Quad>,
    /// Reused while a row is turned into bytes.
    bytes: Vec<u8>,
    /// How many of each row's reserved slots were written last time, so a row
    /// that needs fewer can have the rest cleared. See [`encode_row`].
    row_counts: Vec<usize>,
    /// Reused while an atlas rectangle is uploaded.
    upload: Vec<u8>,
}

impl Renderer {
    /// Create the surface, the device and the pipeline for `window`.
    ///
    /// `scale` is the window's scale factor, which fixes the size of a cell (and
    /// so of a glyph in the atlas).
    ///
    /// Every failure here is one a user can act on: a machine with no working
    /// adapter, a device that cannot be created, a window with no drawable
    /// format, or a system with no font.
    pub fn new(window: Arc<Window>, scale: f32) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface: wgpu::Surface<'static> = instance
            .create_surface(window.clone())
            .map_err(|err| anyhow!("the window cannot be drawn to: {err}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .map_err(|err| {
            anyhow!(
                "no graphics adapter can draw this window ({err}); \
                 update your graphics driver, or run on a machine with a working GPU"
            )
        })?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("inkstone-gui"),
            required_features: wgpu::Features::empty(),
            // Downlevel defaults raised to what this adapter actually reports:
            // the lowest common denominator where a limit does not matter, and
            // the adapter's own where it does (the glyph atlas, above all).
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|err| anyhow!("the graphics device could not be created: {err}"))?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| {
                anyhow!(
                    "the window surface offers no format that can be drawn to; \
                     this is a driver problem, so try updating it"
                )
            })?;
        // The shader writes the theme's colours unchanged, which is only true of
        // a surface whose format does not re-encode them as sRGB on the way out.
        if let Some(plain) = surface
            .get_capabilities(&adapter)
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
        {
            config.format = plain;
        }
        surface.configure(&device, &config);

        let glyphs = Glyphs::new(scale, device.limits().max_texture_dimension_2d)?;
        let atlas_size = glyphs.atlas_size();
        let atlas = create_atlas(&device, atlas_size);

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("inkstone-gui uniforms"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("inkstone-gui atlas"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("inkstone-gui"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = bind_group(&device, &bind_group_layout, &sampler, &uniform, &atlas);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("inkstone-gui quads"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("inkstone-gui"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("inkstone-gui"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: QUAD_SIZE as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &QUAD_ATTRIBUTES,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // A placeholder for the frames before the daemon has sent a screen: the
        // first `update` replaces it with a buffer sized for the real grid.
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("inkstone-gui quads"),
            size: QUAD_SIZE as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let cell = glyphs.cell_size();
        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            sampler,
            bind_group,
            uniform,
            atlas,
            atlas_size,
            glyphs,
            cell,
            grid: (0, 0),
            quads_per_row: 0,
            instances,
            quads: Vec::new(),
            bytes: Vec::new(),
            row_counts: Vec::new(),
            upload: Vec::new(),
        })
    }

    /// The cell a glyph is drawn in, in physical pixels. The grid size the
    /// daemon is told about comes from this.
    pub fn cell_size(&self) -> (f32, f32) {
        self.cell
    }

    /// Match the surface to a new window size.
    ///
    /// A zero-sized window (minimised) keeps its old configuration: a swap chain
    /// of no pixels is not something a driver accepts.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Rebuild and upload the rows that changed, and draw the cursor where the
    /// daemon says it is.
    ///
    /// `rows` are row indexes into `grid`; every other row's quads are left
    /// exactly as they were uploaded, which is the point of laying the instance
    /// buffer out row by row.
    pub fn update(
        &mut self,
        grid: &Grid,
        rows: &[u16],
        cursor: (u16, u16),
        cursor_visible: bool,
        insert: bool,
    ) {
        if grid.width == 0 || grid.height == 0 {
            return;
        }
        self.ensure_instances(grid.width, grid.height);

        // Taken out of `self` so that building a row can borrow the glyph cache
        // mutably; put back (with its capacity) at the end.
        let mut quads = std::mem::take(&mut self.quads);
        for &y in rows {
            if y >= grid.height {
                continue;
            }
            quads.clear();
            self.build_row(grid, y, cursor, cursor_visible, insert, &mut quads);
            let written = encode_row(&quads, self.row_counts[y as usize], &mut self.bytes);
            self.row_counts[y as usize] = written;
            let offset = y as u64 * self.quads_per_row as u64 * QUAD_SIZE as u64;
            self.queue
                .write_buffer(&self.instances, offset, &self.bytes);
        }
        self.quads = quads;

        self.sync_atlas();
    }

    /// Draw the frame, presenting it to the window.
    ///
    /// A swap chain that is momentarily unusable (occluded, timed out, outdated)
    /// is not an error: the frame is skipped and the next one redraws.
    pub fn render(&mut self) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // A swap chain that no longer matches the window is remade here and
            // drawn into on the next frame.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(())
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow!(
                    "the window surface refused a frame; this is a bug in inkstone-gui"
                ));
            }
        };

        // The view borrows the frame's texture, so everything that uses it has
        // to finish before the frame is handed to the queue to present.
        {
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            let uniforms = uniform_bytes((self.config.width, self.config.height), self.atlas_size);
            self.queue.write_buffer(&self.uniform, 0, &uniforms);

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("inkstone-gui"),
                });
            {
                let background = theme::BACKGROUND;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("inkstone-gui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: f64::from(background[0]),
                                g: f64::from(background[1]),
                                b: f64::from(background[2]),
                                a: f64::from(background[3]),
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                let instances = self.instance_count();
                if instances > 0 {
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &self.bind_group, &[]);
                    pass.set_vertex_buffer(0, self.instances.slice(..));
                    pass.draw(0..QUAD_VERTICES, 0..instances);
                }
            }
            self.queue.submit([encoder.finish()]);
        }
        self.queue.present(frame);
        Ok(())
    }

    /// How many quads the frame draws: every row's whole reservation, the unused
    /// ones of which are the zeroed quads that draw nothing.
    fn instance_count(&self) -> u32 {
        (self.grid.1 as usize * self.quads_per_row) as u32
    }

    /// Give the instance buffer room for `rows` rows of `cols` columns.
    ///
    /// The buffer is rebuilt rather than grown when the grid changes shape,
    /// because a delta frame always follows a resize with a full one: every row
    /// is rewritten anyway, and the new buffer starts zeroed.
    fn ensure_instances(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == self.grid {
            return;
        }
        self.grid = (cols, rows);
        self.quads_per_row = QUADS_PER_COLUMN * cols as usize + 4;
        // A fresh buffer is zeroed, which is a quad that draws nothing.
        self.row_counts = vec![0; rows as usize];
        let size = rows as usize * self.quads_per_row * QUAD_SIZE;
        self.instances = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("inkstone-gui quads"),
            size: size as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
    }

    /// Fill `out` with the quads of row `y`, in the order they must be drawn:
    /// the row's background, then the backgrounds of its runs, then its glyphs,
    /// then its rules, then the cursor.
    fn build_row(
        &mut self,
        grid: &Grid,
        y: u16,
        cursor: (u16, u16),
        cursor_visible: bool,
        insert: bool,
        out: &mut Vec<Quad>,
    ) {
        let (cell_w, cell_h) = self.cell;
        let capacity = self.quads_per_row;
        let cols = grid.width as usize;
        let row = grid.row(y);
        let top = f32::from(y) * cell_h;

        // The row's own background, first, so that a row is never unpainted even
        // if the runs below it are thin.
        push(
            out,
            capacity,
            Quad::fill(0.0, top, cols as f32 * cell_w, cell_h, theme::BACKGROUND),
        );

        // Backgrounds, one quad per run of the same colour.
        let mut x = 0;
        while x < cols {
            let color = style_at(row, x).bg;
            let start = x;
            x += 1;
            while x < cols && style_at(row, x).bg == color {
                x += 1;
            }
            push(
                out,
                capacity,
                Quad::fill(
                    start as f32 * cell_w,
                    top,
                    (x - start) as f32 * cell_w,
                    cell_h,
                    color,
                ),
            );
        }

        // Glyphs. A continuation cell is the right half of the wide grapheme in
        // the cell to its left, so it has nothing of its own to draw.
        for (x, cell) in row.iter().enumerate() {
            if cell.is_continuation() {
                continue;
            }
            let styled = theme::style(cell);
            let Some(glyph) = self.glyphs.glyph(&cell.text, styled.bold, styled.italic) else {
                continue;
            };
            if glyph.rect.is_empty() {
                continue;
            }
            push(
                out,
                capacity,
                glyph_quad(x as f32 * cell_w, top, &glyph, styled.fg),
            );
        }

        // Rules, drawn over the glyphs so an underline is not hidden by a
        // descender.
        let thickness = (cell_h * RULE_THICKNESS).max(1.0);
        push_rules(
            row,
            out,
            capacity,
            top + cell_h * UNDERLINE_POSITION,
            cell_w,
            thickness,
            underlined,
        );
        push_rules(
            row,
            out,
            capacity,
            top + cell_h * STRIKETHROUGH_POSITION,
            cell_w,
            thickness,
            struck,
        );

        // The cursor last, so it covers whatever is under it: a block in the
        // cell's foreground colour, with the glyph redrawn over it in the cell's
        // background colour — or a bar at the cell's left edge while text is
        // being inserted, which is where the next character will go.
        if !cursor_visible || cursor.1 != y || cursor.0 as usize >= cols {
            return;
        }
        let column = cursor.0 as usize;
        let cell = &row[column];
        let styled = style_at(row, column);
        let left = cursor.0 as f32 * cell_w;
        if insert {
            // A bar the width of the cell's own stroke: wide enough to see,
            // narrow enough to read the character under it.
            let bar = (cell_w * 0.12).max(1.0);
            push(out, capacity, Quad::fill(left, top, bar, cell_h, styled.fg));
            return;
        }
        push(
            out,
            capacity,
            Quad::fill(left, top, cell_w, cell_h, styled.fg),
        );
        if cell.is_continuation() {
            return;
        }
        if let Some(glyph) = self.glyphs.glyph(&cell.text, styled.bold, styled.italic) {
            if !glyph.rect.is_empty() {
                push(out, capacity, glyph_quad(left, top, &glyph, styled.bg));
            }
        }
    }

    /// Upload whatever the glyph atlas gained since the last call.
    ///
    /// When the atlas grew it is a *different texture* — a texture cannot be
    /// resized — so everything already packed into it is uploaded again. The
    /// rectangles themselves do not move, so no quad has to be rebuilt for it.
    fn sync_atlas(&mut self) {
        let size = self.glyphs.atlas_size();
        if size != self.atlas_size {
            self.atlas = create_atlas(&self.device, size);
            self.atlas_size = size;
            self.bind_group = bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.sampler,
                &self.uniform,
                &self.atlas,
            );
            self.glyphs.take_pending();
            self.upload_atlas(AtlasRect {
                x: 0,
                y: 0,
                w: size.0,
                h: size.1,
            });
            return;
        }
        for rect in self.glyphs.take_pending() {
            self.upload_atlas(rect);
        }
    }

    /// Copy one rectangle of the CPU-side atlas into the texture.
    fn upload_atlas(&mut self, rect: AtlasRect) {
        if rect.is_empty()
            || rect.x + rect.w > self.atlas_size.0
            || rect.y + rect.h > self.atlas_size.1
        {
            return;
        }
        // A copy of more than one row has to start every row on a 256-byte
        // boundary, which a glyph's width is not.
        let stride = if rect.h > 1 {
            rect.w.div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT
        } else {
            rect.w
        };
        self.upload.clear();
        self.upload.resize(stride as usize * rect.h as usize, 0);

        let atlas_width = self.atlas_size.0 as usize;
        let width = rect.w as usize;
        for row in 0..rect.h as usize {
            let source = (rect.y as usize + row) * atlas_width + rect.x as usize;
            let destination = row * stride as usize;
            if let (Some(from), Some(to)) = (
                self.glyphs.pixels().get(source..source + width),
                self.upload.get_mut(destination..destination + width),
            ) {
                to.copy_from_slice(from);
            }
        }

        let mut copy = self.atlas.as_image_copy();
        copy.origin = wgpu::Origin3d {
            x: rect.x,
            y: rect.y,
            z: 0,
        };
        self.queue.write_texture(
            copy,
            &self.upload,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(rect.h),
            },
            wgpu::Extent3d {
                width: rect.w,
                height: rect.h,
                depth_or_array_layers: 1,
            },
        );
    }
}

impl Quad {
    /// A flat rectangle of `color`, in pixels.
    fn fill(x: f32, y: f32, width: f32, height: f32, color: Rgba) -> Self {
        Self {
            rect: [x, y, width, height],
            atlas: [0.0; 4],
            color,
            mode: MODE_FILL,
        }
    }

    /// Append this quad to `out`, in the order the vertex layout declares.
    fn encode(&self, out: &mut Vec<u8>) {
        for value in self
            .rect
            .iter()
            .chain(self.atlas.iter())
            .chain(self.color.iter())
        {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&self.mode.to_le_bytes());
    }
}

/// Encode one rebuilt row, clearing the slots it no longer uses.
///
/// The frame draws every row's whole reservation — one range per row, so a
/// delta can upload one row — and a quad of all zeros draws nothing (a zero-sized
/// rectangle). That makes "the slots this row is not using" safe to draw *only*
/// if they still hold zeros, and a row that shrinks does not: deleting the last
/// line of a file, or moving a cursor off a long line, would leave the old
/// glyphs on screen. So a row is padded back out to whatever it used before, and
/// the returned count is what that row now occupies.
fn encode_row(quads: &[Quad], previously: usize, out: &mut Vec<u8>) -> usize {
    out.clear();
    for quad in quads {
        quad.encode(out);
    }
    let written = quads.len().max(previously);
    out.resize(written * QUAD_SIZE, 0);
    written
}

/// A glyph's quad, positioned from the cell's top-left corner.
fn glyph_quad(left: f32, top: f32, glyph: &Glyph, color: Rgba) -> Quad {
    Quad {
        rect: [
            left + glyph.offset.0,
            top + glyph.offset.1,
            glyph.rect.w as f32,
            glyph.rect.h as f32,
        ],
        atlas: [
            glyph.rect.x as f32,
            glyph.rect.y as f32,
            glyph.rect.w as f32,
            glyph.rect.h as f32,
        ],
        color,
        mode: MODE_GLYPH,
    }
}

/// Add a quad to a row, if the row's reservation has room for it.
///
/// A row can only overflow its reservation if the layout above it is wrong; the
/// excess is dropped rather than written over the next row's quads.
fn push(out: &mut Vec<Quad>, capacity: usize, quad: Quad) {
    if out.len() < capacity {
        out.push(quad);
    }
}

/// Whether a style asks for a rule under the glyph.
fn underlined(style: &Style) -> bool {
    style.underline
}

/// Whether a style asks for a rule through the glyph.
fn struck(style: &Style) -> bool {
    style.strikethrough
}

/// One quad per run of consecutive cells that `want` says should have a rule.
fn push_rules(
    row: &[Cell],
    out: &mut Vec<Quad>,
    capacity: usize,
    y: f32,
    cell_w: f32,
    thickness: f32,
    want: fn(&Style) -> bool,
) {
    let mut x = 0;
    while x < row.len() {
        if !want(&style_at(row, x)) {
            x += 1;
            continue;
        }
        let color = style_at(row, x).fg;
        let start = x;
        x += 1;
        while x < row.len() && want(&style_at(row, x)) && style_at(row, x).fg == color {
            x += 1;
        }
        push(
            out,
            capacity,
            Quad::fill(
                start as f32 * cell_w,
                y,
                (x - start) as f32 * cell_w,
                thickness,
                color,
            ),
        );
    }
}

/// The style of column `x`, with a continuation cell taking the style of the
/// cell its wide grapheme started in.
///
/// The daemon marks a continuation cell with empty text and reset colours, so
/// without this a wide grapheme's own background would stop one column short.
fn style_at(row: &[Cell], x: usize) -> Style {
    let mut index = x;
    loop {
        let cell = &row[index];
        if !cell.is_continuation() || index == 0 {
            return theme::style(cell);
        }
        index -= 1;
    }
}

/// The one-channel texture all glyphs are rasterized into.
fn create_atlas(device: &wgpu::Device, size: (u32, u32)) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("inkstone-gui atlas"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Bind the atlas, the sampler and the uniforms for the pipeline.
fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
    atlas: &wgpu::Texture,
) -> wgpu::BindGroup {
    let view = atlas.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("inkstone-gui"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// The sizes the shader needs, packed the way the uniform block declares them.
fn uniform_bytes(viewport: (u32, u32), atlas: (u32, u32)) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let values = [
        viewport.0 as f32,
        viewport.1 as f32,
        atlas.0 as f32,
        atlas.1 as f32,
    ];
    for (index, value) in values.iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_rpc::{Cell, Grid};

    /// A grid of spaces with `text` written into row 0, for layout tests.
    fn grid_of(text: &str) -> Grid {
        let mut grid = Grid::new(text.chars().count() as u16, 1);
        for (x, character) in text.chars().enumerate() {
            grid.put(x as u16, 0, Cell::new(character.to_string()));
        }
        grid
    }

    #[test]
    fn the_shader_parses_and_validates() {
        // wgpu compiles the shader when the window opens, which is far too late
        // to find a typo: a broken shader is a blank window, not a failed build.
        // Validating it here means a test catches it instead.
        let module = wgpu::naga::front::wgsl::parse_str(include_str!("shader.wgsl"))
            .expect("the shader parses");
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("the shader validates");

        let entry_points: Vec<&str> = module
            .entry_points
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert!(entry_points.contains(&"vs_main"), "{entry_points:?}");
        assert!(entry_points.contains(&"fs_main"), "{entry_points:?}");
    }

    /// The frame draws every row's whole reservation, so the slots a row no
    /// longer uses have to be zeroed rather than left holding last frame's
    /// glyphs.
    #[test]
    fn a_row_that_shrank_clears_the_slots_it_no_longer_uses() {
        let full: Vec<Quad> = (0..12)
            .map(|i| Quad::fill(i as f32, 0.0, 4.0, 4.0, theme::FOREGROUND))
            .collect();
        let mut bytes = Vec::new();
        assert_eq!(encode_row(&full, 0, &mut bytes), 12);
        assert_eq!(bytes.len(), 12 * QUAD_SIZE);

        let one = vec![Quad::fill(0.0, 0.0, 4.0, 4.0, theme::FOREGROUND)];
        assert_eq!(
            encode_row(&one, 12, &mut bytes),
            12,
            "as many slots as the row took before, so nothing is left over"
        );
        assert_eq!(bytes.len(), 12 * QUAD_SIZE);
        assert!(
            bytes[..QUAD_SIZE].iter().any(|byte| *byte != 0),
            "the quad the row needs is written"
        );
        assert!(
            bytes[QUAD_SIZE..].iter().all(|byte| *byte == 0),
            "and every slot it gave up is a quad that draws nothing"
        );
    }

    #[test]
    fn a_quad_is_encoded_field_by_field_in_little_endian() {
        let quad = Quad {
            rect: [1.0, 2.0, 3.0, 4.0],
            atlas: [5.0, 6.0, 7.0, 8.0],
            color: [0.5, 0.25, 0.0, 1.0],
            mode: MODE_GLYPH,
        };
        let mut bytes = Vec::new();
        quad.encode(&mut bytes);
        assert_eq!(bytes.len(), QUAD_SIZE, "the layout and the encoder agree");

        let mut expected = Vec::new();
        for value in [
            1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 0.5, 0.25, 0.0, 1.0,
        ] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        expected.extend_from_slice(&MODE_GLYPH.to_le_bytes());
        assert_eq!(bytes, expected);
    }

    #[test]
    fn the_uniform_holds_both_sizes_as_floats() {
        let bytes = uniform_bytes((800, 600), (512, 1024));
        let mut expected = Vec::new();
        for value in [800.0f32, 600.0, 512.0, 1024.0] {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(bytes.to_vec(), expected);
    }

    #[test]
    fn a_continuation_cell_takes_the_style_of_the_glyph_it_belongs_to() {
        let mut grid = Grid::new(2, 1);
        grid.put_grapheme(
            0,
            0,
            2,
            Cell::new("好").with_bg(inkstone_rpc::Color::Rgb(9, 9, 9)),
        );
        let row = grid.row(0);
        assert!(
            row[1].is_continuation(),
            "the second cell is the right half"
        );
        assert_eq!(
            style_at(row, 1).bg,
            style_at(row, 0).bg,
            "the wide background reaches across both cells"
        );
    }

    #[test]
    fn the_style_of_the_first_cell_is_available_even_when_it_is_a_continuation() {
        // A grid that starts with a continuation cell is not something the
        // daemon sends, but reading the style of column 0 must not panic.
        let mut grid = grid_of(" ");
        grid.put(0, 0, Cell::blank());
        let styled = style_at(grid.row(0), 0);
        assert_eq!(styled.bg, theme::BACKGROUND);
    }

    #[test]
    fn a_row_cannot_grow_past_its_reservation() {
        let mut out = Vec::new();
        for _ in 0..8 {
            push(
                &mut out,
                3,
                Quad::fill(0.0, 0.0, 1.0, 1.0, theme::FOREGROUND),
            );
        }
        assert_eq!(out.len(), 3, "the excess is dropped, not written anywhere");
    }

    #[test]
    fn rules_merge_into_one_quad_per_run() {
        let mut grid = Grid::new(4, 1);
        for x in 0..4 {
            grid.put(x, 0, Cell::new("x"));
        }
        grid.put(
            0,
            0,
            Cell::new("x").with_attrs(inkstone_rpc::ATTR_UNDERLINE),
        );
        grid.put(
            1,
            0,
            Cell::new("x").with_attrs(inkstone_rpc::ATTR_UNDERLINE),
        );

        let mut out = Vec::new();
        push_rules(grid.row(0), &mut out, 16, 0.0, 10.0, 2.0, underlined);
        assert_eq!(out.len(), 1, "one underline for the two cells in a row");
        assert_eq!(out[0].rect, [0.0, 0.0, 20.0, 2.0], "covering both cells");
    }
}
