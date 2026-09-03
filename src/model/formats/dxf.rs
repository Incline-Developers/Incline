//! DXF import and design export for the editable [`Document`] model.
//!
//! The DXF is flattened and simplified for open-pit design: block/INSERT
//! instances are baked into world space, ByLayer/ByBlock colour is resolved,
//! dimensions are dropped, and curves become bulge-aware polylines (arcs/circles
//! preserved; ellipses/splines tessellated). The renderer then reads only the
//! `Document`.

use std::{collections::HashMap, io::Cursor};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write, path::Path};

use anyhow::{Context, Result};
use dxf::{
    Block, Color, Drawing, LwPolylineVertex, Point,
    entities::{Entity, EntityType, LwPolyline, ModelPoint, Polyline as DxfPolyline, Text as DxfText, Vertex as DxfVertex},
    enums::AcadVersion,
};
use glam::{DMat4, DQuat, DVec3, DVec4};

use crate::{
    model::{
        Document, LayerId, Object, ObjectColor, PolyVertex,
        geometry::{Transform, normalize_or_z, point_to_dvec3, vector_to_dvec3},
        project::ProjectFile,
    },
    rendering::color::{COLOR_TABLE, aci_to_linear_rgba, linear_to_srgb_byte},
    userspace_warn,
};

const MAX_BLOCK_DEPTH: usize = 32;
/// Target world-space segment length when tessellating non-bulge curves.
const TESSELLATION_SEGMENT_LEN: f64 = 0.15;
const MIN_TESSELLATION_SEGMENTS: usize = 32;
const MAX_TESSELLATION_SEGMENTS: usize = 4096;
/// Total INSERT/MINSERT instances one import may expand. Nested blocks and
/// large row/column grids multiply, so a small file could otherwise expand
/// into billions of entities.
const MAX_EXPANDED_INSERT_INSTANCES: usize = 65_536;
/// Total polyline vertices one import may generate before further geometry
/// is dropped with a warning.
const MAX_GENERATED_VERTICES: usize = 50_000_000;

/// Build a `Document` from a loaded DXF drawing.
pub(crate) fn from_dxf(drawing: &Drawing) -> Document {
    let mut doc = Document::new();

    let mut layer_ids: HashMap<String, LayerId> = HashMap::new();
    for layer in drawing.layers() {
        let aci = layer.color.index();
        let rgba = aci.and_then(|index| aci_to_linear_rgba(index as i32)).unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let id = doc.add_layer(layer.name.clone(), aci, rgba, layer.is_layer_on, 0.0);
        layer_ids.insert(layer.name.clone(), id);
    }
    // Guarantee at least one layer to place geometry on.
    doc.ensure_default_layer();

    let blocks: HashMap<&str, &Block> = drawing.blocks().map(|block| (block.name.as_str(), block)).collect();

    let mut ctx = ImportCtx {
        doc: &mut doc,
        layer_ids,
        blocks: &blocks,
        stack: Vec::new(),
        unknown_layer_ids: HashMap::new(),
        next_unknown_layer_index: 0,
        expanded_insert_instances: 0,
        generated_vertices: 0,
        budget_warning_emitted: false,
    };
    let transform = Transform::identity();
    let scope = BlockScope::world();
    import_entities(drawing.entities(), &mut ctx, &transform, scope);

    // The DXF spec mandates a default "0" layer and `Drawing::new()` always
    // emits one, so an exported DXF carrying a single real layer still
    // includes an empty "0". Drop it on import when it has no geometry,
    // unless it is the only layer (the document must keep at least one).
    if doc.layers().len() > 1
        && let Some(zero_id) = doc.layer_id_by_name("0")
        && !doc.objects().iter().any(|object| object.layer() == zero_id)
    {
        doc.delete_layer(zero_id);
    }

    doc.repair_degenerate_closed_polylines();
    doc
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_document(path: impl AsRef<Path>) -> Result<Document> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    document_from_bytes(path.to_string_lossy().as_ref(), &bytes)
}

