//! Editable mine-design document model and source of truth for the editor.

pub(crate) mod asset_residency;
pub(crate) mod asset_storage;
pub(crate) mod history_storage;
pub(crate) mod layer_residency;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod atomic_file;
pub(crate) mod block_model;
pub(crate) mod drill_hole;
pub(crate) mod formats;
pub(crate) mod geometry;
#[cfg(target_arch = "wasm32")]
pub(crate) mod input;
pub(crate) mod kernel;
pub(crate) mod kriging;
pub(crate) mod plot;
pub(crate) mod point_cloud;
pub(crate) mod progress;
pub(crate) mod project;
pub(crate) mod raster;
pub(crate) mod spatial;
pub(crate) mod triangulation;

use std::collections::HashMap;

use glam::DVec3;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct LayerId(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct ObjectId(pub(crate) u64);

/// Cartesian axis targeted by axis-wide edits such as Design > Move to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    /// Single-letter name used in menus, dialog titles and console reports.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

/// Stable identity used by rendering, selection and spatial queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SceneEntityId {
    Object(ObjectId),
    Triangulation(triangulation::TriangulationId),
    BlockModel(block_model::BlockModelId),
    DrillHole(drill_hole::DrillHoleId),
    PointCloud(point_cloud::PointCloudId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Layer {
    pub(crate) id: LayerId,
    pub(crate) name: String,
    pub(crate) color_index: Option<u8>,
    /// Resolved RGBA used for rendering objects with `ObjectColor::ByLayer`.
    pub(crate) color: [f32; 4],
    #[serde(alias = "visible")]
    pub(crate) loaded: bool,
    pub(crate) elevation: f32,
}

/// Fill style for closed polylines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FillStyle {
    #[default]
    Clear,
    Crosses,
    Slashes,
    Solid,
}

/// How an object's colour is determined.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum ObjectColor {
    /// Follow the owning layer's colour.
    ByLayer,
    /// An explicit resolved RGBA colour.
    Fixed([f32; 4]),
}

/// A polyline vertex. `bulge` is the DXF arc encoding (`tan(includedAngle/4)`)
/// for the segment starting at this vertex; `0.0` is a straight segment.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolyVertex {
    pub(crate) pos: DVec3,
    pub(crate) bulge: f64,
}

/// A semantic edit handle on a design object.
///
/// Most polylines expose their stored vertices. Compact circles are stored as
/// two bulged semicircles, but expose their geometric centre instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObjectPoint {
    Vertex(usize),
    Center,
}

impl PolyVertex {
    pub(crate) fn straight(pos: DVec3) -> Self {
        Self { pos, bulge: 0.0 }
    }
}

/// A drawable design element. `Polyline` with `closed == true` represents a
/// polyline; vertices carry bulges so arcs/circles are preserved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum Object {
    Point {
        id: ObjectId,
        layer: LayerId,
        pos: DVec3,
        color: ObjectColor,
    },
    Polyline {
        id: ObjectId,
        layer: LayerId,
        verts: Vec<PolyVertex>,
        closed: bool,
        color: ObjectColor,
        fill: FillStyle,
        line_weight: f32,
    },
    Text {
        id: ObjectId,
        layer: LayerId,
        pos: DVec3,
        content: String,
        height: f64,
        rotation: f64,
        color: ObjectColor,
    },
}

impl Object {
    pub(crate) fn id(&self) -> ObjectId {
        match self {
            Object::Point { id, .. } | Object::Polyline { id, .. } | Object::Text { id, .. } => *id,
        }
    }

    pub(crate) fn layer(&self) -> LayerId {
        match self {
            Object::Point { layer, .. } | Object::Polyline { layer, .. } | Object::Text { layer, .. } => *layer,
        }
    }

    /// Short human-readable variant name, for diagnostics/logging. A two-vertex
    /// polyline is a plain line and is named as one.
    pub(crate) fn kind_name(&self) -> String {
        match self {
            Object::Point { .. } => crate::i18n::tr!(literal = "Point"),
            Object::Polyline { verts, .. } if verts.len() == 2 => crate::i18n::tr!(literal = "Line"),
            Object::Polyline { .. } => crate::i18n::tr!(literal = "Polyline"),
            Object::Text { .. } => crate::i18n::tr!(literal = "Text"),
        }
    }

    pub(crate) fn color(&self) -> ObjectColor {
        match self {
            Object::Point { color, .. } | Object::Polyline { color, .. } | Object::Text { color, .. } => *color,
        }
    }

