//! Whole-project Open Mining Format (OMF) interchange.
//!
//! OMF is a container for mining data, not a triangle-mesh encoding.  This
//! module therefore translates a snapshot of every project-owned data family
//! to one project and decodes all matching OMF element types on import.
//! Native OMF geometry and attributes keep the file useful in other programs;
//! `incline:*` metadata preserves application-specific semantics for lossless
//! round trips (bulged design strings, text, drill intervals, and styling).

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use glam::{DMat3, DVec3};
use omf as omf_crate;
use serde_json::{Value, json};

use crate::{
    i18n::{tr, tr_format},
    model::{
        Document, FillStyle, Layer, Object, ObjectColor, PolyVertex,
        block_model::{
            BlockBounds, BlockBoundsSource, Boundary, ColorTransferFunction, LoadedBlockModel, OpenBlockModel, RenderableBlockIndices, StoredColorTransferFunction,
            opaque_irregular_surface_block_count, opaque_surface_block_count,
        },
        drill_hole::{DrillHole, DrillHoleDataset, DrillHoleSource, LoadedDrillHoleDataset, OpenDrillHoleDataset},
        formats::{
            block_model_data::{BlockModelColumn, BlockModelData},
            mesh_data::{Triangulation, Vertex},
        },
        point_cloud::{LoadedPointCloud, OpenPointCloud, finite_bounds, prepare_for_render},
        progress::Phase,
        project::{self, ProjectFile, ProjectMetadata},
        raster::{LoadedRasterTexture, OpenRasterTexture},
        triangulation::{LoadedTriangulation, OpenTriangulation, morton_surface_face_order, unique_edges},
    },
    rendering::color::{linear_to_srgb_byte, rgb_bytes_to_linear_rgba},
};

const META_KIND: &str = "incline:kind";
const META_NAME: &str = "incline:name";
const META_OBJECT: &str = "incline:object";
const META_LAYER: &str = "incline:layer";
const META_SOURCE: &str = "incline:source";
const META_STYLE: &str = "incline:style";
const META_ID: &str = "incline:id";
const META_DRILL_HOLE: &str = "incline:drill_hole";
/// A dataset's tie-in: its surface connectors and where the round starts,
/// both keyed by hole name. Carried on the dataset's own element, because
/// they are what joins its holes rather than anything one hole holds.
const META_TIE_INS: &str = "incline:tie_ins";
const MAX_ARRAY_ITEMS: u64 = 200_000_000;

/// Owned, cheaply-cloned state captured before OMF encoding moves to a worker.
#[derive(Clone, Default)]
pub(crate) struct ProjectSnapshot {
    pub(crate) name: String,
    pub(crate) designs: Option<ProjectFile>,
    pub(crate) triangulations: Vec<OpenTriangulation>,
    pub(crate) block_models: Vec<OpenBlockModel>,
    pub(crate) drill_holes: Vec<OpenDrillHoleDataset>,
    pub(crate) point_clouds: Vec<OpenPointCloud>,
    pub(crate) rasters: Vec<OpenRasterTexture>,
}

impl std::fmt::Debug for ProjectSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectSnapshot")
            .field("name", &self.name)
            .field("designs", &usize::from(self.designs.is_some()))
            .field("triangulations", &self.triangulations.len())
            .field("block_models", &self.block_models.len())
            .field("drill_holes", &self.drill_holes.len())
            .field("point_clouds", &self.point_clouds.len())
            .field("rasters", &self.rasters.len())
            .finish()
    }
}

impl ProjectSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.designs.is_none()
            && self.triangulations.is_empty()
            && self.block_models.is_empty()
            && self.drill_holes.is_empty()
            && self.point_clouds.is_empty()
            && self.rasters.is_empty()
    }

    fn item_count(&self) -> usize {
        usize::from(self.designs.is_some()) + self.triangulations.len() + self.block_models.len() + self.drill_holes.len() + self.point_clouds.len() + self.rasters.len()
    }
}

pub(crate) struct ImportedTriangulation {
    pub(crate) preferred_id: Option<u64>,
    pub(crate) source_name: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) loaded: LoadedTriangulation,
    pub(crate) visible: bool,
    pub(crate) color: [f32; 4],
    pub(crate) line_color: [f32; 4],
    pub(crate) line_weight: Option<f32>,
    pub(crate) raster_opacity: f32,
    pub(crate) raster_texture_id: Option<u64>,
}

pub(crate) struct ImportedBlockModel {
    pub(crate) preferred_id: Option<u64>,
    pub(crate) source_name: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) loaded: LoadedBlockModel,
    pub(crate) visible: bool,
    pub(crate) color: [f32; 4],
    pub(crate) slice: Option<crate::model::block_model::BlockModelSlice>,
    /// Ramps keyed by variable name, resolved from Incline Design's own style
    /// metadata where the source carried it and otherwise from the OMF
    /// colormaps on the attributes themselves. A variable absent here has its
    /// ramp invented at load from the data.
    pub(crate) color_transfers: BTreeMap<String, ColorTransferFunction>,
    pub(crate) hide_empty_color_values: bool,
}

pub(crate) struct ImportedPointCloud {
    pub(crate) preferred_id: Option<u64>,
    pub(crate) source_name: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) loaded: LoadedPointCloud,
    pub(crate) visible: bool,
    pub(crate) color: [f32; 4],
    pub(crate) point_size: f32,
}

pub(crate) struct ImportedDrillHoles {
    pub(crate) preferred_id: Option<u64>,
    pub(crate) source_name: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) loaded: LoadedDrillHoleDataset,
    pub(crate) visible: bool,
    pub(crate) color: crate::model::drill_hole::DrillColorState,
}

pub(crate) struct ImportedRaster {
    pub(crate) preferred_id: Option<u64>,
    pub(crate) source_name: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) loaded: LoadedRasterTexture,
    pub(crate) visible: bool,
}

#[derive(Default)]
pub(crate) struct ImportBundle {
    pub(crate) project_name: String,
    pub(crate) coordinate_reference_system: String,
    pub(crate) units: String,
    pub(crate) origin: [f64; 3],
    pub(crate) designs: Vec<ProjectFile>,
    pub(crate) triangulations: Vec<ImportedTriangulation>,
    pub(crate) block_models: Vec<ImportedBlockModel>,
    pub(crate) drill_holes: Vec<ImportedDrillHoles>,
    pub(crate) point_clouds: Vec<ImportedPointCloud>,
    pub(crate) rasters: Vec<ImportedRaster>,
    pub(crate) warnings: Vec<String>,
}