pub(crate) fn document_from_bytes(source_name: &str, bytes: &[u8]) -> Result<Document> {
    let mut cursor = Cursor::new(bytes);
    let drawing = Drawing::load(&mut cursor).with_context(|| format!("parse {source_name}"))?;
    let document = from_dxf(&drawing);
    document.validate().with_context(|| format!("validate imported DXF {source_name}"))?;
    Ok(document)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn export_project(project: &ProjectFile, path: impl AsRef<Path>) -> Result<()> {
    export_layers(project, None, path)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn export_layer(project: &ProjectFile, layer: LayerId, path: impl AsRef<Path>) -> Result<()> {
    export_layers(project, Some(layer), path)
}

#[cfg(not(target_arch = "wasm32"))]
fn export_layers(project: &ProjectFile, only_layer: Option<LayerId>, path: impl AsRef<Path>) -> Result<()> {
    let drawing = export_drawing(project, only_layer);
    crate::model::atomic_file::write_atomic(path.as_ref(), |file| {
        let mut writer = std::io::BufWriter::new(file);
        drawing.save(&mut writer)?;
        Write::flush(&mut writer)?;
        Ok(())
    })
    .with_context(|| format!("write {}", path.as_ref().display()))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn export_bytes(project: &ProjectFile, only_layer: Option<LayerId>) -> Result<Vec<u8>> {
    let drawing = export_drawing(project, only_layer);
    let mut bytes = Vec::new();
    drawing.save(&mut bytes)?;
    Ok(bytes)
}

fn export_drawing(project: &ProjectFile, only_layer: Option<LayerId>) -> Drawing {
    let mut drawing = Drawing::new();
    drawing.header.version = AcadVersion::R2004;
    for layer in project.document.layers() {
        if only_layer.is_some_and(|id| id != layer.id) {
            continue;
        }
        drawing.add_layer(dxf::tables::Layer {
            name: layer.name.clone(),
            color: Color::from_index(layer.color_index.unwrap_or(7).clamp(1, 255)),
            is_layer_on: layer.visible,
            ..Default::default()
        });
    }

    for object in project.document.objects() {
        if only_layer.is_some_and(|id| id != object.layer()) {
            continue;
        }
        add_object_to_drawing(&project.document, object, &mut drawing);
    }
    drawing
}

fn add_object_to_drawing(document: &Document, object: &Object, drawing: &mut Drawing) {
    let Some(layer) = document.layer(object.layer()) else {
        return;
    };
    let layer_name = layer.name.clone();
    let object_color = object.color();

    match object {
        Object::Point { pos, .. } => {
            let mut entity = Entity::new(EntityType::ModelPoint(ModelPoint {
                location: point_from_vec3(*pos),
                ..Default::default()
            }));
            entity.common.layer = layer_name;
            apply_object_color(&mut entity.common, object_color);
            drawing.add_entity(entity);
        }
        Object::Polyline { verts, closed, .. } => {
            if verts.len() < 2 {
                return;
            }
            let z0 = verts[0].pos.z;
            let is_flat = verts.iter().all(|v| (v.pos.z - z0).abs() < 1e-9);
            if is_flat {
                let mut poly = LwPolyline {
                    vertices: verts.iter().map(lw_vertex_from_poly_vertex).collect(),
                    ..Default::default()
                };
                poly.set_is_closed(*closed);
                let mut entity = Entity::new(EntityType::LwPolyline(poly));
                entity.common.layer = layer_name;
                apply_object_color(&mut entity.common, object_color);
                entity.common.elevation = z0;
                drawing.add_entity(entity);
            } else {
                let mut poly = DxfPolyline::default();
                poly.set_is_3d_polyline(true);
                poly.set_is_closed(*closed);
                let positions: Vec<_> = if verts.iter().any(|vertex| vertex.bulge.abs() > f64::EPSILON) {
                    crate::model::geometry::tessellate_polyline_bulges(verts, *closed)
                } else {
                    verts.iter().map(|vertex| vertex.pos).collect()
                };
                for pos in positions {
                    let mut vertex = DxfVertex {
                        location: Point::new(pos.x, pos.y, pos.z),
                        bulge: 0.0,
                        ..Default::default()
                    };
                    vertex.set_is_3d_polyline_vertex(true);
                    poly.add_vertex(drawing, vertex);
                }
                let mut entity = Entity::new(EntityType::Polyline(poly));
                entity.common.layer = layer_name;
                apply_object_color(&mut entity.common, object_color);
                drawing.add_entity(entity);
            }
        }
        Object::Text {
            pos, content, height, rotation, ..
        } => {
            let location = crate::model::geometry::text_anchor_top_to_bottom(*pos, *height, *rotation);
            let mut entity = Entity::new(EntityType::Text(DxfText {
                location: point_from_vec3(location),
                text_height: *height,
                value: content.clone(),
                rotation: rotation.to_degrees(),
                ..Default::default()
            }));
            entity.common.layer = layer_name;
            apply_object_color(&mut entity.common, object_color);
            drawing.add_entity(entity);
        }
    }
}

fn lw_vertex_from_poly_vertex(vertex: &PolyVertex) -> LwPolylineVertex {
    LwPolylineVertex {
        x: vertex.pos.x,
        y: vertex.pos.y,
        bulge: vertex.bulge,
        ..Default::default()
    }
}

fn point_from_vec3(pos: glam::DVec3) -> Point {
    Point::new(pos.x, pos.y, pos.z)
}

fn apply_object_color(common: &mut dxf::entities::EntityCommon, color: ObjectColor) {
    match color {
        ObjectColor::ByLayer => {
            common.color = Color::by_layer();
            common.color_24_bit = 0;
            common.transparency = 0;
        }
        ObjectColor::Fixed(rgba) => {
            common.color = Color::from_index(nearest_aci(rgba));
            let red = u32::from(linear_to_srgb_byte(rgba[0]));
            let green = u32::from(linear_to_srgb_byte(rgba[1]));
            let blue = u32::from(linear_to_srgb_byte(rgba[2]));
            common.color_24_bit = ((red << 16) | (green << 8) | blue) as i32;
            let alpha = (rgba[3].clamp(0.0, 1.0) * 255.0).round() as i32;
            common.transparency = 0x0200_0000 | alpha;
        }
    }
}

fn nearest_aci(rgba: [f32; 4]) -> u8 {
    let target = [
        linear_to_srgb_byte(rgba[0]) as i32,
        linear_to_srgb_byte(rgba[1]) as i32,
        linear_to_srgb_byte(rgba[2]) as i32,
    ];
    (1..=255)
        .min_by_key(|index| {
            let rgb = COLOR_TABLE[*index as usize];
            let dr = target[0] - i32::from(rgb[0]);
            let dg = target[1] - i32::from(rgb[1]);
            let db = target[2] - i32::from(rgb[2]);
            dr * dr + dg * dg + db * db
        })
        .unwrap_or(7)
}

struct ImportCtx<'a> {
    doc: &'a mut Document,
    layer_ids: HashMap<String, LayerId>,
    blocks: &'a HashMap<&'a str, &'a Block>,
    stack: Vec<String>,
    unknown_layer_ids: HashMap<String, LayerId>,
    next_unknown_layer_index: usize,
    expanded_insert_instances: usize,
    generated_vertices: usize,
    budget_warning_emitted: bool,
}

/// Inherited colour/layer while rendering the contents of a block.
#[derive(Clone, Copy, Default)]
struct BlockScope<'a> {
    color: Option<u8>,
    layer: Option<&'a str>,
}

impl<'a> BlockScope<'a> {
    fn world() -> Self {
        Self::default()
    }
}

impl ImportCtx<'_> {
    /// Reserve `amount` against a running budget. Returns false (and warns
    /// once) when the budget would be exceeded; the caller must then skip the
    /// geometry instead of expanding it.
    fn reserve_budget(counter: &mut usize, limit: usize, amount: usize, warned: &mut bool, what: &str) -> bool {
        let requested = counter.saturating_add(amount);
        if requested > limit {
            if !*warned {
                *warned = true;
                userspace_warn!(
                    "{}",
                    crate::i18n::tr_format!(
                        literal = "DXF import exceeds the %what% budget (%limit%); remaining geometry is skipped",
                        what = what,
                        limit = limit
                    )
                );
            }
            return false;
        }
        *counter = requested;
        true
    }

    fn reserve_insert_instances(&mut self, amount: usize) -> bool {
        Self::reserve_budget(
            &mut self.expanded_insert_instances,
            MAX_EXPANDED_INSERT_INSTANCES,
            amount,
            &mut self.budget_warning_emitted,
            "block-instance",
        )
    }

    fn reserve_vertices(&mut self, amount: usize) -> bool {
        Self::reserve_budget(
            &mut self.generated_vertices,
            MAX_GENERATED_VERTICES,
            amount,
            &mut self.budget_warning_emitted,
            "generated-vertex",
        )
    }

    fn layer_id(&mut self, name: &str) -> LayerId {
        if let Some(id) = self.layer_ids.get(name).or_else(|| self.unknown_layer_ids.get(name)) {
            return *id;
        }

        let fallback_name = loop {
            let candidate = format!("unknown_{}", self.next_unknown_layer_index);
            self.next_unknown_layer_index += 1;
            if self.doc.layer_id_by_name(&candidate).is_none() {
                break candidate;
            }
        };
        let id = self.doc.add_layer(fallback_name.clone(), None, [1.0, 1.0, 1.0, 1.0], true, 0.0);
        self.unknown_layer_ids.insert(name.to_owned(), id);
        userspace_warn!(
            "{}",
            crate::i18n::tr_format!(
                literal = "DXF entity referenced undefined layer '%name%', imported as '%fallback%'",
                name = name,
                fallback = fallback_name
            )
        );
        id
    }
}

