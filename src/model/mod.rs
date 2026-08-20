//! Editable mine-design document model and source of truth for the editor.

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
pub(crate) mod pidb;
pub(crate) mod plot;
pub(crate) mod point_cloud;
pub(crate) mod progress;
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

/// Stable identity used by rendering, selection and spatial queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SceneEntityId {
    Object(ObjectId),
    Triangulation(triangulation::TriangulationId),
    BlockModel(block_model::BlockModelId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Layer {
    pub(crate) id: LayerId,
    pub(crate) name: String,
    pub(crate) color_index: Option<u8>,
    /// Resolved RGBA used for rendering objects with `ObjectColor::ByLayer`.
    pub(crate) color: [f32; 4],
    pub(crate) visible: bool,
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
/// polygon; vertices carry bulges so arcs/circles are preserved.
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

    /// Short human-readable variant name, for diagnostics/logging.
    pub(crate) fn kind_name(&self) -> &'static str {
        match self {
            Object::Point { .. } => "Point",
            Object::Polyline { closed: true, .. } => "Polygon",
            Object::Polyline { closed: false, .. } => "Polyline",
            Object::Text { .. } => "Text",
        }
    }

    pub(crate) fn color(&self) -> ObjectColor {
        match self {
            Object::Point { color, .. } | Object::Polyline { color, .. } | Object::Text { color, .. } => *color,
        }
    }

    /// Content fingerprint for dirty tracking. Ids are masked to their local
    /// 32-bit half so the hash is stable across runtime namespacing; floats
    /// hash by bit pattern so a reverted edit hashes back to the saved value.
    pub(crate) fn content_hash(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        const LOCAL_MASK: u64 = u32::MAX as u64;

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
        (self.id().0 & LOCAL_MASK).hash(&mut hasher);
        (self.layer().0 & LOCAL_MASK).hash(&mut hasher);
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

    pub(crate) fn set_z_position(&mut self, z: f64) {
        match self {
            Object::Point { pos, .. } | Object::Text { pos, .. } => pos.z = z,
            Object::Polyline { verts, .. } => {
                for vertex in verts {
                    vertex.pos.z = z;
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

    /// Convert impossible imported two-vertex polygons into open polylines.
    ///
    /// Some external design formats encode ordinary two-point strings as
    /// closed polygons. Importers use this before constructing a PIDB so the
    /// resulting document satisfies the three-vertex polygon invariant.
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
                bail!("layer '{}' has id {} outside the 32-bit PIDB id range", layer.name, layer.id.0);
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
                bail!("{} object has id {} outside the 32-bit PIDB id range", object.kind_name(), id.0);
            }
            if !object_ids.insert(id) {
                bail!("duplicate object id {}", id.0);
            }
            if !layer_ids.contains(&object.layer()) {
                bail!("{} object {} references missing layer {}", object.kind_name(), id.0, object.layer().0);
            }
            object.validate_geometry().map_err(|error| anyhow::anyhow!("object {}: {error}", id.0))?;
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

    pub(crate) fn add_layer(&mut self, name: String, color_index: Option<u8>, color: [f32; 4], visible: bool, elevation: f32) -> LayerId {
        let id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        self.layers.push(Layer {
            id,
            name,
            color_index,
            color,
            visible,
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

    /// Assign every layer and object a runtime namespace. PIDB ids are local
    /// to a file; namespacing makes them safe to combine in one scene.
    pub(crate) fn apply_runtime_namespace(&mut self, namespace: u32) {
        self.apply_runtime_namespace_inner(namespace);
        self.touch();
    }

    fn apply_runtime_namespace_inner(&mut self, namespace: u32) {
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
    /// load — a fresh cache treats those uniformly).
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
    /// after an edit only the touched objects re-hash — unlike serializing
    /// the whole document to JSON, which interactive drags used to repeat on
    /// every pointer event. Ids are masked to their 32-bit local half so the
    /// fingerprint is identical before and after runtime namespacing.
    pub(crate) fn content_hash(&self, cache: &mut HashMap<ObjectId, (u64, u64)>) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        const LOCAL_MASK: u64 = u32::MAX as u64;

        let mut hasher = DefaultHasher::new();
        // These counters are serialized too. Masking keeps the savepoint
        // stable after a project receives its runtime namespace, while still
        // detecting an allocated-but-not-yet-inserted id.
        (self.next_layer_id & LOCAL_MASK).hash(&mut hasher);
        (self.next_object_id & LOCAL_MASK).hash(&mut hasher);
        self.layers.len().hash(&mut hasher);
        for layer in &self.layers {
            (layer.id.0 & LOCAL_MASK).hash(&mut hasher);
            layer.name.hash(&mut hasher);
            layer.color_index.hash(&mut hasher);
            for channel in layer.color {
                channel.to_bits().hash(&mut hasher);
            }
            layer.visible.hash(&mut hasher);
            layer.elevation.to_bits().hash(&mut hasher);
        }
        self.objects.len().hash(&mut hasher);
        for object in &self.objects {
            let id = object.id();
            let object_revision = self.object_revision(id);
            let object_hash = match cache.get(&id) {
                Some(&(revision, hash)) if revision == object_revision => hash,
                _ => {
                    let hash = object.content_hash();
                    cache.insert(id, (object_revision, hash));
                    hash
                }
            };
            object_hash.hash(&mut hasher);
        }
        // Prune deleted objects once stale entries dominate the cache.
        if cache.len() > self.objects.len().saturating_mul(2).max(64) {
            cache.retain(|id, _| self.object_index.contains_key(id));
        }
        hasher.finish()
    }

    /// Namespace-invariant per-layer fingerprints for layer-level dirty
    /// tracking, keyed by each layer id's 32-bit local half. A layer's hash
    /// covers its own fields plus its objects' content in draw order. Shares
    /// the per-object hash `cache` with [`Self::content_hash`].
    pub(crate) fn layer_content_hashes(&self, cache: &mut HashMap<ObjectId, (u64, u64)>) -> HashMap<u64, u64> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        const LOCAL_MASK: u64 = u32::MAX as u64;

        let mut hashers: HashMap<u64, DefaultHasher> = HashMap::with_capacity(self.layers.len());
        for layer in &self.layers {
            let mut hasher = DefaultHasher::new();
            layer.name.hash(&mut hasher);
            layer.color_index.hash(&mut hasher);
            for channel in layer.color {
                channel.to_bits().hash(&mut hasher);
            }
            layer.visible.hash(&mut hasher);
            layer.elevation.to_bits().hash(&mut hasher);
            hashers.insert(layer.id.0 & LOCAL_MASK, hasher);
        }
        for object in &self.objects {
            let id = object.id();
            let object_revision = self.object_revision(id);
            let object_hash = match cache.get(&id) {
                Some(&(revision, hash)) if revision == object_revision => hash,
                _ => {
                    let hash = object.content_hash();
                    cache.insert(id, (object_revision, hash));
                    hash
                }
            };
            if let Some(hasher) = hashers.get_mut(&(object.layer().0 & LOCAL_MASK)) {
                object_hash.hash(hasher);
            }
        }
        hashers.into_iter().map(|(id, hasher)| (id, hasher.finish())).collect()
    }
}

/// A reversible edit to the document.
#[derive(Clone, Debug)]
pub(crate) enum Command {
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
            }
    }

    fn apply(&mut self, doc: &mut Document) {
        match self {
            Command::AddObject(object) => doc.insert_object(object.clone()),
            Command::DeleteObject { object, index } => {
                *index = doc.object_position(object.id());
                doc.remove_object(object.id());
            }
            Command::Replace { after, .. } => {
                doc.replace_object(after.clone());
            }
            Command::Batch(cmds) => {
                if cmds.iter().all(|command| matches!(command, Command::DeleteObject { .. })) {
                    let ids: std::collections::HashSet<_> = cmds
                        .iter()
                        .filter_map(|command| match command {
                            Command::DeleteObject { object, .. } => Some(object.id()),
                            _ => None,
                        })
                        .collect();
                    let positions = doc.remove_objects_bulk(&ids);
                    for command in cmds {
                        if let Command::DeleteObject { object, index } = command {
                            *index = positions.get(&object.id()).copied();
                        }
                    }
                } else {
                    for cmd in cmds {
                        cmd.apply(doc);
                    }
                }
            }
            Command::RenameLayer { id, after, .. } => doc.rename_layer(*id, after.clone()),
            Command::AddLayerSnapshot { layer, objects } => doc.append_layer_snapshot(layer, objects.iter()),
            Command::DeleteLayerSnapshot { layer, objects, .. } => {
                let ids = objects.iter().map(|(_, object)| object.id()).collect::<std::collections::HashSet<_>>();
                doc.remove_objects_bulk(&ids);
                doc.delete_layer(layer.id);
            }
        }
    }

    fn revert(&self, doc: &mut Document) {
        match self {
            Command::AddObject(object) => {
                doc.remove_object(object.id());
            }
            Command::DeleteObject { object, index } => match index {
                Some(index) => doc.insert_object_at(*index, object.clone()),
                None => doc.insert_object(object.clone()),
            },
            Command::Replace { before, .. } => {
                doc.replace_object(before.clone());
            }
            Command::Batch(cmds) => {
                if cmds.iter().all(|command| matches!(command, Command::DeleteObject { .. })) {
                    let inserts = cmds
                        .iter()
                        .filter_map(|command| match command {
                            Command::DeleteObject { object, index: Some(index) } => Some((*index, object.clone())),
                            _ => None,
                        })
                        .collect();
                    doc.restore_objects_bulk(inserts);
                } else {
                    for cmd in cmds.iter().rev() {
                        cmd.revert(doc);
                    }
                }
            }
            Command::RenameLayer { id, before, .. } => doc.rename_layer(*id, before.clone()),
            Command::AddLayerSnapshot { layer, objects } => {
                for object in objects {
                    doc.remove_object(object.id());
                }
                doc.delete_layer(layer.id);
            }
            Command::DeleteLayerSnapshot { layer, layer_index, objects } => {
                doc.insert_layer_at(*layer_index, layer.clone());
                doc.restore_objects_bulk(objects.clone());
            }
        }
    }
}

struct HistoryEntry {
    command: Command,
    estimated_bytes: usize,
    sequence: u64,
}

#[derive(Default)]
struct ProjectHistory {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

/// Per-project undo/redo histories with a high global retained-memory cap.
/// Switching PIDBs changes the active stack instead of deleting it.
pub(crate) struct History {
    projects: HashMap<u32, ProjectHistory>,
    active_project: Option<u32>,
    retained_bytes: usize,
    next_sequence: u64,
    max_retained_bytes: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            projects: HashMap::new(),
            active_project: None,
            retained_bytes: 0,
            next_sequence: 0,
            // Large enough for production geometry edits, but finite so a
            // long session cannot retain unbounded cloned meshes forever.
            max_retained_bytes: 1024 * 1024 * 1024,
        }
    }
}

impl History {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn activate(&mut self, runtime_id: u32) {
        self.active_project = Some(runtime_id);
        self.projects.entry(runtime_id).or_default();
    }

    pub(crate) fn deactivate(&mut self) {
        self.active_project = None;
    }

    pub(crate) fn remove_project(&mut self, runtime_id: u32) {
        if let Some(history) = self.projects.remove(&runtime_id) {
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(history.undo.iter().chain(&history.redo).map(|entry| entry.estimated_bytes).sum::<usize>());
        }
        if self.active_project == Some(runtime_id) {
            self.active_project = None;
        }
    }

    pub(crate) fn clear(&mut self) {
        let Some(runtime_id) = self.active_project else {
            return;
        };
        self.clear_project(runtime_id);
    }

    pub(crate) fn clear_project(&mut self, runtime_id: u32) {
        if let Some(history) = self.projects.get_mut(&runtime_id) {
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(history.undo.iter().chain(&history.redo).map(|entry| entry.estimated_bytes).sum::<usize>());
            history.undo.clear();
            history.redo.clear();
        }
    }

    pub(crate) fn execute(&mut self, doc: &mut Document, mut command: Command) {
        command.apply(doc);
        self.push_applied(command);
    }

    /// Apply and record a command against a specific project, even when a
    /// background job completes after the user has activated another project.
    pub(crate) fn execute_for(&mut self, runtime_id: u32, doc: &mut Document, mut command: Command) {
        command.apply(doc);
        self.push_applied_for(runtime_id, command);
    }

    /// Record a command whose effect is already applied to the document
    /// (e.g. an interactive drag-move committed on mouse release).
    pub(crate) fn push_applied(&mut self, command: Command) {
        let Some(runtime_id) = self.active_project else {
            return;
        };
        self.push_applied_for(runtime_id, command);
    }

    fn push_applied_for(&mut self, runtime_id: u32, command: Command) {
        let history = self.projects.entry(runtime_id).or_default();
        self.retained_bytes = self.retained_bytes.saturating_sub(history.redo.iter().map(|entry| entry.estimated_bytes).sum::<usize>());
        history.redo.clear();
        let estimated_bytes = command.estimated_bytes();
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        history.undo.push(HistoryEntry {
            command,
            estimated_bytes,
            sequence,
        });
        self.retained_bytes = self.retained_bytes.saturating_add(estimated_bytes);
        self.enforce_memory_budget();
    }

    /// Revert the most recent command. Returns `true` if something was undone.
    pub(crate) fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(history) = self.active_project.and_then(|runtime_id| self.projects.get_mut(&runtime_id)) else {
            return false;
        };
        match history.undo.pop() {
            Some(entry) => {
                entry.command.revert(doc);
                history.redo.push(entry);
                true
            }
            None => false,
        }
    }

    pub(crate) fn can_undo(&self) -> bool {
        self.active_project
            .and_then(|runtime_id| self.projects.get(&runtime_id))
            .is_some_and(|history| !history.undo.is_empty())
    }

    pub(crate) fn can_redo(&self) -> bool {
        self.active_project
            .and_then(|runtime_id| self.projects.get(&runtime_id))
            .is_some_and(|history| !history.redo.is_empty())
    }

    /// Re-apply the most recently undone command. Returns `true` on success.
    pub(crate) fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(history) = self.active_project.and_then(|runtime_id| self.projects.get_mut(&runtime_id)) else {
            return false;
        };
        match history.redo.pop() {
            Some(mut entry) => {
                entry.command.apply(doc);
                history.undo.push(entry);
                true
            }
            None => false,
        }
    }

    fn enforce_memory_budget(&mut self) {
        while self.retained_bytes > self.max_retained_bytes {
            let oldest = self
                .projects
                .iter()
                .flat_map(|(&runtime_id, history)| {
                    history
                        .undo
                        .iter()
                        .enumerate()
                        .map(move |(index, entry)| (entry.sequence, runtime_id, true, index))
                        .chain(history.redo.iter().enumerate().map(move |(index, entry)| (entry.sequence, runtime_id, false, index)))
                })
                .min_by_key(|(sequence, _, _, _)| *sequence);
            let Some((_, runtime_id, from_undo, index)) = oldest else {
                self.retained_bytes = 0;
                break;
            };
            let history = self.projects.get_mut(&runtime_id).expect("history disappeared");
            if from_undo {
                let entry = history.undo.remove(index);
                self.retained_bytes = self.retained_bytes.saturating_sub(entry.estimated_bytes);
            } else {
                // Redo entries form a dependency chain (the last item must be
                // replayed first). Evicting a middle/last predecessor would
                // make the remaining redo commands invalid, so discard that
                // project's complete redo branch together.
                let bytes = history.redo.iter().map(|entry| entry.estimated_bytes).sum::<usize>();
                history.redo.clear();
                self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
            }
        }
    }
}