    /// World-space bounds used by generic spatial/object information.
    ///
    /// Text includes its laid-out rotated rectangle and polylines include arc
    /// bulges, so callers see the same extent the viewport uses rather than
    /// merely the stored anchor points.
    pub(crate) fn world_bounds(&self) -> Option<(DVec3, DVec3)> {
        match self {
            Object::Point { pos, .. } => Some((*pos, *pos)),
            Object::Polyline { verts, closed, .. } => crate::model::geometry::polyline_bulge_bounds(verts, *closed),
            Object::Text {
                pos, content, height, rotation, ..
            } => {
                let corners = crate::model::geometry::text_bounds_corners(*pos, content, *height, *rotation);
                let min = corners.iter().copied().fold(DVec3::splat(f64::INFINITY), DVec3::min);
                let max = corners.iter().copied().fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max);
                min.is_finite().then_some((min, max))
            }
        }
    }

    /// Geometry/style fingerprint for layer dirty tracking. Object order is
    /// hashed by the caller, and IDs do not change geometry when a backed layer
    /// is merged or namespaced. Floats hash by bit pattern for exact undo.
    pub(crate) fn geometry_hash(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        fn hash_color(hasher: &mut DefaultHasher, color: ObjectColor) {
            match color {
                ObjectColor::ByLayer => 0u8.hash(hasher),
                ObjectColor::Fixed(rgba) => {
                    1u8.hash(hasher);
                    for channel in rgba {
                        channel.to_bits().hash(hasher);
                    }
                }
            }
        }
        fn hash_pos(hasher: &mut DefaultHasher, pos: DVec3) {
            pos.x.to_bits().hash(hasher);
            pos.y.to_bits().hash(hasher);
            pos.z.to_bits().hash(hasher);
        }
        fn hash_verts(hasher: &mut DefaultHasher, verts: &[PolyVertex]) {
            verts.len().hash(hasher);
            for vertex in verts {
                hash_pos(hasher, vertex.pos);
                vertex.bulge.to_bits().hash(hasher);
            }
        }

        let mut hasher = DefaultHasher::new();
        match self {
            Object::Point { pos, color, .. } => {
                0u8.hash(&mut hasher);
                hash_pos(&mut hasher, *pos);
                hash_color(&mut hasher, *color);
            }
            Object::Polyline {
                verts,
                closed,
                color,
                fill,
                line_weight,
                ..
            } => {
                1u8.hash(&mut hasher);
                hash_verts(&mut hasher, verts);
                closed.hash(&mut hasher);
                hash_color(&mut hasher, *color);
                std::mem::discriminant(fill).hash(&mut hasher);
                line_weight.to_bits().hash(&mut hasher);
            }
            Object::Text {
                pos,
                content,
                height,
                rotation,
                color,
                ..
            } => {
                2u8.hash(&mut hasher);
                hash_pos(&mut hasher, *pos);
                content.hash(&mut hasher);
                height.to_bits().hash(&mut hasher);
                rotation.to_bits().hash(&mut hasher);
                hash_color(&mut hasher, *color);
            }
        }
        hasher.finish()
    }

    /// Check variant-specific geometry invariants: finite coordinates,
    /// non-negative sizes/widths, and minimum vertex counts.
    pub(crate) fn validate_geometry(&self) -> Result<(), String> {
        fn require_finite_verts(verts: &[PolyVertex], what: &str) -> Result<(), String> {
            if verts.len() < 2 {
                return Err(format!("{what} needs at least 2 vertices"));
            }
            for vertex in verts {
                if !vertex.pos.is_finite() {
                    return Err(format!("{what} contains a non-finite vertex"));
                }
                if !vertex.bulge.is_finite() {
                    return Err(format!("{what} contains a non-finite bulge"));
                }
            }
            Ok(())
        }
        if let ObjectColor::Fixed(rgba) = self.color()
            && rgba.iter().any(|channel| !channel.is_finite())
        {
            return Err("non-finite colour".to_string());
        }
        match self {
            Object::Point { pos, .. } => {
                if !pos.is_finite() {
                    return Err("non-finite point position".to_string());
                }
            }
            Object::Polyline { verts, closed, line_weight, .. } => {
                require_finite_verts(verts, "polyline")?;
                // Two bulged vertices form a valid closed shape (most notably
                // the compact two-semicircle representation of a circle).
                if *closed && verts.len() < 3 && verts.iter().all(|vertex| vertex.bulge.abs() <= f64::EPSILON) {
                    return Err("closed polyline needs at least 3 vertices unless it contains an arc".to_string());
                }
                if !line_weight.is_finite() || *line_weight < 0.0 {
                    return Err("invalid polyline line weight".to_string());
                }
            }
            Object::Text { pos, height, rotation, .. } => {
                if !pos.is_finite() {
                    return Err("non-finite text position".to_string());
                }
                if !height.is_finite() || *height < 0.0 {
                    return Err("invalid text height".to_string());
                }
                if !rotation.is_finite() {
                    return Err("non-finite text rotation".to_string());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn translate(&mut self, delta: DVec3) {
        match self {
            Object::Point { pos, .. } | Object::Text { pos, .. } => *pos += delta,
            Object::Polyline { verts, .. } => {
                for vertex in verts {
                    vertex.pos += delta;
                }
            }
        }
    }

    /// Coordinate along `axis` of the object's first vertex, used to seed axis dialogs.
    pub(crate) fn axis_position(&self, axis: Axis) -> f64 {
        let pos = match self {
            Object::Point { pos, .. } | Object::Text { pos, .. } => *pos,
            Object::Polyline { verts, .. } => verts.first().map_or(DVec3::ZERO, |vertex| vertex.pos),
        };
        match axis {
            Axis::X => pos.x,
            Axis::Y => pos.y,
            Axis::Z => pos.z,
        }
    }

    pub(crate) fn set_axis_position(&mut self, axis: Axis, value: f64) {
        let set = |pos: &mut DVec3| match axis {
            Axis::X => pos.x = value,
            Axis::Y => pos.y = value,
            Axis::Z => pos.z = value,
        };
        match self {
            Object::Point { pos, .. } | Object::Text { pos, .. } => set(pos),
            Object::Polyline { verts, .. } => {
                for vertex in verts {
                    set(&mut vertex.pos);
                }
            }
        }
    }

    pub(crate) fn with_id_and_layer(&self, id: ObjectId, layer: LayerId) -> Self {
        match self {
            Object::Point { pos, color, .. } => Object::Point {
                id,
                layer,
                pos: *pos,
                color: *color,
            },
            Object::Polyline {
                verts,
                closed,
                color,
                fill,
                line_weight,
                ..
            } => Object::Polyline {
                id,
                layer,
                verts: verts.clone(),
                closed: *closed,
                color: *color,
                fill: *fill,
                line_weight: *line_weight,
            },
            Object::Text {
                pos,
                content,
                height,
                rotation,
                color,
                ..
            } => Object::Text {
                id,
                layer,
                pos: *pos,
                content: content.clone(),
                height: *height,
                rotation: *rotation,
                color: *color,
            },
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Document {
    layers: Vec<Layer>,
    objects: Vec<Object>,
    #[serde(skip)]
    pub(crate) deferred_layers: HashMap<LayerId, layer_residency::DeferredLayer>,
    /// Project-persistent visibility overrides for individual design objects.
    /// Layers have their own loaded flag; this set covers object-level
    /// Hide Selection without changing the object's geometry or styling.
    #[serde(default)]
    hidden_objects: std::collections::HashSet<ObjectId>,
    #[serde(skip)]
    object_index: HashMap<ObjectId, usize>,
    next_layer_id: u64,
    next_object_id: u64,
    #[serde(skip)]
    revision: u64,
    /// Document revision at which each object was last mutated. Lets the
    /// renderer's static stroke cache re-tessellate only the objects an edit
    /// actually touched instead of the whole document.
    #[serde(skip)]
    object_revisions: HashMap<ObjectId, u64>,
}

impl Document {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub(crate) fn objects(&self) -> &[Object] {
        &self.objects
    }

    pub(crate) fn is_object_hidden(&self, id: ObjectId) -> bool {
        self.hidden_objects.contains(&id)
    }

    pub(crate) fn hidden_object_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.hidden_objects.iter().copied()
    }

    /// Persist an individual object's visibility. Returns whether it changed.
    pub(crate) fn set_object_hidden(&mut self, id: ObjectId, hidden: bool) -> bool {
        if self.get_object(id).is_none() {
            return false;
        }
        let changed = if hidden { self.hidden_objects.insert(id) } else { self.hidden_objects.remove(&id) };
        if changed {
            self.touch_object(id);
        }
        changed
    }

    /// Convert impossible imported two-vertex closed polylines into open ones.
    ///
    /// Some external design formats encode ordinary two-point strings as
    /// closed polylines. Importers use this before constructing a project so the
    /// resulting document satisfies the three-vertex polyline invariant.
    pub(crate) fn repair_degenerate_closed_polylines(&mut self) -> usize {
        let mut repaired = 0;
        for object in &mut self.objects {
            if let Object::Polyline { verts, closed, .. } = object
                && *closed
                && verts.len() == 2
            {
                *closed = false;
                repaired += 1;
            }
        }
        if repaired > 0 {
            self.touch();
        }
        repaired
    }

    /// Validate on-disk model invariants. Runs on deserialized documents
    /// before runtime namespacing, so all ids must still be in the local
    /// 32-bit namespace. Identity collisions are reported, never silently
    /// repaired: a repaired duplicate would alias a different object than the
    /// file author intended.
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        use anyhow::bail;
        const LOCAL_MASK: u64 = u32::MAX as u64;

        let mut layer_ids = std::collections::HashSet::with_capacity(self.layers.len());
        for layer in &self.layers {
            if layer.id.0 > LOCAL_MASK {
                bail!("layer '{}' has id {} outside the 32-bit project id range", layer.name, layer.id.0);
            }
            if !layer_ids.insert(layer.id) {
                bail!("duplicate layer id {} ('{}')", layer.id.0, layer.name);
            }
            if !layer.elevation.is_finite() {
                bail!("layer '{}' has a non-finite elevation", layer.name);
            }
            if layer.color.iter().any(|channel| !channel.is_finite()) {
                bail!("layer '{}' has a non-finite colour", layer.name);
            }
        }

        let mut object_ids = std::collections::HashSet::with_capacity(self.objects.len());
        for object in &self.objects {
            let id = object.id();
            if id.0 > LOCAL_MASK {
                bail!("{} object has id {} outside the 32-bit project id range", object.kind_name(), id.0);
            }
            if !object_ids.insert(id) {
                bail!("duplicate object id {}", id.0);
            }
            if !layer_ids.contains(&object.layer()) {
                bail!("{} object {} references missing layer {}", object.kind_name(), id.0, object.layer().0);
            }
            object.validate_geometry().map_err(|error| anyhow::anyhow!("object {}: {error}", id.0))?;
        }
        for id in &self.hidden_objects {
            if !object_ids.contains(id) {
                bail!("hidden object id {} does not reference a design object", id.0);
            }
        }
        Ok(())
    }

    /// Recompute the id counters from the actual ids present, instead of
    /// trusting serialized counters that may be stale or malicious.
    pub(crate) fn recompute_id_counters(&mut self) {
        let max_layer = self.layers.iter().map(|layer| layer.id.0).max();
        let max_object = self.objects.iter().map(|object| object.id().0).max();
        self.next_layer_id = max_layer.map_or(0, |id| id.saturating_add(1));
        self.next_object_id = max_object.map_or(0, |id| id.saturating_add(1));
        self.next_object_id = self
            .next_object_id
            .max(self.deferred_layers.values().map(|stored| stored.max_object_id + 1).max().unwrap_or(0));
    }

    pub(crate) fn rebuild_object_index(&mut self) {
        self.object_index = self.objects.iter().enumerate().map(|(index, object)| (object.id(), index)).collect();
    }

    fn object_position(&self, id: ObjectId) -> Option<usize> {
        self.object_index
            .get(&id)
            .copied()
            .filter(|&index| self.objects.get(index).is_some_and(|object| object.id() == id))
            .or_else(|| self.objects.iter().position(|object| object.id() == id))
    }

    pub(crate) fn add_layer(&mut self, name: String, color_index: Option<u8>, color: [f32; 4], loaded: bool, elevation: f32) -> LayerId {
        let id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.layers.push(Layer {
            id,
            name,
            color_index,
            color,
            loaded,
            elevation,
        });
        self.touch();
        id
    }

    pub(crate) fn allocate_layer_id(&mut self) -> LayerId {
        let id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.touch();
        id
    }

    /// Append an object, supplying its freshly allocated id to the constructor.
    pub(crate) fn add_object(&mut self, make: impl FnOnce(ObjectId) -> Object) -> ObjectId {
        let id = ObjectId(self.next_object_id);
        self.next_object_id += 1;
        self.object_index.insert(id, self.objects.len());
        self.objects.push(make(id));
        self.touch_object(id);
        id
    }

    /// Reserve a fresh object id without inserting anything.
    pub(crate) fn allocate_object_id(&mut self) -> ObjectId {
        let id = ObjectId(self.next_object_id);
        self.next_object_id += 1;
        self.touch();
        id
    }

    /// Insert an object that already carries its id (used by commands/redo).
    pub(crate) fn insert_object(&mut self, object: Object) {
        let id = object.id();
        self.bump_next_object_id(id);
        if let Some(index) = self.object_position(id)
            && let Some(existing) = self.objects.get_mut(index)
        {
            self.object_index.insert(id, index);
            *existing = object;
        } else {
            self.object_index.insert(id, self.objects.len());
            self.objects.push(object);
        }
        self.touch_object(id);
    }

    /// Insert an object at a specific draw-order index (used when undoing a
    /// delete, so the object returns below whatever it was drawn under).
    pub(crate) fn insert_object_at(&mut self, index: usize, object: Object) {
        let id = object.id();
        if self.object_position(id).is_some() {
            self.replace_object(object);
            return;
        }
        self.bump_next_object_id(id);
        let index = index.min(self.objects.len());
        self.objects.insert(index, object);
        for shifted_index in index..self.objects.len() {
            self.object_index.insert(self.objects[shifted_index].id(), shifted_index);
        }
        self.touch_object(id);
    }

    /// Advance the id counter past `id`. Runtime ids are
    /// `namespace << 32 | local`; the increment must never carry out of the
    /// local half into another project's namespace.
    fn bump_next_object_id(&mut self, id: ObjectId) {
        const LOCAL_MASK: u64 = u32::MAX as u64;
        let next = if id.0 & LOCAL_MASK == LOCAL_MASK { id.0 } else { id.0 + 1 };
        self.next_object_id = self.next_object_id.max(next);
    }

    /// Replace an object in place, preserving draw order.
    pub(crate) fn replace_object(&mut self, object: Object) -> bool {
        let Some(index) = self.object_position(object.id()) else {
            return false;
        };
        let Some(existing) = self.objects.get_mut(index) else {
            self.rebuild_object_index();
            return false;
        };
        let id = object.id();
        self.object_index.insert(id, index);
        *existing = object;
        self.touch_object(id);
        true
    }

    /// Remove the object with `id`, returning it if present.
    pub(crate) fn remove_object(&mut self, id: ObjectId) -> Option<Object> {
        let index = self.object_position(id)?;
        self.object_index.remove(&id);
        self.object_revisions.remove(&id);
        self.hidden_objects.remove(&id);
        let object = self.objects.remove(index);
        for shifted_index in index..self.objects.len() {
            self.object_index.insert(self.objects[shifted_index].id(), shifted_index);
        }
        self.touch();
        Some(object)
    }

    /// Remove many objects in one retain/reindex pass. Returns each object's
    /// original draw-order index for exact undo restoration.
    fn remove_objects_bulk(&mut self, ids: &std::collections::HashSet<ObjectId>) -> HashMap<ObjectId, usize> {
        let positions: HashMap<_, _> = self
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| ids.contains(&object.id()))
            .map(|(index, object)| (object.id(), index))
            .collect();
        if positions.is_empty() {
            return positions;
        }
        self.objects.retain(|object| !ids.contains(&object.id()));
        for id in positions.keys() {
            self.object_revisions.remove(id);
            self.hidden_objects.remove(id);
        }
        self.rebuild_object_index();
        self.touch();
        positions
    }

    fn restore_objects_bulk(&mut self, mut inserts: Vec<(usize, Object)>) {
        if inserts.is_empty() {
            return;
        }
        inserts.sort_by_key(|(index, _)| *index);
        let total = self.objects.len().saturating_add(inserts.len());
        let existing = std::mem::take(&mut self.objects);
        let mut existing = existing.into_iter();
        let mut inserts = inserts.into_iter().peekable();
        self.objects = Vec::with_capacity(total);
        for index in 0..total {
            if inserts.peek().is_some_and(|(insert_at, _)| *insert_at == index) {
                let (_, object) = inserts.next().expect("peeked insert disappeared");
                self.bump_next_object_id(object.id());
                self.objects.push(object);
            } else if let Some(object) = existing.next() {
                self.objects.push(object);
            }
        }
        self.objects.extend(existing);
        self.objects.extend(inserts.map(|(_, object)| object));
        self.rebuild_object_index();
        self.touch();
        let revision = self.revision;
        for object in &self.objects {
            self.object_revisions.entry(object.id()).or_insert(revision);
        }
    }

    pub(crate) fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    fn insert_layer_at(&mut self, index: usize, layer: Layer) {
        if self.layer(layer.id).is_some() {
            return;
        }
        self.next_layer_id = self.next_layer_id.max(layer.id.0.saturating_add(1));
        self.layers.insert(index.min(self.layers.len()), layer);
        self.touch();
    }

    /// Remove a layer by id. Returns false if the layer did not exist.
    pub(crate) fn delete_layer(&mut self, id: LayerId) -> bool {
        let before = self.layers.len();
        self.layers.retain(|l| l.id != id);
        self.deferred_layers.remove(&id);
        let removed = self.layers.len() < before;
        if removed {
            self.touch();
        }
        removed
    }

    /// Replace one layer and its objects with a complete snapshot while
    /// leaving every other layer untouched. Used when a saved layer is
    /// reloaded from disk but other layers still contain unsaved edits.
    #[cfg(any(not(target_arch = "wasm32"), test))]
    pub(crate) fn replace_layer_snapshot(&mut self, layer_index: usize, layer: Layer, objects: Vec<(usize, Object)>) {
        self.deferred_layers.remove(&layer.id);
        let old_object_ids = self
            .objects
            .iter()
            .filter(|object| object.layer() == layer.id)
            .map(Object::id)
            .collect::<std::collections::HashSet<_>>();
        self.remove_objects_bulk(&old_object_ids);
        self.layers.retain(|existing| existing.id != layer.id);
        self.insert_layer_at(layer_index, layer);
        self.restore_objects_bulk(objects);
    }

    /// Resolved RGBA for an object, following its layer when `ByLayer`.
    pub(crate) fn object_rgba(&self, object: &Object) -> [f32; 4] {
        match object.color() {
            ObjectColor::Fixed(rgba) => rgba,
            ObjectColor::ByLayer => self.layer(object.layer()).map(|layer| layer.color).unwrap_or([1.0, 1.0, 1.0, 1.0]),
        }
    }

    /// Resolved fill RGBA for an object. Filled polylines use their object color.
    pub(crate) fn object_fill_rgba(&self, object: &Object) -> [f32; 4] {
        self.object_rgba(object)
    }

    pub(crate) fn rename_layer(&mut self, id: LayerId, new_name: String) {
        if let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) {
            layer.name = new_name;
            self.touch();
        }
    }

    /// Set a layer's viewport visibility. Returns the new state, or `None`
    /// when the layer no longer exists.
    pub(crate) fn set_layer_loaded(&mut self, id: LayerId, loaded: bool) -> Option<bool> {
        let layer = self.layers.iter_mut().find(|layer| layer.id == id)?;
        if layer.loaded != loaded {
            layer.loaded = loaded;
            self.touch();
        }
        Some(loaded)
    }

    pub(crate) fn layer_id_by_name(&self, name: &str) -> Option<LayerId> {
        self.layers.iter().find(|layer| layer.name == name).map(|layer| layer.id)
    }

    pub(crate) fn first_layer_id(&self) -> Option<LayerId> {
        self.layers.first().map(|layer| layer.id)
    }

    pub(crate) fn ensure_default_layer(&mut self) -> LayerId {
        self.first_layer_id()
            .unwrap_or_else(|| self.add_layer("0".to_string(), Some(7), [1.0, 1.0, 1.0, 1.0], true, 0.0))
    }

    /// Assign every layer and object a runtime namespace. Project ids are local
    /// to a file; namespacing makes them safe to combine in one scene.
    pub(crate) fn apply_runtime_namespace(&mut self, namespace: u32) {
        self.apply_runtime_namespace_inner(namespace);
        self.touch();
    }

    fn apply_runtime_namespace_inner(&mut self, namespace: u32) {
        self.deferred_layers = std::mem::take(&mut self.deferred_layers)
            .into_iter()
            .map(|(id, stored)| (LayerId((u64::from(namespace) << 32) | (id.0 & u64::from(u32::MAX))), stored))
            .collect();
        const LOCAL_MASK: u64 = u32::MAX as u64;
        let prefix = u64::from(namespace) << 32;
        let runtime_id = |id: u64| prefix | (id & LOCAL_MASK);

        for layer in &mut self.layers {
            layer.id = LayerId(runtime_id(layer.id.0));
        }
        self.objects = self
            .objects
            .iter()
            .map(|object| object.with_id_and_layer(ObjectId(runtime_id(object.id().0)), LayerId(runtime_id(object.layer().0))))
            .collect();
        self.hidden_objects = self.hidden_objects.iter().map(|id| ObjectId(runtime_id(id.0))).collect();
        self.rebuild_object_index();
        // Ids changed identity: restamp everything at the current revision so
        // stale pre-namespace entries cannot alias new ids.
        self.object_revisions = self.objects.iter().map(|object| (object.id(), self.revision)).collect();
        self.next_layer_id = runtime_id(self.next_layer_id);
        self.next_object_id = runtime_id(self.next_object_id);
    }

    /// Append a layer and its objects while retaining their runtime ids.
    pub(crate) fn append_layer_snapshot<'a>(&mut self, layer: &Layer, objects: impl Iterator<Item = &'a Object>) {
        if self.layer(layer.id).is_none() {
            self.next_layer_id = self.next_layer_id.max(layer.id.0.saturating_add(1));
            self.layers.push(layer.clone());
        }
        for object in objects {
            self.insert_object(object.clone());
        }
        self.touch();
    }

    /// Bulk variant of [`Self::append_layer_snapshot`] for composite-scene
    /// construction: objects are appended without per-insert index
    /// maintenance or duplicate-id probing, while retaining their source
    /// revisions for renderer cache invalidation. The caller must guarantee
    /// unique ids (runtime namespacing does, across projects) and call
    /// [`Self::rebuild_object_index`] once after the final append.
    pub(crate) fn append_layer_snapshot_unindexed<'a>(&mut self, layer: &Layer, objects: impl Iterator<Item = (&'a Object, u64)>) {
        if self.layer(layer.id).is_none() {
            self.next_layer_id = self.next_layer_id.max(layer.id.0.saturating_add(1));
            self.layers.push(layer.clone());
        }
        for (object, source_revision) in objects {
            let id = object.id();
            self.bump_next_object_id(id);
            self.objects.push(object.clone());
            self.object_revisions.insert(id, source_revision);
        }
        self.touch();
    }

    pub(crate) fn get_object(&self, id: ObjectId) -> Option<&Object> {
        let fast = self.object_index.get(&id).and_then(|&index| self.objects.get(index)).filter(|object| object.id() == id);
        if fast.is_some() {
            return fast;
        }
        let slow = self.objects.iter().find(|object| object.id() == id);
        // The linear scan finding an object the index missed means the index
        // is corrupt; surface that in debug builds instead of hiding it
        // behind O(N) lookups.
        debug_assert!(slow.is_none(), "object_index out of sync for {id:?}");
        slow
    }

    /// Translate the object with `id` by `delta`. Returns `true` if found.
    pub(crate) fn translate_object(&mut self, id: ObjectId, delta: DVec3) -> bool {
        let Some(index) = self.object_position(id) else {
            return false;
        };
        match self.objects.get_mut(index) {
            Some(object) => {
                self.object_index.insert(id, index);
                object.translate(delta);
                self.touch_object(id);
                true
            }
            None => false,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Revision at which `id` was last mutated (0 for objects untouched since
    /// load - a fresh cache treats those uniformly).
    pub(crate) fn object_revision(&self, id: ObjectId) -> u64 {
        self.object_revisions.get(&id).copied().unwrap_or(0)
    }

    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn touch_object(&mut self, id: ObjectId) {
        self.touch();
        self.object_revisions.insert(id, self.revision);
    }

    /// Namespace-invariant content fingerprint used for dirty tracking.
    ///
    /// Per-object hashes are cached in `cache` keyed by object revision, so
    /// after an edit only the touched objects re-hash - unlike serializing
    /// the whole document to JSON, which interactive drags used to repeat on
    /// every pointer event. Ids are masked to their 32-bit local half so the
    /// fingerprint is identical before and after runtime namespacing.
    pub(crate) fn content_hash(&self, cache: &mut HashMap<ObjectId, (u64, u64)>) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let hashes = self.layer_content_hashes(cache);
        let mut hasher = DefaultHasher::new();
        for layer in &self.layers {
            let id = layer.id.0 & u64::from(u32::MAX);
            id.hash(&mut hasher);
            hashes.get(&id).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Hash logical layer contents identically whether its payload is resident
    /// or backed by a file. Residency cannot change the saved-content baseline.
    pub(crate) fn layer_content_hashes(&self, cache: &mut HashMap<ObjectId, (u64, u64)>) -> HashMap<u64, u64> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let payloads = self.payload_hashes(cache);
        self.layers
            .iter()
            .map(|layer| {
                let mut hasher = DefaultHasher::new();
                layer.name.hash(&mut hasher);
                layer.color_index.hash(&mut hasher);
                for channel in layer.color {
                    channel.to_bits().hash(&mut hasher);
                }
                layer.loaded.hash(&mut hasher);
                layer.elevation.to_bits().hash(&mut hasher);
                payloads.get(&layer.id).hash(&mut hasher);
                (layer.id.0 & u64::from(u32::MAX), hasher.finish())
            })
            .collect()
    }
}

/// A project-owned item that is not part of the design document: the things
/// the explorer lists under Triangulations, Block Models, Drillholes, Point
/// Clouds and Rasters.
///
/// [`SceneEntityId`] cannot serve here - rasters are project content but not
/// scene entities, and design objects are document content rather than items -
/// so this is the identity the undo history uses when it names an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ItemRef {
    Triangulation(triangulation::TriangulationId),
    BlockModel(block_model::BlockModelId),
    DrillHole(drill_hole::DrillHoleId),
    PointCloud(point_cloud::PointCloudId),
    Raster(raster::RasterTextureId),
}

impl ItemRef {
    pub(crate) fn from_entity(entity: SceneEntityId) -> Option<Self> {
        match entity {
            SceneEntityId::Object(_) => None,
            SceneEntityId::Triangulation(id) => Some(Self::Triangulation(id)),
            SceneEntityId::BlockModel(id) => Some(Self::BlockModel(id)),
            SceneEntityId::DrillHole(id) => Some(Self::DrillHole(id)),
            SceneEntityId::PointCloud(id) => Some(Self::PointCloud(id)),
        }
    }
}

/// Every persisted presentation field of one project item.
///
/// Deliberately one snapshot per item kind rather than a command per field:
/// visibility, colour, ramps and drapes are all small, they are all saved into
/// OMF the same way, and a new styling field then becomes a field here instead
/// of a new [`Command`] variant plus its undo, redo and byte-estimate arms.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ItemStyle {
    Triangulation {
        loaded: bool,
        color: [f32; 4],
        line_color: [f32; 4],
        line_weight: Option<f32>,
        raster_texture: Option<raster::RasterTextureId>,
        raster_opacity: f32,
    },
    BlockModel {
        loaded: bool,
        color: [f32; 4],
        slice: Option<block_model::BlockModelSlice>,
        active_color_variable: Option<String>,
        color_transfers: std::collections::BTreeMap<String, block_model::ColorTransferFunction>,
        hide_empty_color_values: bool,
    },
    DrillHole {
        loaded: bool,
        color: drill_hole::DrillColorState,
    },
    PointCloud {
        loaded: bool,
        color: [f32; 4],
        point_size: f32,
    },
    Raster {
        loaded: bool,
    },
}