fn resolve_color(entity: &Entity, block_color: Option<u8>, layer_color: Option<[f32; 4]>) -> ObjectColor {
    let explicit_alpha = dxf_entity_alpha(entity.common.transparency);
    if entity.common.color_24_bit != 0 {
        let rgb = entity.common.color_24_bit as u32;
        let mut rgba = crate::rendering::color::rgb_bytes_to_linear_rgba([((rgb >> 16) & 0xff) as u8, ((rgb >> 8) & 0xff) as u8, (rgb & 0xff) as u8]);
        if let Some(alpha) = explicit_alpha {
            rgba[3] = alpha;
        }
        return ObjectColor::Fixed(rgba);
    }
    let color = &entity.common.color;
    let aci = if color.is_by_layer() {
        if let Some(alpha) = explicit_alpha
            && let Some(mut rgba) = layer_color
        {
            rgba[3] = alpha;
            return ObjectColor::Fixed(rgba);
        }
        return ObjectColor::ByLayer;
    } else if color.is_by_block() {
        block_color
    } else {
        color.index()
    };
    match aci.and_then(|index| aci_to_linear_rgba(index as i32)) {
        Some(mut rgba) => {
            if let Some(alpha) = explicit_alpha {
                rgba[3] = alpha;
            }
            ObjectColor::Fixed(rgba)
        }
        None => ObjectColor::ByLayer,
    }
}

fn dxf_entity_alpha(raw: i32) -> Option<f32> {
    let raw = raw as u32;
    // 0 means no explicit transparency. Method byte 0x02 carries a direct
    // alpha value in the low byte (0 transparent, 255 opaque).
    ((raw & 0xff00_0000) == 0x0200_0000).then(|| (raw & 0xff) as f32 / 255.0)
}

