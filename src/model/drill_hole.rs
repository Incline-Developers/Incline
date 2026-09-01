use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::model::{formats::csv_drill_hole::CsvDrillFileMapping, project::ProjectItemState};

pub(crate) const MIN_RENDER_PIXEL_DIAMETER: f32 = 2.0;
/// Screen diameter of the marker drawn at every collar. It is the floor, not
/// the size: a hole wide enough on screen grows its marker past it.
pub(crate) const COLLAR_MARKER_PIXEL_DIAMETER: f32 = 11.0;
/// How much wider than the hole itself the collar marker is drawn.
pub(crate) const COLLAR_MARKER_RADIUS_SCALE: f64 = 2.5;
pub(crate) const COLLAR_MARKER_OUTLINE_COLOR: [f32; 3] = [0.086, 0.376, 0.851];
pub(crate) const COLLAR_MARKER_FILL_COLOR: [f32; 3] = [1.0, 1.0, 1.0];
pub(crate) const MAX_DRILL_COLOR_STOPS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DrillHoleId(pub(crate) u64);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum DrillHoleSource {
    /// Deserialization-only compatibility for sessions created before DHD
    /// support was removed. These sources are never restored or opened.
    #[serde(rename = "Dhd")]
    LegacyDhd { path: PathBuf },
    Csv {
        name: String,
        files: Vec<CsvDrillFileMapping>,
        /// Filename-only browser identity. Native imports use their first
        /// mapped file path while the project retains the decoded dataset.
        #[serde(default)]
        browser_path: Option<PathBuf>,
    },
    /// A dataset decoded from an Open Mining Format container. It stays
    /// in-memory until exported or explicitly saved in another format.
    Omf { name: String, path: PathBuf },
}

