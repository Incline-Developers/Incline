use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use glam::DVec3;
use wgpu::util::DeviceExt;

use crate::model::drill_hole::{
    COLLAR_MARKER_FILL_COLOR, COLLAR_MARKER_OUTLINE_COLOR, COLLAR_MARKER_PIXEL_DIAMETER, COLLAR_MARKER_RADIUS_SCALE, DrillColorState, DrillFieldKind, DrillHoleId, DrillValue,
    MIN_RENDER_PIXEL_DIAMETER, OpenDrillHoleDataset,
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
    pub(crate) collar_buffer: Option<wgpu::Buffer>,
    pub(crate) collar_count: u32,
    key: u64,
}

#[derive(Default)]
pub(crate) struct DrillHoleGpuCache {
    entries: HashMap<DrillHoleId, CachedDrillHoles>,
}

impl DrillHoleGpuCache {
    pub(crate) fn sync(&mut self, device: &wgpu::Device, scene_origin: DVec3, datasets: &[OpenDrillHoleDataset], editor: &crate::ui::state::EditorState) {
        self.entries.retain(|id, _| datasets.iter().any(|dataset| dataset.id == *id && dataset.state.loaded));
        for dataset in datasets {
            if !dataset.state.loaded {
                continue;
            }
            let selected = editor.selected_handles.contains(&dataset.entity_id());
            let key = dataset_key(dataset, scene_origin, selected);
            if self.entries.get(&dataset.id).is_some_and(|cached| cached.key == key) {
                continue;
            }
            let instances = build_instances(dataset, scene_origin, selected);
            let buffer = (!instances.is_empty()).then(|| {
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("Drillhole Segment Instances"),
                    contents: bytemuck::cast_slice(&instances),
                    usage: wgpu::BufferUsages::VERTEX,
                })
            });
            let collars = build_collar_instances(dataset, scene_origin, selected);
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
                    collar_buffer,
                    collar_count: collars.len().min(u32::MAX as usize) as u32,
                    key,
                },
            );
        }
    }

    pub(crate) fn get(&self, id: DrillHoleId) -> Option<&CachedDrillHoles> {
        self.entries.get(&id)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.values().all(|entry| entry.count == 0 && entry.collar_count == 0)
    }
}

fn dataset_key(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selected: bool) -> u64 {
    let mut hash = DefaultHasher::new();
    dataset.id.hash(&mut hash);
    dataset.visible.hash(&mut hash);
    selected.hash(&mut hash);
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

fn build_instances(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selected: bool) -> Vec<DrillSegmentInstance> {
    if !dataset.state.loaded || !dataset.visible {
        return Vec::new();
    }
    let mut instances = Vec::new();
    let field = dataset.color.active_field.as_deref().and_then(|key| dataset.dataset.field(key));
    for hole in &dataset.dataset.holes {
        if hole.trace.len() < 2 {
            continue;
        }
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
            let radius = hole.diameter.map_or(0.0, |diameter| (diameter * 0.5) as f32);
            instances.push(DrillSegmentInstance {
                start: (start - scene_origin).as_vec3().to_array(),
                radius,
                end: (end - scene_origin).as_vec3().to_array(),
                pixel_diameter: MIN_RENDER_PIXEL_DIAMETER,
                color,
                _pad1: 0.0,
            });
        }
    }
    instances
}

fn build_collar_instances(dataset: &OpenDrillHoleDataset, scene_origin: DVec3, selected: bool) -> Vec<DrillCollarInstance> {
    if !dataset.state.loaded || !dataset.visible {
        return Vec::new();
    }
    let (outline, fill) = if selected {
        let [red, green, blue, _] = crate::ui::SELECTION_COLOR_F32;
        (COLLAR_MARKER_FILL_COLOR, [red, green, blue])
    } else {
        (COLLAR_MARKER_OUTLINE_COLOR, COLLAR_MARKER_FILL_COLOR)
    };
    dataset
        .dataset
        .holes
        .iter()
        .map(|hole| {
            // The trace's first station is the collar; `hole.collar` only
            // stands in for a dataset that arrived without one.
            let center = hole.trace.first().map_or(hole.collar, |station| station.position);
            let hole_radius = hole.diameter.map_or(0.0, |diameter| diameter * 0.5);
            DrillCollarInstance {
                center: (center - scene_origin).as_vec3().to_array(),
                marker_radius: (hole_radius * COLLAR_MARKER_RADIUS_SCALE) as f32,
                outline,
                pixel_diameter: COLLAR_MARKER_PIXEL_DIAMETER,
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