fn import_entities<'e>(entities: impl IntoIterator<Item = &'e Entity>, ctx: &mut ImportCtx<'_>, transform: &Transform, scope: BlockScope<'_>) {
    for entity in entities {
        if !entity.common.is_visible {
            continue;
        }
        let effective_layer = if entity.common.layer == "0" {
            scope.layer.unwrap_or("0")
        } else {
            entity.common.layer.as_str()
        };
        let layer = ctx.layer_id(effective_layer);
        let color = resolve_color(entity, scope.color, ctx.doc.layer(layer).map(|layer| layer.color));

        match &entity.specific {
            EntityType::Line(line) => {
                let verts = vec![
                    PolyVertex::straight(transform.apply(point_to_dvec3(&line.p1))),
                    PolyVertex::straight(transform.apply(point_to_dvec3(&line.p2))),
                ];
                push_polyline(ctx, layer, verts, false, color);
            }
            EntityType::LwPolyline(poly) => {
                let elevation = entity.common.elevation;
                let mut entity_transform = transform.clone();
                entity_transform.push_with_affine(DVec3::ZERO, DVec3::ZERO, 0.0, DVec3::ONE, ocs_to_wcs_affine(&poly.extrusion_direction));
                let verts: Vec<PolyVertex> = poly
                    .vertices
                    .iter()
                    .map(|v| PolyVertex {
                        pos: DVec3::new(v.x, v.y, elevation),
                        bulge: v.bulge,
                    })
                    .collect();
                if verts.len() >= 2 {
                    push_bulge_polyline(ctx, layer, verts, poly.is_closed(), DVec3::Z, &entity_transform, color);
                }
            }
            EntityType::Arc(arc) => {
                let mut entity_transform = transform.clone();
                entity_transform.push_with_affine(DVec3::ZERO, DVec3::ZERO, 0.0, DVec3::ONE, ocs_to_wcs_affine(&arc.normal));
                let verts = arc_to_verts(
                    point_to_dvec3(&arc.center),
                    arc.radius,
                    DVec3::Z,
                    arc.start_angle.to_radians(),
                    arc.end_angle.to_radians(),
                    &entity_transform,
                );
                push_polyline(ctx, layer, verts, false, color);
            }
            EntityType::Circle(circle) => {
                let mut entity_transform = transform.clone();
                entity_transform.push_with_affine(DVec3::ZERO, DVec3::ZERO, 0.0, DVec3::ONE, ocs_to_wcs_affine(&circle.normal));
                let (verts, closed) = circle_to_verts(point_to_dvec3(&circle.center), circle.radius, DVec3::Z, &entity_transform);
                push_polyline(ctx, layer, verts, closed, color);
            }
            EntityType::Ellipse(ell) => {
                let closed = ellipse_is_full_loop(ell.start_parameter, ell.end_parameter);
                let mut verts = ellipse_to_verts(ell, transform);
                // For a full (closed) ellipse, ellipse_to_verts generates segments+1 points
                // where the first and last coincide - drop the duplicate closing vertex.
                if closed && verts.len() > 1 {
                    verts.pop();
                }
                if verts.len() >= 2 {
                    push_polyline(ctx, layer, verts, closed, color);
                }
            }
            EntityType::Spline(spline) => {
                let verts = spline_to_verts(spline, transform);
                if verts.len() >= 2 {
                    push_polyline(ctx, layer, verts, spline.is_closed(), color);
                }
            }
            EntityType::Solid(solid) => {
                // Outline as a closed quad (DXF solid corner order is 1,2,4,3).
                let verts = vec![
                    PolyVertex::straight(transform.apply(point_to_dvec3(&solid.first_corner))),
                    PolyVertex::straight(transform.apply(point_to_dvec3(&solid.second_corner))),
                    PolyVertex::straight(transform.apply(point_to_dvec3(&solid.fourth_corner))),
                    PolyVertex::straight(transform.apply(point_to_dvec3(&solid.third_corner))),
                ];
                push_polyline(ctx, layer, verts, true, color);
            }
            EntityType::Leader(leader) => {
                let verts: Vec<PolyVertex> = leader.vertices.iter().map(|p| PolyVertex::straight(transform.apply(point_to_dvec3(p)))).collect();
                if verts.len() >= 2 {
                    push_polyline(ctx, layer, verts, false, color);
                }
            }
            EntityType::ModelPoint(point) => {
                let pos = transform.apply(point_to_dvec3(&point.location));
                ctx.doc.add_object(|id| Object::Point { id, layer, pos, color });
            }
            EntityType::Text(text) => {
                let (height, rotation) = transformed_text_style(transform, text.text_height, text.rotation.to_radians());
                // DXF TEXT anchors at the bottom-left/baseline of its line;
                // Object::Text anchors the top-left. (MTEXT below needs no
                // conversion: its default attachment point is already
                // top-left.)
                let pos = crate::model::geometry::text_anchor_bottom_to_top(transform.apply(point_to_dvec3(&text.location)), height, rotation);
                push_text(ctx, layer, pos, plain_text(&text.value), height, rotation, color);
            }
            EntityType::MText(mtext) => {
                let pos = transform.apply(point_to_dvec3(&mtext.insertion_point));
                let (height, rotation) = transformed_text_style(transform, mtext.initial_text_height, mtext.rotation_angle.to_radians());
                let mut raw = String::new();
                for chunk in &mtext.extended_text {
                    raw.push_str(chunk);
                }
                raw.push_str(&mtext.text);
                push_text(ctx, layer, pos, plain_mtext(&raw), height, rotation, color);
            }
            EntityType::Polyline(poly) => {
                // Skip polygon meshes and polyface meshes; only import 2D/3D polylines.
                if poly.is_polyface_mesh() || poly.is_3d_polygon_mesh() {
                    continue;
                }
                let is_3d = poly.is_3d_polyline();
                let elevation = poly.location.z;
                let mut entity_transform = transform.clone();
                if !is_3d {
                    entity_transform.push_with_affine(DVec3::ZERO, DVec3::ZERO, 0.0, DVec3::ONE, ocs_to_wcs_affine(&poly.normal));
                }
                let verts: Vec<PolyVertex> = poly
                    .vertices()
                    .filter(|v| {
                        // Skip spline control points and curve-fit extras;
                        // keep plain vertices and 3D polyline vertices.
                        !v.is_spline_frame_control_point() && !v.is_extra_created_by_curve_fit() && !v.is_spline_vertex_created_by_spline_fitting()
                    })
                    .map(|v| {
                        let z = if is_3d { v.location.z } else { elevation };
                        PolyVertex {
                            pos: DVec3::new(v.location.x, v.location.y, z),
                            bulge: v.bulge,
                        }
                    })
                    .collect();
                if verts.len() >= 2 {
                    push_bulge_polyline(ctx, layer, verts, poly.is_closed(), DVec3::Z, &entity_transform, color);
                }
            }
            EntityType::Insert(insert) => {
                import_insert(entity, insert, ctx, transform, scope);
            }
            // Dimensions and anything else are dropped.
            _ => {}
        }
    }
}

