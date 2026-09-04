use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    i18n::tr,
    model::{Document, LayerId},
};

/// Persistence state shared by every project-owned dataset. The source name
/// is informational provenance only; it is never used to reload content.
///
/// Two counters, deliberately: `revision` only ever climbs and is what GPU and
/// view caches key on, while `epoch` names *which* content the item is holding.
/// Undoing an edit puts the old epoch back (see
/// [`Self::restore_epoch`]) while still advancing the revision, so a caches-are-
/// stale signal and a this-matches-what-was-saved signal never have to be the
/// same number. Without the split, undoing back to the last save would leave
/// the item starred forever, or reusing the revision would hand a stale GPU
/// buffer to different content.
#[derive(Clone, Debug)]
pub(crate) struct ProjectItemState {
    pub(crate) source_name: Option<String>,
    pub(crate) source_format: Option<String>,
    pub(crate) loaded: bool,
    revision: u64,
    epoch: u64,
    saved_epoch: u64,
}

impl ProjectItemState {
    pub(crate) fn dirty(source_name: Option<String>) -> Self {
        Self::dirty_with_format(source_name, None)
    }

    pub(crate) fn dirty_with_format(source_name: Option<String>, source_format: Option<String>) -> Self {
        let source_name = provenance_filename(source_name);
        let source_format = provenance_format(source_format).or_else(|| {
            source_name
                .as_deref()
                .and_then(|name| Path::new(name).extension())
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase())
        });
        Self {
            source_name,
            source_format,
            loaded: true,
            revision: 1,
            epoch: 1,
            saved_epoch: 0,
        }
    }

    /// Record that the item's content changed, giving it an epoch no earlier
    /// state can collide with. Returns the epoch it was holding, which is what
    /// an undo step keeps so it can put the identity back.
    pub(crate) fn touch(&mut self) -> u64 {
        let previous = self.epoch;
        self.revision = self.revision.wrapping_add(1);
        self.epoch = self.revision;
        previous
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Put back an epoch captured by an earlier [`Self::touch`], after undo or
    /// redo has restored the content that epoch named. The revision still
    /// advances: the bytes changed even though the identity is an old one.
    pub(crate) fn restore_epoch(&mut self, epoch: u64) {
        self.revision = self.revision.wrapping_add(1);
        self.epoch = epoch;
    }

    pub(crate) fn set_provenance(&mut self, source_name: Option<String>, source_format: Option<String>) {
        let source_name = provenance_filename(source_name);
        self.source_format = provenance_format(source_format).or_else(|| {
            source_name
                .as_deref()
                .and_then(|name| Path::new(name).extension())
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase())
        });
        self.source_name = source_name;
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.epoch != self.saved_epoch
    }

    /// Monotonic mutation counter. Cache keys only - never compare it against
    /// a saved baseline, because undo deliberately advances it while putting
    /// older content back. Use [`Self::epoch`] for that.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn mark_snapshot_saved(&mut self, epoch: u64) {
        self.saved_epoch = epoch;
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved_epoch = self.epoch;
    }
}

/// The same two-counter split as [`ProjectItemState`], for the project-level
/// content outside the design document: which items exist, and every
/// item-owned field an OMF save writes. `revision` keys the dirty cache;
/// `epoch` is folded into the project content hash, so putting an old epoch
/// back is what lets undo clear the title-bar dirty marker.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProjectContentState {
    revision: u64,
    epoch: u64,
}

impl ProjectContentState {
    pub(crate) fn touch(&mut self) -> u64 {
        let previous = self.epoch;
        self.revision = self.revision.wrapping_add(1);
        self.epoch = self.revision;
        previous
    }

    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn restore_epoch(&mut self, epoch: u64) {
        self.revision = self.revision.wrapping_add(1);
        self.epoch = epoch;
    }
}

fn provenance_filename(source_name: Option<String>) -> Option<String> {
    let source_name = source_name?;
    let filename = Path::new(&source_name).file_name().and_then(|name| name.to_str()).unwrap_or(&source_name).trim();
    (!filename.is_empty()).then(|| filename.to_owned())
}