impl ItemStyle {
    pub(crate) fn of_triangulation(item: &triangulation::OpenTriangulation) -> Self {
        Self::Triangulation {
            loaded: item.state.loaded,
            color: item.color,
            line_color: item.line_color,
            line_weight: item.line_weight,
            raster_texture: item.raster_texture,
            raster_opacity: item.raster_opacity,
        }
    }

    pub(crate) fn of_block_model(item: &block_model::OpenBlockModel) -> Self {
        Self::BlockModel {
            loaded: item.state.loaded,
            color: item.color,
            slice: item.slice,
            active_color_variable: item.active_color_variable.clone(),
            color_transfers: item.color_transfers.clone(),
            hide_empty_color_values: item.hide_empty_color_values,
        }
    }

    pub(crate) fn of_drill_hole(item: &drill_hole::OpenDrillHoleDataset) -> Self {
        Self::DrillHole {
            loaded: item.state.loaded,
            color: item.color.clone(),
        }
    }

    pub(crate) fn of_point_cloud(item: &point_cloud::OpenPointCloud) -> Self {
        Self::PointCloud {
            loaded: item.state.loaded,
            color: item.color,
            point_size: item.point_size,
        }
    }

    pub(crate) fn of_raster(item: &raster::OpenRasterTexture) -> Self {
        Self::Raster { loaded: item.state.loaded }
    }