fn import_insert(entity: &Entity, insert: &dxf::entities::Insert, ctx: &mut ImportCtx<'_>, transform: &Transform, scope: BlockScope<'_>) {
    let Some(block) = ctx.blocks.get(insert.name.as_str()).copied() else {
        userspace_warn!("{}", crate::i18n::tr_format!(literal = "DXF INSERT references unknown block '%name%'", name = &insert.name));
        return;
    };
    if ctx.stack.len() >= MAX_BLOCK_DEPTH {
        userspace_warn!(
            "{}",
            crate::i18n::tr_format!(
                literal = "DXF block nesting exceeds maximum depth (%depth%), skipping '%name%'",
                depth = MAX_BLOCK_DEPTH,
                name = &insert.name
            )
        );
        return;
    }
    if ctx.stack.iter().any(|n| n == &block.name) {
        userspace_warn!(
            "{}",
            crate::i18n::tr_format!(literal = "DXF circular block reference detected: '%name%'", name = &insert.name)
        );
        return;
    }

    let child_color = if entity.common.color.is_by_layer() {
        scope.color
    } else {
        entity.common.color.index().or(scope.color)
    };
    // Resolve the child layer name to an owned String so it outlives the borrow.
    let child_layer = if entity.common.layer == "0" {
        scope.layer.unwrap_or("0").to_string()
    } else {
        entity.common.layer.clone()
    };

    let rows = usize::try_from(insert.row_count.max(1)).unwrap_or(1);
    let columns = usize::try_from(insert.column_count.max(1)).unwrap_or(1);
    let instances = rows.saturating_mul(columns);
    if !ctx.reserve_insert_instances(instances) {
        return;
    }

    let base_point = point_to_dvec3(&block.base_point);
    let location = point_to_dvec3(&insert.location);
    let rotation = insert.rotation.to_radians();
    let scale = DVec3::new(insert.x_scale_factor, insert.y_scale_factor, insert.z_scale_factor);
    let ocs = ocs_to_wcs_affine(&insert.extrusion_direction);

    ctx.stack.push(block.name.clone());
    let mut local = transform.clone();
    local.push_with_affine(base_point, location, rotation, scale, ocs);
    let child_scope = BlockScope {
        color: child_color,
        layer: Some(child_layer.as_str()),
    };
    for row in 0..insert.row_count.max(1) {
        for col in 0..insert.column_count.max(1) {
            let offset = DVec3::new(col as f64 * insert.column_spacing, row as f64 * insert.row_spacing, 0.0);
            local.push(DVec3::ZERO, offset, 0.0, DVec3::ONE);
            import_entities(block.entities.iter(), ctx, &local, child_scope);
            local.pop();
        }
    }
    ctx.stack.pop();
}

fn push_polyline(ctx: &mut ImportCtx<'_>, layer: LayerId, verts: Vec<PolyVertex>, closed: bool, color: ObjectColor) {
    if verts.len() < 2 {
        return;
    }
    if !ctx.reserve_vertices(verts.len()) {
        return;
    }
    ctx.doc.add_object(|id| Object::Polyline {
        id,
        layer,
        verts,
        closed,
        color,
        fill: crate::model::FillStyle::Clear,
        line_weight: 1.0,
    });
}

fn push_bulge_polyline(ctx: &mut ImportCtx<'_>, layer: LayerId, verts: Vec<PolyVertex>, closed: bool, normal: DVec3, transform: &Transform, color: ObjectColor) {
    let verts = trim_duplicate_open_endpoint(verts, closed);
    if verts.len() < 2 {
        return;
    }

    let has_bulges = verts.iter().any(|vertex| vertex.bulge.abs() > f64::EPSILON);
    if !has_bulges {
        push_polyline(
            ctx,
            layer,
            verts.into_iter().map(|vertex| PolyVertex::straight(transform.apply(vertex.pos))).collect(),
            closed,
            color,
        );
        return;
    }

    if let Some(sign) = xy_bulge_transform_sign(transform, normal) {
        push_polyline(
            ctx,
            layer,
            verts
                .into_iter()
                .map(|vertex| PolyVertex {
                    pos: transform.apply(vertex.pos),
                    bulge: vertex.bulge * sign,
                })
                .collect(),
            closed,
            color,
        );
        return;
    }

    let mut baked = Vec::new();
    for segment in verts.windows(2) {
        append_baked_bulge_segment(&mut baked, segment[0].pos, segment[1].pos, segment[0].bulge, normal, transform);
    }
    if closed && let (Some(first), Some(last)) = (verts.first(), verts.last()) {
        append_baked_bulge_segment(&mut baked, last.pos, first.pos, last.bulge, normal, transform);
    }
    let baked_closed = closed;
    push_polyline(ctx, layer, baked, baked_closed, color);
}

#[allow(clippy::too_many_arguments)]
fn push_text(ctx: &mut ImportCtx<'_>, layer: LayerId, pos: DVec3, content: String, height: f64, rotation: f64, color: ObjectColor) {
    if content.trim().is_empty() {
        return;
    }
    ctx.doc.add_object(|id| Object::Text {
        id,
        layer,
        pos,
        content,
        height,
        rotation,
        color,
    });
}

fn transformed_text_style(transform: &Transform, height: f64, rotation: f64) -> (f64, f64) {
    let matrix = transform.matrix();
    let baseline = matrix.transform_vector3(DVec3::new(rotation.cos(), rotation.sin(), 0.0));
    let up = matrix.transform_vector3(DVec3::new(-rotation.sin(), rotation.cos(), 0.0));
    let transformed_height = height * up.length();
    let transformed_rotation = if baseline.x.abs() + baseline.y.abs() <= f64::EPSILON {
        rotation
    } else {
        baseline.y.atan2(baseline.x)
    };
    (transformed_height.abs(), transformed_rotation)
}

fn spline_to_verts(spline: &dxf::entities::Spline, transform: &Transform) -> Vec<PolyVertex> {
    if spline.control_points.len() >= 2 {
        return nurbs_spline_to_verts(spline, transform);
    }
    fit_spline_to_verts(spline, transform)
}