fn provenance_format(source_format: Option<String>) -> Option<String> {
    let source_format = source_format?;
    let source_format = source_format.trim();
    (!source_format.is_empty()).then(|| source_format.to_owned())
}

/// Content epochs captured alongside an immutable whole-project OMF snapshot,
/// paired with the id each belongs to. Completion applies these exact epochs,
/// so edits made while encoding stay dirty instead of being accidentally
/// acknowledged by the older save - and an undo back to the snapshotted state
/// still reads as saved, because it restores the same epoch.
#[derive(Clone, Debug, Default)]
pub(crate) struct SaveToken {
    pub(crate) triangulations: Vec<(u64, u64)>,
    pub(crate) block_models: Vec<(u64, u64)>,
    pub(crate) drill_holes: Vec<(u64, u64)>,
    pub(crate) point_clouds: Vec<(u64, u64)>,
    pub(crate) rasters: Vec<(u64, u64)>,
}

pub(crate) const PROJECT_FORMAT_VERSION: u32 = 2;

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectMetadata {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) coordinate_reference_system: String,
    #[serde(default)]
    pub(crate) units: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectFile {
    pub(crate) format_version: u32,
    pub(crate) document: Document,
    pub(crate) metadata: ProjectMetadata,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenProject {
    #[cfg(target_arch = "wasm32")]
    pub(crate) id: ProjectId,
    /// Stable identity for this open instance. Namespace zero is reserved for
    /// persistent IDs and a runtime namespace prevents stale session handles.
    pub(crate) runtime_id: u32,
    pub(crate) path: Option<PathBuf>,
    #[cfg(target_arch = "wasm32")]
    pub(crate) persistence: ProjectPersistence,
    pub(crate) project: ProjectFile,
    /// Content reported by the OMF decoder that Incline Design cannot guarantee it
    /// will reproduce. Native Open retains these until a confirmed rewrite
    /// succeeds; Merge reports them without attaching them to the target.
    pub(crate) lossy_save_warnings: Vec<String>,
    pub(crate) lossy_save_confirmed: bool,
    /// Layers currently present in the shared scene. The project remains the
    /// source of truth even while a layer is unloaded.
    pub(crate) loaded_layers: HashSet<LayerId>,
    /// Identity of project-owned content outside the design document. Every
    /// persisted dataset/style/membership change advances it; load and unload
    /// do not. Public so an [`crate::model::EditTarget`] can borrow it
    /// alongside the document, which lives one field over.
    pub(crate) content: ProjectContentState,
    /// Namespace-invariant content fingerprint of the complete project at its
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
    dirty_cache: RefCell<Option<ProjectDirtyCache>>,
    /// Dirty-layer set cached against the document revision it was computed at.
    layer_dirty_cache: RefCell<Option<(u64, HashSet<LayerId>)>>,
}

/// Cached whole-project dirty result. Document mutations bump `revision`; the
/// other serialized project fields are included directly in the cache key.
#[derive(Clone, Debug)]
struct ProjectDirtyCache {
    revision: u64,
    content_revision: u64,
    content_epoch: u64,
    format_version: u32,
    metadata: ProjectMetadata,
    dirty: bool,
}

impl OpenProject {
    pub(crate) fn has_unsaved_changes(&self) -> bool {
        let Some(saved_content_hash) = self.saved_content_hash else {
            return true;
        };
        let revision = self.project.document.revision();
        if let Some(cache) = self.dirty_cache.borrow().as_ref()
            && cache.revision == revision
            && cache.content_revision == self.content.revision()
            && cache.content_epoch == self.content.epoch()
            && cache.format_version == self.project.format_version
            && cache.metadata == self.project.metadata
        {
            return cache.dirty;
        }

        let dirty = self.content_hash() != saved_content_hash;
        *self.dirty_cache.borrow_mut() = Some(ProjectDirtyCache {
            revision,
            content_revision: self.content.revision(),
            content_epoch: self.content.epoch(),
            format_version: self.project.format_version,
            metadata: self.project.metadata.clone(),
            dirty,
        });
        dirty
    }

    /// Fingerprint of everything the project file serializes, computed from the
    /// runtime document with cached per-object hashes - only objects touched
    /// since the previous call re-hash.
    fn content_hash(&self) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        self.project.format_version.hash(&mut hasher);
        self.project.metadata.name.hash(&mut hasher);
        self.project.metadata.coordinate_reference_system.hash(&mut hasher);
        self.project.metadata.units.hash(&mut hasher);
        self.content.epoch().hash(&mut hasher);
        self.project.document.content_hash(&mut self.content_hash_cache.borrow_mut()).hash(&mut hasher);
        hasher.finish()
    }

    pub(crate) fn current_content_hash(&self) -> u64 {
        self.content_hash()
    }

    pub(crate) fn touch_content(&mut self) -> u64 {
        let previous = self.content.touch();
        *self.dirty_cache.borrow_mut() = None;
        previous
    }

    /// Per-layer fingerprints of the current document state. Captured next to
    /// [`Self::current_content_hash`] when an asynchronous saver snapshots the
    /// project.
    pub(crate) fn current_layer_hashes(&self) -> HashMap<u64, u64> {
        self.project.document.layer_content_hashes(&mut self.content_hash_cache.borrow_mut())
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
        let revision = self.project.document.revision();
        if let Some((cached_revision, dirty)) = self.layer_dirty_cache.borrow().as_ref()
            && *cached_revision == revision
        {
            return dirty.clone();
        }
        let dirty: HashSet<LayerId> = match &self.saved_layer_hashes {
            None => self.project.document.layers().iter().map(|layer| layer.id).collect(),
            Some(saved) => {
                let current = self.current_layer_hashes();
                self.project
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

    /// Whether the Designs collection itself differs from the save baseline.
    /// Comparing the complete layer-hash maps catches deleted layers after
    /// their explorer rows have disappeared.
    pub(crate) fn designs_dirty(&self) -> bool {
        match &self.saved_layer_hashes {
            None => !self.project.document.layers().is_empty(),
            Some(saved) => self.current_layer_hashes() != *saved,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectStore {
    /// The single native project, represented as a one-element collection
    /// while older document helpers are progressively simplified.
    pub(crate) projects: Vec<OpenProject>,
    /// `Some(0)` while a project is open, otherwise `None`.
    pub(crate) active_index: Option<usize>,
    next_runtime_namespace: u32,
}

impl Default for ProjectStore {
    fn default() -> Self {
        Self {
            projects: Vec::new(),
            active_index: None,
            next_runtime_namespace: 1,
        }
    }
}

impl ProjectStore {
    pub(crate) fn active_project(&self) -> Option<&OpenProject> {
        self.active_index.and_then(|index| self.projects.get(index))
    }

    pub(crate) fn active_project_mut(&mut self) -> Option<&mut OpenProject> {
        self.active_index.and_then(|index| self.projects.get_mut(index))
    }

    pub(crate) fn active_document(&self) -> Option<&Document> {
        self.active_project().map(|p| &p.project.document)
    }

    pub(crate) fn active_document_mut(&mut self) -> Option<&mut Document> {
        self.active_project_mut().map(|p| &mut p.project.document)
    }

    pub(crate) fn has_active_project(&self) -> bool {
        self.active_index.is_some_and(|index| index < self.projects.len())
    }

    pub(crate) fn project_index_for_runtime_id(&self, runtime_id: u32) -> Option<usize> {
        self.projects.iter().position(|project| project.runtime_id == runtime_id)
    }

    pub(crate) fn project_index_for_object(&self, object_id: crate::model::ObjectId) -> Option<usize> {
        self.projects.iter().position(|project| project.project.document.get_object(object_id).is_some())
    }

    pub(crate) fn project_index_for_layer(&self, layer_id: LayerId) -> Option<usize> {
        self.projects.iter().position(|project| project.project.document.layer(layer_id).is_some())
    }

    /// Install the one native project and make it the editing target.
    pub(crate) fn add_and_activate(&mut self, mut project: OpenProject) -> usize {
        self.prepare_project(&mut project);
        self.projects.clear();
        self.projects.push(project);
        self.active_index = Some(0);
        0
    }

    fn prepare_project(&mut self, project: &mut OpenProject) {
        let namespace = self.next_runtime_namespace;
        self.next_runtime_namespace = self.next_runtime_namespace.saturating_add(1);
        project.runtime_id = namespace;
        project.project.document.apply_runtime_namespace(namespace);
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
                .project
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
            project.project.document.revision().hash(&mut hasher);
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

    /// Build the runtime document rendered and queried by the viewport from
    /// the retained project document and its loaded-layer cache state.
    pub(crate) fn scene_document(&self) -> Document {
        let mut scene = Document::new();
        for project in &self.projects {
            let document = &project.project.document;
            let mut per_layer: HashMap<LayerId, Vec<usize>> = HashMap::new();
            for (index, object) in document.objects().iter().enumerate() {
                if project.loaded_layers.contains(&object.layer()) && !document.is_object_hidden(object.id()) {
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

pub(crate) fn new_empty(path: Option<PathBuf>) -> ProjectFile {
    let document = Document::new();

    ProjectFile {
        format_version: PROJECT_FORMAT_VERSION,
        document,
        metadata: ProjectMetadata {
            name: project_name(path.as_deref(), &tr!(literal = "Untitled")),
            ..Default::default()
        },
    }
}

pub(crate) fn validate(project: &mut ProjectFile) -> Result<()> {
    if project.format_version != PROJECT_FORMAT_VERSION {
        bail!("Unsupported legacy design format version {} (expected {})", project.format_version, PROJECT_FORMAT_VERSION);
    }
    // Enforce model invariants before runtime namespacing: duplicate or
    // out-of-range ids would otherwise silently alias distinct records once
    // ids are masked into the 32-bit local namespace.
    project.document.validate().context("invalid project document")?;
    // Serialized counters are advisory only; derive them from the actual ids.
    project.document.recompute_id_counters();
    project.document.rebuild_object_index();
    Ok(())
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
        if imported.is_object_hidden(object.id()) {
            target.set_object_hidden(id, true);
        }
        added += 1;
    }
    added
}

/// Merge a foreign design while keeping every incoming layer distinct. Name
/// collisions receive conventional numbered suffixes and every object gets a
/// fresh target id, avoiding aliases with native OMF ids already in use.
pub(crate) fn merge_document_unique_layers(target: &mut Document, imported: &Document) -> usize {
    let mut layer_map = HashMap::new();
    for layer in imported.layers() {
        let name = unique_layer_name(target, &layer.name);
        let target_layer = target.add_layer(name, layer.color_index, layer.color, layer.visible, layer.elevation);
        layer_map.insert(layer.id, target_layer);
    }

    let mut added = 0;
    for object in imported.objects() {
        let layer = layer_map.get(&object.layer()).copied().unwrap_or_else(|| target.ensure_default_layer());
        let id = target.allocate_object_id();
        target.insert_object(object.with_id_and_layer(id, layer));
        if imported.is_object_hidden(object.id()) {
            target.set_object_hidden(id, true);
        }
        added += 1;
    }
    added
}

/// Merge documents belonging to one native OMF while preserving stable local
/// IDs whenever they are valid and unused. Legacy multi-composite files can
/// contain collisions, which are remapped into the target document.
pub(crate) fn merge_document_preserve_ids(target: &mut Document, imported: &Document) -> usize {
    const LOCAL_MASK: u64 = u32::MAX as u64;
    let mut layer_map = HashMap::new();
    for layer in imported.layers() {
        let target_layer = if layer.id.0 <= LOCAL_MASK && target.layer(layer.id).is_none() {
            target.append_layer_snapshot(layer, std::iter::empty());
            layer.id
        } else {
            let mut remapped = layer.clone();
            remapped.id = target.allocate_layer_id();
            remapped.name = unique_layer_name(target, &remapped.name);
            let id = remapped.id;
            target.append_layer_snapshot(&remapped, std::iter::empty());
            id
        };
        layer_map.insert(layer.id, target_layer);
    }

    let mut added = 0;
    for object in imported.objects() {
        let layer = layer_map.get(&object.layer()).copied().unwrap_or_else(|| target.ensure_default_layer());
        let source_id = object.id();
        let id = if source_id.0 <= LOCAL_MASK && target.get_object(source_id).is_none() {
            source_id
        } else {
            target.allocate_object_id()
        };
        target.insert_object(object.with_id_and_layer(id, layer));
        if imported.is_object_hidden(source_id) {
            target.set_object_hidden(id, true);
        }
        added += 1;
    }
    added
}

fn unique_layer_name(document: &Document, requested: &str) -> String {
    let fallback;
    let base = if requested.trim().is_empty() {
        fallback = tr!(literal = "Layer");
        fallback.as_str()
    } else {
        requested.trim()
    };
    if document.layer_id_by_name(base).is_none() {
        return base.to_owned();
    }
    for suffix in 2u64.. {
        let candidate = format!("{base} ({suffix})");
        if document.layer_id_by_name(&candidate).is_none() {
            return candidate;
        }
    }
    unreachable!()
}

/// Resolve a project-item display name without using a source path as its
/// identity. Imports and generated data share this rule across all dataset
/// families.
pub(crate) fn unique_item_name<'a>(requested: String, existing: impl Iterator<Item = &'a str>) -> String {
    let base = if requested.trim().is_empty() {
        tr!(literal = "Item")
    } else {
        requested.trim().to_owned()
    };
    let existing = existing.collect::<HashSet<_>>();
    if !existing.contains(base.as_str()) {
        return base;
    }
    for suffix in 2u64.. {
        let candidate = format!("{base} ({suffix})");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!()
}

/// Derive the name of an imported project item from its source filename.
/// Provenance retains the complete filename; only the in-project display name
/// drops the final extension.
pub(crate) fn imported_item_name(path: &Path, fallback: &str) -> String {
    path.file_stem()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn project_name(path: Option<&Path>, fallback: &str) -> String {
    path.and_then(Path::file_stem)
        .and_then(|name| name.to_str())
        .or_else(|| Path::new(fallback).file_stem().and_then(|name| name.to_str()))
        .unwrap_or(fallback)
        .to_owned()
}

pub(crate) fn open_project(path: Option<PathBuf>, mut project: ProjectFile) -> Result<OpenProject> {
    validate(&mut project)?;
    let mut project = OpenProject {
        #[cfg(target_arch = "wasm32")]
        id: uuid::Uuid::new_v4(),
        runtime_id: 0,
        #[cfg(target_arch = "wasm32")]
        persistence: path.clone().map(ProjectPersistence::NativePath).unwrap_or(ProjectPersistence::Untitled),
        path,
        project,
        lossy_save_warnings: Vec::new(),
        lossy_save_confirmed: false,
        loaded_layers: HashSet::new(),
        content: ProjectContentState::default(),
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
pub(crate) fn open_imported_project(source_name: String, mut project: ProjectFile) -> Result<OpenProject> {
    validate(&mut project)?;
    let mut project = OpenProject {
        #[cfg(target_arch = "wasm32")]
        id: uuid::Uuid::new_v4(),
        runtime_id: 0,
        path: None,
        persistence: ProjectPersistence::ImportedFile { source_name },
        project,
        lossy_save_warnings: Vec::new(),
        lossy_save_confirmed: false,
        loaded_layers: HashSet::new(),
        content: ProjectContentState::default(),
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
