struct CameraUniform {
    view_proj: mat4x4<f32>,
    cam_forward: vec4<f32>,
    cam_position: vec4<f32>,
    viewport: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
};
@group(0) @binding(0)
var<uniform> camera: CameraUniform;

struct ColorStop {
    color: vec4<f32>,
    pos: vec4<f32>,
};

struct BlockVolume {
    fallback_color: vec4<f32>,
    // x: stop count, y: reference cell length, z: opacity cutoff,
    // w: maximum DDA steps.
    options: vec4<f32>,
    // x: pixel-footprint multiple of the reference cell length at which the
    // march switches to whole-brick LOD integration; y: ray-guided residency
    // feedback enabled; z: which pixel of each 8x8 tile samples feedback this
    // cycle (0..63, x = phase & 7, y = phase >> 3); w: beam lookup enabled.
    lod: vec4<f32>,
    // xyz: cell counts; w: bitmask of axes whose planes are uniformly spaced
    // (1 = x, 2 = y, 4 = z) — those axes resolve plane positions and cell
    // indices arithmetically instead of via storage-buffer searches.
    dims: vec4<u32>,
    // xyz: brick counts per axis; w: brick edge length in cells.
    brick_dims: vec4<u32>,
    // xyz: local bounds; w: boundary rim highlights enabled.
    bounds_min: vec4<f32>,
    bounds_max: vec4<f32>,
    scene_to_local_0: vec4<f32>,
    scene_to_local_1: vec4<f32>,
    scene_to_local_2: vec4<f32>,
    // First plane / spacing / inverse spacing per axis (valid for axes
    // flagged in dims.w).
    plane_origin: vec4<f32>,
    plane_step: vec4<f32>,
    plane_inv_step: vec4<f32>,
    // xyz: super-brick (SUPER_FACTOR^3 bricks) counts per axis; w: index of
    // the first super-brick aggregate in `brick_aggregates` (they live in the
    // buffer's tail — the bind group is at the 8-storage-buffer limit), or 0
    // when the level-1 LOD is disabled.
    super_dims: vec4<u32>,
    stops: array<ColorStop, 12>,
};
@group(1) @binding(0)
var<uniform> volume: BlockVolume;

struct PlaneBuffer {
    values: array<f32>,
};
@group(1) @binding(1)
var<storage, read> x_planes: PlaneBuffer;
@group(1) @binding(2)
var<storage, read> y_planes: PlaneBuffer;
@group(1) @binding(3)
var<storage, read> z_planes: PlaneBuffer;

// Bounded cell-payload pool: `BRICK_SIZE^3` 16-bit payloads per *slot*,
// packed two-per-`u32` (so `BRICK_SIZE^3 / 2` words per slot; read through
// `fetch_cell_payload`). Which brick occupies which slot is dynamic — the CPU
// streams bricks in by camera proximity and `brick_info` maps a brick to its
// slot (or marks it non-resident, in which case the march renders it from its
// aggregate). For models whose mixed bricks all fit the pool this is filled
// once at build and never changes.
struct CellBuffer {
    values: array<u32>,
};
@group(1) @binding(4)
var<storage, read> cells: CellBuffer;

// Dense per-brick *ordinals* (index into `brick_aggregates` / `brick_info`),
// or `EMPTY_BRICK` for bricks with no occupied cells.
struct BrickTable {
    values: array<u32>,
};
@group(1) @binding(5)
var<storage, read> brick_table: BrickTable;

// One entry per *occupied* brick, indexed by ordinal: rgb =
// optical-depth-weighted mean ramp colour, w = volume-weighted mean optical
// depth. Segment lengths are normalized by the ray's complete chord through
// the model for translucent stops, so a homogeneous translucent volume has
// the opacity selected in the colour picker rather than compounding it once
// per cell. Opaque stops carry a saturating depth and still occlude at their
// first occupied cell. Used to integrate a whole brick when its cells project
// smaller than a pixel, when the brick is uniform, or when its cells are not
// resident in the pool yet.
struct BrickAggregates {
    values: array<vec4<f32>>,
};
@group(1) @binding(6)
var<storage, read> brick_aggregates: BrickAggregates;

// One entry per *occupied* brick, indexed by ordinal. Bit 31
// (`UNIFORM_BRICK_FLAG`): every real cell in the brick resolves to the same
// appearance — one ramp RGBA, or invisible (empty / alpha below the epsilon)
// — so its aggregate reproduces per-cell output exactly and it never needs
// pool residency. Bits 0..31: the brick's cell-pool slot, or
// `NOT_RESIDENT_SLOT` when its cells are not currently uploaded. The flag
// half is ramp-dependent (recomputed on restyle); the slot half is rewritten
// as the CPU streams bricks in and out.
struct BrickInfo {
    values: array<u32>,
};
@group(1) @binding(7)
var<storage, read> brick_info: BrickInfo;

// Sparse ray-guided residency feedback. Only one fragment per 8x8 tile writes
// this bitset, and only when the CPU-side pool is oversubscribed; this keeps
// atomic contention out of the common path where every mixed brick fits.
struct BrickUsage {
    words: array<atomic<u32>>,
};
@group(1) @binding(8)
var<storage, read_write> brick_usage: BrickUsage;

@group(2) @binding(0)
var scene_depth: texture_depth_multisampled_2d;

// Conservative per-8x8-tile ray entry depth from the beam pre-pass
// (`fs_beam`), indexed at frag_coord / 8 in the scaled sub-rect. 0 means "no
// information" (cleared, pre-pass disabled, or bailed out) and leaves the
// march start unchanged.
@group(3) @binding(0)
var beam_depth: texture_2d<f32>;