fn nurbs_spline_to_verts(spline: &dxf::entities::Spline, transform: &Transform) -> Vec<PolyVertex> {
    let control_points: Vec<DVec3> = spline.control_points.iter().map(point_to_dvec3).collect();
    if control_points.iter().any(|point| !point.is_finite()) {
        return Vec::new();
    }
    let count = control_points.len();
    let degree = usize::try_from(spline.degree_of_curve).unwrap_or(1).clamp(1, count - 1);
    let expected_knots = count + degree + 1;
    let valid_knots =
        spline.knot_values.len() >= expected_knots && spline.knot_values.iter().all(|value| value.is_finite()) && spline.knot_values.windows(2).all(|pair| pair[0] <= pair[1]);
    let knots = if valid_knots {
        spline.knot_values[..expected_knots].to_vec()
    } else {
        open_uniform_knots(count, degree)
    };
    let start = knots[degree];
    let end = knots[count];
    if !start.is_finite() || !end.is_finite() || end <= start {
        return Vec::new();
    }
    let weights: Vec<f64> = if spline.weight_values.len() >= count && spline.weight_values.iter().take(count).all(|weight| weight.is_finite() && *weight > 0.0) {
        spline.weight_values[..count].to_vec()
    } else {
        vec![1.0; count]
    };
    let segments = curve_segment_count(&control_points);
    let sample_count = if spline.is_closed() { segments } else { segments + 1 };
    (0..sample_count)
        .filter_map(|sample| {
            let t = sample as f64 / segments as f64;
            let parameter = if sample == segments { end } else { start + (end - start) * t };
            de_boor_point(&control_points, &weights, &knots, degree, parameter)
                .filter(|point| point.is_finite())
                .map(|point| PolyVertex::straight(transform.apply(point)))
        })
        .collect()
}

fn open_uniform_knots(control_count: usize, degree: usize) -> Vec<f64> {
    let knot_count = control_count + degree + 1;
    let interior = control_count.saturating_sub(degree);
    (0..knot_count)
        .map(|index| {
            if index <= degree {
                0.0
            } else if index >= control_count {
                1.0
            } else {
                (index - degree) as f64 / interior as f64
            }
        })
        .collect()
}

fn de_boor_point(control_points: &[DVec3], weights: &[f64], knots: &[f64], degree: usize, parameter: f64) -> Option<DVec3> {
    let count = control_points.len();
    let span = if parameter >= knots[count] {
        count - 1
    } else {
        (degree..count).find(|&index| parameter < knots[index + 1])?
    };
    let mut work = Vec::with_capacity(degree + 1);
    for local in 0..=degree {
        let index = span - degree + local;
        let weight = weights[index];
        work.push(DVec4::new(
            control_points[index].x * weight,
            control_points[index].y * weight,
            control_points[index].z * weight,
            weight,
        ));
    }
    for level in 1..=degree {
        for local in (level..=degree).rev() {
            let index = span - degree + local;
            let denominator = knots[index + degree - level + 1] - knots[index];
            let alpha = if denominator.abs() <= f64::EPSILON {
                0.0
            } else {
                ((parameter - knots[index]) / denominator).clamp(0.0, 1.0)
            };
            work[local] = work[local - 1].lerp(work[local], alpha);
        }
    }
    let point = work[degree];
    (point.w.abs() > f64::EPSILON).then(|| point.truncate() / point.w)
}

fn fit_spline_to_verts(spline: &dxf::entities::Spline, transform: &Transform) -> Vec<PolyVertex> {
    let points: Vec<DVec3> = spline.fit_points.iter().map(point_to_dvec3).collect();
    let supplied_start_tangent = point_to_dvec3(&spline.start_tangent);
    let supplied_end_tangent = point_to_dvec3(&spline.end_tangent);
    if points.len() < 2 || points.iter().any(|point| !point.is_finite()) || !supplied_start_tangent.is_finite() || !supplied_end_tangent.is_finite() {
        return Vec::new();
    }
    let closed = spline.is_closed();
    let edge_count = if closed { points.len() } else { points.len() - 1 };
    let mut result = Vec::new();
    for index in 0..edge_count {
        let next = (index + 1) % points.len();
        let previous = if index == 0 { if closed { points.len() - 1 } else { 0 } } else { index - 1 };
        let after_next = if next + 1 < points.len() {
            next + 1
        } else if closed {
            0
        } else {
            next
        };
        let mut tangent_start = (points[next] - points[previous]) * 0.5;
        let mut tangent_end = (points[after_next] - points[index]) * 0.5;
        if index == 0 {
            let supplied = supplied_start_tangent;
            if supplied.length_squared() > f64::EPSILON {
                tangent_start = supplied;
            }
        }
        if !closed && next == points.len() - 1 {
            let supplied = supplied_end_tangent;
            if supplied.length_squared() > f64::EPSILON {
                tangent_end = supplied;
            }
        }
        let subsegments = ((points[next] - points[index]).length() / TESSELLATION_SEGMENT_LEN).ceil() as usize;
        // Share the global tessellation budget across all edges. With many
        // fit points the per-edge budget can fall below the preferred minimum
        // of 2; degrade to one sample per edge instead of constructing an
        // invalid clamp range.
        let per_edge_budget = (MAX_TESSELLATION_SEGMENTS / edge_count).max(1);
        let subsegments = subsegments.max(2).min(per_edge_budget).max(1);
        for sample in 0..subsegments {
            let t = sample as f64 / subsegments as f64;
            let t2 = t * t;
            let t3 = t2 * t;
            let point = (2.0 * t3 - 3.0 * t2 + 1.0) * points[index] + (t3 - 2.0 * t2 + t) * tangent_start + (-2.0 * t3 + 3.0 * t2) * points[next] + (t3 - t2) * tangent_end;
            result.push(PolyVertex::straight(transform.apply(point)));
        }
    }
    if !closed {
        result.push(PolyVertex::straight(transform.apply(*points.last().unwrap_or(&points[0]))));
    }
    result
}

fn curve_segment_count(points: &[DVec3]) -> usize {
    let length: f64 = points.windows(2).map(|pair| pair[0].distance(pair[1])).sum();
    ((length / TESSELLATION_SEGMENT_LEN).ceil() as usize).clamp(MIN_TESSELLATION_SEGMENTS, MAX_TESSELLATION_SEGMENTS)
}

/// Normalise an arc sweep into `(0, TAU]`. Returns 0 for non-finite input so
/// callers can reject the arc instead of looping or producing NaN geometry.
fn normalize_sweep(start: f64, end: f64) -> f64 {
    let span = end - start;
    if !span.is_finite() {
        return 0.0;
    }
    let sweep = span.rem_euclid(std::f64::consts::TAU);
    // Coincident or exactly TAU-separated endpoints mean a full circle.
    if sweep <= 0.0 { std::f64::consts::TAU } else { sweep }
}

