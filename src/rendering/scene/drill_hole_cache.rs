use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use glam::DVec3;
use wgpu::util::DeviceExt;

use crate::model::drill_hole::{
    COLLAR_MARKER_FILL_COLOR, COLLAR_MARKER_MIN_PIXEL_DIAMETER, COLLAR_MARKER_OUTLINE_COLOR, COLLAR_MARKER_RADIUS_SCALE, DrillColorState, DrillFieldKind, DrillHoleId, DrillValue,
    MIN_RENDER_PIXEL_DIAMETER, OpenDrillHoleDataset, TIE_RADIUS_SCALE,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrillSegmentInstance {
    pub(crate) start: [f32; 3],
    pub(crate) radius: f32,
    pub(crate) end: [f32; 3],
    pub(crate) pixel_diameter: f32,
    pub(crate) color: [f32; 3],
    pub(crate) _pad1: f32,
}

/// The disc drawn at the top of a hole so its collar reads at a glance
/// instead of being the indistinguishable end of a cylinder.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrillCollarInstance {
    pub(crate) center: [f32; 3],
    pub(crate) marker_radius: f32,
    pub(crate) outline: [f32; 3],
    pub(crate) pixel_diameter: f32,
    pub(crate) fill: [f32; 3],
    pub(crate) hole_radius: f32,
}

pub(crate) struct CachedDrillHoles {
    pub(crate) buffer: Option<wgpu::Buffer>,
    pub(crate) count: u32,
    pub(crate) tie_buffer: Option<wgpu::Buffer>,
    pub(crate) tie_count: u32,
    pub(crate) collar_buffer: Option<wgpu::Buffer>,
    pub(crate) collar_count: u32,
    key: u64,
}

#[derive(Default)]
pub(crate) struct DrillHoleGpuCache {
    entries: HashMap<DrillHoleId, CachedDrillHoles>,
    /// Transient pattern-menu geometry, kept outside the id-keyed project
    /// entries so it can use the normal drill shaders without pretending to
    /// be a dataset before Create is pressed.
    preview: Option<CachedDrillHoles>,
}