    pub(crate) fn loaded(&self) -> bool {
        match self {
            Self::Triangulation { loaded, .. } | Self::BlockModel { loaded, .. } | Self::DrillHole { loaded, .. } | Self::PointCloud { loaded, .. } | Self::Raster { loaded } => {
                *loaded
            }
        }
    }

    /// The same style with only its visibility changed - how every show/hide
    /// action builds the `after` half of its command.
    pub(crate) fn with_loaded(mut self, value: bool) -> Self {
        match &mut self {
            Self::Triangulation { loaded, .. } | Self::BlockModel { loaded, .. } | Self::DrillHole { loaded, .. } | Self::PointCloud { loaded, .. } | Self::Raster { loaded } => {
                *loaded = value
            }
        }
        self
    }

    /// The same style with only its base colour changed. A raster has none:
    /// its colours are its pixels.
    pub(crate) fn with_color(mut self, value: [f32; 4]) -> Self {
        match &mut self {
            Self::Triangulation { color, .. } | Self::BlockModel { color, .. } | Self::PointCloud { color, .. } => *color = value,
            Self::DrillHole { .. } | Self::Raster { .. } => {}
        }
        self
    }

    /// The same triangulation style with a different raster draped over it, or
    /// none. Ignored for every other kind, which cannot carry a drape.
    pub(crate) fn with_raster_texture(mut self, value: Option<raster::RasterTextureId>) -> Self {
        if let Self::Triangulation { raster_texture, .. } = &mut self {
            *raster_texture = value;
        }
        self
    }

    /// The same block-model style with a different crop, or none.
    pub(crate) fn with_slice(mut self, value: Option<block_model::BlockModelSlice>) -> Self {
        if let Self::BlockModel { slice, .. } = &mut self {
            *slice = value;
        }
        self
    }

    fn estimated_bytes(&self) -> usize {
        size_of::<Self>()
            + match self {
                Self::BlockModel {
                    active_color_variable,
                    color_transfers,
                    ..
                } => {
                    active_color_variable.as_ref().map_or(0, String::len)
                        + color_transfers
                            .iter()
                            .map(|(name, transfer)| name.len() + size_of::<block_model::ColorTransferFunction>() + transfer.gradient_len() * size_of::<[f32; 4]>())
                            .fold(0usize, usize::saturating_add)
                }
                Self::DrillHole { color, .. } => {
                    color.active_field.as_ref().map_or(0, String::len)
                        + color.stops.len() * size_of::<drill_hole::DrillColorStop>()
                        + color
                            .categories
                            .iter()
                            .map(|category| size_of::<drill_hole::DrillCategoryColor>() + category.value.len())
                            .fold(0usize, usize::saturating_add)
                }
                _ => 0,
            }
    }
}

/// A whole project item, lifted out of the app while a deletion of it sits in
/// the undo stack. Boxed because a block model's metadata dwarfs every other
/// [`Command`] variant, and `Command`'s size is paid by every entry.
#[derive(Clone)]
pub(crate) enum OpenItem {
    Triangulation(Box<triangulation::OpenTriangulation>),
    BlockModel(Box<block_model::OpenBlockModel>),
    DrillHole(Box<drill_hole::OpenDrillHoleDataset>),
    PointCloud(Box<point_cloud::OpenPointCloud>),
    Raster(Box<raster::OpenRasterTexture>),
}

impl OpenItem {
    pub(crate) fn item_ref(&self) -> ItemRef {
        match self {
            Self::Triangulation(item) => ItemRef::Triangulation(item.id),
            Self::BlockModel(item) => ItemRef::BlockModel(item.id),
            Self::DrillHole(item) => ItemRef::DrillHole(item.id),
            Self::PointCloud(item) => ItemRef::PointCloud(item.id),
            Self::Raster(item) => ItemRef::Raster(item.id),
        }
    }

    /// Retained size, counted for the history's memory budget. Buffers shared
    /// through an `Arc` are counted in full: over-counting only makes the
    /// budget evict a deletion sooner, while under-counting would let a stack
    /// of deleted meshes outgrow it.
    fn estimated_bytes(&self) -> usize {
        match self {
            Self::Triangulation(item) => {
                size_of::<triangulation::OpenTriangulation>()
                    + item.name.len()
                    + item.mesh.estimated_bytes()
                    + item.edges.len() * size_of::<[u32; 2]>()
                    + item.surface_face_order.len() * size_of::<u32>()
            }
            Self::BlockModel(item) => size_of::<block_model::OpenBlockModel>() + item.name.len() + item.estimated_bytes(),
            Self::DrillHole(item) => size_of::<drill_hole::OpenDrillHoleDataset>() + item.name.len() + item.dataset.estimated_bytes(),
            Self::PointCloud(item) => {
                size_of::<point_cloud::OpenPointCloud>()
                    + item.name.len()
                    + item.points.len() * size_of::<DVec3>()
                    + item.colors.as_ref().map_or(0, |colors| colors.len() * size_of::<u32>())
            }
            Self::Raster(item) => size_of::<raster::OpenRasterTexture>() + item.name.len() + item.full_rgba.len() + item.rgba.len(),
        }
    }
}

impl std::fmt::Debug for OpenItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("OpenItem").field(&self.item_ref()).finish()
    }
}

/// Work an applied or reverted [`Command`] leaves for the caller: what to
/// invalidate, and any item that needs its background decode restarting.
///
/// A command mutates project state only; anything that touches the GPU, the
/// job queue or the session file is reported here and done by `App`, which is
/// what keeps the model layer free of app plumbing.
#[derive(Clone, Debug, Default)]
pub(crate) struct StepEffects {
    /// Design document content changed: rebuild scene geometry.
    pub(crate) document_changed: bool,
    /// A project item changed: refresh topology bounds and redraw.
    pub(crate) items_changed: bool,
    pub(crate) unloaded_items: Vec<ItemRef>,
    /// Items were added or removed: the session file no longer matches.
    pub(crate) membership_changed: bool,
    /// Block models whose active colour variable changed and whose values
    /// therefore have to be decoded again before they can render.
    pub(crate) block_model_decodes: Vec<block_model::BlockModelId>,
}

