use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    io::Cursor,
    path::{Path, PathBuf},
};
#[cfg(not(target_arch = "wasm32"))]
use std::{fs, io::Write};

use anyhow::{Context, Result, bail};
use dxf::{
    Color, Drawing, LwPolylineVertex, Point,
    entities::{Entity, EntityType, LwPolyline, ModelPoint, Polyline as DxfPolyline, Text as DxfText, Vertex as DxfVertex},
    enums::AcadVersion,
};
use serde::{Deserialize, Serialize};

use crate::{
    model::{Document, LayerId, Object, ObjectColor, PolyVertex},
    rendering::color::{COLOR_TABLE, linear_to_srgb_byte},
};

pub(crate) const PIDB_FORMAT_VERSION: u32 = 2;

#[cfg(target_arch = "wasm32")]
pub(crate) type ProjectId = uuid::Uuid;

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectPersistence {
    Untitled,
    NativePath(PathBuf),
    ImportedFile { source_name: String },
    BrowserRecord(ProjectId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PidbMetadata {
    pub(crate) name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PidbFile {
    pub(crate) format_version: u32,
    pub(crate) document: Document,
    pub(crate) metadata: PidbMetadata,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenProject {
    #[cfg(target_arch = "wasm32")]
    pub(crate) id: ProjectId,
    /// Stable identity for this open instance and the runtime namespace used
    /// by all of its layer/object ids. Namespace zero is reserved for disk.
    pub(crate) runtime_id: u32,
    pub(crate) path: Option<PathBuf>,
    #[cfg(target_arch = "wasm32")]
    pub(crate) persistence: ProjectPersistence,
    pub(crate) pidb: PidbFile,
    /// Layers currently present in the shared scene. The PIDB remains the
    /// source of truth even while a layer is unloaded.
    pub(crate) loaded_layers: HashSet<LayerId>,
    /// Namespace-invariant content fingerprint of the complete PIDB at its
    /// last successful load/save. `None` means the project has never been
    /// saved. Replaces whole-document JSON snapshots: an interactive drag
    /// used to re-clone and re-serialize the full project per pointer event.
    saved_content_hash: Option<u64>,
    /// Per-layer fingerprints (keyed by the layer id's 32-bit local half) at
    /// the last successful load/save. `None` means never saved: every layer
    /// counts as dirty.
    saved_layer_hashes: Option<HashMap<u64, u64>>,
    /// Changes whenever a successful load/save establishes a new dirty-state
    /// baseline. The document revision can stay unchanged while an async save
    /// completes, so UI caches cannot use that revision alone.
    savepoint_revision: u64,
    /// Per-object hash cache backing [`Document::content_hash`].
    content_hash_cache: RefCell<HashMap<crate::model::ObjectId, (u64, u64)>>,
    dirty_cache: RefCell<Option<PidbDirtyCache>>,
    /// Dirty-layer set cached against the document revision it was computed at.
    layer_dirty_cache: RefCell<Option<(u64, HashSet<LayerId>)>>,
}

/// Cached whole-PIDB dirty result. Document mutations bump `revision`; the
/// other serialized PIDB fields are included directly in the cache key.
#[derive(Clone, Debug)]
struct PidbDirtyCache {
    revision: u64,
    format_version: u32,
    metadata: PidbMetadata,
    dirty: bool,
}

impl OpenProject {
    pub(crate) fn has_unsaved_changes(&self) -> bool {
        let Some(saved_content_hash) = self.saved_content_hash else {
            return true;
        };
        let revision = self.pidb.document.revision();
        if let Some(cache) = self.dirty_cache.borrow().as_ref()
            && cache.revision == revision
            && cache.format_version == self.pidb.format_version
            && cache.metadata == self.pidb.metadata
        {
            return cache.dirty;
        }

        let dirty = self.content_hash() != saved_content_hash;
        *self.dirty_cache.borrow_mut() = Some(PidbDirtyCache {
            revision,
            format_version: self.pidb.format_version,
            metadata: self.pidb.metadata.clone(),
            dirty,
        });
        dirty
    }

    /// Fingerprint of everything the PIDB file serializes, computed from the
    /// runtime document with cached per-object hashes — only objects touched
    /// since the previous call re-hash.
    fn content_hash(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.pidb.format_version.hash(&mut hasher);
        self.pidb.metadata.name.hash(&mut hasher);
        self.pidb.document.content_hash(&mut self.content_hash_cache.borrow_mut()).hash(&mut hasher);
        hasher.finish()
    }

    pub(crate) fn current_content_hash(&self) -> u64 {
        self.content_hash()
    }

    /// Per-layer fingerprints of the current document state. Captured next to
    /// [`Self::current_content_hash`] when an asynchronous saver snapshots the
    /// project.
    pub(crate) fn current_layer_hashes(&self) -> HashMap<u64, u64> {
        self.pidb.document.layer_content_hashes(&mut self.content_hash_cache.borrow_mut())
    }

    /// Record the exact snapshot written by an asynchronous saver. If edits
    /// happened while it was writing, `has_unsaved_changes` compares the live
    /// content with this hash and correctly remains dirty.
    pub(crate) fn mark_snapshot_saved(&mut self, snapshot_hash: u64, snapshot_layer_hashes: HashMap<u64, u64>) {
        self.saved_content_hash = Some(snapshot_hash);
        self.saved_layer_hashes = Some(snapshot_layer_hashes);
        self.savepoint_revision = self.savepoint_revision.wrapping_add(1);
        self.dirty_cache = RefCell::new(None);
        self.layer_dirty_cache = RefCell::new(None);
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved_content_hash = Some(self.content_hash());
        self.saved_layer_hashes = Some(self.current_layer_hashes());
        self.savepoint_revision = self.savepoint_revision.wrapping_add(1);
        self.dirty_cache = RefCell::new(None);
        self.layer_dirty_cache = RefCell::new(None);
    }

    pub(crate) fn savepoint_revision(&self) -> u64 {
        self.savepoint_revision
    }

    /// Layers whose content differs from the last successful load/save.
    /// Recomputed only when the document revision changes.
    pub(crate) fn dirty_layer_ids(&self) -> HashSet<LayerId> {
        const LOCAL_MASK: u64 = u32::MAX as u64;
        let revision = self.pidb.document.revision();
        if let Some((cached_revision, dirty)) = self.layer_dirty_cache.borrow().as_ref()
            && *cached_revision == revision
        {
            return dirty.clone();
        }
        let dirty: HashSet<LayerId> = match &self.saved_layer_hashes {
            None => self.pidb.document.layers().iter().map(|layer| layer.id).collect(),
            Some(saved) => {
                let current = self.current_layer_hashes();
                self.pidb
                    .document
                    .layers()
                    .iter()
                    .filter(|layer| {
                        let key = layer.id.0 & LOCAL_MASK;
                        saved.get(&key) != current.get(&key)
                    })
                    .map(|layer| layer.id)
                    .collect()
            }
        };
        *self.layer_dirty_cache.borrow_mut() = Some((revision, dirty.clone()));
        dirty
    }
}

/// A copy of `pidb` with runtime-namespaced layer/object ids returned to the
/// on-disk namespace. Every serialization boundary has to go through this:
/// [`validate`] rejects ids outside the 32-bit PIDB range on the way back in.
pub(crate) fn portable_pidb(pidb: &PidbFile) -> PidbFile {
    let mut portable = pidb.clone();
    portable.document.apply_runtime_namespace(0);
    portable
}

#[derive(Clone, Debug)]
pub(crate) struct Workspace {
    /// Every PIDB currently open and parsed in memory.
    pub(crate) projects: Vec<OpenProject>,
    /// The project receiving editing commands. Other projects remain open and
    /// may contribute loaded layers to the shared scene.
    pub(crate) active_index: Option<usize>,
    next_runtime_namespace: u32,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
            active_index: None,
            next_runtime_namespace: 1,
        }
    }
}

impl Workspace {
    pub(crate) fn active_project(&self) -> Option<&OpenProject> {
        self.active_index.and_then(|index| self.projects.get(index))
    }

    pub(crate) fn active_project_mut(&mut self) -> Option<&mut OpenProject> {
        self.active_index.and_then(|index| self.projects.get_mut(index))
    }

    pub(crate) fn active_document(&self) -> Option<&Document> {
        self.active_project().map(|p| &p.pidb.document)
    }

    pub(crate) fn active_document_mut(&mut self) -> Option<&mut Document> {
        self.active_project_mut().map(|p| &mut p.pidb.document)
    }

    pub(crate) fn has_active_project(&self) -> bool {
        self.active_index.is_some_and(|index| index < self.projects.len())
    }

    pub(crate) fn project_index_for_path(&self, path: &Path) -> Option<usize> {
        self.projects.iter().position(|project| project.path.as_deref() == Some(path))
    }

    pub(crate) fn project_index_for_runtime_id(&self, runtime_id: u32) -> Option<usize> {
        self.projects.iter().position(|project| project.runtime_id == runtime_id)
    }

    pub(crate) fn project_index_for_object(&self, object_id: crate::model::ObjectId) -> Option<usize> {
        self.projects.iter().position(|project| project.pidb.document.get_object(object_id).is_some())
    }

    pub(crate) fn project_index_for_layer(&self, layer_id: LayerId) -> Option<usize> {
        self.projects.iter().position(|project| project.pidb.document.layer(layer_id).is_some())
    }

    /// Add a project without changing the editing target. Existing paths are
    /// deduplicated and return the already-open index.
    pub(crate) fn add_inactive(&mut self, mut project: OpenProject) -> usize {
        if let Some(path) = project.path.as_deref()
            && let Some(index) = self.project_index_for_path(path)
        {
            return index;
        }
        self.prepare_project(&mut project);
        self.projects.push(project);
        self.projects.len() - 1
    }

    /// Add a project and make it the editing target. Existing paths are
    /// activated rather than opened twice.
    pub(crate) fn add_and_activate(&mut self, project: OpenProject) -> usize {
        let index = self.add_inactive(project);
        self.active_index = Some(index);
        index
    }

    fn prepare_project(&mut self, project: &mut OpenProject) {
        let namespace = self.next_runtime_namespace;
        self.next_runtime_namespace = self.next_runtime_namespace.saturating_add(1);
        project.runtime_id = namespace;
        project.pidb.document.apply_runtime_namespace(namespace);
        // `open_project` hashes disk-local ObjectIds to establish the saved
        // baseline. The hash values are namespace-invariant, but those cache
        // keys are not; retaining them would permanently duplicate every
        // entry after runtime ids are assigned.
        project.content_hash_cache.borrow_mut().clear();
        project.loaded_layers.clear();
    }

    /// Replace the project at `index` with a freshly parsed copy (revert to
    /// disk). The replacement gets a new runtime namespace, so the caller must
    /// drop undo history and selections that reference the old namespace.
    /// Loaded layers are restored by their stable local id, which remains
    /// unambiguous even when multiple layers share a name. Returns the new
    /// runtime id.
    #[cfg(any(not(target_arch = "wasm32"), test))]
    pub(crate) fn replace_project(&mut self, index: usize, mut project: OpenProject) -> Option<u32> {
        if index >= self.projects.len() {
            return None;
        }
        const LOCAL_MASK: u64 = u32::MAX as u64;
        let loaded_local_ids: HashSet<u64> = self.projects[index].loaded_layers.iter().map(|layer| layer.0 & LOCAL_MASK).collect();
        self.prepare_project(&mut project);
        project.loaded_layers.extend(
            project
                .pidb
                .document
                .layers()
                .iter()
                .filter(|layer| loaded_local_ids.contains(&(layer.id.0 & LOCAL_MASK)))
                .map(|layer| layer.id),
        );
        let runtime_id = project.runtime_id;
        self.projects[index] = project;
        Some(runtime_id)
    }

    pub(crate) fn set_active_index(&mut self, index: usize) {
        if index < self.projects.len() {
            self.active_index = Some(index);
        }
    }

    /// Fingerprint of everything `scene_document()` reads: the active
    /// document's revision (bumped by every document mutation) and loaded-layer
    /// set. Equal keys guarantee an identical composite, letting callers skip
    /// the rebuild.
    pub(crate) fn composite_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for project in &self.projects {
            project.runtime_id.hash(&mut hasher);
            project.pidb.document.revision().hash(&mut hasher);
            // Order-insensitive fold over the loaded-layer set.
            let mut layers_fold: u64 = 0;
            for layer in &project.loaded_layers {
                layers_fold ^= layer.0.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            }
            layers_fold.hash(&mut hasher);
            project.loaded_layers.len().hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Build the composite document rendered and queried by the viewport.
    ///
    /// One pass per project's objects (bucketed by loaded layer) instead of
    /// rescanning every object once per loaded layer, and one index rebuild
    /// at the end instead of per-insert map maintenance. Draw order matches
    /// the incremental path: per project, loaded layers in layer order, each
    /// layer's objects in document order.
    pub(crate) fn scene_document(&self) -> Document {
        let mut scene = Document::new();
        for project in &self.projects {
            let document = &project.pidb.document;
            let mut per_layer: HashMap<LayerId, Vec<usize>> = HashMap::new();
            for (index, object) in document.objects().iter().enumerate() {
                if project.loaded_layers.contains(&object.layer()) {
                    per_layer.entry(object.layer()).or_default().push(index);
                }
            }
            for layer in document.layers() {
                if !project.loaded_layers.contains(&layer.id) {
                    continue;
                }
                let indices = per_layer.remove(&layer.id).unwrap_or_default();
                scene.append_layer_snapshot_unindexed(
                    layer,
                    indices.iter().map(|&index| {
                        let object = &document.objects()[index];
                        (object, document.object_revision(object.id()))
                    }),
                );
            }
        }
        scene.rebuild_object_index();
        scene
    }
}

pub(crate) fn new_empty(path: Option<PathBuf>) -> PidbFile {
    let document = Document::new();

    PidbFile {
        format_version: PIDB_FORMAT_VERSION,
        document,
        metadata: PidbMetadata {
            name: project_name(path.as_deref(), "Untitled.pidb"),
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load(path: impl AsRef<Path>) -> Result<PidbFile> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut pidb: PidbFile = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    validate(&mut pidb)?;
    if pidb.metadata.name.trim().is_empty() {
        pidb.metadata.name = project_name(Some(path), "Untitled.pidb");
    }
    Ok(pidb)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_bytes(source_name: &str, bytes: &[u8]) -> Result<PidbFile> {
    let mut pidb: PidbFile = serde_json::from_slice(bytes).with_context(|| format!("parse {source_name}"))?;
    validate(&mut pidb)?;
    if pidb.metadata.name.trim().is_empty() {
        pidb.metadata.name = project_name(Some(Path::new(source_name)), "Untitled.pidb");
    }
    Ok(pidb)
}

pub(crate) fn to_bytes(pidb: &PidbFile) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&portable_pidb(pidb))?)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save(path: impl AsRef<Path>, pidb: &PidbFile) -> Result<()> {
    save_with_progress(path, pidb, &crate::model::progress::Progress::new())
}

/// Chunk size for the progress-reporting write. Large enough that the extra
/// `write_all` calls cost nothing next to the syscall they already make, small
/// enough that a slow disk moves the bar several times.
#[cfg(not(target_arch = "wasm32"))]
const SAVE_CHUNK_BYTES: usize = 1 << 20;

/// Save `pidb`, reporting into `progress`. Serialization has no measurable
/// steps, so it runs as an indeterminate first phase; the write that follows
/// knows the exact byte count and drives a real percentage.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn save_with_progress(path: impl AsRef<Path>, pidb: &PidbFile, progress: &crate::model::progress::Progress) -> Result<()> {
    let path = path.as_ref();
    progress.set_indeterminate();
    let json = to_bytes(pidb)?;
    let total = json.len() as u64;
    crate::model::atomic_file::write_atomic(path, |file| {
        let mut written = 0u64;
        progress.set_fraction(0.0);
        for chunk in json.chunks(SAVE_CHUNK_BYTES) {
            file.write_all(chunk).with_context(|| format!("write {}", path.display()))?;
            written += chunk.len() as u64;
            progress.set_fraction(written as f32 / total.max(1) as f32);
        }
        Ok(())
    })
}

pub(crate) fn validate(pidb: &mut PidbFile) -> Result<()> {
    if pidb.format_version != PIDB_FORMAT_VERSION {
        bail!("Unsupported PIDB format version {} (expected {})", pidb.format_version, PIDB_FORMAT_VERSION);
    }
    // Enforce model invariants before runtime namespacing: duplicate or
    // out-of-range ids would otherwise silently alias distinct records once
    // ids are masked into the 32-bit local namespace.
    pidb.document.validate().context("invalid PIDB document")?;
    // Serialized counters are advisory only; derive them from the actual ids.
    pidb.document.recompute_id_counters();
    pidb.document.rebuild_object_index();
    Ok(())
}

/// Parse a DXF file into a standalone document without touching any project.
/// Lets multi-file imports parse everything up front and commit atomically.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn parse_dxf_document(path: impl AsRef<Path>) -> Result<Document> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut cursor = Cursor::new(bytes.as_slice());
    let drawing = Drawing::load(&mut cursor).with_context(|| format!("parse {}", path.display()))?;
    let document = crate::model::formats::dxf::from_dxf(&drawing);
    document.validate().with_context(|| format!("validate imported DXF {}", path.display()))?;
    Ok(document)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn parse_dxf_document_bytes(source_name: &str, bytes: &[u8]) -> Result<Document> {
    let mut cursor = Cursor::new(bytes);
    let drawing = Drawing::load(&mut cursor).with_context(|| format!("parse {source_name}"))?;
    let document = crate::model::formats::dxf::from_dxf(&drawing);
    document.validate().with_context(|| format!("validate imported DXF {source_name}"))?;
    Ok(document)
}

pub(crate) fn merge_document(target: &mut Document, imported: &Document) -> usize {
    let mut layer_map = HashMap::new();
    for layer in imported.layers() {
        let target_layer = target
            .layer_id_by_name(&layer.name)
            .unwrap_or_else(|| target.add_layer(layer.name.clone(), layer.color_index, layer.color, layer.visible, layer.elevation));
        layer_map.insert(layer.id, target_layer);
    }

    let mut added = 0;
    for object in imported.objects() {
        let layer = layer_map.get(&object.layer()).copied().unwrap_or_else(|| target.ensure_default_layer());
        let id = target.allocate_object_id();
        target.insert_object(object.with_id_and_layer(id, layer));
        added += 1;
    }
    added
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn export_to_dxf(pidb: &PidbFile, path: impl AsRef<Path>) -> Result<()> {
    export_layers_to_dxf(pidb, None, path)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn export_layer_to_dxf(pidb: &PidbFile, layer: LayerId, path: impl AsRef<Path>) -> Result<()> {
    export_layers_to_dxf(pidb, Some(layer), path)
}

#[cfg(not(target_arch = "wasm32"))]
fn export_layers_to_dxf(pidb: &PidbFile, only_layer: Option<LayerId>, path: impl AsRef<Path>) -> Result<()> {
    let drawing = export_drawing(pidb, only_layer);
    // Atomic replacement keeps the previous valid export intact when the
    // write fails part-way (disk full, permissions, encoding errors).
    crate::model::atomic_file::write_atomic(path.as_ref(), |file| {
        let mut writer = std::io::BufWriter::new(file);
        drawing.save(&mut writer)?;
        Write::flush(&mut writer)?;
        Ok(())
    })
    .with_context(|| format!("write {}", path.as_ref().display()))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn export_to_dxf_bytes(pidb: &PidbFile, only_layer: Option<LayerId>) -> Result<Vec<u8>> {
    let drawing = export_drawing(pidb, only_layer);
    let mut bytes = Vec::new();
    drawing.save(&mut bytes)?;
    Ok(bytes)
}

fn export_drawing(pidb: &PidbFile, only_layer: Option<LayerId>) -> Drawing {
    let mut drawing = Drawing::new();
    drawing.header.version = AcadVersion::R2004;
    for layer in pidb.document.layers() {
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

    for object in pidb.document.objects() {
        if only_layer.is_some_and(|id| id != object.layer()) {
            continue;
        }
        add_object_to_drawing(&pidb.document, object, &mut drawing);
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
                // All vertices share the same elevation — use the compact 2D form.
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
                // DXF 3D polyline vertices do not portably support bulge.
                // Tessellate bulged segments (including interpolated Z) into
                // straight 3D vertices before export.
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
            // DXF TEXT anchors at the bottom-left/baseline of its first line;
            // Object::Text anchors the top-left.
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

fn project_name(path: Option<&Path>, fallback: &str) -> String {
    path.and_then(Path::file_name).and_then(|name| name.to_str()).unwrap_or(fallback).to_string()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn pidb_from_dxf_path(path: impl AsRef<Path>) -> Result<PidbFile> {
    let path = path.as_ref();
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut cursor = Cursor::new(bytes);
    let drawing = Drawing::load(&mut cursor)?;
    let document = crate::model::formats::dxf::from_dxf(&drawing);
    Ok(PidbFile {
        format_version: PIDB_FORMAT_VERSION,
        document,
        metadata: PidbMetadata {
            name: project_name(Some(path), "Imported.pidb"),
        },
    })
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn pidb_from_dxf_bytes(source_name: &str, bytes: &[u8]) -> Result<PidbFile> {
    let document = parse_dxf_document_bytes(source_name, bytes)?;
    Ok(PidbFile {
        format_version: PIDB_FORMAT_VERSION,
        document,
        metadata: PidbMetadata {
            name: project_name(Some(Path::new(source_name)), "Imported.pidb"),
        },
    })
}

pub(crate) fn open_project(path: Option<PathBuf>, mut pidb: PidbFile) -> Result<OpenProject> {
    validate(&mut pidb)?;
    let mut project = OpenProject {
        #[cfg(target_arch = "wasm32")]
        id: uuid::Uuid::new_v4(),
        runtime_id: 0,
        #[cfg(target_arch = "wasm32")]
        persistence: path.clone().map(ProjectPersistence::NativePath).unwrap_or(ProjectPersistence::Untitled),
        path,
        pidb,
        loaded_layers: HashSet::new(),
        saved_content_hash: None,
        saved_layer_hashes: None,
        savepoint_revision: 0,
        content_hash_cache: RefCell::new(HashMap::new()),
        dirty_cache: RefCell::new(None),
        layer_dirty_cache: RefCell::new(None),
    };
    // A pathless project has never been saved and stays dirty; the hash is
    // namespace-invariant, so capturing it before runtime namespacing is fine.
    if project.path.is_some() {
        project.mark_saved();
    }
    Ok(project)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn open_imported_project(source_name: String, mut pidb: PidbFile) -> Result<OpenProject> {
    validate(&mut pidb)?;
    let mut project = OpenProject {
        #[cfg(target_arch = "wasm32")]
        id: uuid::Uuid::new_v4(),
        runtime_id: 0,
        path: None,
        persistence: ProjectPersistence::ImportedFile { source_name },
        pidb,
        loaded_layers: HashSet::new(),
        saved_content_hash: None,
        saved_layer_hashes: None,
        savepoint_revision: 0,
        content_hash_cache: RefCell::new(HashMap::new()),
        dirty_cache: RefCell::new(None),
        layer_dirty_cache: RefCell::new(None),
    };
    project.mark_saved();
    Ok(project)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn open_browser_project(id: ProjectId, mut pidb: PidbFile) -> Result<OpenProject> {
    validate(&mut pidb)?;
    let mut project = OpenProject {
        #[cfg(target_arch = "wasm32")]
        id,
        runtime_id: 0,
        path: None,
        persistence: ProjectPersistence::BrowserRecord(id),
        pidb,
        loaded_layers: HashSet::new(),
        saved_content_hash: None,
        saved_layer_hashes: None,
        savepoint_revision: 0,
        content_hash_cache: RefCell::new(HashMap::new()),
        dirty_cache: RefCell::new(None),
        layer_dirty_cache: RefCell::new(None),
    };
    project.mark_saved();
    Ok(project)
}