/// An arc in the XY plane becomes one bulged segment; otherwise it is
/// tessellated to straight segments.
fn arc_to_verts(center: DVec3, radius: f64, normal: DVec3, start: f64, end: f64, transform: &Transform) -> Vec<PolyVertex> {
    if !(center.is_finite() && radius.is_finite() && radius > 0.0 && start.is_finite() && end.is_finite()) {
        return Vec::new();
    }
    let sweep = normalize_sweep(start, end);
    if sweep <= 0.0 {
        return Vec::new();
    }
    if let Some(sign) = xy_bulge_transform_sign(transform, normal) {
        let start_pos = center + radius * DVec3::new(start.cos(), start.sin(), 0.0);
        let end_pos = center + radius * DVec3::new(end.cos(), end.sin(), 0.0);
        vec![
            PolyVertex {
                pos: transform.apply(start_pos),
                bulge: (sweep / 4.0).tan() * sign,
            },
            PolyVertex::straight(transform.apply(end_pos)),
        ]
    } else {
        let segments = arc_segment_count(radius, sweep);
        let start_vec = DQuat::from_axis_angle(normal, start).mul_vec3(orthonormal_basis(normal).0 * radius);
        (0..=segments)
            .map(|i| {
                let angle = start + sweep * (i as f64 / segments as f64);
                let pos = center + DQuat::from_axis_angle(normal, angle - start).mul_vec3(start_vec);
                PolyVertex::straight(transform.apply(pos))
            })
            .collect()
    }
}

fn circle_to_verts(center: DVec3, radius: f64, normal: DVec3, transform: &Transform) -> (Vec<PolyVertex>, bool) {
    if let Some(sign) = xy_bulge_transform_sign(transform, normal) {
        let a = center + DVec3::new(radius, 0.0, 0.0);
        let b = center - DVec3::new(radius, 0.0, 0.0);
        (
            vec![
                PolyVertex {
                    pos: transform.apply(a),
                    bulge: sign,
                },
                PolyVertex {
                    pos: transform.apply(b),
                    bulge: sign,
                },
            ],
            true,
        )
    } else {
        let (u, v) = orthonormal_basis(normal);
        let segments = arc_segment_count(radius, std::f64::consts::TAU);
        let verts = (0..segments)
            .map(|i| {
                let angle = std::f64::consts::TAU * (i as f64 / segments as f64);
                let pos = center + radius * (u * angle.cos() + v * angle.sin());
                PolyVertex::straight(transform.apply(pos))
            })
            .collect();
        (verts, true)
    }
}

fn ellipse_to_verts(ell: &dxf::entities::Ellipse, transform: &Transform) -> Vec<PolyVertex> {
    let center = point_to_dvec3(&ell.center);
    let major = vector_to_dvec3(&ell.major_axis);
    let a = major.length();
    if a <= f64::EPSILON {
        return Vec::new();
    }
    let major_dir = major / a;
    let normal = normalize_or_z(vector_to_dvec3(&ell.normal));
    let minor_dir = normal.cross(major_dir).normalize_or_zero();
    let b = a * ell.minor_axis_ratio;
    let start = ell.start_parameter;
    let end = ell.end_parameter;
    if !(center.is_finite() && start.is_finite() && end.is_finite() && b.is_finite()) {
        return Vec::new();
    }
    let sweep = if ellipse_is_full_loop(start, end) { std::f64::consts::TAU } else { end - start };
    let segments = arc_segment_count(a.max(b), sweep.abs());
    (0..=segments)
        .map(|i| {
            let t = start + sweep * (i as f64 / segments as f64);
            let pos = center + major_dir * a * t.cos() + minor_dir * b * t.sin();
            PolyVertex::straight(transform.apply(pos))
        })
        .collect()
}

fn ellipse_is_full_loop(start: f64, end: f64) -> bool {
    let span = (end - start).abs();
    !(f64::EPSILON..std::f64::consts::TAU - 1.0e-6).contains(&span)
}

fn arc_segment_count(radius: f64, sweep: f64) -> usize {
    let arc_len = radius.abs() * sweep.abs();
    ((arc_len / TESSELLATION_SEGMENT_LEN).ceil() as usize).clamp(MIN_TESSELLATION_SEGMENTS, MAX_TESSELLATION_SEGMENTS)
}

fn trim_duplicate_open_endpoint(mut verts: Vec<PolyVertex>, closed: bool) -> Vec<PolyVertex> {
    if closed || verts.len() <= 2 {
        return verts;
    }
    let Some(first) = verts.first() else {
        return verts;
    };
    let Some(last) = verts.last() else {
        return verts;
    };
    if crate::model::kernel::points_coincident_3d(first.pos, last.pos) {
        let _ = verts.pop();
    }
    verts
}

fn xy_bulge_transform_sign(transform: &Transform, normal: DVec3) -> Option<f64> {
    if (normal - DVec3::Z).length_squared() > 1.0e-6 {
        return None;
    }

    let matrix = transform.matrix();
    let origin = matrix.transform_point3(DVec3::ZERO);
    let x_axis = matrix.transform_point3(DVec3::X) - origin;
    let y_axis = matrix.transform_point3(DVec3::Y) - origin;
    if x_axis.z.abs() > 1.0e-5 || y_axis.z.abs() > 1.0e-5 {
        return None;
    }

    let x = x_axis.truncate();
    let y = y_axis.truncate();
    let x_len = x.length();
    let y_len = y.length();
    if x_len <= f64::EPSILON || y_len <= f64::EPSILON {
        return None;
    }
    if (x_len - y_len).abs() > x_len.max(y_len) * 1.0e-4 {
        return None;
    }
    if x.dot(y).abs() > x_len * y_len * 1.0e-4 {
        return None;
    }

    let orientation = x.perp_dot(y);
    if orientation.abs() <= f64::EPSILON { None } else { Some(orientation.signum()) }
}

