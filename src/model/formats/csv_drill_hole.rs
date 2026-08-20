//! Mapped CSV drillhole import. A bundle may describe relational collar,
//! survey and interval tables, or explicit measured-depth line segments.

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::PathBuf,
};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::model::drill_hole::{DrillHole, DrillHoleDataset, DrillInterval, DrillValue, SurveyObservation, TraceStation, resolve_trace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum CsvDrillFileRole {
    Unassigned,
    Collar,
    Survey,
    Interval,
    ExplicitSegments,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum CsvDrillColumnRole {
    Ignore,
    Dhid,
    East,
    North,
    Elevation,
    Depth,
    Azimuth,
    Dip,
    From,
    To,
    StartEast,
    StartNorth,
    StartElevation,
    EndEast,
    EndNorth,
    EndElevation,
    Diameter,
    Attribute(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct CsvDrillFileMapping {
    pub(crate) path: PathBuf,
    pub(crate) role: CsvDrillFileRole,
    pub(crate) columns: Vec<CsvDrillColumnRole>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CsvDrillPreview {
    pub(crate) headers: Vec<String>,
    pub(crate) rows: Vec<Vec<String>>,
}

#[derive(Debug)]
pub(crate) enum CsvDrillError {
    Io(std::io::Error),
    Invalid(String),
}

impl fmt::Display for CsvDrillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CsvDrillError {}
impl From<std::io::Error> for CsvDrillError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub(crate) fn preview(bytes: &[u8]) -> Result<CsvDrillPreview, CsvDrillError> {
    let rows = parse_csv(bytes)?;
    let headers = rows.first().cloned().ok_or_else(|| CsvDrillError::Invalid("CSV file is empty".into()))?;
    if headers.is_empty() || headers.iter().all(|header| header.trim().is_empty()) {
        return Err(CsvDrillError::Invalid("CSV header has no columns".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for header in &headers {
        let key = header.trim().to_ascii_lowercase();
        if key.is_empty() || !seen.insert(key) {
            return Err(CsvDrillError::Invalid("CSV headers must be nonblank and unique".into()));
        }
    }
    Ok(CsvDrillPreview {
        headers,
        rows: rows.into_iter().skip(1).take(8).collect(),
    })
}

pub(crate) fn unassigned_mapping(path: PathBuf, preview: &CsvDrillPreview) -> CsvDrillFileMapping {
    CsvDrillFileMapping {
        path,
        columns: vec![CsvDrillColumnRole::Ignore; preview.headers.len()],
        role: CsvDrillFileRole::Unassigned,
    }
}

pub(crate) fn default_columns(role: CsvDrillFileRole, headers: &[String]) -> Vec<CsvDrillColumnRole> {
    headers
        .iter()
        .map(|header| {
            if role == CsvDrillFileRole::Unassigned {
                return CsvDrillColumnRole::Ignore;
            }
            let h = normalize_header(header);
            if ["dhid", "holeid", "hole", "id"].contains(&h.as_str()) {
                return CsvDrillColumnRole::Dhid;
            }
            let structural = match role {
                CsvDrillFileRole::Unassigned => None,
                CsvDrillFileRole::Collar => match h.as_str() {
                    "east" | "easting" | "x" => Some(CsvDrillColumnRole::East),
                    "north" | "northing" | "y" => Some(CsvDrillColumnRole::North),
                    "elevation" | "elev" | "rl" | "z" => Some(CsvDrillColumnRole::Elevation),
                    "diameter" | "diam" => Some(CsvDrillColumnRole::Diameter),
                    _ => None,
                },
                CsvDrillFileRole::Survey => match h.as_str() {
                    "depth" | "at" | "md" => Some(CsvDrillColumnRole::Depth),
                    "east" | "easting" | "x" => Some(CsvDrillColumnRole::East),
                    "north" | "northing" | "y" => Some(CsvDrillColumnRole::North),
                    "elevation" | "elev" | "rl" | "z" => Some(CsvDrillColumnRole::Elevation),
                    "azimuth" | "azi" | "bearing" => Some(CsvDrillColumnRole::Azimuth),
                    "dip" | "inclination" => Some(CsvDrillColumnRole::Dip),
                    _ => None,
                },
                CsvDrillFileRole::Interval => match h.as_str() {
                    "from" => Some(CsvDrillColumnRole::From),
                    "to" => Some(CsvDrillColumnRole::To),
                    _ => None,
                },
                CsvDrillFileRole::ExplicitSegments => match h.as_str() {
                    "from" => Some(CsvDrillColumnRole::From),
                    "to" => Some(CsvDrillColumnRole::To),
                    "starteast" | "startx" | "x1" => Some(CsvDrillColumnRole::StartEast),
                    "startnorth" | "starty" | "y1" => Some(CsvDrillColumnRole::StartNorth),
                    "startelevation" | "startz" | "z1" => Some(CsvDrillColumnRole::StartElevation),
                    "endeast" | "endx" | "x2" => Some(CsvDrillColumnRole::EndEast),
                    "endnorth" | "endy" | "y2" => Some(CsvDrillColumnRole::EndNorth),
                    "endelevation" | "endz" | "z2" => Some(CsvDrillColumnRole::EndElevation),
                    "diameter" | "diam" => Some(CsvDrillColumnRole::Diameter),
                    _ => None,
                },
            };
            structural.unwrap_or_else(|| {
                if matches!(role, CsvDrillFileRole::Interval | CsvDrillFileRole::ExplicitSegments) {
                    CsvDrillColumnRole::Attribute(header.trim().to_owned())
                } else {
                    CsvDrillColumnRole::Ignore
                }
            })
        })
        .collect()
}

fn normalize_header(header: &str) -> String {
    header.chars().filter(|character| character.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn preview_path(path: &Path) -> Result<CsvDrillPreview, CsvDrillError> {
    preview(&std::fs::read(path)?)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn parse_paths(files: &[CsvDrillFileMapping]) -> Result<DrillHoleDataset, CsvDrillError> {
    let buffers = files
        .iter()
        .map(|mapping| std::fs::read(&mapping.path).map_err(CsvDrillError::Io))
        .collect::<Result<Vec<_>, _>>()?;
    parse_bundle(files.iter().zip(buffers.iter()).map(|(mapping, bytes)| (mapping, bytes.as_slice())))
}

pub(crate) fn parse_bundle<'a>(inputs: impl IntoIterator<Item = (&'a CsvDrillFileMapping, &'a [u8])>) -> Result<DrillHoleDataset, CsvDrillError> {
    let inputs = inputs.into_iter().collect::<Vec<_>>();
    if inputs.is_empty() {
        return Err(CsvDrillError::Invalid("Select at least one CSV file".into()));
    }
    if let Some((mapping, _)) = inputs.iter().find(|(mapping, _)| mapping.role == CsvDrillFileRole::Unassigned) {
        return Err(CsvDrillError::Invalid(format!("Choose a file purpose for {}", mapping.path.display())));
    }
    let collar_files = inputs.iter().filter(|(mapping, _)| mapping.role == CsvDrillFileRole::Collar).count();
    let survey_files = inputs.iter().filter(|(mapping, _)| mapping.role == CsvDrillFileRole::Survey).count();
    let segment_files = inputs.iter().filter(|(mapping, _)| mapping.role == CsvDrillFileRole::ExplicitSegments).count();
    if collar_files > 1 || survey_files > 1 || segment_files > 1 {
        return Err(CsvDrillError::Invalid("Use at most one collar, survey, and explicit-segment CSV".into()));
    }
    if segment_files == 0 && collar_files != 1 {
        return Err(CsvDrillError::Invalid("A relational CSV bundle requires one collar file".into()));
    }
    if segment_files == 1 && collar_files + survey_files != 0 {
        return Err(CsvDrillError::Invalid("Explicit-segment geometry cannot be combined with collar or survey geometry".into()));
    }

    let mut collars: HashMap<String, Collar> = HashMap::new();
    let mut surveys: HashMap<String, Vec<SurveyObservation>> = HashMap::new();
    let mut intervals: HashMap<String, Vec<DrillInterval>> = HashMap::new();
    let mut segment_stations: HashMap<String, Vec<TraceStation>> = HashMap::new();
    let mut segment_ranges: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    let mut segment_diameters: HashMap<String, Option<f64>> = HashMap::new();
    let mut attribute_counts: HashMap<String, usize> = HashMap::new();
    for (mapping, bytes) in &inputs {
        let rows = parse_csv(bytes)?;
        let headers = rows.first().ok_or_else(|| CsvDrillError::Invalid(format!("{} is empty", mapping.path.display())))?;
        for (index, role) in mapping.columns.iter().enumerate() {
            if let CsvDrillColumnRole::Attribute(mapped) = role {
                let label = if mapped.trim().is_empty() { headers[index].trim() } else { mapped.trim() };
                *attribute_counts.entry(label.to_owned()).or_default() += 1;
            }
        }
    }

    for (mapping, bytes) in &inputs {
        let rows = parse_csv(bytes)?;
        let headers = rows.first().ok_or_else(|| CsvDrillError::Invalid(format!("{} is empty", mapping.path.display())))?;
        if mapping.columns.len() != headers.len() {
            return Err(CsvDrillError::Invalid(format!(
                "{} mapping has {} columns, CSV has {}",
                mapping.path.display(),
                mapping.columns.len(),
                headers.len()
            )));
        }
        validate_roles(mapping)?;
        let stem = mapping.path.file_stem().and_then(|value| value.to_str()).unwrap_or("interval");
        let numeric_attributes = mapping
            .columns
            .iter()
            .enumerate()
            .filter_map(|(index, role)| matches!(role, CsvDrillColumnRole::Attribute(_)).then_some(index))
            .filter(|&index| {
                rows.iter()
                    .skip(1)
                    .filter_map(|row| row.get(index).map(|value| value.trim()))
                    .filter(|value| !value.is_empty())
                    .all(|value| value.parse::<f64>().is_ok_and(f64::is_finite))
            })
            .collect::<std::collections::HashSet<_>>();
        for (row_index, row) in rows.iter().enumerate().skip(1) {
            if row.iter().all(|value| value.trim().is_empty()) {
                continue;
            }
            if row.len() != headers.len() {
                return Err(CsvDrillError::Invalid(format!(
                    "{} row {} has {} columns; expected {}",
                    mapping.path.display(),
                    row_index + 1,
                    row.len(),
                    headers.len()
                )));
            }
            let dhid = required_text(mapping, row, &CsvDrillColumnRole::Dhid, row_index)?;
            match mapping.role {
                CsvDrillFileRole::Unassigned => unreachable!("unassigned mappings are rejected before rows are parsed"),
                CsvDrillFileRole::Collar => {
                    let collar = Collar {
                        position: DVec3::new(
                            required_number(mapping, row, &CsvDrillColumnRole::East, row_index)?,
                            required_number(mapping, row, &CsvDrillColumnRole::North, row_index)?,
                            required_number(mapping, row, &CsvDrillColumnRole::Elevation, row_index)?,
                        ),
                        diameter: optional_number(mapping, row, &CsvDrillColumnRole::Diameter, row_index)?,
                    };
                    validate_diameter(collar.diameter, &dhid)?;
                    if collars.insert(dhid.clone(), collar).is_some() {
                        return Err(CsvDrillError::Invalid(format!("duplicate collar DHID '{dhid}'")));
                    }
                }
                CsvDrillFileRole::Survey => {
                    let position = optional_xyz(mapping, row, row_index, CsvDrillColumnRole::East, CsvDrillColumnRole::North, CsvDrillColumnRole::Elevation)?;
                    let azimuth = optional_number(mapping, row, &CsvDrillColumnRole::Azimuth, row_index)?;
                    let dip = optional_number(mapping, row, &CsvDrillColumnRole::Dip, row_index)?;
                    if azimuth.is_some() != dip.is_some() {
                        return Err(CsvDrillError::Invalid(format!(
                            "{} row {} has a partial azimuth/dip orientation",
                            mapping.path.display(),
                            row_index + 1
                        )));
                    }
                    if position.is_none() && azimuth.is_none() {
                        return Err(CsvDrillError::Invalid(format!(
                            "{} row {} has no XYZ or azimuth/dip geometry",
                            mapping.path.display(),
                            row_index + 1
                        )));
                    }
                    surveys.entry(dhid).or_default().push(SurveyObservation {
                        depth: required_number(mapping, row, &CsvDrillColumnRole::Depth, row_index)?,
                        azimuth,
                        dip,
                        position,
                    });
                }
                CsvDrillFileRole::Interval => {
                    intervals
                        .entry(dhid)
                        .or_default()
                        .push(interval_row(mapping, headers, row, row_index, stem, &numeric_attributes, &attribute_counts)?);
                }
                CsvDrillFileRole::ExplicitSegments => {
                    let from = required_number(mapping, row, &CsvDrillColumnRole::From, row_index)?;
                    let to = required_number(mapping, row, &CsvDrillColumnRole::To, row_index)?;
                    validate_interval(from, to, &dhid, row_index)?;
                    let start = DVec3::new(
                        required_number(mapping, row, &CsvDrillColumnRole::StartEast, row_index)?,
                        required_number(mapping, row, &CsvDrillColumnRole::StartNorth, row_index)?,
                        required_number(mapping, row, &CsvDrillColumnRole::StartElevation, row_index)?,
                    );
                    let end = DVec3::new(
                        required_number(mapping, row, &CsvDrillColumnRole::EndEast, row_index)?,
                        required_number(mapping, row, &CsvDrillColumnRole::EndNorth, row_index)?,
                        required_number(mapping, row, &CsvDrillColumnRole::EndElevation, row_index)?,
                    );
                    let diameter = optional_number(mapping, row, &CsvDrillColumnRole::Diameter, row_index)?;
                    validate_diameter(diameter, &dhid)?;
                    if let Some(existing) = segment_diameters.get(&dhid).copied() {
                        let consistent = match (existing, diameter) {
                            (None, None) => true,
                            (Some(a), Some(b)) => (a - b).abs() <= 1.0e-9,
                            _ => false,
                        };
                        if !consistent {
                            return Err(CsvDrillError::Invalid(format!("inconsistent diameter for DHID '{dhid}'")));
                        }
                    } else {
                        segment_diameters.insert(dhid.clone(), diameter);
                    }
                    segment_stations
                        .entry(dhid.clone())
                        .or_default()
                        .extend([TraceStation { depth: from, position: start }, TraceStation { depth: to, position: end }]);
                    segment_ranges.entry(dhid.clone()).or_default().push((from, to));
                    intervals
                        .entry(dhid)
                        .or_default()
                        .push(interval_row(mapping, headers, row, row_index, stem, &numeric_attributes, &attribute_counts)?);
                }
            }
        }
    }

    let known = if segment_files == 1 {
        segment_stations.keys().collect::<std::collections::HashSet<_>>()
    } else {
        collars.keys().collect()
    };
    for dhid in surveys.keys().chain(intervals.keys()) {
        if !known.contains(dhid) {
            return Err(CsvDrillError::Invalid(format!("rows reference unknown DHID '{dhid}'")));
        }
    }
    validate_overlaps(&intervals)?;

    let mut holes = Vec::new();
    if segment_files == 1 {
        for (dhid, mut trace) in segment_stations {
            trace.sort_by(|a, b| a.depth.total_cmp(&b.depth));
            trace.dedup_by(|a, b| (a.depth - b.depth).abs() <= 1.0e-9 && (a.position - b.position).length() <= 1.0e-7);
            if trace.windows(2).any(|pair| (pair[0].depth - pair[1].depth).abs() <= 1.0e-9) {
                return Err(CsvDrillError::Invalid(format!("DHID '{dhid}' has inconsistent coordinates at the same measured depth")));
            }
            let collar = trace
                .first()
                .map(|station| station.position)
                .ok_or_else(|| CsvDrillError::Invalid(format!("DHID '{dhid}' has no segments")))?;
            let hole_intervals = intervals.remove(&dhid).unwrap_or_default();
            let render_ranges = segment_ranges.remove(&dhid).unwrap_or_default();
            holes.push(DrillHole {
                dhid: dhid.clone(),
                collar,
                diameter: segment_diameters.remove(&dhid).flatten(),
                trace,
                render_ranges,
                intervals: hole_intervals,
            });
        }
    } else {
        for (dhid, collar) in collars {
            let hole_intervals = intervals.remove(&dhid).unwrap_or_default();
            let target_depth = hole_intervals.iter().map(|interval| interval.to).fold(0.0, f64::max);
            let trace = resolve_trace(collar.position, surveys.entry(dhid.clone()).or_default(), target_depth);
            holes.push(DrillHole {
                dhid,
                collar: collar.position,
                diameter: collar.diameter,
                trace,
                render_ranges: Vec::new(),
                intervals: hole_intervals,
            });
        }
    }
    Ok(DrillHoleDataset::new(holes))
}

#[derive(Clone, Copy)]
struct Collar {
    position: DVec3,
    diameter: Option<f64>,
}

fn validate_roles(mapping: &CsvDrillFileMapping) -> Result<(), CsvDrillError> {
    let count = |role: &CsvDrillColumnRole| mapping.columns.iter().filter(|value| *value == role).count();
    let require = |role: CsvDrillColumnRole, label: &str| -> Result<(), CsvDrillError> {
        if count(&role) == 1 {
            Ok(())
        } else {
            Err(CsvDrillError::Invalid(format!("{} requires exactly one {label} column", mapping.path.display())))
        }
    };
    match mapping.role {
        CsvDrillFileRole::Unassigned => {
            return Err(CsvDrillError::Invalid(format!("Choose a file purpose for {}", mapping.path.display())));
        }
        CsvDrillFileRole::Collar => {
            require(CsvDrillColumnRole::Dhid, "DHID")?;
            require(CsvDrillColumnRole::East, "east/X")?;
            require(CsvDrillColumnRole::North, "north/Y")?;
            require(CsvDrillColumnRole::Elevation, "elevation/Z")?;
        }
        CsvDrillFileRole::Survey => {
            require(CsvDrillColumnRole::Dhid, "DHID")?;
            require(CsvDrillColumnRole::Depth, "depth")?;
            let xyz = count(&CsvDrillColumnRole::East) == 1 && count(&CsvDrillColumnRole::North) == 1 && count(&CsvDrillColumnRole::Elevation) == 1;
            let angles = count(&CsvDrillColumnRole::Azimuth) == 1 && count(&CsvDrillColumnRole::Dip) == 1;
            if !xyz && !angles {
                return Err(CsvDrillError::Invalid(format!("{} survey requires XYZ or azimuth and dip", mapping.path.display())));
            }
        }
        CsvDrillFileRole::Interval => {
            require(CsvDrillColumnRole::Dhid, "DHID")?;
            require(CsvDrillColumnRole::From, "FROM")?;
            require(CsvDrillColumnRole::To, "TO")?;
        }
        CsvDrillFileRole::ExplicitSegments => {
            require(CsvDrillColumnRole::Dhid, "DHID")?;
            for (role, label) in [
                (CsvDrillColumnRole::From, "FROM"),
                (CsvDrillColumnRole::To, "TO"),
                (CsvDrillColumnRole::StartEast, "start east/X"),
                (CsvDrillColumnRole::StartNorth, "start north/Y"),
                (CsvDrillColumnRole::StartElevation, "start elevation/Z"),
                (CsvDrillColumnRole::EndEast, "end east/X"),
                (CsvDrillColumnRole::EndNorth, "end north/Y"),
                (CsvDrillColumnRole::EndElevation, "end elevation/Z"),
            ] {
                require(role, label)?;
            }
        }
    }
    if count(&CsvDrillColumnRole::Diameter) > 1 {
        return Err(CsvDrillError::Invalid(format!("{} has multiple diameter mappings", mapping.path.display())));
    }
    Ok(())
}

fn interval_row(
    mapping: &CsvDrillFileMapping,
    headers: &[String],
    row: &[String],
    row_index: usize,
    stem: &str,
    numeric_attributes: &std::collections::HashSet<usize>,
    attribute_counts: &HashMap<String, usize>,
) -> Result<DrillInterval, CsvDrillError> {
    let dhid = required_text(mapping, row, &CsvDrillColumnRole::Dhid, row_index)?;
    let from = required_number(mapping, row, &CsvDrillColumnRole::From, row_index)?;
    let to = required_number(mapping, row, &CsvDrillColumnRole::To, row_index)?;
    validate_interval(from, to, &dhid, row_index)?;
    let mut values = BTreeMap::new();
    for (index, role) in mapping.columns.iter().enumerate() {
        let CsvDrillColumnRole::Attribute(mapped_name) = role else {
            continue;
        };
        let raw = row[index].trim();
        if raw.is_empty() {
            continue;
        }
        let label = if mapped_name.trim().is_empty() { headers[index].trim() } else { mapped_name.trim() };
        let key = if attribute_counts.get(label).copied().unwrap_or(0) > 1 {
            format!("{stem}.{label}")
        } else {
            label.to_owned()
        };
        let value = if numeric_attributes.contains(&index) {
            DrillValue::Numeric(raw.parse::<f64>().expect("column-wide numeric inference validated this value"))
        } else {
            DrillValue::Category(raw.to_owned())
        };
        values.insert(key, value);
    }
    Ok(DrillInterval { from, to, values })
}

fn validate_interval(from: f64, to: f64, dhid: &str, row_index: usize) -> Result<(), CsvDrillError> {
    if !from.is_finite() || !to.is_finite() || from < 0.0 || to <= from {
        return Err(CsvDrillError::Invalid(format!("row {} has invalid interval {from}..{to} for DHID '{dhid}'", row_index + 1)));
    }
    Ok(())
}

fn validate_diameter(diameter: Option<f64>, dhid: &str) -> Result<(), CsvDrillError> {
    if diameter.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err(CsvDrillError::Invalid(format!("DHID '{dhid}' has an invalid diameter")));
    }
    Ok(())
}

fn validate_overlaps(intervals: &HashMap<String, Vec<DrillInterval>>) -> Result<(), CsvDrillError> {
    for (dhid, rows) in intervals {
        let mut by_field: HashMap<&str, Vec<(f64, f64)>> = HashMap::new();
        for row in rows {
            for key in row.values.keys() {
                by_field.entry(key).or_default().push((row.from, row.to));
            }
        }
        for (field, ranges) in &mut by_field {
            ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
            if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1 - 1.0e-9) {
                return Err(CsvDrillError::Invalid(format!("overlapping intervals for DHID '{dhid}', field '{field}'")));
            }
        }
    }
    Ok(())
}

fn role_index(mapping: &CsvDrillFileMapping, role: &CsvDrillColumnRole) -> Option<usize> {
    mapping.columns.iter().position(|value| value == role)
}

fn required_text(mapping: &CsvDrillFileMapping, row: &[String], role: &CsvDrillColumnRole, row_index: usize) -> Result<String, CsvDrillError> {
    let value = role_index(mapping, role).and_then(|index| row.get(index)).map(|value| value.trim()).unwrap_or("");
    if value.is_empty() {
        Err(CsvDrillError::Invalid(format!(
            "{} row {} has a blank required value",
            mapping.path.display(),
            row_index + 1
        )))
    } else {
        Ok(value.to_owned())
    }
}

fn required_number(mapping: &CsvDrillFileMapping, row: &[String], role: &CsvDrillColumnRole, row_index: usize) -> Result<f64, CsvDrillError> {
    let value = required_text(mapping, row, role, row_index)?;
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| CsvDrillError::Invalid(format!("{} row {} has invalid number '{value}'", mapping.path.display(), row_index + 1)))
}

fn optional_number(mapping: &CsvDrillFileMapping, row: &[String], role: &CsvDrillColumnRole, row_index: usize) -> Result<Option<f64>, CsvDrillError> {
    let Some(index) = role_index(mapping, role) else {
        return Ok(None);
    };
    let value = row[index].trim();
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(Some)
        .ok_or_else(|| CsvDrillError::Invalid(format!("{} row {} has invalid number '{value}'", mapping.path.display(), row_index + 1)))
}

fn optional_xyz(
    mapping: &CsvDrillFileMapping,
    row: &[String],
    row_index: usize,
    x_role: CsvDrillColumnRole,
    y_role: CsvDrillColumnRole,
    z_role: CsvDrillColumnRole,
) -> Result<Option<DVec3>, CsvDrillError> {
    if role_index(mapping, &x_role).is_none() || role_index(mapping, &y_role).is_none() || role_index(mapping, &z_role).is_none() {
        return Ok(None);
    }
    let values = [
        optional_number(mapping, row, &x_role, row_index)?,
        optional_number(mapping, row, &y_role, row_index)?,
        optional_number(mapping, row, &z_role, row_index)?,
    ];
    match values {
        [Some(x), Some(y), Some(z)] => Ok(Some(DVec3::new(x, y, z))),
        [None, None, None] => Ok(None),
        _ => Err(CsvDrillError::Invalid(format!(
            "{} row {} has a partial XYZ coordinate",
            mapping.path.display(),
            row_index + 1
        ))),
    }
}

fn parse_csv(bytes: &[u8]) -> Result<Vec<Vec<String>>, CsvDrillError> {
    std::str::from_utf8(bytes).map_err(|_| CsvDrillError::Invalid("CSV file must be UTF-8".into()))?;
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = Vec::new();
    let mut quoted = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    field.push(b'"');
                    index += 1;
                } else {
                    quoted = false;
                }
            } else {
                field.push(byte);
            }
        } else {
            match byte {
                b'"' if field.is_empty() => quoted = true,
                b',' => {
                    row.push(String::from_utf8(std::mem::take(&mut field)).expect("validated UTF-8"));
                }
                b'\n' => {
                    row.push(String::from_utf8(std::mem::take(&mut field)).expect("validated UTF-8"));
                    rows.push(std::mem::take(&mut row));
                }
                b'\r' => {}
                _ => field.push(byte),
            }
        }
        index += 1;
    }
    if quoted {
        return Err(CsvDrillError::Invalid("CSV has an unterminated quoted field".into()));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(String::from_utf8(field).expect("validated UTF-8"));
        rows.push(row);
    }
    Ok(rows)
}
