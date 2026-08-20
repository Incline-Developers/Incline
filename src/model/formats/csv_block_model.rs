//! CSV block-model import/export.
//!
//! Geometry columns describe block centroids (`x`, `y`, `z`) and full block
//! dimensions (`dx`, `dy`, `dz`). `Value` and optional `ID` columns become
//! numeric block-model variables, `Category` columns preserve text labels,
//! and `Ignore` columns are discarded.

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    io::{self, Write},
    mem::size_of,
    sync::Arc,
};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs::File, io::Read, path::Path};

use glam::DVec3;
use serde::{Deserialize, Serialize};

use crate::model::{
    block_model::{BlockBounds, BlockBoundsSource, RenderableBlockIndices, color_variable_default, is_no_data_sentinel},
    formats::block_model_data::{BlockModelColumn, BlockModelData},
};

pub(crate) const PREVIEW_ROW_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum CsvColumnRole {
    #[serde(alias = "Resource")]
    Value,
    Category,
    Ignore,
    Id,
    X,
    Y,
    Z,
    Dx,
    Dy,
    Dz,
}

impl CsvColumnRole {
    pub(crate) const ALL: [Self; 10] = [Self::Value, Self::Category, Self::Ignore, Self::Id, Self::X, Self::Y, Self::Z, Self::Dx, Self::Dy, Self::Dz];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Value => "Value",
            Self::Category => "Category",
            Self::Ignore => "Ignore",
            Self::Id => "ID",
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
            Self::Dx => "dx",
            Self::Dy => "dy",
            Self::Dz => "dz",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct CsvColumnMapping {
    pub(crate) roles: Vec<CsvColumnRole>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CsvPreview {
    pub(crate) headers: Vec<String>,
    pub(crate) rows: Vec<Vec<String>>,
    pub(crate) mapping: CsvColumnMapping,
}

#[derive(Debug)]
pub(crate) enum CsvBlockModelError {
    Invalid(String),
    Io(io::Error),
}

impl fmt::Display for CsvBlockModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CsvBlockModelError {}

impl From<io::Error> for CsvBlockModelError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug)]
pub(crate) struct ParsedCsvBlockModel {
    pub(crate) model: BlockModelData,
    pub(crate) blocks: BlockBoundsSource,
}