/// Everything a [`Command`] is allowed to mutate: the active project's design
/// document, its content epoch, and the project items the app holds alongside
/// it.
///
/// Undo used to reach only the document, which is why moving a drillhole
/// collar or hiding a surface could dirty a project but never be taken back.
/// Widening the target - rather than giving items a second, parallel history -
/// is what keeps one Ctrl-Z timeline over edits that touch both.
pub(crate) struct EditTarget<'a> {
    pub(crate) document: &'a mut Document,
    pub(crate) content: &'a mut project::ProjectContentState,
    pub(crate) triangulations: &'a mut Vec<triangulation::OpenTriangulation>,
    pub(crate) block_models: &'a mut Vec<block_model::OpenBlockModel>,
    pub(crate) drill_holes: &'a mut Vec<drill_hole::OpenDrillHoleDataset>,
    pub(crate) point_clouds: &'a mut Vec<point_cloud::OpenPointCloud>,
    pub(crate) rasters: &'a mut Vec<raster::OpenRasterTexture>,
    /// Accumulated by `apply`/`revert`; drained by the caller once the whole
    /// step is done, so one batch invalidates once.
    pub(crate) effects: StepEffects,
}

impl EditTarget<'_> {
    fn item_state_mut(&mut self, item: ItemRef) -> Option<&mut project::ProjectItemState> {
        match item {
            ItemRef::Triangulation(id) => self.triangulations.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.state),
            ItemRef::BlockModel(id) => self.block_models.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.state),
            ItemRef::DrillHole(id) => self.drill_holes.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.state),
            ItemRef::PointCloud(id) => self.point_clouds.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.state),
            ItemRef::Raster(id) => self.rasters.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.state),
        }
    }

    fn item_epoch(&self, item: ItemRef) -> Option<u64> {
        match item {
            ItemRef::Triangulation(id) => self.triangulations.iter().find(|entry| entry.id == id).map(|entry| entry.state.epoch()),
            ItemRef::BlockModel(id) => self.block_models.iter().find(|entry| entry.id == id).map(|entry| entry.state.epoch()),
            ItemRef::DrillHole(id) => self.drill_holes.iter().find(|entry| entry.id == id).map(|entry| entry.state.epoch()),
            ItemRef::PointCloud(id) => self.point_clouds.iter().find(|entry| entry.id == id).map(|entry| entry.state.epoch()),
            ItemRef::Raster(id) => self.rasters.iter().find(|entry| entry.id == id).map(|entry| entry.state.epoch()),
        }
    }

    /// Mark one item and the project's content as changed, and report the
    /// redraw the caller owes. Every item mutation funnels through here so a
    /// command can never dirty an item without also dirtying the project that
    /// serializes it.
    fn touch_item(&mut self, item: ItemRef) {
        if let Some(state) = self.item_state_mut(item) {
            state.touch();
        }
        self.content.touch();
        self.effects.items_changed = true;
    }

    /// Write a style snapshot back onto its item. A mismatched pair (a block
    /// model style handed a triangulation id) is ignored rather than partially
    /// applied, so a malformed command cannot leave an item half-styled.
    fn set_item_style(&mut self, item: ItemRef, style: &ItemStyle) {
        let was_loaded = self.item_state_mut(item).is_some_and(|state| state.loaded);
        let mut changed = false;
        match (item, style) {
            (
                ItemRef::Triangulation(id),
                ItemStyle::Triangulation {
                    loaded,
                    color,
                    line_color,
                    line_weight,
                    raster_texture,
                    raster_opacity,
                },
            ) => {
                if let Some(entry) = self.triangulations.iter_mut().find(|entry| entry.id == id) {
                    entry.state.loaded = *loaded;
                    entry.color = *color;
                    entry.line_color = *line_color;
                    entry.line_weight = *line_weight;
                    entry.raster_texture = *raster_texture;
                    entry.raster_opacity = *raster_opacity;
                    changed = true;
                }
            }
            (
                ItemRef::BlockModel(id),
                ItemStyle::BlockModel {
                    loaded,
                    color,
                    slice,
                    active_color_variable,
                    color_transfers,
                    hide_empty_color_values,
                },
            ) => {
                if let Some(entry) = self.block_models.iter_mut().find(|entry| entry.id == id) {
                    let variable_changed = entry.active_color_variable != *active_color_variable;
                    entry.state.loaded = *loaded;
                    entry.color = *color;
                    entry.slice = *slice;
                    entry.active_color_variable = active_color_variable.clone();
                    entry.color_transfers = color_transfers.clone();
                    entry.hide_empty_color_values = *hide_empty_color_values;
                    if variable_changed {
                        // The decoded values cached against the old variable
                        // no longer describe what is being coloured; drop them
                        // and let the caller queue the decode.
                        match active_color_variable {
                            Some(variable) => entry.begin_active_values_decode(variable),
                            None => entry.clear_active_values_cache(),
                        }
                        self.effects.block_model_decodes.push(id);
                    }
                    changed = true;
                }
            }
            (ItemRef::DrillHole(id), ItemStyle::DrillHole { loaded, color }) => {
                if let Some(entry) = self.drill_holes.iter_mut().find(|entry| entry.id == id) {
                    entry.state.loaded = *loaded;
                    entry.color = color.clone();
                    changed = true;
                }
            }
            (ItemRef::PointCloud(id), ItemStyle::PointCloud { loaded, color, point_size }) => {
                if let Some(entry) = self.point_clouds.iter_mut().find(|entry| entry.id == id) {
                    entry.state.loaded = *loaded;
                    entry.color = *color;
                    entry.point_size = *point_size;
                    changed = true;
                }
            }
            (ItemRef::Raster(id), ItemStyle::Raster { loaded }) => {
                if let Some(entry) = self.rasters.iter_mut().find(|entry| entry.id == id) {
                    entry.state.loaded = *loaded;
                    changed = true;
                }
            }
            _ => {}
        }
        if changed {
            if was_loaded && !style.loaded() {
                self.effects.unloaded_items.push(item);
            }
            self.touch_item(item);
        }
    }

    fn set_item_name(&mut self, item: ItemRef, name: &str) {
        let target = match item {
            ItemRef::Triangulation(id) => self.triangulations.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.name),
            ItemRef::BlockModel(id) => self.block_models.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.name),
            ItemRef::DrillHole(id) => self.drill_holes.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.name),
            ItemRef::PointCloud(id) => self.point_clouds.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.name),
            ItemRef::Raster(id) => self.rasters.iter_mut().find(|entry| entry.id == id).map(|entry| &mut entry.name),
        };
        let Some(target) = target else {
            return;
        };
        if target == name {
            return;
        }
        name.clone_into(target);
        self.touch_item(item);
    }

    /// Lift an item out of the project, returning where it stood so undo can
    /// put it back in explorer order rather than at the end.
    fn take_item(&mut self, item: ItemRef) -> Option<(usize, OpenItem)> {
        fn take<T>(items: &mut Vec<T>, index: Option<usize>) -> Option<(usize, T)> {
            index.map(|index| (index, items.remove(index)))
        }
        let taken = match item {
            ItemRef::Triangulation(id) => {
                let index = self.triangulations.iter().position(|entry| entry.id == id);
                take(self.triangulations, index).map(|(index, entry)| (index, OpenItem::Triangulation(Box::new(entry))))
            }
            ItemRef::BlockModel(id) => {
                let index = self.block_models.iter().position(|entry| entry.id == id);
                take(self.block_models, index).map(|(index, entry)| (index, OpenItem::BlockModel(Box::new(entry))))
            }
            ItemRef::DrillHole(id) => {
                let index = self.drill_holes.iter().position(|entry| entry.id == id);
                take(self.drill_holes, index).map(|(index, entry)| (index, OpenItem::DrillHole(Box::new(entry))))
            }
            ItemRef::PointCloud(id) => {
                let index = self.point_clouds.iter().position(|entry| entry.id == id);
                take(self.point_clouds, index).map(|(index, entry)| (index, OpenItem::PointCloud(Box::new(entry))))
            }
            ItemRef::Raster(id) => {
                let index = self.rasters.iter().position(|entry| entry.id == id);
                take(self.rasters, index).map(|(index, entry)| (index, OpenItem::Raster(Box::new(entry))))
            }
        };
        if taken.is_some() {
            self.content.touch();
            self.effects.items_changed = true;
            self.effects.membership_changed = true;
        }
        taken
    }

    /// Rewrite every captured hole as its original translated by `delta`, so a
    /// zero delta restores. The dataset is unshared once and its extent
    /// recomputed, and its revision carries the change through to the instance
    /// cache in `rendering::scene::drill_hole_cache`.
    fn move_collars(&mut self, dataset: drill_hole::DrillHoleId, originals: &[(usize, drill_hole::HolePlacement)], delta: DVec3) {
        let Some(entry) = self.drill_holes.iter_mut().find(|entry| entry.id == dataset) else {
            return;
        };
        let data = std::sync::Arc::make_mut(&mut entry.dataset);
        let mut moved = false;
        for (index, original) in originals {
            let Some(hole) = data.holes.get_mut(*index) else {
                continue;
            };
            hole.set_placement(original, delta);
            moved = true;
        }
        if !moved {
            return;
        }
        data.refresh_bounds();
        self.touch_item(ItemRef::DrillHole(dataset));
    }

    /// The turn counterpart of [`Self::move_collars`], hole for hole: each
    /// captured placement is rewritten swung about its own collar, so a
    /// selection turns in place rather than orbiting a shared centre.
    fn rotate_collars(&mut self, dataset: drill_hole::DrillHoleId, originals: &[(usize, drill_hole::HolePlacement)], rotation: drill_hole::CollarRotation) {
        let Some(entry) = self.drill_holes.iter_mut().find(|entry| entry.id == dataset) else {
            return;
        };
        let data = std::sync::Arc::make_mut(&mut entry.dataset);
        let mut turned = false;
        for (index, original) in originals {
            let Some(hole) = data.holes.get_mut(*index) else {
                continue;
            };
            hole.set_rotated_placement(original, rotation);
            turned = true;
        }
        if !turned {
            return;
        }
        data.refresh_bounds();
        self.touch_item(ItemRef::DrillHole(dataset));
    }

    /// Clear every hole pair named by either side of a tie-in edit, then lay
    /// `insert` across them. One connector to a pair, so a pair is cleared
    /// before it is written whichever direction the old one ran in.
    fn write_tie_ins(&mut self, dataset: drill_hole::DrillHoleId, remove: &[drill_hole::TieIn], insert: &[drill_hole::TieIn]) {
        let Some(entry) = self.drill_holes.iter_mut().find(|entry| entry.id == dataset) else {
            return;
        };
        let data = std::sync::Arc::make_mut(&mut entry.dataset);
        data.ties.retain(|tie| !remove.iter().chain(insert).any(|touched| tie.joins(touched.from, touched.to)));
        data.ties.extend(insert.iter().cloned());
        self.touch_item(ItemRef::DrillHole(dataset));
    }

    /// Replace the initiation on the collar named by either side of the edit.
    /// Other initiation points remain untouched, which makes each red card an
    /// independently undoable part of a multi-start round.
    fn set_initiation(&mut self, dataset: drill_hole::DrillHoleId, remove: Option<drill_hole::Initiation>, insert: Option<drill_hole::Initiation>) {
        let Some(entry) = self.drill_holes.iter_mut().find(|entry| entry.id == dataset) else {
            return;
        };
        let data = std::sync::Arc::make_mut(&mut entry.dataset);
        data.initiations
            .retain(|initiation| ![remove, insert].into_iter().flatten().any(|touched| touched.hole == initiation.hole));
        if let Some(initiation) = insert {
            data.initiations.push(initiation);
        }
        self.touch_item(ItemRef::DrillHole(dataset));
    }

    fn insert_item(&mut self, index: usize, item: OpenItem) {
        let handle = item.item_ref();
        match item {
            OpenItem::Triangulation(entry) => self.triangulations.insert(index.min(self.triangulations.len()), *entry),
            OpenItem::BlockModel(entry) => self.block_models.insert(index.min(self.block_models.len()), *entry),
            OpenItem::DrillHole(entry) => self.drill_holes.insert(index.min(self.drill_holes.len()), *entry),
            OpenItem::PointCloud(entry) => self.point_clouds.insert(index.min(self.point_clouds.len()), *entry),
            OpenItem::Raster(entry) => self.rasters.insert(index.min(self.rasters.len()), *entry),
        }
        // The restored item keeps the epoch it was deleted with, so undoing a
        // delete of never-edited content leaves the project exactly as clean
        // as it was before the delete.
        self.content.touch();
        self.effects.items_changed = true;
        self.effects.membership_changed = true;
        if let ItemRef::BlockModel(id) = handle {
            self.effects.block_model_decodes.push(id);
        }
    }
}