impl DrillHoleGpuCache {
    pub(crate) fn sync(&mut self, device: &wgpu::Device, scene_origin: DVec3, datasets: &[OpenDrillHoleDataset], editor: &crate::ui::state::EditorState) {
        self.entries.retain(|id, _| datasets.iter().any(|dataset| dataset.id == *id && dataset.state.loaded));
        for dataset in datasets {
            if !dataset.state.loaded {
                continue;
            }
            let selection = HoleSelection::of(dataset, editor);
            let key = dataset_key(dataset, scene_origin, &selection);
            if self.entries.get(&dataset.id).is_some_and(|cached| cached.key == key) {
                continue;
            }
            let instances = build_instances(dataset, scene_origin, &selection);
            let buffer = (!instances.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Drillhole Segment Instances"),
                    contents: bytemuck::cast_slice(&instances),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            let ties = build_tie_instances(dataset, scene_origin, &selection);
            let tie_buffer = (!ties.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Drillhole Tie-In Instances"),
                    contents: bytemuck::cast_slice(&ties),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            let collars = build_collar_instances(dataset, scene_origin, &selection);
            let collar_buffer = (!collars.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Drillhole Collar Instances"),
                    contents: bytemuck::cast_slice(&collars),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            self.entries.insert(
                dataset.id,
                CachedDrillHoles {
                    buffer,
                    count: instances.len().min(u32::MAX as usize) as u32,
                    tie_buffer,
                    tie_count: ties.len().min(u32::MAX as usize) as u32,
                    collar_buffer,
                    collar_count: collars.len().min(u32::MAX as usize) as u32,
                    key,
                },
            );
        }
        self.sync_pattern_preview(device, scene_origin, editor);
    }

    fn sync_pattern_preview(&mut self, device: &wgpu::Device, scene_origin: DVec3, editor: &crate::ui::state::EditorState) {
        if !editor.drill_pattern_open
            || editor.drill_pattern_preview_collars.is_empty()
            || !editor.drill_pattern_preview_depth.is_finite()
            || editor.drill_pattern_preview_depth <= 0.0
            || !editor.drill_pattern_preview_diameter.is_finite()
            || editor.drill_pattern_preview_diameter <= 0.0
        {
            self.preview = None;
            return;
        }

        let mut hash = DefaultHasher::new();
        editor.drill_pattern_preview_depth.to_bits().hash(&mut hash);
        editor.drill_pattern_preview_diameter.to_bits().hash(&mut hash);
        for value in scene_origin.to_array() {
            value.to_bits().hash(&mut hash);
        }
        for collar in &editor.drill_pattern_preview_collars {
            for value in collar.to_array() {
                value.to_bits().hash(&mut hash);
            }
        }
        let key = hash.finish();
        if self.preview.as_ref().is_some_and(|cached| cached.key == key) {
            return;
        }

        let preview_radius = editor.drill_pattern_preview_diameter * 0.5;
        let instances: Vec<_> = editor
            .drill_pattern_preview_collars
            .iter()
            .map(|&collar| DrillSegmentInstance {
                start: (collar - scene_origin).as_vec3().to_array(),
                radius: preview_radius as f32,
                end: (collar - DVec3::Z * editor.drill_pattern_preview_depth - scene_origin).as_vec3().to_array(),
                pixel_diameter: MIN_RENDER_PIXEL_DIAMETER,
                color: [1.0; 3],
                _pad1: 0.0,
            })
            .collect();
        let collars: Vec<_> = editor
            .drill_pattern_preview_collars
            .iter()
            .map(|&collar| DrillCollarInstance {
                center: (collar - scene_origin).as_vec3().to_array(),
                marker_radius: (preview_radius * COLLAR_MARKER_RADIUS_SCALE) as f32,
                outline: COLLAR_MARKER_OUTLINE_COLOR,
                pixel_diameter: COLLAR_MARKER_MIN_PIXEL_DIAMETER,
                fill: COLLAR_MARKER_FILL_COLOR,
                hole_radius: preview_radius as f32,
            })
            .collect();
        let buffer = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Drill Pattern Preview Segment Instances"),
            contents: bytemuck::cast_slice(&instances),
            usage: wgpu::BufferUsages::VERTEX,
        }));
        let collar_buffer = Some(device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Drill Pattern Preview Collar Instances"),
            contents: bytemuck::cast_slice(&collars),
            usage: wgpu::BufferUsages::VERTEX,
        }));
        self.preview = Some(CachedDrillHoles {
            buffer,
            count: instances.len().min(u32::MAX as usize) as u32,
            tie_buffer: None,
            tie_count: 0,
            collar_buffer,
            collar_count: collars.len().min(u32::MAX as usize) as u32,
            key,
        });
    }

    pub(crate) fn get(&self, id: DrillHoleId) -> Option<&CachedDrillHoles> {
        self.entries.get(&id)
    }
    pub(crate) fn preview(&self) -> Option<&CachedDrillHoles> {
        self.preview.as_ref()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.preview.is_none() && self.entries.values().all(|entry| entry.count == 0 && entry.tie_count == 0 && entry.collar_count == 0)
    }
}

/// Which of a dataset's holes are drawn as selected.
///
/// Production selects a dataset whole; Drill & Blast selects holes one at a
/// time - see [`crate::ui::state::EditorState::selected_drill_holes`] - so
/// this carries both and the instance builders ask it per hole.
struct HoleSelection {
    /// The whole dataset is selected: every hole in it is.
    whole: bool,
    /// Indices of the individually selected holes, ascending, so the cache
    /// key below hashes the same set the same way every frame.
    holes: Vec<usize>,
    /// Canonical hole pairs of selected tie-ins in this dataset.
    ties: Vec<(usize, usize)>,
}

impl HoleSelection {
    fn of(dataset: &OpenDrillHoleDataset, editor: &crate::ui::state::EditorState) -> Self {
        let mut holes: Vec<usize> = editor.selected_drill_holes.iter().filter(|hole| hole.dataset == dataset.id).map(|hole| hole.hole).collect();
        holes.sort_unstable();
        let mut ties: Vec<_> = editor.selected_tie_ins.iter().filter(|tie| tie.dataset == dataset.id).map(|tie| (tie.a, tie.b)).collect();
        ties.sort_unstable();
        Self {
            whole: editor.selected_handles.contains(&dataset.entity_id()),
            holes,
            ties,
        }
    }

    fn contains(&self, index: usize) -> bool {
        self.whole || self.holes.binary_search(&index).is_ok()
    }