pub(crate) fn preview(bytes: &[u8]) -> Result<CsvPreview, CsvBlockModelError> {
    let mut records = Vec::new();
    for_each_record(bytes, Some(PREVIEW_ROW_COUNT + 1), |_, record| {
        records.push(record);
        Ok(())
    })?;
    let headers = records.first().cloned().ok_or_else(|| CsvBlockModelError::Invalid("CSV file is empty".to_owned()))?;
    if headers.is_empty() {
        return Err(CsvBlockModelError::Invalid("CSV header has no columns".to_owned()));
    }
    let rows = records.into_iter().skip(1).collect::<Vec<_>>();
    let mut mapping = auto_mapping(&headers);
    // Text columns are overwhelmingly likely to be categories. Infer those
    // from the preview while leaving numeric-looking category codes as Value;
    // users can explicitly select Category for that ambiguous case.
    for column in 0..headers.len() {
        if mapping.roles[column] == CsvColumnRole::Value
            && rows
                .iter()
                .filter_map(|row| row.get(column))
                .map(|field| field.trim())
                .any(|field| !field.is_empty() && field.parse::<f64>().is_err())
        {
            mapping.roles[column] = CsvColumnRole::Category;
        }
    }
    Ok(CsvPreview { headers, rows, mapping })
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn preview_path(path: &Path) -> Result<CsvPreview, CsvBlockModelError> {
    // A preview needs only the header and five records. Cap the initial read
    // so selecting a multi-gigabyte model never duplicates the whole file on
    // the UI thread; `preview` stops parsing as soon as those rows are found.
    const PREVIEW_READ_LIMIT: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    File::open(path)?.take(PREVIEW_READ_LIMIT).read_to_end(&mut bytes)?;
    preview(&bytes)
}

pub(crate) fn auto_mapping(headers: &[String]) -> CsvColumnMapping {
    let mut claimed = HashSet::new();
    let roles = headers
        .iter()
        .map(|header| {
            let role = match header.trim().to_ascii_lowercase().as_str() {
                "x" => CsvColumnRole::X,
                "y" => CsvColumnRole::Y,
                "z" => CsvColumnRole::Z,
                "dx" => CsvColumnRole::Dx,
                "dy" => CsvColumnRole::Dy,
                "dz" => CsvColumnRole::Dz,
                "id" => CsvColumnRole::Id,
                _ => CsvColumnRole::Value,
            };
            if matches!(role, CsvColumnRole::Value | CsvColumnRole::Category | CsvColumnRole::Ignore) || claimed.insert(role) {
                role
            } else {
                // A second identically named semantic column remains a value.
                // This preserves round-tripping when a value itself is named x.
                CsvColumnRole::Value
            }
        })
        .collect();
    CsvColumnMapping { roles }
}

pub(crate) fn validate_mapping(mapping: &CsvColumnMapping, column_count: usize) -> Result<(), CsvBlockModelError> {
    if mapping.roles.len() != column_count {
        return Err(CsvBlockModelError::Invalid(format!(
            "CSV mapping has {} columns but the file has {column_count}",
            mapping.roles.len()
        )));
    }
    for required in [
        CsvColumnRole::X,
        CsvColumnRole::Y,
        CsvColumnRole::Z,
        CsvColumnRole::Dx,
        CsvColumnRole::Dy,
        CsvColumnRole::Dz,
    ] {
        let count = mapping.roles.iter().filter(|role| **role == required).count();
        if count != 1 {
            return Err(CsvBlockModelError::Invalid(format!(
                "Select exactly one '{}' column (currently selected {count})",
                required.label()
            )));
        }
    }
    let id_count = mapping.roles.iter().filter(|role| **role == CsvColumnRole::Id).count();
    if id_count > 1 {
        return Err(CsvBlockModelError::Invalid(format!("Select at most one 'ID' column (currently selected {id_count})")));
    }
    Ok(())
}

pub(crate) fn parse(bytes: &[u8], mapping: &CsvColumnMapping) -> Result<ParsedCsvBlockModel, CsvBlockModelError> {
    let mut headers: Option<Vec<String>> = None;
    let mut role_indices = BTreeMap::new();
    let mut resource_indices = Vec::new();
    let mut resource_names = Vec::new();
    let mut resource_roles = Vec::new();
    let mut resource_values: Vec<Vec<f64>> = Vec::new();
    let mut resource_categories: Vec<BTreeMap<String, u32>> = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut blocks = Vec::new();
    let mut overall_lower = DVec3::splat(f64::INFINITY);
    let mut overall_upper = DVec3::splat(f64::NEG_INFINITY);

    for_each_record(bytes, None, |record_number, record| {
        if record_number == 1 {
            validate_mapping(mapping, record.len())?;
            let names = unique_resource_names(&record, mapping);
            for (index, role) in mapping.roles.iter().copied().enumerate() {
                if matches!(role, CsvColumnRole::Value | CsvColumnRole::Category | CsvColumnRole::Id) {
                    resource_indices.push(index);
                    resource_roles.push(role);
                } else if role != CsvColumnRole::Ignore {
                    role_indices.insert(role, index);
                }
            }
            resource_values = (0..resource_indices.len()).map(|_| Vec::new()).collect();
            resource_categories = (0..resource_indices.len()).map(|_| BTreeMap::new()).collect();
            resource_names = names;
            headers = Some(record);
            return Ok(());
        }

        let headers = headers.as_ref().expect("header is processed first");
        if record.iter().all(|field| field.trim().is_empty()) {
            return Ok(());
        }
        if record.len() > headers.len() {
            return Err(CsvBlockModelError::Invalid(format!(
                "CSV row {record_number} has {} fields; the header has {}",
                record.len(),
                headers.len()
            )));
        }
        let value = |role| {
            let index = role_indices[&role];
            parse_required_number(record.get(index).map(String::as_str).unwrap_or(""), record_number, &headers[index])
        };
        let center = DVec3::new(value(CsvColumnRole::X)?, value(CsvColumnRole::Y)?, value(CsvColumnRole::Z)?);
        let size = DVec3::new(value(CsvColumnRole::Dx)?, value(CsvColumnRole::Dy)?, value(CsvColumnRole::Dz)?);
        if !center.is_finite() {
            return Err(CsvBlockModelError::Invalid(format!("CSV row {record_number} contains a non-finite block position")));
        }
        if !size.is_finite() || size.min_element() <= 0.0 {
            return Err(CsvBlockModelError::Invalid(format!(
                "CSV row {record_number} block sizes must all be finite and greater than zero"
            )));
        }
        let half = size * 0.5;
        let block = BlockBounds {
            lower: center - half,
            upper: center + half,
        };
        overall_lower = overall_lower.min(block.lower);
        overall_upper = overall_upper.max(block.upper);
        blocks.push(block);

        for (resource, (values, &index)) in resource_values.iter_mut().zip(&resource_indices).enumerate() {
            let field = record.get(index).map(String::as_str).unwrap_or("").trim();
            let role = resource_roles[resource];
            if role == CsvColumnRole::Category {
                if field.is_empty() {
                    values.push(f64::NAN);
                } else if let Some(&code) = resource_categories[resource].get(field) {
                    values.push(code as f64);
                } else {
                    let code = u32::try_from(resource_categories[resource].len())
                        .map_err(|_| CsvBlockModelError::Invalid(format!("CSV column '{}' has more categories than Incline can represent", headers[index])))?;
                    resource_categories[resource].insert(field.to_owned(), code);
                    values.push(code as f64);
                }
                continue;
            }
            let parsed = if field.is_empty() && role == CsvColumnRole::Value {
                f64::NAN
            } else {
                field.parse::<f64>().map_err(|_| {
                    let expectation = if role == CsvColumnRole::Id { "a numeric ID" } else { "a number or blank" };
                    CsvBlockModelError::Invalid(format!("CSV row {record_number}, column '{}': expected {expectation}, found '{field}'", headers[index]))
                })?
            };
            if role == CsvColumnRole::Id {
                if !parsed.is_finite() {
                    return Err(CsvBlockModelError::Invalid(format!(
                        "CSV row {record_number}, column '{}': ID must be finite",
                        headers[index]
                    )));
                }
                let key = if parsed == 0.0 { 0 } else { parsed.to_bits() };
                if !seen_ids.insert(key) {
                    return Err(CsvBlockModelError::Invalid(format!(
                        "CSV row {record_number}, column '{}': duplicate ID '{field}'",
                        headers[index]
                    )));
                }
            }
            values.push(parsed);
        }
        Ok(())
    })?;

    let headers = headers.ok_or_else(|| CsvBlockModelError::Invalid("CSV file is empty".to_owned()))?;
    validate_mapping(mapping, headers.len())?;
    if blocks.is_empty() {
        return Err(CsvBlockModelError::Invalid("CSV file has no block rows".to_owned()));
    }
    let retained_geometry_bytes = blocks
        .len()
        .checked_mul(size_of::<BlockBounds>())
        .ok_or_else(|| CsvBlockModelError::Invalid("CSV block geometry size overflows".to_owned()))?;
    let columns = resource_names
        .into_iter()
        .zip(resource_values)
        .zip(resource_roles)
        .zip(resource_categories)
        .map(|(((name, values), role), categories)| BlockModelColumn {
            name,
            values: Arc::new(values),
            categories: (role == CsvColumnRole::Category).then(|| categories.into_iter().map(|(label, code)| (code, label)).collect()),
        })
        .collect();
    let model = BlockModelData::from_columns(blocks.len(), overall_lower, overall_upper, columns, retained_geometry_bytes)
        .map_err(|error| CsvBlockModelError::Invalid(error.to_string()))?;
    Ok(ParsedCsvBlockModel {
        model,
        blocks: BlockBoundsSource::Explicit(blocks),
    })
}

pub(crate) fn write<W: Write>(model: &BlockModelData, blocks: &BlockBoundsSource, renderable: &RenderableBlockIndices, mut output: W) -> Result<(), CsvBlockModelError> {
    if blocks.len() != model.metadata.n_blocks {
        return Err(CsvBlockModelError::Invalid(format!(
            "block model has {} geometry rows but {} data rows",
            blocks.len(),
            model.metadata.n_blocks
        )));
    }
    let variables: Vec<_> = model.color_variables().into_iter().filter(|variable| !variable.special).collect();
    let empty_values: Vec<_> = variables.iter().map(|variable| color_variable_default(variable)).collect();
    for (index, header) in ["x", "y", "z", "dx", "dy", "dz"]
        .into_iter()
        .chain(variables.iter().map(|variable| variable.name.as_str()))
        .enumerate()
    {
        if index > 0 {
            output.write_all(b",")?;
        }
        write_field(&mut output, header)?;
    }
    output.write_all(b"\n")?;

    const ROW_CHUNK: usize = 16_384;
    let mut renderable_rows = renderable.iter().peekable();
    let mut start = 0;
    while start < blocks.len() {
        let end = start.saturating_add(ROW_CHUNK).min(blocks.len());
        let columns: Vec<Vec<f64>> = variables
            .iter()
            .map(|variable| {
                model
                    .color_values_range(&variable.name, start, end)
                    .map_err(|error| CsvBlockModelError::Invalid(error.to_string()))
            })
            .collect::<Result<_, _>>()?;
        while renderable_rows.peek().is_some_and(|&row| row < end) {
            let row = renderable_rows.next().expect("peeked renderable row exists");
            if row < start {
                return Err(CsvBlockModelError::Invalid("renderable block indices must be ordered and contain no duplicates".to_owned()));
            }
            let block = blocks.get(row).ok_or_else(|| CsvBlockModelError::Invalid(format!("missing block geometry row {row}")))?;
            let center = model.local_to_world((block.lower + block.upper) * 0.5);
            let size = block.upper - block.lower;
            let geometry = [center.x, center.y, center.z, size.x, size.y, size.z];
            for (index, value) in geometry.into_iter().enumerate() {
                if index > 0 {
                    output.write_all(b",")?;
                }
                output.write_all(value.to_string().as_bytes())?;
            }
            for ((column, variable), empty_value) in columns.iter().zip(&variables).zip(&empty_values) {
                output.write_all(b",")?;
                let value = column[row - start];
                let categorical = matches!(variable.physical_type.as_str(), "namedbyte" | "namedshort");
                let is_empty = !value.is_finite() || empty_value.is_some_and(|empty| (value - empty).abs() < 1e-8) || (!categorical && is_no_data_sentinel(value));
                if !is_empty {
                    if categorical
                        && value.fract() == 0.0
                        && (0.0..=u32::MAX as f64).contains(&value)
                        && let Some(label) = variable.strings.get(&(value as u32))
                    {
                        write_field(&mut output, label)?;
                        continue;
                    }
                    output.write_all(value.to_string().as_bytes())?;
                }
            }
            output.write_all(b"\n")?;
        }
        start = end;
    }
    if let Some(row) = renderable_rows.next() {
        return Err(CsvBlockModelError::Invalid(format!("renderable block index {row} is outside the model")));
    }
    Ok(())
}

/// Mapping for a CSV emitted by [`write`], suitable for reopening a generated
/// model after Save As without asking the user to map its columns again.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn export_mapping(model: &BlockModelData) -> CsvColumnMapping {
    let mut roles = vec![
        CsvColumnRole::X,
        CsvColumnRole::Y,
        CsvColumnRole::Z,
        CsvColumnRole::Dx,
        CsvColumnRole::Dy,
        CsvColumnRole::Dz,
    ];
    roles.extend(
        model
            .color_variables()
            .into_iter()
            .filter(|variable| !variable.special)
            .map(|variable| match variable.physical_type.as_str() {
                "namedbyte" | "namedshort" => CsvColumnRole::Category,
                _ => CsvColumnRole::Value,
            }),
    );
    CsvColumnMapping { roles }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn to_bytes(model: &BlockModelData, blocks: &BlockBoundsSource, renderable: &RenderableBlockIndices) -> Result<Vec<u8>, CsvBlockModelError> {
    let mut bytes = Vec::new();
    write(model, blocks, renderable, &mut bytes)?;
    Ok(bytes)
}

fn unique_resource_names(headers: &[String], mapping: &CsvColumnMapping) -> Vec<String> {
    let mut used = HashSet::new();
    headers
        .iter()
        .enumerate()
        .filter(|(index, _)| matches!(mapping.roles[*index], CsvColumnRole::Value | CsvColumnRole::Category | CsvColumnRole::Id))
        .map(|(index, header)| {
            let base = if header.trim().is_empty() {
                format!("resource_{}", index + 1)
            } else {
                header.trim().to_owned()
            };
            let mut name = base.clone();
            let mut suffix = 2;
            while !used.insert(name.clone()) {
                name = format!("{base}_{suffix}");
                suffix += 1;
            }
            name
        })
        .collect()
}

fn parse_required_number(field: &str, row: usize, header: &str) -> Result<f64, CsvBlockModelError> {
    field
        .trim()
        .parse::<f64>()
        .map_err(|_| CsvBlockModelError::Invalid(format!("CSV row {row}, column '{header}': expected a number, found '{}'", field.trim())))
}

fn write_field(output: &mut impl Write, field: &str) -> io::Result<()> {
    if field.contains([',', '"', '\r', '\n']) {
        output.write_all(b"\"")?;
        for byte in field.bytes() {
            if byte == b'"' {
                output.write_all(b"\"\"")?;
            } else {
                output.write_all(&[byte])?;
            }
        }
        output.write_all(b"\"")
    } else {
        output.write_all(field.as_bytes())
    }
}

fn for_each_record(bytes: &[u8], limit: Option<usize>, mut visit: impl FnMut(usize, Vec<String>) -> Result<(), CsvBlockModelError>) -> Result<(), CsvBlockModelError> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|_| CsvBlockModelError::Invalid("CSV file must be UTF-8 text".to_owned()))?;
    let mut record = Vec::new();
    let mut field = Vec::new();
    let mut in_quotes = false;
    let mut index = 0;
    let mut record_number = 1;
    let mut emitted = 0;

    let finish_field = |field: &mut Vec<u8>, record: &mut Vec<String>| -> Result<(), CsvBlockModelError> {
        let bytes = std::mem::take(field);
        let value = String::from_utf8(bytes).map_err(|_| CsvBlockModelError::Invalid("CSV field is not valid UTF-8".to_owned()))?;
        record.push(value);
        Ok(())
    };

    while index < bytes.len() {
        let byte = bytes[index];
        if in_quotes {
            if byte == b'"' {
                if bytes.get(index + 1) == Some(&b'"') {
                    field.push(b'"');
                    index += 2;
                    continue;
                }
                in_quotes = false;
            } else {
                field.push(byte);
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' if field.is_empty() => {
                in_quotes = true;
                index += 1;
            }
            b',' => {
                finish_field(&mut field, &mut record)?;
                index += 1;
            }
            b'\r' | b'\n' => {
                finish_field(&mut field, &mut record)?;
                visit(record_number, std::mem::take(&mut record))?;
                emitted += 1;
                if limit.is_some_and(|limit| emitted >= limit) {
                    return Ok(());
                }
                record_number += 1;
                if byte == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                    index += 2;
                } else {
                    index += 1;
                }
            }
            _ => {
                field.push(byte);
                index += 1;
            }
        }
    }
    if in_quotes {
        return Err(CsvBlockModelError::Invalid(format!("CSV row {record_number} has an unterminated quoted field")));
    }
    if !field.is_empty() || !record.is_empty() {
        finish_field(&mut field, &mut record)?;
        visit(record_number, record)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED_EXAMPLE: &[u8] = include_bytes!("../../../examples/example_block_model_mixed.csv");

    #[test]
    fn preview_detects_text_columns_as_categories() {
        let preview = preview(MIXED_EXAMPLE).expect("example preview should parse");
        let role = |header: &str| {
            let index = preview.headers.iter().position(|candidate| candidate == header).expect("header should exist");
            preview.mapping.roles[index]
        };

        assert_eq!(role("id"), CsvColumnRole::Id);
        assert_eq!(role("tonnes"), CsvColumnRole::Value);
        assert_eq!(role("grade"), CsvColumnRole::Value);
        assert_eq!(role("lithology"), CsvColumnRole::Category);
        assert_eq!(role("ore_type"), CsvColumnRole::Category);
    }

    #[test]
    fn categorical_columns_parse_and_export_as_labels() {
        let initial_preview = preview(MIXED_EXAMPLE).expect("example preview should parse");
        let parsed = parse(MIXED_EXAMPLE, &initial_preview.mapping).expect("example model should parse");

        assert_eq!(parsed.model.metadata.n_blocks, 128);
        assert_eq!(parsed.model.numeric_variables().len(), 4);
        assert_eq!(parsed.model.color_variables().len(), 6);

        let lithology = parsed.model.variable("lithology").expect("lithology variable should exist");
        assert_eq!(lithology.physical_type, "namedbyte");
        assert!(lithology.strings.values().any(|label| label == "CLAY, WEATHERED"));
        assert_eq!(parsed.model.color_values("lithology").expect("category values should decode").len(), 128);

        let ore_values = parsed.model.color_values("ore_type").expect("category values should decode");
        assert!(ore_values[127].is_nan());

        let mut exported = Vec::new();
        write(&parsed.model, &parsed.blocks, &RenderableBlockIndices::All(parsed.model.metadata.n_blocks), &mut exported).expect("example model should export");
        let exported_text = String::from_utf8(exported.clone()).expect("CSV export should be UTF-8");
        assert!(exported_text.contains("\"CLAY, WEATHERED\""));

        let exported_preview = preview(&exported).expect("exported preview should parse");
        let saved_mapping = export_mapping(&parsed.model);
        assert_eq!(
            &saved_mapping.roles[..6],
            &[
                CsvColumnRole::X,
                CsvColumnRole::Y,
                CsvColumnRole::Z,
                CsvColumnRole::Dx,
                CsvColumnRole::Dy,
                CsvColumnRole::Dz
            ]
        );
        assert_eq!(saved_mapping.roles.len(), exported_preview.headers.len());
        assert_eq!(saved_mapping.roles.iter().filter(|role| **role == CsvColumnRole::Category).count(), 2);
        let reparsed = parse(&exported, &saved_mapping).expect("exported model should re-import with its saved mapping");
        let ore_type = reparsed.model.variable("ore_type").expect("ore_type variable should exist");
        assert!(ore_type.strings.values().any(|label| label == "Ore"));
        assert!(reparsed.model.color_values("ore_type").expect("category values should decode")[127].is_nan());
    }

    #[test]
    fn explicit_category_role_preserves_numeric_looking_labels() {
        let bytes = b"x,y,z,dx,dy,dz,zone\n0,0,0,1,1,1,01\n1,0,0,1,1,1,02\n";
        let mut mapping = preview(bytes).expect("preview should parse").mapping;
        mapping.roles[6] = CsvColumnRole::Category;

        let parsed = parse(bytes, &mapping).expect("numeric-looking categories should parse");
        let zone = parsed.model.variable("zone").expect("zone variable should exist");
        assert_eq!(zone.strings.get(&0).map(String::as_str), Some("01"));
        assert_eq!(zone.strings.get(&1).map(String::as_str), Some("02"));
    }
}