/// A reversible edit to the project: the design document, the project items
/// beside it, or both in one step.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    #[serde(skip)]
    Archived {
        backing: asset_storage::Backing,
        layers: Vec<LayerId>,
        items: Vec<ItemRef>,
    },
    AddObject(Object),
    DeleteObject {
        object: Object,
        /// Draw-order position captured when the delete is applied, so undo
        /// re-inserts the object where it was rather than on top.
        index: Option<usize>,
    },
    /// Replace an object's state (e.g. after a move). `before`/`after` share an id.
    Replace {
        before: Object,
        after: Object,
    },
    /// Apply/revert a sequence of commands atomically (single Ctrl-Z).
    Batch(Vec<Command>),
    /// Rename a layer (undo restores the old name).
    RenameLayer {
        id: LayerId,
        before: String,
        after: String,
    },
    /// Add a complete layer and all objects generated with it.
    AddLayerSnapshot {
        layer: Layer,
        objects: Vec<Object>,
    },
    /// Remove a complete layer and all objects on it.
    DeleteLayerSnapshot {
        layer: Layer,
        /// Original layer-stack position, restored exactly by undo.
        layer_index: usize,
        /// Original global draw-order position of every object on the layer.
        objects: Vec<(usize, Object)>,
    },
    /// Show or hide a design layer.
    SetLayerLoaded {
        id: LayerId,
        before: bool,
        after: bool,
    },
    /// Persisted per-object visibility (canvas Hide Selection / Reveal All).
    SetObjectHidden {
        id: ObjectId,
        before: bool,
        after: bool,
    },
    /// Replace every persisted presentation field of one project item. This
    /// is what makes showing, hiding, colouring, ramping and draping an item
    /// undoable, all through one variant.
    #[serde(skip)]
    SetItemStyle {
        item: ItemRef,
        before: ItemStyle,
        after: ItemStyle,
    },
    /// Rename a project item. Layers have [`Command::RenameLayer`]; they live
    /// in the document rather than in the item collections.
    #[serde(skip)]
    RenameItem {
        item: ItemRef,
        before: String,
        after: String,
    },
    /// Move drillhole collars, and the traces hanging off them, by `delta`.
    ///
    /// The captured placements are the originals, so applying writes
    /// `original + delta` and reverting writes `original` - the same
    /// rewrite-from-the-original rule a live Move Collar preview uses, which
    /// is what stops repeated apply/revert cycles accumulating drift.
    MoveCollars {
        dataset: drill_hole::DrillHoleId,
        /// Hole index within the dataset, paired with where it started.
        originals: Vec<(usize, drill_hole::HolePlacement)>,
        delta: DVec3,
    },
    /// Turn drillhole collars about themselves, each hole swinging around its
    /// own collar so the pattern stays laid out where it was surveyed.
    ///
    /// Captured placements are the originals on the same rewrite-from-the-
    /// original rule [`Command::MoveCollars`] follows: applying writes
    /// `original` turned by `rotation`, reverting writes `original` back, and
    /// no amount of apply/revert accumulates drift.
    RotateCollars {
        dataset: drill_hole::DrillHoleId,
        /// Hole index within the dataset, paired with where it started.
        originals: Vec<(usize, drill_hole::HolePlacement)>,
        rotation: drill_hole::CollarRotation,
    },
    /// Lay, replace or lift the surface connectors of a tie-in.
    ///
    /// The two sides name the same pairs of holes: `before` is whatever was
    /// tied across them, `after` what is tied across them now. Every pair
    /// either side names is cleared before the new ties go on, so overwriting
    /// an existing connector, laying a fresh one and cutting one out are all
    /// the same command - the last with an empty `after`.
    SetTieIns {
        dataset: drill_hole::DrillHoleId,
        before: Vec<drill_hole::TieIn>,
        after: Vec<drill_hole::TieIn>,
    },
    /// Move, set or lift the hole a round starts at.
    SetInitiation {
        dataset: drill_hole::DrillHoleId,
        before: Option<drill_hole::Initiation>,
        after: Option<drill_hole::Initiation>,
    },
    /// Add a complete project item. While the item is present, `added` is
    /// `None`; undo lifts it back into the command so redo can restore the
    /// exact same data and explorer position without cloning it.
    #[serde(skip)]
    AddItem {
        item: ItemRef,
        index: usize,
        added: Option<OpenItem>,
    },
    /// Delete a project item. The item itself is moved into the command when
    /// it is applied and moved back out when it is reverted, so a deletion
    /// sitting in the undo stack never holds a second copy of a mesh.
    #[serde(skip)]
    DeleteItem {
        item: ItemRef,
        /// Explorer position captured on apply, so undo restores the order.
        index: usize,
        /// `Some` exactly while the deletion stands.
        removed: Option<OpenItem>,
    },
}

impl Command {
    pub(crate) fn delete_object(object: Object) -> Self {
        Command::DeleteObject { object, index: None }
    }

    fn estimated_bytes(&self) -> usize {
        fn object_bytes(object: &Object) -> usize {
            size_of::<Object>()
                + match object {
                    Object::Polyline { verts, .. } => verts.len().saturating_mul(size_of::<PolyVertex>()),
                    Object::Text { content, .. } => content.len(),
                    Object::Point { .. } => 0,
                }
        }
        fn layer_bytes(layer: &Layer) -> usize {
            size_of::<Layer>() + layer.name.len()
        }

        size_of::<Command>()
            + match self {
                Command::AddObject(object) | Command::DeleteObject { object, .. } => object_bytes(object),
                Command::Replace { before, after } => object_bytes(before).saturating_add(object_bytes(after)),
                Command::Batch(commands) => commands.iter().map(Command::estimated_bytes).fold(0usize, usize::saturating_add),
                Command::RenameLayer { before, after, .. } => before.len().saturating_add(after.len()),
                Command::AddLayerSnapshot { layer, objects } => layer_bytes(layer).saturating_add(objects.iter().map(object_bytes).fold(0usize, usize::saturating_add)),
                Command::DeleteLayerSnapshot { layer, objects, .. } => {
                    layer_bytes(layer).saturating_add(objects.iter().map(|(_, object)| object_bytes(object)).fold(0usize, usize::saturating_add))
                }
                Command::SetLayerLoaded { .. } | Command::SetObjectHidden { .. } | Command::Archived { .. } => 0,
                Command::SetItemStyle { before, after, .. } => before.estimated_bytes().saturating_add(after.estimated_bytes()),
                Command::RenameItem { before, after, .. } => before.len().saturating_add(after.len()),
                Command::SetTieIns { before, after, .. } => before
                    .iter()
                    .chain(after)
                    .map(|tie| size_of::<drill_hole::TieIn>() + tie.product.len())
                    .fold(0usize, usize::saturating_add),
                Command::SetInitiation { .. } => 0,
                Command::MoveCollars { originals, .. } | Command::RotateCollars { originals, .. } => originals
                    .iter()
                    .map(|(_, placement)| size_of::<drill_hole::HolePlacement>() + placement.trace.len() * size_of::<drill_hole::TraceStation>())
                    .fold(0usize, usize::saturating_add),
                // Counted while the addition is undone and the item is held
                // in the redo command rather than by the project.
                Command::AddItem { added, .. } => added.as_ref().map_or(0, OpenItem::estimated_bytes),
                // Counted while the deletion stands. Reverting hands the item
                // back to the project, and the entry - now only a redo - is
                // over-counted until it is dropped, which is the safe way to
                // be wrong about a budget.
                Command::DeleteItem { removed, .. } => removed.as_ref().map_or(0, OpenItem::estimated_bytes),
            }
    }