impl ImportBundle {
    pub(crate) fn item_count(&self) -> usize {
        self.designs.len() + self.triangulations.len() + self.block_models.len() + self.drill_holes.len() + self.point_clouds.len() + self.rasters.len()
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write_path(snapshot: ProjectSnapshot, path: &Path, progress: &Phase) -> Result<()> {
    crate::model::atomic_file::write_atomic(path, |file| {
        write_to(snapshot, file, progress)?;
        Ok(())
    })
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(crate) fn to_bytes(snapshot: ProjectSnapshot, progress: &Phase) -> Result<Vec<u8>> {
    let cursor = write_to(snapshot, Cursor::new(Vec::new()), progress)?;
    Ok(cursor.into_inner())
}

fn write_to<W: Write + Seek + Send>(snapshot: ProjectSnapshot, output: W, progress: &Phase) -> Result<W> {
    if snapshot.is_empty() {
        bail!("There is no open Incline Design data to export");
    }
    let mut writer = omf_crate::file::Writer::new(output).context("create OMF writer")?;
    let total = snapshot.item_count().max(1) as u64;
    let mut complete = 0u64;
    let mut elements = Vec::with_capacity(snapshot.item_count());
    let coordinate_reference_systems = snapshot
        .rasters
        .iter()
        .map(|raster| raster.projection.trim())
        .filter(|projection| !projection.is_empty())
        .collect::<BTreeSet<_>>();

    if let Some(design) = &snapshot.designs {
        elements.push(write_design(&mut writer, design)?);
        complete += 1;
        progress.set_items(complete, total);
    }
    for triangulation in &snapshot.triangulations {
        elements.push(write_triangulation(&mut writer, triangulation)?);
        complete += 1;
        progress.set_items(complete, total);
    }
    for block_model in &snapshot.block_models {
        elements.push(write_block_model(&mut writer, block_model)?);
        complete += 1;
        progress.set_items(complete, total);
    }
    for drill_holes in &snapshot.drill_holes {
        if let Some(element) = write_drill_holes(&mut writer, drill_holes)? {
            elements.push(element);
        }
        complete += 1;
        progress.set_items(complete, total);
    }
    for point_cloud in &snapshot.point_clouds {
        elements.push(write_point_cloud(&mut writer, point_cloud)?);
        complete += 1;
        progress.set_items(complete, total);
    }
    for raster in &snapshot.rasters {
        elements.push(write_raster(&mut writer, raster)?);
        complete += 1;
        progress.set_items(complete, total);
    }

    make_element_names_unique(&mut elements);
    let fallback_name = tr!(literal = "Incline Design project");
    let mut project = omf_crate::Project::new(if snapshot.name.trim().is_empty() { fallback_name.as_str() } else { &snapshot.name });
    project.application = format!("Incline {}", env!("CARGO_PKG_VERSION"));
    project.description = tr!(literal = "Mining data exported by Incline");
    if let Some(design) = snapshot.designs.as_ref()
        && !design.metadata.coordinate_reference_system.trim().is_empty()
    {
        project.coordinate_reference_system = design.metadata.coordinate_reference_system.clone();
    } else if coordinate_reference_systems.len() == 1 {
        project.coordinate_reference_system = coordinate_reference_systems.into_iter().next().unwrap_or_default().to_owned();
    }
    if let Some(design) = snapshot.designs.as_ref() {
        project.units = design.metadata.units.clone();
    }
    project.elements = elements;
    let (output, _warnings) = writer.finish(project).context("finish project")?;
    progress.finish();
    Ok(output)
}

fn put(element: &mut omf_crate::Element, key: &str, value: impl Into<Value>) {
    element.metadata.insert(key.to_owned(), value.into());
}

fn kind(element: &omf_crate::Element) -> Option<&str> {
    element.metadata.get(META_KIND).and_then(Value::as_str)
}

fn element_name(element: &omf_crate::Element) -> &str {
    element.metadata.get(META_NAME).and_then(Value::as_str).unwrap_or(&element.name)
}

fn element_id(element: &omf_crate::Element) -> Option<u64> {
    element
        .metadata
        .get(META_ID)
        .and_then(|value| value.as_str().and_then(|value| value.parse().ok()).or_else(|| value.as_u64()))
}

fn element_source_name(element: &omf_crate::Element) -> Option<String> {
    element
        .metadata
        .get(META_SOURCE)
        .and_then(|value| value.get("filename"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn element_source_format(element: &omf_crate::Element) -> Option<String> {
    element
        .metadata
        .get(META_SOURCE)
        .and_then(|value| value.get("format"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn put_item_identity(element: &mut omf_crate::Element, id: u64, source_name: Option<&str>, source_format: Option<&str>, fallback_format: &str) {
    put(element, META_ID, id.to_string());
    if source_name.is_some() || source_format.is_some() {
        put(element, META_SOURCE, json!({ "filename": source_name, "format": source_format.unwrap_or(fallback_format) }));
    }
}

fn make_element_names_unique(elements: &mut [omf_crate::Element]) {
    let mut used = BTreeSet::new();
    for element in elements {
        let original = element.name.clone();
        let mut unique = original.clone();
        let mut suffix = 2usize;
        while !used.insert(unique.clone()) {
            unique = format!("{original} ({suffix})");
            suffix += 1;
        }
        if unique != original {
            put(element, META_NAME, original);
            element.name = unique;
        }
        if let omf_crate::Geometry::Composite(composite) = &mut element.geometry {
            make_element_names_unique(&mut composite.elements);
        }
    }
}

fn rgba8(color: [f32; 4]) -> [u8; 4] {
    [
        linear_to_srgb_byte(color[0]),
        linear_to_srgb_byte(color[1]),
        linear_to_srgb_byte(color[2]),
        (color[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

fn linear_rgba(color: [u8; 4]) -> [f32; 4] {
    let mut rgba = rgb_bytes_to_linear_rgba([color[0], color[1], color[2]]);
    rgba[3] = f32::from(color[3]) / 255.0;
    rgba
}

fn element_color(element: &omf_crate::Element, fallback: [f32; 4]) -> [f32; 4] {
    element.color.map(linear_rgba).unwrap_or(fallback)
}

fn write_design<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, design: &ProjectFile) -> Result<omf_crate::Element> {
    const LOCAL_MASK: u64 = u32::MAX as u64;
    let document = &design.document;
    let mut layers = Vec::with_capacity(document.layers().len());
    for layer in document.layers() {
        let mut objects = Vec::new();
        for object in document.objects().iter().filter(|object| object.layer() == layer.id) {
            objects.push(write_design_object(writer, document, object)?);
        }
        let mut element = omf_crate::Element::new(layer.name.clone(), omf_crate::Composite::new(objects));
        element.color = Some(rgba8(layer.color));
        put(&mut element, META_KIND, "design_layer");
        let mut portable_layer = layer.clone();
        portable_layer.id = crate::model::LayerId(layer.id.0 & LOCAL_MASK);
        put(&mut element, META_LAYER, serde_json::to_value(portable_layer)?);
        layers.push(element);
    }
    let mut element = omf_crate::Element::new("Designs", omf_crate::Composite::new(layers));
    put(&mut element, META_KIND, "designs");
    Ok(element)
}

fn write_design_object<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, document: &Document, object: &Object) -> Result<omf_crate::Element> {
    const LOCAL_MASK: u64 = u32::MAX as u64;
    let local_id = object.id().0 & LOCAL_MASK;
    let (name, geometry): (String, omf_crate::Geometry) = match object {
        Object::Point { pos, .. } => (format!("Point {local_id}"), omf_crate::PointSet::new(writer.array_vertices([pos.to_array()])?).into()),
        Object::Polyline { verts, closed, .. } => {
            let vertices = verts.iter().map(|vertex| vertex.pos.to_array());
            let mut segments = (0..verts.len().saturating_sub(1)).map(|index| [index as u32, index as u32 + 1]).collect::<Vec<_>>();
            if *closed && verts.len() > 2 {
                segments.push([(verts.len() - 1) as u32, 0]);
            }
            (
                format!("{} {local_id}", if verts.len() == 2 { "Line" } else { "Polyline" }),
                omf_crate::LineSet::new(writer.array_vertices(vertices)?, writer.array_segments(segments)?).into(),
            )
        }
        Object::Text { pos, content, .. } => (
            if content.trim().is_empty() { format!("Text {local_id}") } else { content.clone() },
            omf_crate::PointSet::new(writer.array_vertices([pos.to_array()])?).into(),
        ),
    };
    let mut element = omf_crate::Element::new(name, geometry);
    element.color = Some(rgba8(document.object_rgba(object)));
    put(
        &mut element,
        META_KIND,
        match object {
            Object::Point { .. } => "design_point",
            Object::Polyline { .. } => "design_polyline",
            Object::Text { .. } => "design_text",
        },
    );
    let portable_object = object.with_id_and_layer(crate::model::ObjectId(local_id), crate::model::LayerId(object.layer().0 & LOCAL_MASK));
    put(&mut element, META_OBJECT, serde_json::to_value(portable_object)?);
    put(&mut element, META_STYLE, json!({ "visible": !document.is_object_hidden(object.id()) }));
    Ok(element)
}

fn write_triangulation<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, triangulation: &OpenTriangulation) -> Result<omf_crate::Element> {
    let mesh = &triangulation.mesh;
    let vertices = mesh.vertices().iter().map(|vertex| vertex.as_array());
    let triangles = mesh.face_vertex_indices_iter().map(|face| face.map(|index| index as u32));
    let mut element = omf_crate::Element::new(
        triangulation.name.clone(),
        omf_crate::Surface::new(writer.array_vertices(vertices)?, writer.array_triangles(triangles)?),
    );
    element.color = Some(rgba8(triangulation.color));
    put(&mut element, META_KIND, "triangulation");
    put_item_identity(
        &mut element,
        triangulation.id.0,
        triangulation.state.source_name.as_deref(),
        triangulation.state.source_format.as_deref(),
        "surface",
    );
    put(
        &mut element,
        META_STYLE,
        json!({
            "visible": triangulation.visible,
            "color": triangulation.color,
            "line_color": triangulation.line_color,
            "line_weight": triangulation.line_weight,
            "raster_opacity": triangulation.raster_opacity,
            "raster_texture_id": triangulation.raster_texture.map(|id| id.0),
        }),
    );
    Ok(element)
}

fn write_point_cloud<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, cloud: &OpenPointCloud) -> Result<omf_crate::Element> {
    let mut element = omf_crate::Element::new(
        cloud.name.clone(),
        omf_crate::PointSet::new(writer.array_vertices(cloud.points.iter().map(|point| point.to_array()))?),
    );
    element.color = Some(rgba8(cloud.color));
    if let Some(colors) = cloud.colors.as_ref().filter(|colors| colors.len() == cloud.points.len()) {
        element.attributes.push(omf_crate::Attribute::from_colors(
            "Color",
            omf_crate::Location::Vertices,
            writer.array_colors(colors.iter().map(|color| Some(color.to_le_bytes())))?,
        ));
    }
    put(&mut element, META_KIND, "point_cloud");
    put_item_identity(
        &mut element,
        cloud.id.0,
        cloud.state.source_name.as_deref(),
        cloud.state.source_format.as_deref(),
        "point-set",
    );
    put(
        &mut element,
        META_STYLE,
        json!({ "visible": cloud.visible, "color": cloud.color, "point_size": cloud.point_size }),
    );
    Ok(element)
}

/// Write a numeric attribute, carrying its colormap when it has one.
///
/// Both colormap kinds are stored in exactly OMF's shape, so this is a copy
/// rather than a conversion. Boundaries and the range are written as `f64` to
/// match the `f64` values alongside them, which the standard requires the types
/// to agree on - [`BlockModelData`] decodes every numeric column to `f64`, so
/// that is the only honest choice.
fn write_number_attribute<W: Write + Seek + Send>(
    writer: &mut omf_crate::file::Writer<W>,
    name: &str,
    location: omf_crate::Location,
    values: omf_crate::Array<omf_crate::array_type::Number>,
    transfer: Option<&ColorTransferFunction>,
) -> Result<omf_crate::Attribute> {
    match transfer {
        Some(ColorTransferFunction::Discrete { boundaries, gradient }) if !boundaries.is_empty() && gradient.len() == boundaries.len() + 1 => {
            let boundaries = writer.array_boundaries(boundaries.iter().map(|boundary| {
                if boundary.inclusive {
                    omf_crate::data::Boundary::LessEqual(boundary.value)
                } else {
                    omf_crate::data::Boundary::Less(boundary.value)
                }
            }))?;
            let gradient = writer.array_gradient(gradient.iter().map(|color| rgba8(*color)))?;
            Ok(omf_crate::Attribute::from_numbers_discrete_colormap(
                name.to_owned(),
                location,
                values,
                boundaries,
                gradient,
            ))
        }
        Some(ColorTransferFunction::Continuous { range, gradient }) if !gradient.is_empty() => {
            let gradient = writer.array_gradient(gradient.iter().map(|color| rgba8(*color)))?;
            Ok(omf_crate::Attribute::from_numbers_continuous_colormap(name.to_owned(), location, values, *range, gradient))
        }
        // A `Category` colormap on a numeric column, or a malformed one, has
        // nothing valid to say here.
        _ => Ok(omf_crate::Attribute::from_numbers(name.to_owned(), location, values)),
    }
}

/// One RGBA per category, in `codes` order, ready for `Category.gradient`.
fn category_gradient(variable: &crate::model::formats::block_model_data::BlockVariable, codes: &[u32], transfer: Option<&ColorTransferFunction>) -> Vec<[u8; 4]> {
    codes
        .iter()
        .map(|code| {
            let color = match transfer {
                Some(ColorTransferFunction::Category { gradient }) => gradient.get(code).copied(),
                _ => None,
            }
            .or_else(|| variable.category_colors.get(code).copied())
            .unwrap_or([0.72, 0.72, 0.75, 1.0]);
            rgba8(color)
        })
        .collect()
}

/// An OMF `NumberColormap` as an Incline Design colormap. A move, not a translation:
/// gradients are kept whole (the render side caps its own working copy) and
/// boundary inclusiveness is preserved.
fn read_number_colormap<R: omf_crate::file::ReadAt>(reader: &omf_crate::file::Reader<R>, colormap: &omf_crate::NumberColormap) -> Result<ColorTransferFunction> {
    match colormap {
        omf_crate::NumberColormap::Continuous { range, gradient } => {
            let gradient = collect_results(reader.array_gradient(gradient)?)?.into_iter().map(linear_rgba).collect::<Vec<_>>();
            if gradient.is_empty() {
                bail!("continuous colormap has no colours");
            }
            Ok(ColorTransferFunction::Continuous {
                range: number_range_bounds(range),
                gradient,
            })
        }
        omf_crate::NumberColormap::Discrete { boundaries, gradient } => {
            let gradient = collect_results(reader.array_gradient(gradient)?)?.into_iter().map(linear_rgba).collect::<Vec<_>>();
            let boundaries = read_boundaries(reader, boundaries)?;
            if boundaries.is_empty() {
                bail!("discrete colormap has no boundaries");
            }
            if gradient.len() != boundaries.len() + 1 {
                bail!("discrete colormap has {} colours for {} boundaries", gradient.len(), boundaries.len());
            }
            Ok(ColorTransferFunction::Discrete { boundaries, gradient })
        }
    }
}

/// A colormap's range as `f64`. Dates and date-times become their numeric epoch
/// offsets, matching how `array_numbers().try_into_f64()` decodes the values the
/// range describes.
fn number_range_bounds(range: &omf_crate::NumberRange) -> (f64, f64) {
    match range {
        omf_crate::NumberRange::Float { min, max } => (*min, *max),
        omf_crate::NumberRange::Integer { min, max } => (*min as f64, *max as f64),
        omf_crate::NumberRange::Date { min, max } => (date_to_f64(*min), date_to_f64(*max)),
        omf_crate::NumberRange::DateTime { min, max } => (min.timestamp() as f64, max.timestamp() as f64),
    }
}

/// Days since the 1970-01-01 epoch, the numeric form OMF defines for dates.
fn date_to_f64(date: chrono::NaiveDate) -> f64 {
    date.signed_duration_since(chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch is a valid date"))
        .num_days() as f64
}

fn read_boundaries<R: omf_crate::file::ReadAt>(reader: &omf_crate::file::Reader<R>, array: &omf_crate::Array<omf_crate::array_type::Boundary>) -> Result<Vec<Boundary>> {
    let boundaries = reader.array_boundaries(array)?;
    // Integer boundaries refuse the f64 cast, so fall back rather than losing
    // the colormap entirely.
    let decoded = match boundaries.try_into_f64() {
        Ok(iter) => collect_results(iter)?,
        Err(_) => collect_results(reader.array_boundaries(array)?.try_into_i64()?)?
            .into_iter()
            .map(|boundary| boundary.map(|value| value as f64))
            .collect(),
    };
    Ok(decoded
        .into_iter()
        .enumerate()
        .map(|(index, boundary)| Boundary {
            id: index as u64 + 1,
            value: boundary.value(),
            inclusive: boundary.is_inclusive(),
        })
        .collect())
}

fn write_block_model<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, open: &OpenBlockModel) -> Result<omf_crate::Element> {
    let model = &open.model;
    let lower = model.metadata.lower;
    let upper = model.metadata.upper;
    let extent = upper - lower;
    if !extent.is_finite() || extent.min_element() <= 0.0 {
        bail!("Block model '{}' has invalid bounds", open.name);
    }
    let selected = open.renderable_block_indices.iter().collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("Block model '{}' has no renderable blocks", open.name);
    }
    let subblocks = selected
        .iter()
        .map(|&index| {
            let block = open.blocks.get(index).with_context(|| format!("Block model '{}' is missing block {index}", open.name))?;
            // Normalizing by `extent` should land boundary blocks exactly on
            // 0.0/1.0, but float rounding can push a corner a hair outside
            // that range, which OMF's strict validation rejects.
            let min = ((block.lower - lower) / extent).clamp(DVec3::ZERO, DVec3::ONE);
            let max = ((block.upper - lower) / extent).clamp(DVec3::ZERO, DVec3::ONE);
            Ok(([0, 0, 0], [min.x, min.y, min.z, max.x, max.y, max.z]))
        })
        .collect::<Result<Vec<_>>>()?;
    let rotation = model.rotation();
    let geometry = omf_crate::BlockModel::with_freeform_subblocks(
        omf_crate::Orient3::new(
            model.local_to_world(lower).to_array(),
            rotation.x_axis.to_array(),
            rotation.y_axis.to_array(),
            rotation.z_axis.to_array(),
        ),
        omf_crate::Grid3::from_size_and_count(extent.to_array(), [1, 1, 1]),
        writer.array_freeform_subblocks(subblocks)?,
    );
    let mut element = omf_crate::Element::new(open.name.clone(), geometry);
    element.color = Some(rgba8(open.color));
    for variable in &model.metadata.variables {
        let Some(values) = model.shared_numeric_values(&variable.name) else {
            continue;
        };
        if variable.strings.is_empty() {
            let selected_values = selected.iter().map(|&index| values.get(index).copied().filter(|value| value.is_finite()));
            let numbers = writer.array_numbers(selected_values)?;
            element.attributes.push(write_number_attribute(
                writer,
                &variable.name,
                omf_crate::Location::Subblocks,
                numbers,
                open.color_transfers.get(&variable.name),
            )?);
        } else {
            let codes = variable.strings.keys().copied().collect::<Vec<_>>();
            let code_to_index = codes.iter().enumerate().map(|(index, code)| (*code, index as u32)).collect::<BTreeMap<_, _>>();
            let indices = selected.iter().map(|&index| {
                values
                    .get(index)
                    .copied()
                    .filter(|value| value.is_finite() && value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(value))
                    .and_then(|value| code_to_index.get(&(value as u32)).copied())
            });
            let names = codes.iter().map(|code| variable.strings.get(code).cloned().unwrap_or_else(|| code.to_string()));
            let original_codes = omf_crate::Attribute::from_numbers(
                "Incline category code",
                omf_crate::Location::Categories,
                writer.array_numbers(codes.iter().map(|code| Some(i64::from(*code))))?,
            );
            // One colour per name, in the same order - OMF's `gradient` is
            // exactly Incline Design's per-category ramp, so other applications see
            // the colours the user chose rather than inventing their own.
            let gradient = category_gradient(variable, &codes, open.color_transfers.get(&variable.name));
            let indices = writer.array_indices(indices)?;
            let names = writer.array_names(names)?;
            element.attributes.push(omf_crate::Attribute::from_categories(
                variable.name.clone(),
                omf_crate::Location::Subblocks,
                indices,
                names,
                Some(writer.array_gradient(gradient)?),
                [original_codes],
            ));
        }
    }
    put(&mut element, META_KIND, "block_model");
    put_item_identity(
        &mut element,
        open.id.0,
        open.state.source_name.as_deref(),
        open.state.source_format.as_deref(),
        "block-model",
    );
    put(
        &mut element,
        META_STYLE,
        json!({
            "visible": open.visible,
            "color": open.color,
            "slice": open.slice,
            "active_color_variable": open.active_color_variable,
            "hide_empty_color_values": open.hide_empty_color_values,
        }),
    );
    Ok(element)
}

fn write_drill_holes<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, open: &OpenDrillHoleDataset) -> Result<Option<omf_crate::Element>> {
    let mut holes = Vec::new();
    for hole in &open.dataset.holes {
        if let Some(element) = write_drill_hole(writer, hole, &open.dataset)? {
            holes.push(element);
        }
    }
    if holes.is_empty() {
        return Ok(None);
    }
    let mut element = omf_crate::Element::new(open.name.clone(), omf_crate::Composite::new(holes));
    put(&mut element, META_KIND, "drillhole_dataset");
    put_item_identity(
        &mut element,
        open.id.0,
        open.state.source_name.as_deref(),
        open.state.source_format.as_deref(),
        "drill-holes",
    );
    put(&mut element, META_STYLE, json!({ "visible": open.visible, "color": open.color }));
    let ties = open.dataset.stored_ties();
    if !ties.is_empty() {
        put(&mut element, META_TIE_INS, serde_json::to_value(&ties)?);
    }
    Ok(Some(element))
}

fn write_drill_hole<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, hole: &DrillHole, dataset: &DrillHoleDataset) -> Result<Option<omf_crate::Element>> {
    if hole.trace.len() < 2 {
        return Ok(None);
    }
    let mut depths = hole.trace.iter().map(|station| station.depth).collect::<Vec<_>>();
    depths.extend(hole.intervals.iter().flat_map(|interval| [interval.from, interval.to]));
    depths.extend(hole.render_ranges.iter().flat_map(|&(from, to)| [from, to]));
    depths.retain(|depth| depth.is_finite());
    depths.sort_by(f64::total_cmp);
    depths.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-9);
    let positions = depths.iter().filter_map(|depth| hole.position_at_depth(*depth)).collect::<Vec<_>>();
    if positions.len() != depths.len() || positions.len() < 2 {
        return Ok(None);
    }
    let mut segments = Vec::new();
    let mut ranges = Vec::new();
    for index in 0..depths.len() - 1 {
        let from = depths[index];
        let to = depths[index + 1];
        let midpoint = (from + to) * 0.5;
        let visible = hole.render_ranges.is_empty() || hole.render_ranges.iter().any(|&(start, end)| midpoint >= start && midpoint <= end);
        if visible && to > from {
            segments.push([index as u32, index as u32 + 1]);
            ranges.push((from, to, midpoint));
        }
    }
    if segments.is_empty() {
        return Ok(None);
    }
    let mut element = omf_crate::Element::new(
        hole.dhid.clone(),
        omf_crate::LineSet::new(
            writer.array_vertices(positions.iter().map(|position| position.to_array()))?,
            writer.array_segments(segments)?,
        ),
    );
    element.attributes.push(omf_crate::Attribute::from_numbers(
        "Measured depth",
        omf_crate::Location::Vertices,
        writer.array_numbers(depths.iter().copied().map(Some))?,
    ));
    element.attributes.push(omf_crate::Attribute::from_strings(
        "Hole ID",
        omf_crate::Location::Primitives,
        writer.array_text(ranges.iter().map(|_| Some(hole.dhid.clone())))?,
    ));
    element.attributes.push(omf_crate::Attribute::from_numbers(
        "From",
        omf_crate::Location::Primitives,
        writer.array_numbers(ranges.iter().map(|(from, _, _)| Some(*from)))?,
    ));
    element.attributes.push(omf_crate::Attribute::from_numbers(
        "To",
        omf_crate::Location::Primitives,
        writer.array_numbers(ranges.iter().map(|(_, to, _)| Some(*to)))?,
    ));
    for field in &dataset.fields {
        let values = ranges
            .iter()
            .map(|(_, _, midpoint)| {
                hole.intervals
                    .iter()
                    .find(|interval| *midpoint >= interval.from && *midpoint < interval.to)
                    .and_then(|interval| interval.values.get(&field.key))
            })
            .collect::<Vec<_>>();
        match &field.kind {
            crate::model::drill_hole::DrillFieldKind::Numeric { .. } => {
                element.attributes.push(omf_crate::Attribute::from_numbers(
                    field.label.clone(),
                    omf_crate::Location::Primitives,
                    writer.array_numbers(values.iter().map(|value| match value {
                        Some(crate::model::drill_hole::DrillValue::Numeric(value)) if value.is_finite() => Some(*value),
                        _ => None,
                    }))?,
                ));
            }
            crate::model::drill_hole::DrillFieldKind::Categorical { .. } => {
                let names = values
                    .iter()
                    .filter_map(|value| match value {
                        Some(crate::model::drill_hole::DrillValue::Category(value)) if !value.is_empty() => Some(value.clone()),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                if names.is_empty() {
                    continue;
                }
                let lookup = names.iter().enumerate().map(|(index, name)| (name.as_str(), index as u32)).collect::<BTreeMap<_, _>>();
                let indices = values.iter().map(|value| match value {
                    Some(crate::model::drill_hole::DrillValue::Category(value)) => lookup.get(value.as_str()).copied(),
                    _ => None,
                });
                element.attributes.push(omf_crate::Attribute::from_categories(
                    field.label.clone(),
                    omf_crate::Location::Primitives,
                    writer.array_indices(indices)?,
                    writer.array_names(names)?,
                    None,
                    [],
                ));
            }
        }
    }
    put(&mut element, META_KIND, "drillhole");
    put(&mut element, META_DRILL_HOLE, serde_json::to_value(hole)?);
    Ok(Some(element))
}

fn write_raster<W: Write + Seek + Send>(writer: &mut omf_crate::file::Writer<W>, raster: &OpenRasterTexture) -> Result<omf_crate::Element> {
    let [a, b, c, d, e, f] = raster.world_to_uv;
    let determinant = a * e - b * d;
    if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
        bail!("Raster '{}' has a singular world transform", raster.name);
    }
    let world = |u: f64, v: f64| {
        let x = (e * (u - c) - b * (v - f)) / determinant;
        let y = (-d * (u - c) + a * (v - f)) / determinant;
        DVec3::new(x, y, 0.0)
    };
    let origin = world(0.0, 0.0);
    let u_vector = world(1.0, 0.0) - origin;
    let v_vector = world(0.0, 1.0) - origin;
    let corners = [origin, origin + u_vector, origin + u_vector + v_vector, origin + v_vector];
    let mut element = omf_crate::Element::new(
        raster.name.clone(),
        omf_crate::Surface::new(
            writer.array_vertices(corners.map(|corner| corner.to_array()))?,
            writer.array_triangles([[0, 1, 2], [0, 2, 3]])?,
        ),
    );
    let png = encode_png(raster.source_size, &raster.full_rgba)?;
    element.attributes.push(omf_crate::Attribute::from_texture_map(
        "Raster",
        writer.image_bytes(&png)?,
        omf_crate::Location::Vertices,
        writer.array_texcoords([[0.0_f64, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])?,
    ));
    put(&mut element, META_KIND, "raster");
    put_item_identity(
        &mut element,
        raster.id.0,
        raster.state.source_name.as_deref(),
        raster.state.source_format.as_deref(),
        "raster",
    );
    put(
        &mut element,
        META_STYLE,
        json!({
            "visible": raster.visible,
            "world_to_uv": raster.world_to_uv,
            "source_size": raster.source_size,
            "preview_size": raster.preview_size,
            "projection": raster.projection,
            "driver_name": raster.driver_name,
        }),
    );
    Ok(element)
}

fn encode_png(size: [u32; 2], rgba: &[u8]) -> Result<Vec<u8>> {
    let expected = usize::try_from(u64::from(size[0]) * u64::from(size[1]) * 4).context("raster dimensions exceed addressable memory")?;
    if rgba.len() != expected {
        bail!("Raster pixel buffer has {} bytes; expected {expected}", rgba.len());
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, size[0], size[1]);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(bytes)
}

/// Read an OMF 2 container.
pub(crate) fn from_bytes(source_name: &str, bytes: Vec<u8>, progress: &Phase) -> Result<ImportBundle> {
    progress.set_fraction(0.0);
    reject_omf1(&bytes).with_context(|| format!("read OMF container {source_name}"))?;
    let mut reader = omf_crate::file::Reader::new(bytes).context("open OMF archive")?;
    reader.set_limits(omf_crate::file::Limits {
        json_bytes: Some(64 * 1024 * 1024),
        image_bytes: Some(4 * 1024 * 1024 * 1024),
        image_dim: Some(65_536),
        validation: Some(1_000),
    });
    let (project, problems) = reader.project().context("read project index")?;
    let mut bundle = ImportBundle {
        project_name: project.name.clone(),
        coordinate_reference_system: project.coordinate_reference_system.clone(),
        units: project.units.clone(),
        origin: project.origin,
        ..Default::default()
    };
    let written_by_incline = project.application.starts_with("Incline ");
    if !project.description.trim().is_empty() && !written_by_incline {
        bundle.warnings.push(tr!(literal = "Project description is not retained"));
    }
    if !project.author.trim().is_empty() {
        bundle.warnings.push(tr!(literal = "Project author is not retained"));
    }
    if !project.application.trim().is_empty() && !project.application.starts_with("Incline ") {
        bundle.warnings.push(tr_format!(
            literal = "Project application metadata '%application%' is not retained",
            application = &project.application
        ));
    }
    if !project.metadata.is_empty() {
        bundle.warnings.push(tr_format!(
            literal = "Project has unsupported metadata keys: %keys%",
            keys = project.metadata.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !problems.is_empty() {
        bundle
            .warnings
            .push(tr_format!(literal = "OMF validation warnings: %warnings%", warnings = format!("{problems:?}")));
    }
    let mut decoder = Decoder {
        reader: &reader,
        project_origin: DVec3::from_array(project.origin),
        project_crs: project.coordinate_reference_system,
        project_units: project.units,
        source_name,
        bundle,
        generic_design: Document::new(),
    };
    let total = project.elements.len().max(1) as u64;
    for (index, element) in project.elements.iter().enumerate() {
        decoder.walk(element)?;
        progress.set_items(index as u64 + 1, total);
    }
    decoder.finish()
}

/// OMF 1 containers open with this magic and an `OMF-v...` version string.
/// Incline Design reads OMF 2 only, so they are named in the error rather than left
/// to fail later as an unrecognised archive.
fn reject_omf1(bytes: &[u8]) -> Result<()> {
    const OMF1_MAGIC: [u8; 4] = [0x84, 0x83, 0x82, 0x81];
    if !bytes.starts_with(&OMF1_MAGIC) {
        return Ok(());
    }
    let version = String::from_utf8_lossy(bytes.get(4..36).unwrap_or_default());
    let version = version.trim_end_matches('\0');
    let detail = if version.starts_with("OMF-") { format!(" ({version})") } else { String::new() };
    bail!("This is an OMF 1 file{detail}. Incline Design reads OMF 2 only; re-export it as OMF 2 from the application that wrote it.");
}

struct Decoder<'a, R: omf_crate::file::ReadAt> {
    reader: &'a omf_crate::file::Reader<R>,
    project_origin: DVec3,
    project_crs: String,
    project_units: String,
    source_name: &'a str,
    bundle: ImportBundle,
    generic_design: Document,
}

impl<R: omf_crate::file::ReadAt> Decoder<'_, R> {
    fn finish(mut self) -> Result<ImportBundle> {
        if !self.generic_design.layers().is_empty() {
            self.generic_design.validate().context("validate OMF line-set designs")?;
            self.bundle.designs.push(ProjectFile {
                format_version: project::PROJECT_FORMAT_VERSION,
                document: self.generic_design,
                metadata: ProjectMetadata {
                    name: file_stem(self.source_name),
                    coordinate_reference_system: self.project_crs.clone(),
                    units: self.project_units.clone(),
                },
            });
        }
        Ok(self.bundle)
    }

    fn walk(&mut self, element: &omf_crate::Element) -> Result<()> {
        self.record_unsupported_content(element);
        match kind(element) {
            Some("designs" | "design_database") => {
                let design = self.read_design(element)?;
                self.bundle.designs.push(design);
                return Ok(());
            }
            Some("drillhole_dataset") => {
                if let Some(drill_holes) = self.read_drill_dataset(element)? {
                    self.bundle.drill_holes.push(drill_holes);
                }
                return Ok(());
            }
            Some("raster") => {
                self.read_textures(element)?;
                return Ok(());
            }
            _ => {}
        }

        match &element.geometry {
            omf_crate::Geometry::Surface(surface) => self.read_surface(element, surface)?,
            omf_crate::Geometry::GridSurface(surface) => self.read_grid_surface(element, surface)?,
            omf_crate::Geometry::PointSet(points) => self.read_point_cloud(element, points)?,
            omf_crate::Geometry::LineSet(lines) => self.read_generic_lines(element, lines)?,
            omf_crate::Geometry::BlockModel(model) => self.read_block_model(element, model)?,
            omf_crate::Geometry::Composite(composite) => {
                for child in &composite.elements {
                    self.walk(child)?;
                }
            }
        }
        if !element.attributes.is_empty() {
            self.read_textures(element)?;
        }
        Ok(())
    }

    fn record_unsupported_content(&mut self, element: &omf_crate::Element) {
        const KNOWN_METADATA: &[&str] = &[
            META_KIND,
            META_NAME,
            META_OBJECT,
            META_LAYER,
            META_SOURCE,
            META_STYLE,
            META_ID,
            META_DRILL_HOLE,
            META_TIE_INS,
        ];
        let unknown_metadata = element.metadata.keys().filter(|key| !KNOWN_METADATA.contains(&key.as_str())).cloned().collect::<Vec<_>>();
        if !unknown_metadata.is_empty() {
            self.bundle
                .warnings
                .push(format!("Element '{}' has unsupported metadata keys: {}", element.name, unknown_metadata.join(", ")));
        }
        if !element.description.trim().is_empty() {
            self.bundle
                .warnings
                .push(format!("Element '{}' has a description that Incline Design does not retain", element.name));
        }

        let incline_design_kind = kind(element);
        let unsupported_attributes = element
            .attributes
            .iter()
            .filter(|attribute| {
                if matches!(incline_design_kind, Some("designs" | "design_database" | "drillhole_dataset" | "drillhole" | "raster")) {
                    return false;
                }
                match &element.geometry {
                    omf_crate::Geometry::Surface(_) | omf_crate::Geometry::GridSurface(_) => !matches!(
                        attribute.data,
                        omf_crate::AttributeData::MappedTexture { .. } | omf_crate::AttributeData::ProjectedTexture { .. }
                    ),
                    omf_crate::Geometry::PointSet(_) => !matches!(attribute.data, omf_crate::AttributeData::Color { .. }),
                    omf_crate::Geometry::BlockModel(_) => !matches!(attribute.data, omf_crate::AttributeData::Number { .. } | omf_crate::AttributeData::Category { .. }),
                    omf_crate::Geometry::LineSet(_) | omf_crate::Geometry::Composite(_) => true,
                }
            })
            .map(|attribute| attribute.name.clone())
            .collect::<Vec<_>>();
        if !unsupported_attributes.is_empty() {
            self.bundle.warnings.push(format!(
                "Element '{}' has unsupported attributes that will be omitted: {}",
                element.name,
                unsupported_attributes.join(", ")
            ));
        }
        for attribute in &element.attributes {
            if !attribute.description.trim().is_empty() || !attribute.metadata.is_empty() {
                self.bundle.warnings.push(format!(
                    "Attribute '{}' on element '{}' has descriptive metadata that Incline Design does not retain",
                    attribute.name, element.name
                ));
            }
        }
    }

    fn read_design(&mut self, element: &omf_crate::Element) -> Result<ProjectFile> {
        let omf_crate::Geometry::Composite(composite) = &element.geometry else {
            bail!("Incline Design designs element '{}' is not an OMF composite", element.name);
        };
        let mut document = Document::new();
        for layer_element in &composite.elements {
            self.record_unsupported_content(layer_element);
            let mut layer_template = layer_element
                .metadata
                .get(META_LAYER)
                .cloned()
                .and_then(|value| serde_json::from_value::<Layer>(value).ok());
            if let Some(layer) = layer_template.as_mut() {
                layer.elevation += self.project_origin.z as f32;
            }
            let color = layer_template
                .as_ref()
                .map(|layer| layer.color)
                .unwrap_or_else(|| element_color(layer_element, [1.0, 1.0, 1.0, 1.0]));
            let layer_id = if let Some(layer) = layer_template.as_ref()
                && layer.id.0 <= u32::MAX as u64
                && document.layer(layer.id).is_none()
            {
                document.append_layer_snapshot(layer, std::iter::empty());
                layer.id
            } else {
                document.add_layer(
                    layer_template
                        .as_ref()
                        .map(|layer| layer.name.clone())
                        .unwrap_or_else(|| element_name(layer_element).to_owned()),
                    layer_template.as_ref().and_then(|layer| layer.color_index),
                    color,
                    layer_template.as_ref().is_none_or(|layer| layer.visible),
                    layer_template.as_ref().map_or(0.0, |layer| layer.elevation),
                )
            };
            let children = match &layer_element.geometry {
                omf_crate::Geometry::Composite(layer) => layer.elements.as_slice(),
                _ => std::slice::from_ref(layer_element),
            };
            for object_element in children {
                if !std::ptr::eq(layer_element, object_element) {
                    self.record_unsupported_content(object_element);
                }
                if let Some(value) = object_element.metadata.get(META_OBJECT).cloned() {
                    match serde_json::from_value::<Object>(value) {
                        Ok(mut object) => {
                            object.translate(self.project_origin);
                            let source_id = object.id();
                            let id = if source_id.0 <= u32::MAX as u64 && document.get_object(source_id).is_none() {
                                source_id
                            } else {
                                document.allocate_object_id()
                            };
                            document.insert_object(object.with_id_and_layer(id, layer_id));
                            if style_bool(object_element.metadata.get(META_STYLE), "visible") == Some(false) {
                                document.set_object_hidden(id, true);
                            }
                            continue;
                        }
                        Err(error) => self.bundle.warnings.push(format!(
                            "Design object '{}' has invalid Incline Design metadata and was reconstructed from native geometry where possible: {error}",
                            object_element.name
                        )),
                    }
                }
                self.append_design_geometry(&mut document, layer_id, object_element)?;
            }
        }
        document.validate().with_context(|| format!("validate designs '{}'", element.name))?;
        Ok(ProjectFile {
            format_version: project::PROJECT_FORMAT_VERSION,
            document,
            metadata: ProjectMetadata {
                name: element_name(element).to_owned(),
                coordinate_reference_system: self.project_crs.clone(),
                units: self.project_units.clone(),
            },
        })
    }

    fn append_design_geometry(&mut self, document: &mut Document, layer: crate::model::LayerId, element: &omf_crate::Element) -> Result<()> {
        match &element.geometry {
            omf_crate::Geometry::PointSet(points) => {
                let offset = self.project_origin + DVec3::from_array(points.origin);
                for point in self.read_vertices(&points.vertices)? {
                    let pos = point + offset;
                    let color = ObjectColor::Fixed(element_color(element, [1.0; 4]));
                    document.add_object(|id| Object::Point { id, layer, pos, color });
                }
            }
            omf_crate::Geometry::LineSet(lines) => {
                let offset = self.project_origin + DVec3::from_array(lines.origin);
                let vertices = self.read_vertices(&lines.vertices)?.into_iter().map(|point| point + offset).collect::<Vec<_>>();
                let segments = collect_results(self.reader.array_segments(&lines.segments)?)?;
                for (points, closed) in line_strings(&vertices, &segments) {
                    let color = ObjectColor::Fixed(element_color(element, [1.0; 4]));
                    document.add_object(|id| Object::Polyline {
                        id,
                        layer,
                        verts: points.into_iter().map(PolyVertex::straight).collect(),
                        closed,
                        color,
                        fill: FillStyle::Clear,
                        line_weight: 1.0,
                    });
                }
            }
            _ => self
                .bundle
                .warnings
                .push(format!("Skipped unsupported '{}' geometry nested inside the Designs composite", element.name)),
        }
        Ok(())
    }

    fn read_generic_lines(&mut self, element: &omf_crate::Element, lines: &omf_crate::LineSet) -> Result<()> {
        let color = element_color(element, [1.0, 1.0, 1.0, 1.0]);
        let layer = self.generic_design.add_layer(element_name(element).to_owned(), None, color, true, 0.0);
        // Borrow splitting through a temporary avoids borrowing `self` and
        // `generic_design` mutably at the same time.
        let offset = self.project_origin + DVec3::from_array(lines.origin);
        let vertices = self.read_vertices(&lines.vertices)?.into_iter().map(|point| point + offset).collect::<Vec<_>>();
        let segments = collect_results(self.reader.array_segments(&lines.segments)?)?;
        for (points, closed) in line_strings(&vertices, &segments) {
            self.generic_design.add_object(|id| Object::Polyline {
                id,
                layer,
                verts: points.into_iter().map(PolyVertex::straight).collect(),
                closed,
                color: ObjectColor::ByLayer,
                fill: FillStyle::Clear,
                line_weight: 1.0,
            });
        }
        Ok(())
    }

    fn read_surface(&mut self, element: &omf_crate::Element, surface: &omf_crate::Surface) -> Result<()> {
        let offset = self.project_origin + DVec3::from_array(surface.origin);
        let vertices = self
            .read_vertices(&surface.vertices)?
            .into_iter()
            .map(|point| Vertex::new(point.x + offset.x, point.y + offset.y, point.z + offset.z))
            .collect();
        let faces = collect_results(self.reader.array_triangles(&surface.triangles)?)?;
        self.push_triangulation(element, vertices, faces)
    }

    fn read_grid_surface(&mut self, element: &omf_crate::Element, surface: &omf_crate::GridSurface) -> Result<()> {
        let [u_sizes, v_sizes] = self.grid2_sizes(&surface.grid)?;
        let u_edges = cumulative_edges(&u_sizes);
        let v_edges = cumulative_edges(&v_sizes);
        let width = u_edges.len();
        let height = v_edges.len();
        let vertex_count = width.checked_mul(height).context("OMF grid-surface vertex count overflows")?;
        ensure_items(vertex_count as u64, "grid-surface vertices")?;
        let heights = if let Some(values) = &surface.heights {
            let values = collect_results(self.reader.array_scalars(values)?)?;
            if values.len() != vertex_count {
                bail!("Grid surface '{}' has {} heights for {vertex_count} vertices", element.name, values.len());
            }
            values
        } else {
            vec![0.0; vertex_count]
        };
        let origin = self.project_origin + DVec3::from_array(surface.orient.origin);
        let u = DVec3::from_array(surface.orient.u);
        let v = DVec3::from_array(surface.orient.v);
        let normal = u.cross(v).normalize_or_zero();
        let mut vertices = Vec::with_capacity(vertex_count);
        for (v_index, v_offset) in v_edges.iter().copied().enumerate() {
            for (u_index, u_offset) in u_edges.iter().copied().enumerate() {
                let index = v_index * width + u_index;
                let point = origin + u * u_offset + v * v_offset + normal * heights[index];
                vertices.push(Vertex::new(point.x, point.y, point.z));
            }
        }
        let face_count = u_sizes
            .len()
            .checked_mul(v_sizes.len())
            .and_then(|cells| cells.checked_mul(2))
            .context("OMF grid-surface face count overflows")?;
        ensure_items(face_count as u64, "grid-surface faces")?;
        let mut faces = Vec::with_capacity(face_count);
        for row in 0..v_sizes.len() {
            for column in 0..u_sizes.len() {
                let a = (row * width + column) as u32;
                let b = a + 1;
                let d = ((row + 1) * width + column) as u32;
                let c = d + 1;
                faces.extend([[a, b, c], [a, c, d]]);
            }
        }
        self.push_triangulation(element, vertices, faces)
    }

    fn push_triangulation(&mut self, element: &omf_crate::Element, vertices: Vec<Vertex>, faces: Vec<[u32; 3]>) -> Result<()> {
        let mesh = Triangulation::from_vertices_and_faces(vertices, faces).with_context(|| format!("build OMF surface '{}'", element.name))?;
        let spatial = Arc::new(crate::model::spatial::TriangleBvh::build(&mesh));
        let edges = unique_edges(&mesh);
        let surface_face_order = Arc::new(morton_surface_face_order(&mesh));
        let style = element.metadata.get(META_STYLE);
        let color = style_value(style, "color").unwrap_or_else(|| element_color(element, [0.65, 0.68, 0.72, 1.0]));
        self.bundle.triangulations.push(ImportedTriangulation {
            preferred_id: element_id(element),
            source_name: element_source_name(element),
            source_format: element_source_format(element),
            loaded: LoadedTriangulation {
                name: element_name(element).to_owned(),
                path: virtual_path(self.source_name, element_name(element), "obj"),
                mesh: Arc::new(mesh),
                spatial,
                edges,
                surface_face_order,
            },
            visible: style_bool(style, "visible").unwrap_or(true),
            color,
            line_color: style_value(style, "line_color").unwrap_or([0.05, 0.08, 0.10, 1.0]),
            line_weight: style
                .and_then(|value| value.get("line_weight"))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or(Some(1.0)),
            raster_opacity: style_f32(style, "raster_opacity").unwrap_or(1.0),
            raster_texture_id: style_value(style, "raster_texture_id"),
        });
        Ok(())
    }

    fn read_point_cloud(&mut self, element: &omf_crate::Element, points: &omf_crate::PointSet) -> Result<()> {
        let offset = self.project_origin + DVec3::from_array(points.origin);
        let positions = self.read_vertices(&points.vertices)?.into_iter().map(|point| point + offset).collect::<Vec<_>>();
        if positions.is_empty() {
            self.bundle.warnings.push(format!("Skipped empty OMF point set '{}'", element.name));
            return Ok(());
        }
        let colors = element
            .attributes
            .iter()
            .find_map(|attribute| match (&attribute.location, &attribute.data) {
                (omf_crate::Location::Vertices, omf_crate::AttributeData::Color { values }) => Some(values),
                _ => None,
            })
            .map(|values| {
                collect_results(self.reader.array_colors(values)?)
                    .map(|colors| colors.into_iter().map(|color| color.unwrap_or([255; 4])).map(u32::from_le_bytes).collect::<Vec<_>>())
            })
            .transpose()?;
        let bounds = finite_bounds(&positions).with_context(|| format!("OMF point set '{}' contains no finite points", element.name))?;
        let prepared = prepare_for_render(&positions, colors.as_deref(), bounds);
        let style = element.metadata.get(META_STYLE);
        self.bundle.point_clouds.push(ImportedPointCloud {
            preferred_id: element_id(element),
            source_name: element_source_name(element),
            source_format: element_source_format(element),
            loaded: LoadedPointCloud {
                name: element_name(element).to_owned(),
                path: virtual_path(self.source_name, element_name(element), "pcd"),
                points: Arc::new(positions),
                colors: colors.map(Arc::new),
                prepared: Arc::new(prepared),
                bounds,
            },
            visible: style_bool(style, "visible").unwrap_or(true),
            color: style_value(style, "color").unwrap_or_else(|| element_color(element, [0.85, 0.87, 0.9, 1.0])),
            point_size: style_f32(style, "point_size").unwrap_or(0.1),
        });
        Ok(())
    }

    fn read_block_model(&mut self, element: &omf_crate::Element, geometry: &omf_crate::BlockModel) -> Result<()> {
        let sizes = self.grid3_sizes(&geometry.grid)?;
        let edges = sizes.each_ref().map(|axis| cumulative_edges(axis));
        let count = geometry.grid.count().map(|count| count as usize);
        let mut blocks = Vec::new();
        let mut parent_indices = Vec::new();
        let location = match &geometry.subblocks {
            None => {
                let total = count
                    .into_iter()
                    .try_fold(1usize, |total, count| total.checked_mul(count))
                    .context("OMF block count overflows")?;
                ensure_items(total as u64, "blocks")?;
                blocks.reserve(total);
                for k in 0..count[2] {
                    for j in 0..count[1] {
                        for i in 0..count[0] {
                            blocks.push(BlockBounds {
                                lower: DVec3::new(edges[0][i], edges[1][j], edges[2][k]),
                                upper: DVec3::new(edges[0][i + 1], edges[1][j + 1], edges[2][k + 1]),
                            });
                        }
                    }
                }
                omf_crate::Location::Primitives
            }
            Some(omf_crate::Subblocks::Freeform { subblocks }) => {
                ensure_items(subblocks.item_count(), "free-form sub-blocks")?;
                for (parent, corners) in collect_results(self.reader.array_freeform_subblocks(subblocks)?)? {
                    blocks.push(subblock_bounds(parent, corners, &edges)?);
                    parent_indices.push(block_parent_index(parent, count)?);
                }
                omf_crate::Location::Subblocks
            }
            Some(omf_crate::Subblocks::Regular { count: sub_count, subblocks, .. }) => {
                ensure_items(subblocks.item_count(), "regular sub-blocks")?;
                for (parent, corners) in collect_results(self.reader.array_regular_subblocks(subblocks)?)? {
                    let fractions: [f64; 3] = std::array::from_fn(|axis| f64::from(corners[axis]) / f64::from(sub_count[axis]));
                    let max_fractions: [f64; 3] = std::array::from_fn(|axis| f64::from(corners[axis + 3]) / f64::from(sub_count[axis]));
                    blocks.push(subblock_bounds(
                        parent,
                        [fractions[0], fractions[1], fractions[2], max_fractions[0], max_fractions[1], max_fractions[2]],
                        &edges,
                    )?);
                    parent_indices.push(block_parent_index(parent, count)?);
                }
                omf_crate::Location::Subblocks
            }
        };
        if blocks.is_empty() {
            self.bundle.warnings.push(format!("Skipped empty OMF block model '{}'", element.name));
            return Ok(());
        }
        let mut columns = Vec::new();
        for attribute in &element.attributes {
            let expansion = if location == omf_crate::Location::Subblocks && attribute.location == omf_crate::Location::Primitives {
                Some(parent_indices.as_slice())
            } else if attribute.location == location {
                None
            } else {
                continue;
            };
            if let Some(column) = self.read_block_column(attribute, expansion)? {
                columns.push(column);
            }
        }
        let upper = DVec3::new(*edges[0].last().unwrap_or(&0.0), *edges[1].last().unwrap_or(&0.0), *edges[2].last().unwrap_or(&0.0));
        let rotation = DMat3::from_cols(
            DVec3::from_array(geometry.orient.u),
            DVec3::from_array(geometry.orient.v),
            DVec3::from_array(geometry.orient.w),
        );
        let origin = self.project_origin + DVec3::from_array(geometry.orient.origin);
        let retained_geometry_bytes = blocks.len().checked_mul(std::mem::size_of::<BlockBounds>()).context("block geometry size overflows")?;
        let model = BlockModelData::from_columns(blocks.len(), DVec3::ZERO, upper, columns, retained_geometry_bytes)?.with_transform(origin, rotation)?;
        let blocks = Arc::new(BlockBoundsSource::Explicit(blocks));
        let renderable = Arc::new(RenderableBlockIndices::All(blocks.len()));
        let uniform_grid = blocks.uniform_grid();
        let opaque_surface_blocks = uniform_grid.as_ref().map_or_else(
            || opaque_irregular_surface_block_count(&blocks, &renderable),
            |grid| opaque_surface_block_count(&blocks, &renderable, grid),
        );
        let world_bounds = block_world_bounds(&model, &blocks, &renderable);
        let style = element.metadata.get(META_STYLE);
        let active_color_variable = style_value::<Option<String>>(style, "active_color_variable")
            .flatten()
            .filter(|name| model.variable(name).is_some())
            .or_else(|| model.color_variables().into_iter().find(|variable| !variable.special).map(|variable| variable.name.clone()));
        let active_values_cache = OpenBlockModel::prepare_active_values_cache(&model, &renderable, active_color_variable.as_deref());
        let color_transfers = self.read_block_color_transfers(element, &model, &renderable, style, active_color_variable.as_deref());
        self.bundle.block_models.push(ImportedBlockModel {
            preferred_id: element_id(element),
            source_name: element_source_name(element),
            source_format: element_source_format(element),
            loaded: LoadedBlockModel {
                name: element_name(element).to_owned(),
                source: crate::model::block_model::BlockModelSource {
                    path: virtual_path(self.source_name, element_name(element), "csv"),
                    csv_columns: None,
                },
                model,
                blocks,
                renderable_block_indices: renderable,
                uniform_grid,
                opaque_surface_blocks,
                world_bounds,
                active_color_variable,
                active_values_cache,
            },
            visible: style_bool(style, "visible").unwrap_or(true),
            color: style_value(style, "color").unwrap_or_else(|| element_color(element, [0.72, 0.72, 0.75, 1.0])),
            slice: style_value(style, "slice"),
            color_transfers,
            hide_empty_color_values: style_bool(style, "hide_empty_color_values").unwrap_or(true),
        });
        Ok(())
    }

    /// Colormaps for this element's variables, taken from the OMF attributes
    /// themselves.
    ///
    /// The attribute is the authority: a number attribute's `colormap` and a
    /// category attribute's `gradient` are where a colormap lives, so that is
    /// where Incline Design reads it from and writes it back to. `incline:style` is
    /// consulted only to migrate projects written before that was true, and is
    /// never written any more - one representation, so nothing can drift.
    ///
    /// A colormap that fails to decode is skipped with a warning rather than
    /// failing the import: colours are not worth losing a model over.
    fn read_block_color_transfers(
        &self,
        element: &omf_crate::Element,
        model: &BlockModelData,
        renderable: &RenderableBlockIndices,
        style: Option<&Value>,
        active_color_variable: Option<&str>,
    ) -> BTreeMap<String, ColorTransferFunction> {
        let variable_range = |name: &str| -> Option<(f64, f64)> {
            let variable = model.variable(name)?;
            let default = crate::model::block_model::color_variable_default(variable);
            let values = model.color_values(name).ok()?;
            crate::model::block_model::render_value_range(&values, renderable, default)
        };

        let mut transfers = BTreeMap::new();
        for attribute in &element.attributes {
            let Some(variable) = model.variable(&attribute.name) else {
                continue;
            };
            match &attribute.data {
                omf_crate::AttributeData::Number { colormap: Some(colormap), .. } => match read_number_colormap(self.reader, colormap) {
                    Ok(transfer) => {
                        transfers.insert(attribute.name.clone(), transfer);
                    }
                    Err(error) => crate::userspace_warn!(
                        "{}",
                        crate::i18n::tr_format!(
                            literal = "Ignoring the colour map on OMF attribute '%attribute%': %error%",
                            attribute = &attribute.name,
                            error = format!("{error:#}")
                        )
                    ),
                },
                // Category colours arrive on the column as `category_colors`
                // (see `read_block_column`) and are already merged into the
                // variable, so the colormap is built from there.
                omf_crate::AttributeData::Category { gradient: Some(_), .. } => {
                    transfers.insert(attribute.name.clone(), crate::model::block_model::categorical_color_transfer(variable));
                }
                _ => {}
            }
        }

        // Projects written before colormaps lived in the attribute.
        if let Some(stored) = style_value::<BTreeMap<String, StoredColorTransferFunction>>(style, "color_transfers") {
            for (name, transfer) in stored {
                let range = variable_range(&name);
                transfers.insert(name, transfer.resolve(range));
            }
        } else if let Some(stored) = style_value::<StoredColorTransferFunction>(style, "color_transfer") {
            // Older still: one ramp per model, belonging to whichever variable
            // was active.
            if let Some(name) = active_color_variable {
                let range = variable_range(name);
                transfers.insert(name.to_owned(), stored.resolve(range));
            }
        }
        for (name, transfer) in &mut transfers {
            transfer.sanitise(variable_range(name));
        }
        transfers
    }

    fn read_block_column(&self, attribute: &omf_crate::Attribute, expansion: Option<&[usize]>) -> Result<Option<BlockModelColumn>> {
        match &attribute.data {
            omf_crate::AttributeData::Number { values, .. } => {
                let values = read_numbers(self.reader, values)?.into_iter().map(|value| value.unwrap_or(f64::NAN)).collect::<Vec<_>>();
                let values = expand_block_values(values, expansion)?;
                Ok(Some(BlockModelColumn {
                    name: attribute.name.clone(),
                    values: Arc::new(values),
                    categories: None,
                    category_colors: BTreeMap::new(),
                }))
            }
            omf_crate::AttributeData::Category {
                values,
                names,
                gradient,
                attributes,
            } => {
                let names = collect_results(self.reader.array_names(names)?)?;
                let original_codes = attributes
                    .iter()
                    .find(|attribute| attribute.name == "Incline category code" && attribute.location == omf_crate::Location::Categories)
                    .and_then(|attribute| match &attribute.data {
                        omf_crate::AttributeData::Number { values, .. } => Some(values),
                        _ => None,
                    })
                    .map(|values| read_numbers(self.reader, values))
                    .transpose()?
                    .and_then(|values| {
                        (values.len() == names.len())
                            .then(|| {
                                values
                                    .into_iter()
                                    .map(|value| {
                                        value
                                            .filter(|value| value.is_finite() && value.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(value))
                                            .map(|value| value as u32)
                                    })
                                    .collect::<Option<Vec<_>>>()
                            })
                            .flatten()
                    });
                let codes = original_codes.unwrap_or_else(|| (0..names.len() as u32).collect());
                // The file's own palette, when it ships one, beats Incline Design's
                // generic categorical colours.
                let category_colors = gradient
                    .as_ref()
                    .map(|gradient| collect_results(self.reader.array_gradient(gradient)?))
                    .transpose()?
                    .map(|colors| codes.iter().copied().zip(colors).map(|(code, color)| (code, linear_rgba(color))).collect())
                    .unwrap_or_default();
                let categories = names.into_iter().zip(codes.iter().copied()).map(|(name, code)| (code, name)).collect();
                let values = collect_results(self.reader.array_indices(values)?)?
                    .into_iter()
                    .map(|value| value.and_then(|index| codes.get(index as usize).copied()).map_or(f64::NAN, f64::from))
                    .collect::<Vec<_>>();
                let values = expand_block_values(values, expansion)?;
                Ok(Some(BlockModelColumn {
                    name: attribute.name.clone(),
                    values: Arc::new(values),
                    categories: Some(categories),
                    category_colors,
                }))
            }
            _ => Ok(None),
        }
    }

    fn read_drill_dataset(&mut self, element: &omf_crate::Element) -> Result<Option<ImportedDrillHoles>> {
        let omf_crate::Geometry::Composite(composite) = &element.geometry else {
            return Ok(None);
        };
        let mut holes = Vec::new();
        for child in &composite.elements {
            self.record_unsupported_content(child);
            if let Some(value) = child.metadata.get(META_DRILL_HOLE).cloned()
                && let Ok(mut hole) = serde_json::from_value::<DrillHole>(value)
            {
                hole.collar += self.project_origin;
                for station in &mut hole.trace {
                    station.position += self.project_origin;
                }
                holes.push(hole);
                continue;
            }
            if let Some(hole) = self.read_generic_drill_hole(child)? {
                holes.push(hole);
            }
        }
        if holes.is_empty() {
            return Ok(None);
        }
        let mut dataset = DrillHoleDataset::new(holes);
        // Resolved after construction: it is `new` that fixes the hole order
        // the stored names are looked up against.
        if let Some(value) = element.metadata.get(META_TIE_INS).cloned()
            && let Ok(stored) = serde_json::from_value::<crate::model::drill_hole::StoredTieIns>(value)
        {
            let dropped = dataset.apply_stored_ties(stored);
            if dropped > 0 {
                self.bundle
                    .warnings
                    .push(format!("Element '{}' has {dropped} tie-in(s) naming holes it no longer contains", element.name));
            }
        }
        let dataset = Arc::new(dataset);
        let path = virtual_path(self.source_name, element_name(element), "omf");
        Ok(Some(ImportedDrillHoles {
            preferred_id: element_id(element),
            source_name: element_source_name(element),
            source_format: element_source_format(element),
            loaded: LoadedDrillHoleDataset {
                name: element_name(element).to_owned(),
                source: DrillHoleSource::Omf {
                    name: element_name(element).to_owned(),
                    path,
                },
                dataset,
            },
            visible: style_bool(element.metadata.get(META_STYLE), "visible").unwrap_or(true),
            color: style_value(element.metadata.get(META_STYLE), "color").unwrap_or_default(),
        }))
    }

    fn read_generic_drill_hole(&self, element: &omf_crate::Element) -> Result<Option<DrillHole>> {
        let omf_crate::Geometry::LineSet(lines) = &element.geometry else {
            return Ok(None);
        };
        let offset = self.project_origin + DVec3::from_array(lines.origin);
        let vertices = self.read_vertices(&lines.vertices)?.into_iter().map(|point| point + offset).collect::<Vec<_>>();
        if vertices.len() < 2 {
            return Ok(None);
        }
        let depths = element
            .attributes
            .iter()
            .find(|attribute| attribute.location == omf_crate::Location::Vertices && attribute.name.eq_ignore_ascii_case("measured depth"))
            .and_then(|attribute| match &attribute.data {
                omf_crate::AttributeData::Number { values, .. } => Some(values),
                _ => None,
            })
            .map(|values| read_numbers(self.reader, values))
            .transpose()?
            .map(|values| values.into_iter().enumerate().map(|(index, value)| value.unwrap_or(index as f64)).collect::<Vec<_>>())
            .unwrap_or_else(|| (0..vertices.len()).map(|index| index as f64).collect());
        if depths.len() != vertices.len() {
            return Ok(None);
        }
        Ok(Some(DrillHole {
            dhid: element_name(element).to_owned(),
            collar: vertices[0],
            diameter: None,
            trace: depths
                .into_iter()
                .zip(vertices)
                .map(|(depth, position)| crate::model::drill_hole::TraceStation { depth, position })
                .collect(),
            render_ranges: Vec::new(),
            intervals: Vec::new(),
        }))
    }

    fn read_textures(&mut self, element: &omf_crate::Element) -> Result<()> {
        for attribute in &element.attributes {
            let (image, derived_world_to_uv) = match &attribute.data {
                omf_crate::AttributeData::ProjectedTexture { image, orient, width, height } => {
                    let origin = self.project_origin + DVec3::from_array(orient.origin);
                    let u = DVec3::from_array(orient.u);
                    let v = DVec3::from_array(orient.v);
                    (
                        image,
                        Some([u.x / *width, u.y / *width, -u.dot(origin) / *width, v.x / *height, v.y / *height, -v.dot(origin) / *height]),
                    )
                }
                omf_crate::AttributeData::MappedTexture { image, texcoords } => {
                    let omf_crate::Geometry::Surface(surface) = &element.geometry else {
                        self.bundle.warnings.push(format!(
                            "Skipped mapped texture '{}' on '{}': only surface textures can become Incline Design rasters",
                            attribute.name, element.name
                        ));
                        continue;
                    };
                    let offset = self.project_origin + DVec3::from_array(surface.origin);
                    let vertices = self.read_vertices(&surface.vertices)?.into_iter().map(|point| point + offset).collect::<Vec<_>>();
                    ensure_items(texcoords.item_count(), "texture coordinates")?;
                    let texcoords = collect_results(self.reader.array_texcoords(texcoords)?)?;
                    (image, affine_from_texcoords(&vertices, &texcoords))
                }
                _ => continue,
            };
            let decoded = self.reader.image(image).with_context(|| format!("decode texture '{}'", attribute.name))?.to_rgba8();
            let size = [decoded.width(), decoded.height()];
            let style = element.metadata.get(META_STYLE);
            let world_to_uv = style
                .and_then(|style| style.get("world_to_uv"))
                .and_then(|value| serde_json::from_value::<[f64; 6]>(value.clone()).ok())
                .map(|[a, b, c, d, e, f]| {
                    [
                        a,
                        b,
                        c - a * self.project_origin.x - b * self.project_origin.y,
                        d,
                        e,
                        f - d * self.project_origin.x - e * self.project_origin.y,
                    ]
                })
                .or(derived_world_to_uv);
            let Some(world_to_uv) = world_to_uv else {
                self.bundle.warnings.push(format!(
                    "Skipped texture '{}' on '{}': its UV mapping is not affine in world XY",
                    attribute.name, element.name
                ));
                continue;
            };
            let declared_source_size = style
                .and_then(|style| style.get("source_size"))
                .and_then(|value| serde_json::from_value::<[u32; 2]>(value.clone()).ok())
                .unwrap_or(size);
            if declared_source_size != size {
                self.bundle.warnings.push(format!(
                    "Raster '{}' declared source size {} × {}, but its embedded full-resolution image is {} × {}; the image dimensions were used",
                    element_name(element),
                    declared_source_size[0],
                    declared_source_size[1],
                    size[0],
                    size[1]
                ));
            }
            let preview_size = style
                .and_then(|style| style.get("preview_size"))
                .and_then(|value| serde_json::from_value::<[u32; 2]>(value.clone()).ok())
                .filter(|[width, height]| *width > 0 && *height > 0 && *width <= size[0] && *height <= size[1])
                .unwrap_or(size);
            let projection = style
                .and_then(|style| style.get("projection"))
                .and_then(Value::as_str)
                .unwrap_or(&self.project_crs)
                .to_owned();
            let full_rgba = Arc::new(decoded.into_raw());
            let rgba = if preview_size == size {
                Arc::clone(&full_rgba)
            } else {
                Arc::new(crate::model::raster::downscale_rgba(&full_rgba, size, preview_size)?)
            };
            self.bundle.rasters.push(ImportedRaster {
                preferred_id: element_id(element),
                source_name: element_source_name(element),
                source_format: element_source_format(element),
                visible: style_bool(style, "visible").unwrap_or(true),
                loaded: LoadedRasterTexture {
                    name: if attribute.name == "Raster" {
                        element_name(element).to_owned()
                    } else {
                        format!("{} – {}", element_name(element), attribute.name)
                    },
                    path: virtual_path(self.source_name, element_name(element), "tif"),
                    source_size: size,
                    preview_size,
                    full_rgba,
                    rgba,
                    world_to_uv,
                    projection,
                    driver_name: tr!(literal = "OMF texture"),
                },
            });
        }
        Ok(())
    }

    fn read_vertices(&self, array: &omf_crate::Array<omf_crate::array_type::Vertex>) -> Result<Vec<DVec3>> {
        ensure_items(array.item_count(), "vertices")?;
        Ok(collect_results(self.reader.array_vertices(array)?)?.into_iter().map(DVec3::from_array).collect())
    }

    fn grid2_sizes(&self, grid: &omf_crate::Grid2) -> Result<[Vec<f64>; 2]> {
        match grid {
            omf_crate::Grid2::Regular { size, count } => Ok([vec![size[0]; count[0] as usize], vec![size[1]; count[1] as usize]]),
            omf_crate::Grid2::Tensor { u, v } => Ok([collect_results(self.reader.array_scalars(u)?)?, collect_results(self.reader.array_scalars(v)?)?]),
        }
    }

    fn grid3_sizes(&self, grid: &omf_crate::Grid3) -> Result<[Vec<f64>; 3]> {
        match grid {
            omf_crate::Grid3::Regular { size, count } => Ok([vec![size[0]; count[0] as usize], vec![size[1]; count[1] as usize], vec![size[2]; count[2] as usize]]),
            omf_crate::Grid3::Tensor { u, v, w } => Ok([
                collect_results(self.reader.array_scalars(u)?)?,
                collect_results(self.reader.array_scalars(v)?)?,
                collect_results(self.reader.array_scalars(w)?)?,
            ]),
        }
    }
}

fn affine_from_texcoords(vertices: &[DVec3], texcoords: &[[f64; 2]]) -> Option<[f64; 6]> {
    let count = vertices.len().min(texcoords.len());
    for first in 0..count {
        for second in first + 1..count {
            for third in second + 1..count {
                let p0 = vertices[first];
                let p1 = vertices[second];
                let p2 = vertices[third];
                let t0 = texcoords[first];
                let t1 = texcoords[second];
                let t2 = texcoords[third];
                let dx1 = p1.x - p0.x;
                let dy1 = p1.y - p0.y;
                let dx2 = p2.x - p0.x;
                let dy2 = p2.y - p0.y;
                let determinant = dx1 * dy2 - dx2 * dy1;
                if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
                    continue;
                }
                let solve = |v0: f64, v1: f64, v2: f64| {
                    let dv1 = v1 - v0;
                    let dv2 = v2 - v0;
                    let x = (dv1 * dy2 - dv2 * dy1) / determinant;
                    let y = (-dv1 * dx2 + dv2 * dx1) / determinant;
                    [x, y, v0 - x * p0.x - y * p0.y]
                };
                let [a, b, c] = solve(t0[0], t1[0], t2[0]);
                let [d, e, f] = solve(t0[1], t1[1], t2[1]);
                let transform = [a, b, c, d, e, f];
                if transform.iter().all(|value| value.is_finite()) {
                    return Some(transform);
                }
            }
        }
    }
    None
}

fn ensure_items(count: u64, description: &str) -> Result<usize> {
    if count > MAX_ARRAY_ITEMS {
        bail!("OMF {description} count {count} exceeds Incline Design's import limit of {MAX_ARRAY_ITEMS}");
    }
    usize::try_from(count).with_context(|| format!("OMF {description} count exceeds addressable memory"))
}

fn collect_results<T>(items: impl IntoIterator<Item = std::result::Result<T, omf_crate::error::Error>>) -> Result<Vec<T>> {
    items.into_iter().collect::<std::result::Result<Vec<_>, _>>().map_err(anyhow::Error::new)
}

fn read_numbers<R: omf_crate::file::ReadAt>(reader: &omf_crate::file::Reader<R>, array: &omf_crate::Array<omf_crate::array_type::Number>) -> Result<Vec<Option<f64>>> {
    ensure_items(array.item_count(), "number values")?;
    let numbers = reader.array_numbers(array)?;
    if let Ok(numbers) = numbers.try_into_f64() {
        return collect_results(numbers);
    }
    let integers = reader.array_numbers(array)?.try_into_i64()?;
    Ok(collect_results(integers)?.into_iter().map(|value| value.map(|value| value as f64)).collect())
}

fn cumulative_edges(sizes: &[f64]) -> Vec<f64> {
    let mut edges = Vec::with_capacity(sizes.len() + 1);
    edges.push(0.0);
    for size in sizes {
        edges.push(edges.last().copied().unwrap_or(0.0) + size);
    }
    edges
}

fn block_parent_index(parent: [u32; 3], count: [usize; 3]) -> Result<usize> {
    let [i, j, k] = parent.map(|value| value as usize);
    if i >= count[0] || j >= count[1] || k >= count[2] {
        bail!("OMF sub-block parent {parent:?} is outside grid {count:?}");
    }
    k.checked_mul(count[1])
        .and_then(|value| value.checked_add(j))
        .and_then(|value| value.checked_mul(count[0]))
        .and_then(|value| value.checked_add(i))
        .context("OMF block parent index overflows")
}

fn expand_block_values(values: Vec<f64>, expansion: Option<&[usize]>) -> Result<Vec<f64>> {
    let Some(expansion) = expansion else {
        return Ok(values);
    };
    expansion
        .iter()
        .map(|&parent| values.get(parent).copied().with_context(|| format!("OMF parent-block attribute is missing value {parent}")))
        .collect()
}

fn subblock_bounds(parent: [u32; 3], corners: [f64; 6], edges: &[Vec<f64>; 3]) -> Result<BlockBounds> {
    let mut lower = [0.0; 3];
    let mut upper = [0.0; 3];
    for axis in 0..3 {
        let index = parent[axis] as usize;
        let parent_lower = *edges[axis].get(index).context("OMF sub-block parent is out of range")?;
        let parent_upper = *edges[axis].get(index + 1).context("OMF sub-block parent is out of range")?;
        let size = parent_upper - parent_lower;
        lower[axis] = parent_lower + size * corners[axis];
        upper[axis] = parent_lower + size * corners[axis + 3];
    }
    Ok(BlockBounds {
        lower: DVec3::from_array(lower),
        upper: DVec3::from_array(upper),
    })
}

fn block_world_bounds(model: &BlockModelData, blocks: &BlockBoundsSource, renderable: &RenderableBlockIndices) -> Option<(DVec3, DVec3)> {
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for index in renderable.iter() {
        let block = blocks.get(index)?;
        for x in [block.lower.x, block.upper.x] {
            for y in [block.lower.y, block.upper.y] {
                for z in [block.lower.z, block.upper.z] {
                    let point = model.local_to_world(DVec3::new(x, y, z));
                    min = min.min(point);
                    max = max.max(point);
                }
            }
        }
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

fn line_strings(vertices: &[DVec3], segments: &[[u32; 2]]) -> Vec<(Vec<DVec3>, bool)> {
    if vertices.len() >= 2 {
        let open = segments.len() == vertices.len() - 1 && segments.iter().enumerate().all(|(index, segment)| *segment == [index as u32, index as u32 + 1]);
        let closed = segments.len() == vertices.len()
            && segments[..segments.len() - 1]
                .iter()
                .enumerate()
                .all(|(index, segment)| *segment == [index as u32, index as u32 + 1])
            && segments.last() == Some(&[(vertices.len() - 1) as u32, 0]);
        if open || closed {
            return vec![(vertices.to_vec(), closed)];
        }
    }
    segments
        .iter()
        .filter_map(|[a, b]| Some((vec![*vertices.get(*a as usize)?, *vertices.get(*b as usize)?], false)))
        .collect()
}

fn style_bool(style: Option<&Value>, key: &str) -> Option<bool> {
    style?.get(key)?.as_bool()
}

fn style_f32(style: Option<&Value>, key: &str) -> Option<f32> {
    style?.get(key)?.as_f64().map(|value| value as f32)
}

fn style_value<T: serde::de::DeserializeOwned>(style: Option<&Value>, key: &str) -> Option<T> {
    serde_json::from_value(style?.get(key)?.clone()).ok()
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| tr!(literal = "OMF import"))
}

fn safe_component(name: &str) -> String {
    let component = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let component = component.trim().trim_matches('.');
    if component.is_empty() { "element".to_owned() } else { component.to_owned() }
}

fn virtual_path(source: &str, element: &str, extension: &str) -> PathBuf {
    PathBuf::from("omf")
        .join(safe_component(&file_stem(source)))
        .join(format!("{}.{}", safe_component(element), extension))
}