fn append_baked_bulge_segment(out: &mut Vec<PolyVertex>, start: DVec3, end: DVec3, bulge: f64, normal: DVec3, transform: &Transform) {
    if out.is_empty() {
        out.push(PolyVertex::straight(transform.apply(start)));
    }

    let chord = end - start;
    let chord_len = chord.length();
    if bulge.abs() <= f64::EPSILON || chord_len <= f64::EPSILON {
        out.push(PolyVertex::straight(transform.apply(end)));
        return;
    }

    let theta = 4.0 * bulge.atan();
    let radius = chord_len * (1.0 + bulge * bulge) / (4.0 * bulge.abs());
    let midpoint = (start + end) * 0.5;
    let chord_dir = chord / chord_len;
    let perp = normal.cross(chord_dir).normalize_or_zero();
    if perp.length_squared() <= f64::EPSILON {
        out.push(PolyVertex::straight(transform.apply(end)));
        return;
    }

    let offset = chord_len * (1.0 - bulge * bulge) / (4.0 * bulge);
    let center = midpoint + perp * offset;
    let start_vec = start - center;
    let segments = arc_segment_count(radius, theta.abs());
    for i in 1..=segments {
        let point = if i == segments {
            end
        } else {
            let t = theta * (i as f64 / segments as f64);
            center + DQuat::from_axis_angle(normal, t).mul_vec3(start_vec)
        };
        out.push(PolyVertex::straight(transform.apply(point)));
    }
}

fn plain_text(raw: &str) -> String {
    replace_legacy_text_codes(raw).trim().to_string()
}

fn plain_mtext(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some('P') => out.push('\n'),
                Some('p') => {
                    let _ = read_until_semicolon(&mut chars);
                }
                Some('X') => out.push('\n'),
                Some('~') => out.push(' '),
                Some('\\') => out.push('\\'),
                Some('{') => out.push('{'),
                Some('}') => out.push('}'),
                Some('U') if chars.peek() == Some(&'+') => {
                    let _ = chars.next();
                    let mut code = String::new();
                    for _ in 0..4 {
                        if let Some(hex) = chars.next() {
                            code.push(hex);
                        }
                    }
                    if let Ok(value) = u32::from_str_radix(&code, 16)
                        && let Some(decoded) = char::from_u32(value)
                    {
                        out.push(decoded);
                    }
                }
                Some('S') | Some('s') => {
                    let stack = read_until_semicolon(&mut chars);
                    out.push_str(&plain_stack_text(&stack));
                }
                Some('A' | 'a' | 'C' | 'c' | 'F' | 'f' | 'H' | 'h' | 'Q' | 'q' | 'T' | 't' | 'W' | 'w') => {
                    let _ = read_until_semicolon(&mut chars);
                }
                Some('L' | 'l' | 'O' | 'o' | 'K' | 'k') => {}
                Some(other) => out.push(other),
                None => {}
            },
            '{' | '}' => {}
            '\r' => {}
            other => out.push(other),
        }
    }

    replace_legacy_text_codes(&out).lines().map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

fn read_until_semicolon(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    for ch in chars.by_ref() {
        if ch == ';' {
            break;
        }
        value.push(ch);
    }
    value
}

fn plain_stack_text(stack: &str) -> String {
    let mut text = stack.replace(['#', '^'], "/").replace("\\P", "\n").replace("\\~", " ");
    if text.contains('/') {
        text = text.split('/').map(str::trim).filter(|part| !part.is_empty()).collect::<Vec<_>>().join("/");
    }
    text
}

fn replace_legacy_text_codes(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' && chars.peek() == Some(&'%') {
            let _ = chars.next();
            match chars.next() {
                Some('d') | Some('D') => out.push('°'),
                Some('p') | Some('P') => out.push('±'),
                Some('c') | Some('C') => out.push('Ø'),
                Some('%') => out.push('%'),
                Some('u' | 'U' | 'o' | 'O') => {}
                Some(other) => {
                    out.push('%');
                    out.push('%');
                    out.push(other);
                }
                None => {
                    out.push('%');
                    out.push('%');
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// An orthonormal pair spanning the plane perpendicular to `normal`.
fn orthonormal_basis(normal: DVec3) -> (DVec3, DVec3) {
    let seed = if normal.z.abs() < 0.9 { DVec3::Z } else { DVec3::X };
    let u = seed.cross(normal).normalize_or(DVec3::X);
    let v = normal.cross(u).normalize_or(DVec3::Y);
    (u, v)
}

/// DXF Arbitrary Axis Algorithm: OCS→WCS basis from an extrusion normal.
fn ocs_to_wcs_affine(extrusion_direction: &dxf::Vector) -> DMat4 {
    let mut n = vector_to_dvec3(extrusion_direction);
    if n.length_squared() <= f64::EPSILON {
        return DMat4::IDENTITY;
    }
    n = n.normalize();
    if (n - DVec3::Z).length_squared() <= 1.0e-10 {
        return DMat4::IDENTITY;
    }
    let axis_x = if n.x.abs() < (1.0 / 64.0) && n.y.abs() < (1.0 / 64.0) {
        DVec3::Y.cross(n).normalize_or_zero()
    } else {
        DVec3::Z.cross(n).normalize_or_zero()
    };
    if axis_x.length_squared() <= f64::EPSILON {
        return DMat4::IDENTITY;
    }
    let axis_y = n.cross(axis_x).normalize_or_zero();
    if axis_y.length_squared() <= f64::EPSILON {
        return DMat4::IDENTITY;
    }
    DMat4::from_cols(axis_x.extend(0.0), axis_y.extend(0.0), n.extend(0.0), DVec4::new(0.0, 0.0, 0.0, 1.0))
}