// Cell payloads are 16-bit and packed two-per-`u32` in the pool. `0xffff`
// empty, bit 15 the fallback flag, bits 0..14 a 15-bit grade. Must match the
// Rust constants EMPTY_CELL_PAYLOAD / FALLBACK_CELL_FLAG / CELL_GRADE_MASK.
const EMPTY_CELL_PAYLOAD: u32 = 0xffffu;
const FALLBACK_CELL_FLAG: u32 = 0x8000u;
const CELL_GRADE_MASK: u32 = 0x7fffu;
const CELL_GRADE_MAX: f32 = 32767.0;
const EMPTY_BRICK: u32 = 0xffffffffu;
// `brick_info` encoding; must match the constants in `gpu_cache.rs`.
const UNIFORM_BRICK_FLAG: u32 = 0x80000000u;
const NOT_RESIDENT_SLOT: u32 = 0x7fffffffu;
const VISIBLE_ALPHA_EPSILON: f32 = 0.004;
// Opaque stops retain surface-like occlusion: this optical depth saturates
// within even a thin cell instead of being spread across the model chord.
// Must mirror `OPAQUE_OPTICAL_DEPTH` in `block_model_ramp.rs`.
const OPAQUE_OPTICAL_DEPTH: f32 = 1.0e6;
// Ambient brightness fudge so raw ramp colours read at roughly the same
// visual energy as the lit opaque cube path, which darkens/brightens colour
// with face-normal lighting that this raycaster has no equivalent of.
const VOLUME_AMBIENT_INTENSITY: f32 = 0.68;
// Strength of the Fresnel-weighted rim highlight added at material
// boundaries (Zehner's "colored glass" cue for internal structure).
const VOLUME_BOUNDARY_STRENGTH: f32 = 0.35;
const INF: f32 = 3.402823e+38;
// Hard per-ray step ceiling applied only while the camera is interacting.
const MOTION_MAX_STEPS: u32 = 512u;
// Once this much of the pixel's energy is already committed (transmittance
// below the threshold), remaining bricks integrate from their aggregates
// even at full detail: whatever lies behind several translucent layers
// contributes at most this fraction of the final colour, so per-cell
// structure there is visually indistinguishable — and without this floor an
// orthographic zoomed-in view (whose pixel footprint never grows along the
// ray) marches every translucent ray cell-by-cell through the model's full
// depth.
const LOW_TRANSMITTANCE_AGGREGATE_FLOOR: f32 = 0.25;
// Level-1 (super-brick) LOD thresholds. Both sit a factor >= 4 beyond their
// brick-level counterparts so the two LOD boundaries never coincide: the
// footprint must exceed 4x the brick threshold, and the transmittance floor
// is 0.10 against the brick tier's 0.25. Either condition already implies the
// brick-level `coarse`, which keeps rims (gated on `!coarse`) consistent.
const SUPER_FACTOR: u32 = 4u;
const SUPER_LOD_FACTOR: f32 = 4.0;
const SUPER_TRANSMITTANCE_AGGREGATE_FLOOR: f32 = 0.10;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var out: VertexOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn ramp_color(t: f32) -> vec4<f32> {
    let stop_count = max(2, i32(volume.options.x + 0.5));
    let last_index = stop_count - 1;
    let first = volume.stops[0];
    let last = volume.stops[last_index];
    if (t < first.pos.x) {
        return vec4<f32>(0.0);
    }
    if (t >= last.pos.x) {
        return last.color;
    }
    for (var i = 1; i < 12; i = i + 1) {
        if (i >= stop_count) {
            break;
        }
        let stop = volume.stops[i];
        if (t < stop.pos.x) {
            return volume.stops[i - 1].color;
        }
    }
    return last.color;
}

fn scene_to_local(pos: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        dot(volume.scene_to_local_0.xyz, pos) + volume.scene_to_local_0.w,
        dot(volume.scene_to_local_1.xyz, pos) + volume.scene_to_local_1.w,
        dot(volume.scene_to_local_2.xyz, pos) + volume.scene_to_local_2.w,
    );
}

fn intersect_aabb(origin: vec3<f32>, dir: vec3<f32>) -> vec2<f32> {
    let inv_dir = vec3<f32>(1.0) / dir;
    let t0 = (volume.bounds_min.xyz - origin) * inv_dir;
    let t1 = (volume.bounds_max.xyz - origin) * inv_dir;
    let lo = min(t0, t1);
    let hi = max(t0, t1);
    let t_enter = max(max(lo.x, lo.y), lo.z);
    let t_exit = min(min(hi.x, hi.y), hi.z);
    return vec2<f32>(t_enter, t_exit);
}

// Plane positions. Uniform axes (dims.w bitmask) compute the lattice value
// arithmetically — regular grids never touch the plane storage buffers in
// the march. The arithmetic form is used consistently for both plane
// positions and cell lookup on such axes, so the two can never disagree.
fn x_plane(index: u32) -> f32 {
    if ((volume.dims.w & 1u) != 0u) {
        return volume.plane_origin.x + f32(index) * volume.plane_step.x;
    }
    return x_planes.values[index];
}

fn y_plane(index: u32) -> f32 {
    if ((volume.dims.w & 2u) != 0u) {
        return volume.plane_origin.y + f32(index) * volume.plane_step.y;
    }
    return y_planes.values[index];
}

fn z_plane(index: u32) -> f32 {
    if ((volume.dims.w & 4u) != 0u) {
        return volume.plane_origin.z + f32(index) * volume.plane_step.z;
    }
    return z_planes.values[index];
}