    /// Whether anything in the dataset is selected, which is all the collar
    /// colours need to know before they are worked out hole by hole.
    fn any(&self) -> bool {
        self.whole || !self.holes.is_empty()
    }

    fn contains_tie(&self, from: usize, to: usize) -> bool {
        let pair = if from <= to { (from, to) } else { (to, from) };
        self.ties.binary_search(&pair).is_ok()
    }
}

fn dataset_key(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selection: &HoleSelection) -> u64 {
    let mut hash = DefaultHasher::new();
    dataset.id.hash(&mut hash);
    dataset.state.loaded.hash(&mut hash);
    // Hole positions are not hashed one by one: an edit to the geometry - the
    // Move Collar tool is the only one so far - bumps the item's revision, and
    // that is what tells the cache the instances it built are stale.
    dataset.state.revision().hash(&mut hash);
    selection.whole.hash(&mut hash);
    selection.holes.hash(&mut hash);
    selection.ties.hash(&mut hash);
    for value in scene_origin.to_array() {
        value.to_bits().hash(&mut hash);
    }
    dataset.color.active_field.hash(&mut hash);
    (dataset.color.preset as u8).hash(&mut hash);
    dataset.color.smooth.hash(&mut hash);
    for stop in &dataset.color.stops {
        stop.t.to_bits().hash(&mut hash);
        for value in stop.color {
            value.to_bits().hash(&mut hash);
        }
    }
    for category in &dataset.color.categories {
        category.value.hash(&mut hash);
        for value in category.color {
            value.to_bits().hash(&mut hash);
        }
    }
    hash.finish()
}

fn build_instances(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selection: &HoleSelection) -> Vec<DrillSegmentInstance> {
    if !dataset.state.loaded {
        return Vec::new();
    }
    let mut instances = Vec::new();
    let field = dataset.color.active_field.as_deref().and_then(|key| dataset.dataset.field(key));
    for (index, hole) in dataset.dataset.holes.iter().enumerate() {
        if hole.trace.len() < 2 {
            continue;
        }
        let selected = selection.contains(index);
        let min_depth = hole.trace.first().unwrap().depth;
        let max_depth = hole.trace.last().unwrap().depth;
        let mut boundaries = hole.trace.iter().map(|station| station.depth).collect::<Vec<_>>();
        if let Some(field) = field {
            for interval in &hole.intervals {
                if interval.values.contains_key(&field.key) {
                    boundaries.push(interval.from.clamp(min_depth, max_depth));
                    boundaries.push(interval.to.clamp(min_depth, max_depth));
                }
            }
        }
        boundaries.sort_by(f64::total_cmp);
        boundaries.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-9);
        for pair in boundaries.windows(2) {
            if pair[1] <= pair[0] + 1.0e-9 {
                continue;
            }
            let (Some(start), Some(end)) = (hole.position_at_depth(pair[0]), hole.position_at_depth(pair[1])) else {
                continue;
            };
            if start.distance_squared(end) <= 1.0e-18 {
                continue;
            }
            let midpoint = (pair[0] + pair[1]) * 0.5;
            if !hole.render_ranges.is_empty() && !hole.render_ranges.iter().any(|(from, to)| *from <= midpoint && midpoint < *to) {
                continue;
            }
            let value = field.and_then(|field| {
                hole.intervals
                    .iter()
                    .find(|interval| interval.from <= midpoint && midpoint < interval.to && interval.values.contains_key(&field.key))
                    .and_then(|interval| interval.values.get(&field.key))
            });
            let color = if selected {
                let [red, green, blue, _] = crate::ui::SELECTION_COLOR_F32;
                [red, green, blue]
            } else {
                field
                    .and_then(|field| value.map(|value| evaluate_color(field.kind.clone(), value, &dataset.color)))
                    .unwrap_or([1.0; 3])
            };
            instances.push(DrillSegmentInstance {
                start: (start - scene_origin).as_vec3().to_array(),
                radius: hole.diameter.map_or(0.0, |diameter| (diameter * 0.5) as f32),
                end: (end - scene_origin).as_vec3().to_array(),
                pixel_diameter: MIN_RENDER_PIXEL_DIAMETER,
                color,
                _pad1: 0.0,
            });
        }
    }
    instances
}