    /// Whether `other` continues the same edit this command started, and so
    /// should extend its history entry rather than push a new one.
    ///
    /// This is what keeps one colour-picker drag to a single undo step: the
    /// picker reports a new value every frame it moves, and without merging a
    /// drag across the wheel leaves hundreds of entries behind. Matching is by
    /// target alone - the same item, or the same objects in the same order -
    /// because the caller only offers a merge while one pointer gesture is
    /// still running, which is the real boundary between two deliberate edits.
    fn merges_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Command::SetItemStyle { item, .. }, Command::SetItemStyle { item: candidate, .. }) => item == candidate,
            (Command::Replace { after, .. }, Command::Replace { before: candidate, .. }) => after.id() == candidate.id(),
            (Command::Batch(commands), Command::Batch(candidates)) => {
                !commands.is_empty() && commands.len() == candidates.len() && commands.iter().zip(candidates).all(|(command, candidate)| command.merges_with(candidate))
            }
            _ => false,
        }
    }

    /// Absorb a continuation of this edit: take its end state and keep our own
    /// start state, so the merged entry still undoes to where the edit began.
    fn merge_from(&mut self, other: Self) {
        match (self, other) {
            (Command::SetItemStyle { after, .. }, Command::SetItemStyle { after: latest, .. }) => *after = latest,
            (Command::Replace { after, .. }, Command::Replace { after: latest, .. }) => *after = latest,
            (Command::Batch(commands), Command::Batch(latest)) => {
                for (command, latest) in commands.iter_mut().zip(latest) {
                    command.merge_from(latest);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn required_layers(&self, undo: bool, document: &Document, into: &mut Vec<LayerId>) {
        match self {
            Self::Archived { layers, .. } => into.extend(layers.iter().copied()),
            Self::SetLayerLoaded { id, before, after } if if undo { *before } else { *after } => into.push(*id),
            Self::AddObject(object) | Self::DeleteObject { object, .. } => into.push(object.layer()),
            Self::Replace { before, .. } => into.push(before.layer()),
            Self::DeleteLayerSnapshot { layer, .. } => into.push(layer.id),
            Self::SetObjectHidden { id, .. } => {
                if let Some(object) = document.get_object(*id) {
                    into.push(object.layer());
                } else {
                    into.extend(document.deferred_layers.keys().copied());
                }
            }
            Self::Batch(commands) => {
                for command in commands {
                    command.required_layers(undo, document, into);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn required_items(&self, undo: bool, into: &mut Vec<ItemRef>) {
        match self {
            Self::Archived { items, .. } => into.extend(items.iter().copied()),
            Self::SetItemStyle { item, before, after } if (if undo { before } else { after }).loaded() => into.push(*item),
            Self::MoveCollars { dataset, .. } | Self::RotateCollars { dataset, .. } | Self::SetTieIns { dataset, .. } | Self::SetInitiation { dataset, .. } => {
                into.push(ItemRef::DrillHole(*dataset))
            }
            Self::Batch(commands) => {
                for command in commands {
                    command.required_items(undo, into);
                }
            }
            _ => {}
        }
    }

    /// Every project item this command touches, so the history can capture and
    /// restore their content epochs around an apply or a revert.
    fn touched_items(&self, into: &mut Vec<ItemRef>) {
        match self {
            Command::Archived { items, .. } => into.extend(items.iter().copied()),
            Command::SetItemStyle { item, .. } | Command::RenameItem { item, .. } | Command::AddItem { item, .. } | Command::DeleteItem { item, .. } => {
                if !into.contains(item) {
                    into.push(*item);
                }
            }
            Command::MoveCollars { dataset, .. } | Command::RotateCollars { dataset, .. } | Command::SetTieIns { dataset, .. } | Command::SetInitiation { dataset, .. } => {
                let item = ItemRef::DrillHole(*dataset);
                if !into.contains(&item) {
                    into.push(item);
                }
            }
            Command::Batch(commands) => {
                for command in commands {
                    command.touched_items(into);
                }
            }
            Command::AddObject(_)
            | Command::DeleteObject { .. }
            | Command::Replace { .. }
            | Command::RenameLayer { .. }
            | Command::AddLayerSnapshot { .. }
            | Command::DeleteLayerSnapshot { .. }
            | Command::SetLayerLoaded { .. }
            | Command::SetObjectHidden { .. } => {}
        }
    }

    fn apply(&mut self, target: &mut EditTarget<'_>) {
        match self {
            Command::Archived { .. } => unreachable!("restore archived history before applying a step"),
            Command::AddObject(object) => {
                target.document.insert_object(object.clone());
                target.effects.document_changed = true;
            }
            Command::DeleteObject { object, index } => {
                *index = target.document.object_position(object.id());
                target.document.remove_object(object.id());
                target.effects.document_changed = true;
            }
            Command::Replace { after, .. } => {
                target.document.replace_object(after.clone());
                target.effects.document_changed = true;
            }
            Command::Batch(cmds) => {
                if !cmds.is_empty() && cmds.iter().all(|command| matches!(command, Command::DeleteObject { .. })) {
                    let ids: std::collections::HashSet<_> = cmds
                        .iter()
                        .filter_map(|command| match command {
                            Command::DeleteObject { object, .. } => Some(object.id()),
                            _ => None,
                        })
                        .collect();
                    let positions = target.document.remove_objects_bulk(&ids);
                    for command in cmds {
                        if let Command::DeleteObject { object, index } = command {
                            *index = positions.get(&object.id()).copied();
                        }
                    }
                    target.effects.document_changed = true;
                } else {
                    for cmd in cmds {
                        cmd.apply(target);
                    }
                }
            }
            Command::RenameLayer { id, after, .. } => {
                target.document.rename_layer(*id, after.clone());
                target.effects.document_changed = true;
            }
            Command::AddLayerSnapshot { layer, objects } => {
                target.document.append_layer_snapshot(layer, objects.iter());
                target.effects.document_changed = true;
            }
            Command::DeleteLayerSnapshot { layer, objects, .. } => {
                let ids = objects.iter().map(|(_, object)| object.id()).collect::<std::collections::HashSet<_>>();
                target.document.remove_objects_bulk(&ids);
                target.document.delete_layer(layer.id);
                target.effects.document_changed = true;
            }
            Command::SetLayerLoaded { id, after, .. } => {
                target.document.set_layer_loaded(*id, *after);
                target.effects.document_changed = true;
            }
            Command::SetObjectHidden { id, after, .. } => {
                target.document.set_object_hidden(*id, *after);
                target.effects.document_changed = true;
            }
            Command::SetItemStyle { item, after, .. } => target.set_item_style(*item, after),
            Command::RenameItem { item, after, .. } => target.set_item_name(*item, after),
            Command::MoveCollars { dataset, originals, delta } => target.move_collars(*dataset, originals, *delta),
            Command::RotateCollars { dataset, originals, rotation } => target.rotate_collars(*dataset, originals, *rotation),
            Command::SetTieIns { dataset, before, after } => target.write_tie_ins(*dataset, before, after),
            Command::SetInitiation { dataset, before, after } => target.set_initiation(*dataset, *before, *after),
            Command::AddItem { item, index, added } => {
                if let Some(added_item) = added.take() {
                    debug_assert_eq!(added_item.item_ref(), *item);
                    target.insert_item(*index, added_item);
                }
            }
            Command::DeleteItem { item, index, removed } => {
                if let Some((taken_index, taken)) = target.take_item(*item) {
                    *index = taken_index;
                    *removed = Some(taken);
                }
            }
        }
    }

    fn revert(&mut self, target: &mut EditTarget<'_>) {
        match self {
            Command::Archived { .. } => unreachable!("restore archived history before applying a step"),
            Command::AddObject(object) => {
                target.document.remove_object(object.id());
                target.effects.document_changed = true;
            }
            Command::DeleteObject { object, index } => {
                match index {
                    Some(index) => target.document.insert_object_at(*index, object.clone()),
                    None => target.document.insert_object(object.clone()),
                }
                target.effects.document_changed = true;
            }
            Command::Replace { before, .. } => {
                target.document.replace_object(before.clone());
                target.effects.document_changed = true;
            }
            Command::Batch(cmds) => {
                if !cmds.is_empty() && cmds.iter().all(|command| matches!(command, Command::DeleteObject { .. })) {
                    let inserts = cmds
                        .iter()
                        .filter_map(|command| match command {
                            Command::DeleteObject { object, index: Some(index) } => Some((*index, object.clone())),
                            _ => None,
                        })
                        .collect();
                    target.document.restore_objects_bulk(inserts);
                    target.effects.document_changed = true;
                } else {
                    for cmd in cmds.iter_mut().rev() {
                        cmd.revert(target);
                    }
                }
            }
            Command::RenameLayer { id, before, .. } => {
                target.document.rename_layer(*id, before.clone());
                target.effects.document_changed = true;
            }
            Command::AddLayerSnapshot { layer, objects } => {
                for object in objects {
                    target.document.remove_object(object.id());
                }
                target.document.delete_layer(layer.id);
                target.effects.document_changed = true;
            }
            Command::DeleteLayerSnapshot { layer, layer_index, objects } => {
                target.document.insert_layer_at(*layer_index, layer.clone());
                target.document.restore_objects_bulk(objects.clone());
                target.effects.document_changed = true;
            }
            Command::SetLayerLoaded { id, before, .. } => {
                target.document.set_layer_loaded(*id, *before);
                target.effects.document_changed = true;
            }
            Command::SetObjectHidden { id, before, .. } => {
                target.document.set_object_hidden(*id, *before);
                target.effects.document_changed = true;
            }
            Command::SetItemStyle { item, before, .. } => target.set_item_style(*item, before),
            Command::RenameItem { item, before, .. } => target.set_item_name(*item, before),
            // A zero delta puts every captured hole back exactly where it was.
            Command::MoveCollars { dataset, originals, .. } => target.move_collars(*dataset, originals, DVec3::ZERO),
            // And the identity turn does the same for a rotation.
            Command::RotateCollars { dataset, originals, .. } => target.rotate_collars(*dataset, originals, drill_hole::CollarRotation::IDENTITY),
            // The same clear-then-write, with the two sides swapped: the pairs
            // the edit touched are named by both of them.
            Command::SetTieIns { dataset, before, after } => target.write_tie_ins(*dataset, after, before),
            Command::SetInitiation { dataset, before, after } => target.set_initiation(*dataset, *after, *before),
            Command::AddItem { item, index, added } => {
                if let Some((taken_index, taken)) = target.take_item(*item) {
                    *index = taken_index;
                    *added = Some(taken);
                }
            }
            Command::DeleteItem { index, removed, .. } => {
                if let Some(item) = removed.take() {
                    target.insert_item(*index, item);
                }
            }
        }
    }
}

/// The content epochs of everything one history entry touches, captured either
/// side of its command.
///
/// Restoring an epoch is what tells the project "this is the content you had
/// then", so undoing back to the last save clears the dirty markers instead of
/// leaving them stuck on. The project's own epoch is always captured: an item
/// edit dirties the aggregate that serializes it too.
#[derive(Clone, Debug, Default)]
struct EpochSnapshot {
    project: u64,
    items: Vec<(ItemRef, u64)>,
}

impl EpochSnapshot {
    fn capture(target: &EditTarget<'_>, items: &[ItemRef]) -> Self {
        Self {
            project: target.content.epoch(),
            items: items.iter().filter_map(|item| target.item_epoch(*item).map(|epoch| (*item, epoch))).collect(),
        }
    }

    fn epoch_of(&self, item: ItemRef) -> Option<u64> {
        self.items.iter().find(|(other, _)| *other == item).map(|(_, epoch)| *epoch)
    }

    /// Roll epochs forward or back to this snapshot, for the project and for
    /// every item that was still holding exactly what this entry `left_behind`
    /// when the step began.
    ///
    /// A mismatch means something outside the history has changed that item
    /// since - a background job, an import, an unrecorded edit - so its
    /// content corresponds to neither epoch and it keeps the fresh one the
    /// step just minted. Without the guard, undo could mark genuinely unsaved
    /// content as saved.
    fn restore(&self, target: &mut EditTarget<'_>, before_step: &Self, left_behind: &Self) {
        if before_step.project == left_behind.project {
            target.content.restore_epoch(self.project);
        }
        for (item, epoch) in &self.items {
            let current = before_step.epoch_of(*item);
            if current.is_none() || current != left_behind.epoch_of(*item) {
                continue;
            }
            if let Some(state) = target.item_state_mut(*item) {
                state.restore_epoch(*epoch);
            }
        }
    }
}

struct HistoryEntry {
    command: Command,
    /// Epochs before the command was first applied, and after it - so undo and
    /// redo each put back the identity that matches the content they restore.
    before: EpochSnapshot,
    after: EpochSnapshot,
    estimated_bytes: usize,
    sequence: u64,
}

#[derive(Default)]
struct ProjectHistory {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

/// Undo/redo history for the one open project.
pub(crate) struct History {
    archive_revision: u64,
    project: ProjectHistory,
    retained_bytes: usize,
    next_sequence: u64,
    max_retained_bytes: usize,
    /// Whether the newest undo entry was pushed by an edit whose gesture is
    /// still running, and so may still be extended. Cleared the moment the
    /// gesture ends, which is what stops two separate drags of the same
    /// colour picker collapsing into one undo step.
    open_run: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            archive_revision: 0,
            project: ProjectHistory::default(),
            retained_bytes: 0,
            next_sequence: 0,
            // Large enough for production geometry edits, but finite so a
            // long session cannot retain unbounded cloned meshes forever.
            max_retained_bytes: 1024 * 1024 * 1024,
            open_run: false,
        }
    }
}

impl History {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn required_layers(&self, undo: bool, document: &Document) -> Vec<LayerId> {
        let mut layers = Vec::new();
        if let Some(entry) = (if undo { &self.project.undo } else { &self.project.redo }).last() {
            entry.command.required_layers(undo, document, &mut layers);
        }
        layers
    }

    pub(crate) fn required_items(&self, undo: bool) -> Vec<ItemRef> {
        let mut items = Vec::new();
        if let Some(entry) = (if undo { &self.project.undo } else { &self.project.redo }).last() {
            entry.command.required_items(undo, &mut items);
        }
        items
    }

    pub(crate) fn activate(&mut self, _runtime_id: u32) {}

    pub(crate) fn deactivate(&mut self) {}

    pub(crate) fn remove_project(&mut self, _runtime_id: u32) {
        self.clear();
    }

    pub(crate) fn clear(&mut self) {
        self.archive_revision = self.archive_revision.wrapping_add(1);
        self.retained_bytes = 0;
        self.open_run = false;
        self.project.undo.clear();
        self.project.redo.clear();
    }

    /// Apply `command` and record it.
    ///
    /// `continuing` says the edit belongs to a gesture the user has not
    /// finished - a pointer still down on a colour wheel or a slider. Such an
    /// edit extends the entry an identical earlier edit left, instead of
    /// pushing its own, so the whole gesture undoes at once.
    pub(crate) fn execute(&mut self, target: &mut EditTarget<'_>, mut command: Command, continuing: bool) {
        let mut items = Vec::new();
        command.touched_items(&mut items);
        let before = EpochSnapshot::capture(target, &items);
        command.apply(target);
        let after = EpochSnapshot::capture(target, &items);
        self.push_entry(command, before, after, continuing);
    }

    /// Apply and record a command produced by a project-scoped background job.
    /// The caller validates the open-project runtime token before invoking it.
    /// A job's result is never part of a pointer gesture, so it never merges.
    pub(crate) fn execute_for(&mut self, _runtime_id: u32, target: &mut EditTarget<'_>, command: Command) {
        self.execute(target, command, false);
    }

    /// End any gesture in progress, so the next edit starts its own entry.
    /// Called when the pointer stops driving a widget.
    pub(crate) fn end_interaction(&mut self) {
        self.open_run = false;
    }

    /// Record a command whose effect is already applied (e.g. an interactive
    /// drag-move committed on mouse release).
    ///
    /// Only for document edits: the epochs either side are taken as they stand
    /// now, so an item mutation recorded this way could not be un-dirtied by
    /// undo. Item edits go through [`Self::execute`], which applies the
    /// command itself and therefore sees both epochs.
    pub(crate) fn push_applied(&mut self, project_epoch: u64, command: Command) {
        debug_assert!(
            {
                let mut items = Vec::new();
                command.touched_items(&mut items);
                items.is_empty()
            },
            "push_applied cannot capture the epochs an item edit needs; use execute"
        );
        let snapshot = EpochSnapshot {
            project: project_epoch,
            items: Vec::new(),
        };
        self.push_entry(command, snapshot.clone(), snapshot, false);
    }

    fn push_entry(&mut self, command: Command, before: EpochSnapshot, after: EpochSnapshot, continuing: bool) {
        self.archive_revision = self.archive_revision.wrapping_add(1);
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(self.project.redo.iter().map(|entry| entry.estimated_bytes).sum::<usize>());
        self.project.redo.clear();

        if continuing
            && let Some(entry) = self.project.undo.last_mut()
            && self.open_run
            && entry.command.merges_with(&command)
        {
            entry.command.merge_from(command);
            // The entry keeps the epochs the gesture started from and takes
            // the ones it now stands at, so undoing it still returns to where
            // the gesture began.
            entry.after = after;
            self.retained_bytes = self.retained_bytes.saturating_sub(entry.estimated_bytes);
            entry.estimated_bytes = entry.command.estimated_bytes();
            self.retained_bytes = self.retained_bytes.saturating_add(entry.estimated_bytes);
            self.enforce_memory_budget();
            return;
        }

        self.open_run = continuing;
        let estimated_bytes = command.estimated_bytes();
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.project.undo.push(HistoryEntry {
            command,
            before,
            after,
            estimated_bytes,
            sequence,
        });
        self.retained_bytes = self.retained_bytes.saturating_add(estimated_bytes);
        self.enforce_memory_budget();
    }

    /// Revert the most recent command. Returns `true` if something was undone.
    pub(crate) fn undo(&mut self, target: &mut EditTarget<'_>) -> bool {
        self.archive_revision = self.archive_revision.wrapping_add(1);
        // Whatever gesture was running, the entry it was extending is no
        // longer the newest one.
        self.open_run = false;
        match self.project.undo.pop() {
            Some(mut entry) => {
                let mut items = Vec::new();
                entry.command.touched_items(&mut items);
                // Captured before the revert, so `restore` can tell content it
                // left behind from content something else has since changed.
                let current = EpochSnapshot::capture(target, &items);
                entry.command.revert(target);
                entry.before.restore(target, &current, &entry.after);
                self.retained_bytes = self.retained_bytes.saturating_sub(entry.estimated_bytes);
                entry.estimated_bytes = entry.command.estimated_bytes();
                self.retained_bytes = self.retained_bytes.saturating_add(entry.estimated_bytes);
                self.project.redo.push(entry);
                true
            }
            None => false,
        }
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.project.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.project.redo.is_empty()
    }

    /// Re-apply the most recently undone command. Returns `true` on success.
    pub(crate) fn redo(&mut self, target: &mut EditTarget<'_>) -> bool {
        self.archive_revision = self.archive_revision.wrapping_add(1);
        self.open_run = false;
        match self.project.redo.pop() {
            Some(mut entry) => {
                let mut items = Vec::new();
                entry.command.touched_items(&mut items);
                let current = EpochSnapshot::capture(target, &items);
                entry.command.apply(target);
                entry.after.restore(target, &current, &entry.before);
                self.retained_bytes = self.retained_bytes.saturating_sub(entry.estimated_bytes);
                entry.estimated_bytes = entry.command.estimated_bytes();
                self.retained_bytes = self.retained_bytes.saturating_add(entry.estimated_bytes);
                self.project.undo.push(entry);
                true
            }
            None => false,
        }
    }

    fn enforce_memory_budget(&mut self) {
        while self.retained_bytes > self.max_retained_bytes {
            let oldest_undo = self.project.undo.iter().enumerate().min_by_key(|(_, entry)| entry.sequence);
            let oldest_redo = self.project.redo.iter().enumerate().min_by_key(|(_, entry)| entry.sequence);
            let oldest = match (oldest_undo, oldest_redo) {
                (Some((undo_index, undo)), Some((_redo_index, redo))) if undo.sequence <= redo.sequence => Some((true, undo_index)),
                (Some(_), Some((redo_index, _))) => Some((false, redo_index)),
                (Some((undo_index, _)), None) => Some((true, undo_index)),
                (None, Some((redo_index, _))) => Some((false, redo_index)),
                (None, None) => None,
            };
            let Some((from_undo, index)) = oldest else {
                self.retained_bytes = 0;
                break;
            };
            if from_undo {
                let entry = self.project.undo.remove(index);
                self.retained_bytes = self.retained_bytes.saturating_sub(entry.estimated_bytes);
            } else {
                // Redo entries form a dependency chain (the last item must be
                // replayed first). Evicting a middle/last predecessor would
                // make the remaining redo commands invalid, so discard the
                // complete redo branch together.
                let bytes = self.project.redo.iter().map(|entry| entry.estimated_bytes).sum::<usize>();
                self.project.redo.clear();
                self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
            }
        }
    }
}