fn find_x_cell(value: f32) -> u32 {
    if ((volume.dims.w & 1u) != 0u) {
        let t = (value - volume.plane_origin.x) * volume.plane_inv_step.x;
        return u32(clamp(i32(floor(t)), 0, i32(volume.dims.x) - 1));
    }
    var lo = 0u;
    var hi = volume.dims.x + 1u;
    while (lo < hi) {
        let mid = (lo + hi) / 2u;
        if (x_planes.values[mid] <= value) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    if (lo == 0u) {
        return 0u;
    }
    return min(lo - 1u, volume.dims.x - 1u);
}

fn find_y_cell(value: f32) -> u32 {
    if ((volume.dims.w & 2u) != 0u) {
        let t = (value - volume.plane_origin.y) * volume.plane_inv_step.y;
        return u32(clamp(i32(floor(t)), 0, i32(volume.dims.y) - 1));
    }
    var lo = 0u;
    var hi = volume.dims.y + 1u;
    while (lo < hi) {
        let mid = (lo + hi) / 2u;
        if (y_planes.values[mid] <= value) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    if (lo == 0u) {
        return 0u;
    }
    return min(lo - 1u, volume.dims.y - 1u);
}

fn find_z_cell(value: f32) -> u32 {
    if ((volume.dims.w & 4u) != 0u) {
        let t = (value - volume.plane_origin.z) * volume.plane_inv_step.z;
        return u32(clamp(i32(floor(t)), 0, i32(volume.dims.z) - 1));
    }
    var lo = 0u;
    var hi = volume.dims.z + 1u;
    while (lo < hi) {
        let mid = (lo + hi) / 2u;
        if (z_planes.values[mid] <= value) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    if (lo == 0u) {
        return 0u;
    }
    return min(lo - 1u, volume.dims.z - 1u);
}

fn brick_of(i: u32, j: u32, k: u32) -> u32 {
    let bs = volume.brick_dims.w;
    return ((k / bs) * volume.brick_dims.y + (j / bs)) * volume.brick_dims.x + (i / bs);
}

// Resolved appearance of the cell at (i, j, k) for rim highlights: rgb plus
// opacity in `w` (0 = empty or below the visibility epsilon).
// Handles every brick kind: empty bricks are invisible; uniform bricks
// resolve from their aggregate (exact — it *is* the cell colour); resident
// mixed bricks resolve the actual payload; non-resident mixed bricks fall
// back to their aggregate colour (approximate, transient until streamed in).
fn face_appearance(i: u32, j: u32, k: u32) -> vec4<f32> {
    let ordinal = brick_table.values[brick_of(i, j, k)];
    if (ordinal == EMPTY_BRICK) {
        return vec4<f32>(0.0);
    }
    let info = brick_info.values[ordinal];
    let slot = info & NOT_RESIDENT_SLOT;
    if ((info & UNIFORM_BRICK_FLAG) != 0u || slot == NOT_RESIDENT_SLOT) {
        let aggregate = brick_aggregates.values[ordinal];
        let alpha = 1.0 - exp(-aggregate.w);
        return vec4<f32>(aggregate.rgb, select(0.0, alpha, alpha >= VISIBLE_ALPHA_EPSILON));
    }
    let bs = volume.brick_dims.w;
    let local = ((k % bs) * bs + (j % bs)) * bs + (i % bs);
    let payload = fetch_cell_payload(slot, bs, local);
    if (payload == EMPTY_CELL_PAYLOAD) {
        return vec4<f32>(0.0);
    }
    let color = color_for_payload(payload);
    return vec4<f32>(color.rgb, select(0.0, color.a, color.a >= VISIBLE_ALPHA_EPSILON));
}

fn optical_depth_for_alpha(alpha: f32) -> f32 {
    if (alpha >= 0.98) {
        return OPAQUE_OPTICAL_DEPTH;
    }
    return -log(max(1.0 - alpha, 0.001));
}

fn color_for_payload(payload: u32) -> vec4<f32> {
    if ((payload & FALLBACK_CELL_FLAG) != 0u) {
        return volume.fallback_color;
    }
    let grade = f32(payload & CELL_GRADE_MASK) / CELL_GRADE_MAX;
    return ramp_color(grade);
}

// One 16-bit cell payload from the pool's packed `u32`s: `local` selects the
// low half (even) or high half (odd) of word `slot * (bs^3 / 2) + local / 2`.
fn fetch_cell_payload(slot: u32, bs: u32, local: u32) -> u32 {
    let word = cells.values[slot * ((bs * bs * bs) >> 1u) + (local >> 1u)];
    return select(word & 0xffffu, word >> 16u, (local & 1u) == 1u);
}

@fragment
fn fs_main(@builtin(position) frag_coord: vec4<f32>) -> @location(0) vec4<f32> {
    // camera.viewport.w carries the render scale (<1 while interacting): the
    // raycaster draws into a `viewport.xy * render_scale` sub-rect that is
    // upscaled afterwards. `frag_coord` is in that sub-rect's space, so the
    // full-screen ray for this fragment uses the scaled dims, and the
    // full-resolution depth texel is at `frag_coord / render_scale`.
    let render_scale = select(camera.viewport.w, 1.0, camera.viewport.w <= 0.0);
    let scaled_dims = camera.viewport.xy * render_scale;
    let uv = frag_coord.xy / max(scaled_dims, vec2<f32>(1.0));
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near_h = camera.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far_h = camera.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let scene_near = near_h.xyz / near_h.w;
    let scene_far = far_h.xyz / far_h.w;
    let scene_dir = normalize(scene_far - scene_near);

    let local_near = scene_to_local(scene_near);
    let local_far = scene_to_local(scene_far);
    let local_dir = normalize(local_far - local_near);
    let hit = intersect_aabb(local_near, local_dir);
    // `intersect_aabb` describes an infinite ray. The camera's far point is
    // also the back of the slice slab, so cap the ray there explicitly.
    // Rasterized geometry gets this clipping from the fixed-function clip
    // volume; a fullscreen raycaster otherwise continues through the rest of
    // the block model and makes the whole model visible in slice view.
    let frustum_exit = length(local_far - local_near);
    let volume_exit = min(hit.y, frustum_exit);
    if (volume_exit < max(hit.x, 0.0)) {
        discard;
    }
    let volume_entry = max(hit.x, 0.0);
    // Normalize translucent optical depth over the complete model chord. This
    // makes a partial transfer-function alpha an opacity for the visible
    // volume as a whole, independent of how many cells lie along the ray.
    // Opaque stops use a saturating optical depth and remain surface-like.
    let ray_span = max(volume_exit - volume_entry, 1.0e-6);

    // Pixel-footprint growth along the ray: unproject the horizontally
    // adjacent pixel and measure how far the two rays diverge per unit t, so
    // footprint(t) ≈ fp_base + fp_slope * t in world units. Drives the
    // cell-vs-brick LOD decision each step.
    let ndc_next = vec2<f32>(ndc.x + 2.0 / max(scaled_dims.x, 1.0), ndc.y);
    let near_next_h = camera.inv_view_proj * vec4<f32>(ndc_next, 1.0, 1.0);
    let far_next_h = camera.inv_view_proj * vec4<f32>(ndc_next, 0.0, 1.0);
    let near_next = near_next_h.xyz / near_next_h.w;
    let far_next = far_next_h.xyz / far_next_h.w;
    let fp_base = length(near_next - scene_near);
    let fp_slope = length(normalize(far_next - near_next) - scene_dir);
    // camera.viewport.z carries a motion-quality factor (>1 while the camera
    // is being dragged): dividing the threshold makes bricks coarsen sooner,
    // trading detail for far fewer steps during interaction.
    let motion_quality = max(camera.viewport.z, 1.0);
    let lod_len = volume.lod.x * max(volume.options.y, 1.0e-6) / motion_quality;

    let pixel = vec2<i32>(frag_coord.xy / render_scale);
    let opaque_depth = textureLoad(scene_depth, pixel, 0);
    var t = volume_entry;
    var t_exit = volume_exit;
    // Occlusion by the opaque scene: unproject the pixel's opaque depth to a
    // distance along this ray ONCE and clamp the march to it, instead of
    // pushing a segment midpoint through the camera matrix on every step.
    // The scene→local transform is a rigid rotation, so scene-space t equals
    // local-space t. This also clips the final segment exactly at the
    // occluder rather than dropping/keeping whole cells by their midpoint.
    if (opaque_depth > 0.0) {
        let occluder_h = camera.inv_view_proj * vec4<f32>(ndc, opaque_depth, 1.0);
        let occluder = occluder_h.xyz / occluder_h.w;
        t_exit = min(t_exit, dot(occluder - scene_near, scene_dir) - 1.0e-5);
    }
    if (t_exit <= t) {
        discard;
    }
    // Beam optimization (Laine & Karras 2010, §4.2): start the march at the
    // tile's conservative entry depth instead of the volume AABB, skipping
    // the per-pixel walk through empty bricks in front of the content.
    if (volume.lod.w > 0.5) {
        let beam = textureLoad(beam_depth, vec2<i32>(frag_coord.xy) / 8, 0).x;
        if (beam > 0.0) {
            t = max(t, min(beam, t_exit));
        }
    }

    var p = local_near + local_dir * (t + 1.0e-5);
    var i = find_x_cell(p.x);
    var j = find_y_cell(p.y);
    var k = find_z_cell(p.z);
    var color = vec3<f32>(0.0);
    var transmittance = 1.0;
    // While interacting (signalled by the reduced render scale) cap the march
    // length so worst-case deep rays can't blow the frame budget; the
    // resolution drop already hides the early cut-off, and it snaps back to
    // the full cap the moment the camera settles.
    var max_steps = u32(volume.options.w + 0.5);
    if (render_scale < 1.0) {
        max_steps = min(max_steps, MOTION_MAX_STEPS);
    }
    // Rim highlights cost extra neighbour resolutions at every boundary
    // crossing. During interaction the frame is rendered at reduced
    // resolution and upscaled, so the subtle Fresnel rims are not resolvable
    // anyway — skip them and get the frame out faster.
    let enable_rims = render_scale >= 1.0 && volume.bounds_min.w > 0.5;
    // Rotate the sampled pixel through the tile across feedback cycles so a
    // brick projecting onto only a sliver of the tile is still requested
    // eventually instead of being permanently invisible to residency.
    let feedback_phase = u32(volume.lod.z);
    let feedback_sample = volume.lod.y > 0.5
        && (u32(frag_coord.x) & 7u) == (feedback_phase & 7u)
        && (u32(frag_coord.y) & 7u) == (feedback_phase >> 3u);
    var last_feedback_ordinal = EMPTY_BRICK;

    for (var step = 0u; step < max_steps; step = step + 1u) {
        if (t >= t_exit || (1.0 - transmittance) >= volume.options.z) {
            break;
        }

        p = local_near + local_dir * t;

        // Brick-granular fast path — taken for every brick except resident
        // mixed bricks at full detail. One iteration handles the whole brick:
        //  - empty brick: contributes nothing, skipped in one step;
        //  - uniform brick (every cell one appearance): its aggregate IS the
        //    cell appearance, so one Beer–Lambert exp over the span is *exact*
        //    (multiplicative transmittance; internal same-colour crossings
        //    never highlight) — uniform bricks therefore never need cells;
        //  - `coarse` (cells project below 1/lod-factor of a pixel): per-cell
        //    detail is invisible, integrate the aggregate as an approximation;
        //  - non-resident mixed brick: cells not streamed in yet, integrate
        //    the aggregate as a graceful stand-in until the CPU uploads them.
        let ordinal = brick_table.values[brick_of(i, j, k)];
        let coarse = fp_base + fp_slope * t > lod_len
            || transmittance < LOW_TRANSMITTANCE_AGGREGATE_FLOOR;
        var brick_uniform = false;
        var resident = false;
        var slot = NOT_RESIDENT_SLOT;
        if (ordinal != EMPTY_BRICK) {
            let info = brick_info.values[ordinal];
            brick_uniform = (info & UNIFORM_BRICK_FLAG) != 0u;
            slot = info & NOT_RESIDENT_SLOT;
            resident = slot != NOT_RESIDENT_SLOT;
            if (feedback_sample && !brick_uniform && ordinal != last_feedback_ordinal) {
                atomicOr(&brick_usage.words[ordinal / 32u], 1u << (ordinal % 32u));
                last_feedback_ordinal = ordinal;
            }
        }
        if (ordinal == EMPTY_BRICK || coarse || brick_uniform || !resident) {
            let bs = volume.brick_dims.w;
            var region = bs;
            var aggregate = vec4<f32>(0.0);
            if (ordinal != EMPTY_BRICK) {
                aggregate = brick_aggregates.values[ordinal];
            }
            // Whether this region kind would emit an exit-face rim at full
            // detail (empty and uniform bricks only — see the rim block).
            var rims_allowed = ordinal == EMPTY_BRICK || brick_uniform;
            // Level-1 super-brick LOD. An invisible super region (w == 0)
            // implies every child brick is empty or uniform-invisible, so
            // stepping it whole — rim check at its exit face included — is
            // exactly what per-brick stepping would produce, just in one
            // iteration. A visible region integrates from its own aggregate
            // once the footprint/transmittance is a factor SUPER_LOD_FACTOR
            // past the brick-LOD thresholds. Only evaluated on the
            // brick-granular path, so per-cell marching inside resident
            // mixed bricks pays nothing for it.
            if (volume.super_dims.w != 0u) {
                let sbs = bs * SUPER_FACTOR;
                let s_index = ((k / sbs) * volume.super_dims.y + (j / sbs)) * volume.super_dims.x
                    + (i / sbs);
                let s_aggregate = brick_aggregates.values[volume.super_dims.w + s_index];
                if (s_aggregate.w == 0.0) {
                    region = sbs;
                    aggregate = vec4<f32>(0.0);
                    rims_allowed = true;
                } else if (fp_base + fp_slope * t > SUPER_LOD_FACTOR * lod_len
                    || transmittance < SUPER_TRANSMITTANCE_AGGREGATE_FLOOR) {
                    region = sbs;
                    aggregate = s_aggregate;
                    rims_allowed = false;
                }
            }
            let bx0 = (i / region) * region;
            let bx1 = min(bx0 + region, volume.dims.x);
            let by0 = (j / region) * region;
            let by1 = min(by0 + region, volume.dims.y);
            let bz0 = (k / region) * region;
            let bz1 = min(bz0 + region, volume.dims.z);
            let btx = select(
                INF,
                (x_plane(select(bx0, bx1, local_dir.x > 0.0)) - p.x) / local_dir.x,
                abs(local_dir.x) > 1.0e-8,
            );
            let bty = select(
                INF,
                (y_plane(select(by0, by1, local_dir.y > 0.0)) - p.y) / local_dir.y,
                abs(local_dir.y) > 1.0e-8,
            );
            let btz = select(
                INF,
                (z_plane(select(bz0, bz1, local_dir.z > 0.0)) - p.z) / local_dir.z,
                abs(local_dir.z) > 1.0e-8,
            );
            let t_brick_exit = t + max(min(min(btx, bty), btz), 0.0);

            let segment_len = max(min(t_brick_exit, t_exit) - t, 0.0);
            if (aggregate.w > 0.0 && segment_len > 0.0) {
                let alpha = clamp(1.0 - exp(-aggregate.w * segment_len / ray_span), 0.0, 1.0);
                color = color + transmittance * alpha * aggregate.rgb * VOLUME_AMBIENT_INTENSITY;
                transmittance = transmittance * (1.0 - alpha);
            }

            if (t_brick_exit >= t_exit) {
                t = t_exit;
                continue;
            }

            // Advance to the neighbouring brick. The crossed axis steps
            // across the brick face explicitly (which guarantees progress);
            // the other axes re-derive their cell index from the exit point.
            // Departure through a grid boundary ends the march.
            let bcx = btx <= bty + 1.0e-6 && btx <= btz + 1.0e-6;
            let bcy = bty <= btx + 1.0e-6 && bty <= btz + 1.0e-6;
            let bcz = btz <= btx + 1.0e-6 && btz <= bty + 1.0e-6;
            let q = local_near + local_dir * (t_brick_exit + 1.0e-5);
            var departed = false;
            var ni = i;
            var nj = j;
            var nk = k;
            if (bcx) {
                if (local_dir.x > 0.0) {
                    if (bx1 >= volume.dims.x) {
                        departed = true;
                    } else {
                        ni = bx1;
                    }
                } else if (bx0 == 0u) {
                    departed = true;
                } else {
                    ni = bx0 - 1u;
                }
            } else {
                ni = find_x_cell(q.x);
            }
            if (bcy) {
                if (local_dir.y > 0.0) {
                    if (by1 >= volume.dims.y) {
                        departed = true;
                    } else {
                        nj = by1;
                    }
                } else if (by0 == 0u) {
                    departed = true;
                } else {
                    nj = by0 - 1u;
                }
            } else {
                nj = find_y_cell(q.y);
            }
            if (bcz) {
                if (local_dir.z > 0.0) {
                    if (bz1 >= volume.dims.z) {
                        departed = true;
                    } else {
                        nk = bz1;
                    }
                } else if (bz0 == 0u) {
                    departed = true;
                } else {
                    nk = bz0 - 1u;
                }
            } else {
                nk = find_z_cell(q.z);
            }
            if (departed) {
                t = t_exit;
                continue;
            }

            // Rim highlight at the brick's exit face — only for empty/uniform
            // bricks at full detail, where it reproduces exactly what per-cell
            // stepping would emit at this crossing (interior crossings of such
            // bricks never highlight, so the face is the only candidate).
            // Coarse bricks skip rims (sub-pixel) and non-resident mixed
            // bricks skip them (transient; their cell colours are unknown).
            // The neighbour is the cell the ray actually enters at the exit
            // point, resolved across whatever brick kind it lives in.
            if (enable_rims && !coarse && rims_allowed
                && (1.0 - transmittance) < volume.options.z && t_brick_exit < t_exit) {
                let cur_opacity = 1.0 - exp(-aggregate.w);
                let cur_visible = cur_opacity >= VISIBLE_ALPHA_EPSILON;
                let neighbor = face_appearance(ni, nj, nk);
                let neighbor_visible = neighbor.w >= VISIBLE_ALPHA_EPSILON;
                var highlight_rgb = vec3<f32>(0.0);
                var is_visible_boundary = false;
                if (cur_visible && neighbor_visible) {
                    is_visible_boundary = distance(aggregate.rgb, neighbor.rgb) > 0.05;
                    highlight_rgb = (aggregate.rgb + neighbor.rgb) * 0.5;
                } else if (cur_visible) {
                    is_visible_boundary = true;
                    highlight_rgb = aggregate.rgb;
                } else if (neighbor_visible) {
                    is_visible_boundary = true;
                    highlight_rgb = neighbor.rgb;
                }
                if (is_visible_boundary) {
                    var boundary_normal = vec3<f32>(0.0);
                    if (bcx) {
                        boundary_normal = vec3<f32>(select(1.0, -1.0, local_dir.x > 0.0), 0.0, 0.0);
                    } else if (bcy) {
                        boundary_normal = vec3<f32>(0.0, select(1.0, -1.0, local_dir.y > 0.0), 0.0);
                    } else {
                        boundary_normal = vec3<f32>(0.0, 0.0, select(1.0, -1.0, local_dir.z > 0.0));
                    }
                    let fresnel = pow(
                        clamp(1.0 - abs(dot(-local_dir, boundary_normal)), 0.0, 1.0),
                        4.0,
                    );
                    let highlight_color = clamp(
                        highlight_rgb + vec3<f32>(0.15),
                        vec3<f32>(0.0),
                        vec3<f32>(1.0),
                    );
                    let highlight_opacity = max(cur_opacity, neighbor.w);
                    color = color + transmittance * fresnel * VOLUME_BOUNDARY_STRENGTH
                        * highlight_opacity * highlight_color;
                }
            }

            i = ni;
            j = nj;
            k = nk;
            t = max(t_brick_exit, t + 1.0e-6);
            continue;
        }

        let tx = select(
            INF,
            (x_plane(select(i, i + 1u, local_dir.x > 0.0)) - p.x) / local_dir.x,
            abs(local_dir.x) > 1.0e-8,
        );
        let ty = select(
            INF,
            (y_plane(select(j, j + 1u, local_dir.y > 0.0)) - p.y) / local_dir.y,
            abs(local_dir.y) > 1.0e-8,
        );
        let tz = select(
            INF,
            (z_plane(select(k, k + 1u, local_dir.z > 0.0)) - p.z) / local_dir.z,
            abs(local_dir.z) > 1.0e-8,
        );
        let step_t = max(min(min(tx, ty), tz), 0.0);
        let t_next = min(t + step_t, t_exit);
        let segment_len = max(t_next - t, 0.0);

        // Only resident mixed bricks reach the per-cell path, so `slot` is
        // valid here. Fetched once per step; the boundary-highlight block
        // below reuses it instead of re-resolving the same cell.
        let bs = volume.brick_dims.w;
        let local = ((k % bs) * bs + (j % bs)) * bs + (i % bs);
        let current_payload = fetch_cell_payload(slot, bs, local);
        let current_color = color_for_payload(current_payload);

        if (segment_len > 0.0 && current_payload != EMPTY_CELL_PAYLOAD
            && current_color.a >= VISIBLE_ALPHA_EPSILON) {
            let optical_depth = optical_depth_for_alpha(clamp(current_color.a, 0.0, 1.0));
            let alpha = clamp(1.0 - exp(-optical_depth * segment_len / ray_span), 0.0, 1.0);
            let contribution = transmittance * alpha;
            color = color + contribution * current_color.rgb * VOLUME_AMBIENT_INTENSITY;
            transmittance = transmittance * (1.0 - alpha);
        }

        let crossed_x = tx <= ty + 1.0e-6 && tx <= tz + 1.0e-6;
        let crossed_y = ty <= tx + 1.0e-6 && ty <= tz + 1.0e-6;
        let crossed_z = tz <= tx + 1.0e-6 && tz <= ty + 1.0e-6;

        // Boundary highlight: peek at the neighbour across whichever axis
        // is about to be crossed. If its resolved colour differs from the
        // current cell's, add a Fresnel-weighted rim highlight using the
        // crossed axis as a surface normal. This is what gives internal
        // material boundaries visible structure instead of the volume
        // reading as uniform fog (see Phase 7 of the rendering plan). The
        // `t + step_t < t_exit` guard skips crossings that land at/behind the
        // march end (opaque occluder or volume exit) — those boundaries are
        // not visible from this pixel.
        if (enable_rims && (1.0 - transmittance) < volume.options.z && t + step_t < t_exit) {
            var boundary_normal = vec3<f32>(0.0);
            var has_neighbor = false;
            var neighbor = vec4<f32>(0.0);
            if (crossed_x) {
                if (local_dir.x > 0.0) {
                    if (i + 1u < volume.dims.x) {
                        neighbor = face_appearance(i + 1u, j, k);
                        has_neighbor = true;
                    }
                } else if (i > 0u) {
                    neighbor = face_appearance(i - 1u, j, k);
                    has_neighbor = true;
                }
                boundary_normal = vec3<f32>(select(1.0, -1.0, local_dir.x > 0.0), 0.0, 0.0);
            } else if (crossed_y) {
                if (local_dir.y > 0.0) {
                    if (j + 1u < volume.dims.y) {
                        neighbor = face_appearance(i, j + 1u, k);
                        has_neighbor = true;
                    }
                } else if (j > 0u) {
                    neighbor = face_appearance(i, j - 1u, k);
                    has_neighbor = true;
                }
                boundary_normal = vec3<f32>(0.0, select(1.0, -1.0, local_dir.y > 0.0), 0.0);
            } else if (crossed_z) {
                if (local_dir.z > 0.0) {
                    if (k + 1u < volume.dims.z) {
                        neighbor = face_appearance(i, j, k + 1u);
                        has_neighbor = true;
                    }
                } else if (k > 0u) {
                    neighbor = face_appearance(i, j, k - 1u);
                    has_neighbor = true;
                }
                boundary_normal = vec3<f32>(0.0, 0.0, select(1.0, -1.0, local_dir.z > 0.0));
            }

            if (has_neighbor) {
                // A side is "visible" when it is a filled cell whose resolved
                // ramp alpha clears the epsilon. Empty and invisible-filled
                // cells behave identically here: a visible↔invisible crossing
                // is the model's glass surface and rims in the visible side's
                // own colour (never the fallback grey that EMPTY payloads
                // would resolve to via the fallback flag bit); a
                // visible↔visible crossing rims only when the colours differ.
                let current_visible = current_payload != EMPTY_CELL_PAYLOAD
                    && current_color.a >= VISIBLE_ALPHA_EPSILON;
                let neighbor_visible = neighbor.w >= VISIBLE_ALPHA_EPSILON;
                var highlight_rgb = vec3<f32>(0.0);
                var is_visible_boundary = false;
                if (current_visible && neighbor_visible) {
                    is_visible_boundary = distance(current_color.rgb, neighbor.rgb) > 0.05;
                    highlight_rgb = (current_color.rgb + neighbor.rgb) * 0.5;
                } else if (current_visible) {
                    is_visible_boundary = true;
                    highlight_rgb = current_color.rgb;
                } else if (neighbor_visible) {
                    is_visible_boundary = true;
                    highlight_rgb = neighbor.rgb;
                }
                if (is_visible_boundary) {
                    let fresnel = pow(
                        clamp(1.0 - abs(dot(-local_dir, boundary_normal)), 0.0, 1.0),
                        4.0,
                    );
                    let highlight_color = clamp(
                        highlight_rgb + vec3<f32>(0.15),
                        vec3<f32>(0.0),
                        vec3<f32>(1.0),
                    );
                    let current_opacity = select(0.0, current_color.a, current_visible);
                    let highlight_opacity = max(current_opacity, neighbor.w);
                    color = color + transmittance * fresnel * VOLUME_BOUNDARY_STRENGTH
                        * highlight_opacity * highlight_color;
                }
            }
        }

        if (crossed_x) {
            if (local_dir.x > 0.0) {
                if (i + 1u >= volume.dims.x) {
                    t = t_exit;
                } else {
                    i = i + 1u;
                }
            } else if (i == 0u) {
                t = t_exit;
            } else {
                i = i - 1u;
            }
        }
        if (crossed_y) {
            if (local_dir.y > 0.0) {
                if (j + 1u >= volume.dims.y) {
                    t = t_exit;
                } else {
                    j = j + 1u;
                }
            } else if (j == 0u) {
                t = t_exit;
            } else {
                j = j - 1u;
            }
        }
        if (crossed_z) {
            if (local_dir.z > 0.0) {
                if (k + 1u >= volume.dims.z) {
                    t = t_exit;
                } else {
                    k = k + 1u;
                }
            } else if (k == 0u) {
                t = t_exit;
            } else {
                k = k - 1u;
            }
        }
        if (!crossed_x && !crossed_y && !crossed_z) {
            t = t + 1.0e-5;
        } else {
            t = max(t_next, t + 1.0e-6);
        }
    }

    var alpha_out = 1.0 - transmittance;
    if (alpha_out <= 0.0001) {
        discard;
    }
    // Early termination by the opacity cutoff leaves `alpha_out` at ~cutoff
    // (e.g. 0.95) even though the ray stopped *inside* an effectively opaque
    // medium — blending that over the scene would bleed 5% of the background
    // through solid models. Snap to fully opaque, unpremultiplying so the
    // colour keeps its accumulated mix (the residual energy would have been
    // more of the same). Rays that genuinely exit the volume (t >= t_exit)
    // keep their true partial alpha.
    if (alpha_out >= volume.options.z && t < t_exit) {
        color = color / max(alpha_out, 1.0e-4);
        alpha_out = 1.0;
    }
    // Boundary lighting is a non-emissive colour cue. Keep the result a valid
    // premultiplied colour so many highlighted strata cannot collectively
    // glow more strongly than the volume's actual opacity.
    color = min(color, vec3<f32>(alpha_out));
    return vec4<f32>(color, alpha_out);
}

// Beam pre-pass: one fragment per 8x8-pixel tile of the scaled sub-rect,
// marching the brick grid only, testing occupancy dilated by the tile's
// world-space radius. Outputs a conservative ray parameter at which the
// whole tile's rays are still in front of any occupied brick; 0 when no
// useful bound exists (non-uniform axes, oversized dilation radius). The
// main pass starts its march at this depth.
@fragment
fn fs_beam(@builtin(position) frag_coord: vec4<f32>) -> @location(0) f32 {
    // Only fully uniform grids get the arithmetic brick extents the dilation
    // radius needs; sub-blocked models fall back to the unoptimized start.
    if ((volume.dims.w & 7u) != 7u) {
        return 0.0;
    }
    let render_scale = select(camera.viewport.w, 1.0, camera.viewport.w <= 0.0);
    let scaled_dims = camera.viewport.xy * render_scale;
    // Tile-center pixel in the scaled sub-rect.
    let pixel = (frag_coord.xy - vec2<f32>(0.5)) * 8.0 + vec2<f32>(4.0);
    let uv = pixel / max(scaled_dims, vec2<f32>(1.0));
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let near_h = camera.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let far_h = camera.inv_view_proj * vec4<f32>(ndc, 0.0, 1.0);
    let scene_near = near_h.xyz / near_h.w;
    let scene_far = far_h.xyz / far_h.w;
    let scene_dir = normalize(scene_far - scene_near);
    let local_near = scene_to_local(scene_near);
    let local_far = scene_to_local(scene_far);
    let local_dir = normalize(local_far - local_near);
    let hit = intersect_aabb(local_near, local_dir);
    let frustum_exit = length(local_far - local_near);
    let volume_exit = min(hit.y, frustum_exit);
    if (volume_exit < max(hit.x, 0.0)) {
        // The tile centre misses the AABB; edge rays in the tile may still
        // hit it, so claim no bound rather than t_exit.
        return 0.0;
    }

    // Per-pixel world footprint along the ray, as in fs_main.
    let ndc_next = vec2<f32>(ndc.x + 2.0 / max(scaled_dims.x, 1.0), ndc.y);
    let near_next_h = camera.inv_view_proj * vec4<f32>(ndc_next, 1.0, 1.0);
    let far_next_h = camera.inv_view_proj * vec4<f32>(ndc_next, 0.0, 1.0);
    let near_next = near_next_h.xyz / near_next_h.w;
    let far_next = far_next_h.xyz / far_next_h.w;
    let fp_base = length(near_next - scene_near);
    let fp_slope = length(normalize(far_next - near_next) - scene_dir);

    let bs = volume.brick_dims.w;
    let brick_extent = volume.plane_step.xyz * f32(bs);
    let min_brick_extent = min(brick_extent.x, min(brick_extent.y, brick_extent.z));
    let brick_diagonal = length(brick_extent);

    var t = max(hit.x, 0.0);
    var p = local_near + local_dir * (t + 1.0e-5);
    var i = find_x_cell(p.x);
    var j = find_y_cell(p.y);
    var k = find_z_cell(p.z);

    for (var step = 0u; step < 256u; step = step + 1u) {
        if (t >= volume_exit) {
            // Whole tile-centre ray crossed only empty bricks. Edge rays may
            // enter laterally adjacent occupied bricks the dilation below
            // already tested, so the exit bound is safe.
            return max(volume_exit - brick_diagonal, 0.0);
        }
        p = local_near + local_dir * t;

        // The tile's rays fan out to ~6 px around the centre (4 px half-tile
        // plus margin); convert that world radius into a brick-count
        // dilation radius. Bail conservatively when it exceeds one brick —
        // that only happens far away / zoomed out, where the pre-pass gains
        // nothing anyway.
        let tile_radius = 6.0 * (fp_base + fp_slope * t);
        if (tile_radius >= min_brick_extent) {
            return max(t - brick_diagonal - tile_radius, 0.0);
        }
        let bi = i / bs;
        let bj = j / bs;
        let bk = k / bs;
        var occupied = false;
        for (var dz = -1; dz <= 1; dz = dz + 1) {
            for (var dy = -1; dy <= 1; dy = dy + 1) {
                for (var dx = -1; dx <= 1; dx = dx + 1) {
                    let nx = i32(bi) + dx;
                    let ny = i32(bj) + dy;
                    let nz = i32(bk) + dz;
                    if (nx < 0 || ny < 0 || nz < 0
                        || nx >= i32(volume.brick_dims.x)
                        || ny >= i32(volume.brick_dims.y)
                        || nz >= i32(volume.brick_dims.z)) {
                        continue;
                    }
                    let table_index =
                        (u32(nz) * volume.brick_dims.y + u32(ny)) * volume.brick_dims.x + u32(nx);
                    if (brick_table.values[table_index] != EMPTY_BRICK) {
                        occupied = true;
                    }
                }
            }
        }
        if (occupied) {
            return max(t - brick_diagonal - tile_radius, 0.0);
        }

        // Advance to the next brick along the ray (uniform axes only, so all
        // plane positions are arithmetic).
        let bx = select(bi * bs, (bi + 1u) * bs, local_dir.x > 0.0);
        let by = select(bj * bs, (bj + 1u) * bs, local_dir.y > 0.0);
        let bz = select(bk * bs, (bk + 1u) * bs, local_dir.z > 0.0);
        let btx = select(INF, (x_plane(min(bx, volume.dims.x)) - p.x) / local_dir.x, abs(local_dir.x) > 1.0e-8);
        let bty = select(INF, (y_plane(min(by, volume.dims.y)) - p.y) / local_dir.y, abs(local_dir.y) > 1.0e-8);
        let btz = select(INF, (z_plane(min(bz, volume.dims.z)) - p.z) / local_dir.z, abs(local_dir.z) > 1.0e-8);
        let t_next = t + max(min(min(btx, bty), btz), 0.0) + 1.0e-5;
        let q = local_near + local_dir * t_next;
        i = find_x_cell(q.x);
        j = find_y_cell(q.y);
        k = find_z_cell(q.z);
        t = max(t_next, t + 1.0e-6);
    }
    // Step cap reached: everything marched so far was empty.
    return max(t - brick_diagonal, 0.0);
}