impl DrillHoleSource {
    pub(crate) fn display_name(&self) -> String {
        match self {
            Self::LegacyDhd { path } => path.file_name().and_then(|name| name.to_str()).unwrap_or("unsupported drillhole source").to_owned(),
            Self::Csv { name, .. } => name.clone(),
            Self::Omf { name, .. } => name.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceStation {
    pub(crate) depth: f64,
    pub(crate) position: DVec3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum DrillValue {
    Numeric(f64),
    Category(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillInterval {
    pub(crate) from: f64,
    pub(crate) to: f64,
    pub(crate) values: BTreeMap<String, DrillValue>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillHole {
    pub(crate) dhid: String,
    pub(crate) collar: DVec3,
    /// Source diameter. `None` deliberately remains distinct and renders with
    /// [`MIN_RENDER_PIXEL_DIAMETER`] instead of a world-space fallback.
    pub(crate) diameter: Option<f64>,
    pub(crate) trace: Vec<TraceStation>,
    /// Explicit geometry coverage. Empty means the complete measured-depth
    /// trace is continuous.
    pub(crate) render_ranges: Vec<(f64, f64)>,
    pub(crate) intervals: Vec<DrillInterval>,
}

impl DrillHole {
    pub(crate) fn position_at_depth(&self, depth: f64) -> Option<DVec3> {
        let first = *self.trace.first()?;
        if depth <= first.depth {
            return Some(first.position);
        }
        for pair in self.trace.windows(2) {
            let [a, b] = [pair[0], pair[1]];
            if depth <= b.depth {
                let span = b.depth - a.depth;
                let t = if span > 0.0 { ((depth - a.depth) / span).clamp(0.0, 1.0) } else { 0.0 };
                return Some(a.position.lerp(b.position, t));
            }
        }
        self.trace.last().map(|station| station.position)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DrillFieldKind {
    Numeric { min: f64, max: f64 },
    Categorical { categories: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrillField {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) kind: DrillFieldKind,
}

#[derive(Clone, Debug)]
pub(crate) struct DrillHoleDataset {
    pub(crate) holes: Vec<DrillHole>,
    pub(crate) fields: Vec<DrillField>,
    pub(crate) bounds: Option<(DVec3, DVec3)>,
}

impl DrillHoleDataset {
    pub(crate) fn new(mut holes: Vec<DrillHole>) -> Self {
        holes.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.dhid, &b.dhid));
        let fields = collect_fields(&holes);
        let bounds = drill_bounds(&holes);
        Self { holes, fields, bounds }
    }

    pub(crate) fn field(&self, key: &str) -> Option<&DrillField> {
        self.fields.iter().find(|field| field.key == key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum DrillColorPreset {
    Rainbow,
    Grayscale,
    Heat,
    GreenYellowRed,
}

impl DrillColorPreset {
    pub(crate) const ALL: [Self; 4] = [Self::Rainbow, Self::Grayscale, Self::Heat, Self::GreenYellowRed];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Rainbow => "Rainbow",
            Self::Grayscale => "Grayscale",
            Self::Heat => "Heat",
            Self::GreenYellowRed => "Green–Yellow–Red",
        }
    }

    pub(crate) fn smooth(self) -> bool {
        matches!(self, Self::Rainbow | Self::Grayscale | Self::Heat)
    }

    pub(crate) fn stops(self) -> Vec<DrillColorStop> {
        let colors: &[[f32; 3]] = match self {
            Self::Rainbow => &[[0.05, 0.20, 0.95], [0.00, 0.85, 0.95], [0.05, 0.82, 0.20], [1.00, 0.85, 0.00], [0.92, 0.02, 0.02]],
            Self::Grayscale => &[[0.05, 0.05, 0.05], [0.95, 0.95, 0.95]],
            Self::Heat => &[[0.02, 0.02, 0.02], [0.85, 0.00, 0.00], [1.00, 0.78, 0.00], [1.00, 1.00, 0.92]],
            Self::GreenYellowRed => &[[0.00, 0.82, 0.20], [1.00, 0.85, 0.00], [0.92, 0.00, 0.00]],
        };
        let denom = if self.smooth() { colors.len().saturating_sub(1).max(1) } else { colors.len().max(1) } as f32;
        colors
            .iter()
            .enumerate()
            .map(|(index, color)| DrillColorStop {
                t: index as f32 / denom,
                color: *color,
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillColorStop {
    pub(crate) t: f32,
    pub(crate) color: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillCategoryColor {
    pub(crate) value: String,
    pub(crate) color: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DrillColorState {
    pub(crate) active_field: Option<String>,
    pub(crate) preset: DrillColorPreset,
    pub(crate) smooth: bool,
    pub(crate) stops: Vec<DrillColorStop>,
    pub(crate) categories: Vec<DrillCategoryColor>,
}

impl Default for DrillColorState {
    fn default() -> Self {
        let preset = DrillColorPreset::Rainbow;
        Self {
            active_field: None,
            preset,
            smooth: preset.smooth(),
            stops: preset.stops(),
            categories: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedDrillHoleDataset {
    pub(crate) name: String,
    pub(crate) source: DrillHoleSource,
    pub(crate) dataset: Arc<DrillHoleDataset>,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenDrillHoleDataset {
    pub(crate) id: DrillHoleId,
    pub(crate) state: ProjectItemState,
    pub(crate) name: String,
    pub(crate) dataset: Arc<DrillHoleDataset>,
    pub(crate) visible: bool,
    pub(crate) color: DrillColorState,
}

impl OpenDrillHoleDataset {
    pub(crate) fn entity_id(&self) -> crate::model::SceneEntityId {
        crate::model::SceneEntityId::DrillHole(self.id)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SurveyObservation {
    pub(crate) depth: f64,
    pub(crate) azimuth: Option<f64>,
    pub(crate) dip: Option<f64>,
    pub(crate) position: Option<DVec3>,
}

pub(crate) fn resolve_trace(collar: DVec3, observations: &mut [SurveyObservation], target_depth: f64) -> Vec<TraceStation> {
    observations.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    let mut trace = vec![TraceStation { depth: 0.0, position: collar }];
    let mut last_orientation = observations.iter().find_map(SurveyObservation::orientation);
    for observation in observations.iter().copied() {
        if !observation.depth.is_finite() || observation.depth < 0.0 {
            continue;
        }
        let previous = *trace.last().expect("collar station exists");
        if observation.depth + 1.0e-9 < previous.depth {
            continue;
        }
        let stored_position = observation.position.filter(|position| position.is_finite());
        let position = stored_position.unwrap_or_else(|| {
            let (azimuth, dip) = last_orientation.unwrap_or((0.0, -90.0));
            project_tangent(previous.position, observation.depth - previous.depth, azimuth, dip)
        });
        if observation.depth > previous.depth + 1.0e-9 {
            trace.push(TraceStation {
                depth: observation.depth,
                position,
            });
        } else if let Some(first) = trace.first_mut() {
            first.position = collar;
        }
        last_orientation = observation
            .orientation()
            .or_else(|| stored_position.and_then(|_| orientation_from_delta(position - previous.position)))
            .or(last_orientation);
    }
    let previous = *trace.last().expect("collar station exists");
    if target_depth.is_finite() && target_depth > previous.depth + 1.0e-9 {
        let (azimuth, dip) = last_orientation.unwrap_or((0.0, -90.0));
        trace.push(TraceStation {
            depth: target_depth,
            position: project_tangent(previous.position, target_depth - previous.depth, azimuth, dip),
        });
    }
    trace
}

impl SurveyObservation {
    fn orientation(&self) -> Option<(f64, f64)> {
        match (self.azimuth, self.dip) {
            (Some(azimuth), Some(dip)) if azimuth.is_finite() && dip.is_finite() => Some((azimuth, dip)),
            _ => None,
        }
    }
}

fn orientation_from_delta(delta: DVec3) -> Option<(f64, f64)> {
    let horizontal = delta.x.hypot(delta.y);
    let length = horizontal.hypot(delta.z);
    (length > 1.0e-12).then(|| (delta.x.atan2(delta.y).to_degrees().rem_euclid(360.0), delta.z.atan2(horizontal).to_degrees()))
}

fn project_tangent(origin: DVec3, distance: f64, azimuth_degrees: f64, dip_degrees: f64) -> DVec3 {
    let azimuth = azimuth_degrees.to_radians();
    let dip = dip_degrees.to_radians();
    let horizontal = distance * dip.cos();
    origin + DVec3::new(horizontal * azimuth.sin(), horizontal * azimuth.cos(), distance * dip.sin())
}

fn collect_fields(holes: &[DrillHole]) -> Vec<DrillField> {
    let mut numeric: BTreeMap<String, (String, f64, f64)> = BTreeMap::new();
    let mut categorical: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for hole in holes {
        for interval in &hole.intervals {
            for (key, value) in &interval.values {
                let label = key.clone();
                match value {
                    DrillValue::Numeric(value) if value.is_finite() => {
                        numeric
                            .entry(key.clone())
                            .and_modify(|(_, min, max)| {
                                *min = min.min(*value);
                                *max = max.max(*value);
                            })
                            .or_insert((label, *value, *value));
                    }
                    DrillValue::Category(value) if !value.trim().is_empty() => {
                        let values = &mut categorical.entry(key.clone()).or_insert_with(|| (label, Vec::new())).1;
                        if !values.contains(value) {
                            values.push(value.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut fields = Vec::new();
    fields.extend(numeric.into_iter().map(|(key, (label, min, max))| DrillField {
        key,
        label,
        kind: DrillFieldKind::Numeric { min, max },
    }));
    fields.extend(categorical.into_iter().map(|(key, (label, mut categories))| {
        categories.sort_by(|a, b| crate::natural_sort::natural_cmp(a, b));
        categories.truncate(MAX_DRILL_COLOR_STOPS);
        DrillField {
            key,
            label,
            kind: DrillFieldKind::Categorical { categories },
        }
    }));
    fields.sort_by(|a, b| crate::natural_sort::natural_cmp(&a.label, &b.label));
    fields
}

fn drill_bounds(holes: &[DrillHole]) -> Option<(DVec3, DVec3)> {
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    let mut any = false;
    for hole in holes {
        // A collar without a resolvable measured-depth segment remains part
        // of the dataset/explorer counts, but it is not rendered and must not
        // pull camera fitting toward an otherwise empty coordinate.
        if hole.trace.len() < 2 {
            continue;
        }
        // Pixel-sized fallback holes have no stable world radius to add here;
        // their projected width is applied by the shader. Explicit diameters
        // still expand fitting/slice bounds in dataset world units.
        let radius = hole.diameter.unwrap_or(0.0) * 0.5;
        for station in &hole.trace {
            min = min.min(station.position - DVec3::splat(radius));
            max = max.max(station.position + DVec3::splat(radius));
            any = true;
        }
    }
    any.then_some((min, max))
}

pub(crate) fn default_category_colors(categories: &[String]) -> Vec<DrillCategoryColor> {
    const COLORS: [[f32; 3]; 12] = [
        [0.12, 0.47, 0.71],
        [1.00, 0.50, 0.05],
        [0.17, 0.63, 0.17],
        [0.84, 0.15, 0.16],
        [0.58, 0.40, 0.74],
        [0.55, 0.34, 0.29],
        [0.89, 0.47, 0.76],
        [0.50, 0.50, 0.50],
        [0.74, 0.74, 0.13],
        [0.09, 0.75, 0.81],
        [0.30, 0.60, 0.90],
        [0.90, 0.60, 0.20],
    ];
    categories
        .iter()
        .take(MAX_DRILL_COLOR_STOPS)
        .enumerate()
        .map(|(index, value)| DrillCategoryColor {
            value: value.clone(),
            color: COLORS[index],
        })
        .collect()
}