/// The surface connectors, drawn collar to collar through the same instanced
/// cylinder the traces use. A tie has no thickness of its own, so it takes a
/// world radius from the holes it joins and scales with them until reaching
/// the shared two-pixel screen floor.
fn build_tie_instances(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selection: &HoleSelection) -> Vec<DrillSegmentInstance> {
    let holes = &dataset.dataset.holes;
    dataset
        .dataset
        .ties
        .iter()
        .filter_map(|tie| {
            let from = holes.get(tie.from)?;
            let to = holes.get(tie.to)?;
            let start = from.collar_position();
            let end = to.collar_position();
            (start.distance_squared(end) > 1.0e-18).then_some(DrillSegmentInstance {
                start: (start - scene_origin).as_vec3().to_array(),
                // A run between holes of unequal diameter takes their mean, so
                // it reads the same whichever end it was tied from.
                radius: ((from.render_radius() + to.render_radius()) * 0.5 * TIE_RADIUS_SCALE) as f32,
                end: (end - scene_origin).as_vec3().to_array(),
                pixel_diameter: MIN_RENDER_PIXEL_DIAMETER,
                color: if selection.contains_tie(tie.from, tie.to) {
                    let [red, green, blue, _] = crate::ui::SELECTION_COLOR_F32;
                    [red, green, blue]
                } else {
                    tie.color
                },
                _pad1: 0.0,
            })
        })
        .collect()
}

fn build_collar_instances(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selection: &HoleSelection) -> Vec<DrillCollarInstance> {
    if !dataset.state.loaded {
        return Vec::new();
    }
    let selected_colors = selection.any().then(|| {
        let [red, green, blue, _] = crate::ui::SELECTION_COLOR_F32;
        (COLLAR_MARKER_FILL_COLOR, [red, green, blue])
    });
    dataset
        .dataset
        .holes
        .iter()
        .enumerate()
        .map(|(index, hole)| {
            let (outline, fill) = match selected_colors {
                Some(colors) if selection.contains(index) => colors,
                _ => (COLLAR_MARKER_OUTLINE_COLOR, COLLAR_MARKER_FILL_COLOR),
            };
            let center = hole.collar_position();
            let hole_radius = hole.render_radius();
            DrillCollarInstance {
                center: (center - scene_origin).as_vec3().to_array(),
                marker_radius: (hole_radius * COLLAR_MARKER_RADIUS_SCALE) as f32,
                outline,
                pixel_diameter: COLLAR_MARKER_MIN_PIXEL_DIAMETER,
                fill,
                hole_radius: hole_radius as f32,
            }
        })
        .collect()
}

fn evaluate_color(kind: DrillFieldKind, value: &DrillValue, state: &DrillColorState) -> [f32; 3] {
    match (kind, value) {
        (DrillFieldKind::Numeric { min, max }, DrillValue::Numeric(value)) if value.is_finite() => {
            let t = if (max - min).abs() <= f64::EPSILON {
                0.5
            } else {
                ((*value - min) / (max - min)).clamp(0.0, 1.0) as f32
            };
            evaluate_stops(t, &state.stops, state.smooth)
        }
        (DrillFieldKind::Categorical { .. }, DrillValue::Category(value)) => state
            .categories
            .iter()
            .find(|category| category.value == *value)
            .map_or([1.0; 3], |category| category.color),
        _ => [1.0; 3],
    }
}

pub(crate) fn evaluate_stops(t: f32, stops: &[crate::model::drill_hole::DrillColorStop], smooth: bool) -> [f32; 3] {
    let Some(first) = stops.first() else {
        return [1.0; 3];
    };
    if !smooth {
        return stops.iter().rev().find(|stop| t >= stop.t).unwrap_or(first).color;
    }
    if t <= first.t {
        return first.color;
    }
    for pair in stops.windows(2) {
        if t <= pair[1].t {
            let span = (pair[1].t - pair[0].t).max(f32::EPSILON);
            let f = ((t - pair[0].t) / span).clamp(0.0, 1.0);
            return [
                pair[0].color[0] + (pair[1].color[0] - pair[0].color[0]) * f,
                pair[0].color[1] + (pair[1].color[1] - pair[0].color[1]) * f,
                pair[0].color[2] + (pair[1].color[2] - pair[0].color[2]) * f,
            ];
        }
    }
    stops.last().map_or([1.0; 3], |stop| stop.color)
}
