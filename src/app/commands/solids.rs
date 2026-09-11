//! The Solids workspace's Solids setup: the project's list of solids, each
//! pairing a design surface with the topography it is measured against, the
//! kind of volume it is, and the block model it reserves against.
//!
//! Like the Reserves Field List beside it (see [`super::reserves`]), these
//! are config-like list edits rather than design edits, so they are not
//! undoable through `History` and go straight through `Document`.

use std::sync::Arc;

use crate::model::{
    SolidEdit, SolidId, SolidKind,
    block_model::BlockModelId,
    triangulation::{OpenTriangulation, TriangulationId},
};

impl crate::app::App<'_> {
    pub(crate) fn add_solid(&mut self, name: String, kind: SolidKind, surface: Option<TriangulationId>, topography: Option<TriangulationId>, block_model: Option<BlockModelId>) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        let name = crate::model::project::unique_item_name(name, document.solids().iter().map(|solid| solid.name.as_str()));
        let id = document.add_solid(name, kind, surface, topography, block_model);
        self.editor.planning_selected_solid = Some(id);
        self.touch_active_project_content();
        self.invalidate_geometry();
    }

    pub(crate) fn rename_solid(&mut self, id: SolidId, new_name: String) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        let new_name = crate::model::project::unique_item_name(new_name, document.solids().iter().filter(|solid| solid.id != id).map(|solid| solid.name.as_str()));
        document.rename_solid(id, new_name);
        self.touch_active_project_content();
        self.invalidate_geometry();
    }

    pub(crate) fn delete_solid(&mut self, id: SolidId) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        if !document.remove_solid(id) {
            return;
        }
        if self.editor.planning_selected_solid == Some(id) {
            self.editor.planning_selected_solid = None;
        }
        self.touch_active_project_content();
        self.invalidate_geometry();
    }

    /// Apply one property-table edit to a solid. The table hands its value
    /// back every frame, so an edit that changes nothing is dropped here
    /// rather than dirtying the project.
    pub(crate) fn update_solid(&mut self, id: SolidId, edit: SolidEdit) {
        let Some(document) = self.workspace.active_document_mut() else {
            return;
        };
        if !document.update_solid(id, edit) {
            return;
        }
        self.touch_active_project_content();
        self.invalidate_geometry();
    }
}

/// Reserved id for the Solids Setup page's inspection mesh.
///
/// The preview is not a project item - it never reaches the explorer, a save
/// file, or the main viewport - but the renderer's per-surface GPU cache is
/// keyed by id, so it needs one that no project surface can ever take.
pub(crate) const SOLID_PREVIEW_ID: TriangulationId = TriangulationId(u64::MAX);

/// Colour the solid falls back to while one of its benches is picked out, so
/// the selected slice reads against the rest of it.
const UNSELECTED_SOLID_COLOR: [f32; 4] = [0.45, 0.45, 0.45, 1.0];

/// What the Solids page is currently showing for one solid, which surfaces it
/// is tied to, and which of their geometry it was actually built from.
pub(crate) struct SolidPreview {
    key: SolidPreviewKey,
    built_from: BuiltFrom,
    pub(crate) status: SolidPreviewStatus,
    /// What the preview actually draws: the solid as one mesh, or - once it
    /// has a benching plan - one closed solid per flitch.
    ///
    /// Cutting the solid into its flitches up front is what keeps the selected
    /// one from z-fighting with the rest: nothing overlaps, so picking a bench
    /// only changes which of them is coloured. It is also the shape this needs
    /// to be in to carry a tonnage per flitch later.
    body: Vec<OpenTriangulation>,
    /// The band each mesh in `body` covers, in the same order, with where its
    /// style comes from: the plan's range, and the flitch position within its
    /// bench. Looking the style up rather than storing it means recolouring a
    /// flitch costs nothing - the cut stands, only the colour changes.
    /// Empty when the solid is drawn whole.
    bands: Vec<CutBand>,
    /// What `body` was cut from, so it is rebuilt when the solid or the plan
    /// changes but not when the selection or a colour does.
    body_key: Option<(usize, u64)>,
}

/// One cut band: the slice of the solid it covers, and where its style is
/// looked up from.
#[derive(Clone, Copy)]
pub(crate) struct CutBand {
    pub(crate) selection: crate::ui::state::BenchSelection,
    pub(crate) interval: usize,
    pub(crate) position: usize,
}

/// The surfaces a preview is tied to: the solid, and the items it names.
///
/// Identity only - deliberately not the revision. Unloading a surface, or any
/// style edit that touches it, bumps its revision, and a solid that is already
/// built should not be thrown away for either: whether it needs rebuilding is
/// [`BuiltFrom::improves_on`]'s question, not this one's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SolidPreviewKey {
    runtime_id: u32,
    solid: SolidId,
    /// Which volume the pair of surfaces bounds is the solid's type's to say,
    /// so changing a pit to a dump rebuilds the preview.
    kind: SolidKind,
    surface: Option<TriangulationId>,
    topography: Option<TriangulationId>,
}

/// The geometry behind each of a solid's two surfaces, identified by the mesh
/// it is holding. `None` means the input was not used: either the solid does
/// not name it, or it was not loaded.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct BuiltFrom {
    surface: Option<usize>,
    topography: Option<usize>,
}

impl BuiltFrom {
    /// Whether what is loaded now would build a better preview than `built`:
    /// an input that was not used has become available, or one that was used
    /// has changed underneath it.
    ///
    /// An input that has since been *unloaded* is deliberately not a reason to
    /// rebuild. The solid built from it is a finished mesh of its own, and
    /// stays on screen after its surfaces are unloaded - which is also why the
    /// preview holds its own reference to that geometry.
    fn improves_on(self, built: Self) -> bool {
        fn better(now: Option<usize>, then: Option<usize>) -> bool {
            match (now, then) {
                (Some(now), Some(then)) => now != then,
                (Some(_), None) => true,
                (None, _) => false,
            }
        }
        better(self.surface, built.surface) || better(self.topography, built.topography)
    }
}

pub(crate) enum SolidPreviewStatus {
    /// A build is running on a worker. The mesh it is replacing, when there is
    /// one, stays on screen until it finishes rather than blinking out.
    Building {
        previous: Option<Box<OpenTriangulation>>,
    },
    /// A surface this solid needs has been asked back from storage, and the
    /// build starts once it arrives. Unlike `Building` this is re-examined
    /// every frame, so a restore that never lands - a read error, say - ends
    /// as an honest empty preview instead of a bar that spins forever.
    WaitingForInputs {
        previous: Option<Box<OpenTriangulation>>,
    },
    /// Ready to inspect. Carries the enclosed volume for a built solid; a
    /// lone surface encloses nothing, so it has none.
    Ready {
        mesh: Box<OpenTriangulation>,
        volume: Option<f64>,
    },
    Failed(String),
}

impl SolidPreview {
    /// The solid this preview was built for.
    pub(crate) fn solid(&self) -> SolidId {
        self.key.solid
    }

    /// The meshes to draw this frame.
    pub(crate) fn meshes(&self) -> &[OpenTriangulation] {
        &self.body
    }

    /// The solid itself, whatever is picked out of it.
    pub(crate) fn whole_mesh(&self) -> Option<&OpenTriangulation> {
        match &self.status {
            SolidPreviewStatus::Ready { mesh, .. } => Some(mesh),
            SolidPreviewStatus::Building { previous } | SolidPreviewStatus::WaitingForInputs { previous } => previous.as_deref(),
            SolidPreviewStatus::Failed(_) => None,
        }
    }

    /// Whether this preview is only holding a place until geometry arrives,
    /// and so has to be reconsidered rather than left alone.
    fn is_waiting(&self) -> bool {
        matches!(self.status, SolidPreviewStatus::WaitingForInputs { .. })
    }

    /// The mesh to carry into a rebuild, so the page keeps showing the solid
    /// it already has while the next one is computed.
    fn take_mesh(&mut self) -> Option<Box<OpenTriangulation>> {
        match std::mem::replace(&mut self.status, SolidPreviewStatus::Building { previous: None }) {
            SolidPreviewStatus::Ready { mesh, .. } => Some(mesh),
            SolidPreviewStatus::Building { previous } | SolidPreviewStatus::WaitingForInputs { previous } => previous,
            SolidPreviewStatus::Failed(_) => None,
        }
    }

    /// Whether the solid names two surfaces but only one of them was loaded
    /// when this was built, so the preview is that surface rather than the
    /// volume between the two.
    fn is_partial(&self) -> bool {
        self.key.surface.is_some() && self.key.topography.is_some() && (self.built_from.surface.is_none() || self.built_from.topography.is_none())
    }
}

/// Whether a cut band lies inside the selected one - so selecting a bench
/// lights up every flitch in it, and selecting a flitch lights only that one.
fn contains_band(selection: crate::ui::state::BenchSelection, band: crate::ui::state::BenchSelection) -> bool {
    const SLACK: f64 = 1e-6;
    band.base >= selection.base - SLACK && band.top <= selection.top + SLACK
}

/// Lowest and highest Z in a mesh, or `None` when it holds no geometry.
fn mesh_z_extent(mesh: &crate::model::formats::mesh_data::Triangulation) -> Option<(f64, f64)> {
    let mut lowest = f64::INFINITY;
    let mut highest = f64::NEG_INFINITY;
    for vertex in mesh.vertices() {
        lowest = lowest.min(vertex.z);
        highest = highest.max(vertex.z);
    }
    (lowest.is_finite() && highest.is_finite()).then_some((lowest, highest))
}

/// Wrap a finished mesh as the renderable preview surface.
pub(super) fn preview_triangulation(
    name: String,
    mesh: Arc<crate::model::formats::mesh_data::Triangulation>,
    spatial: Arc<crate::model::spatial::TriangleBvh>,
    edges: Vec<[u32; 2]>,
    surface_face_order: Arc<Vec<u32>>,
    color: [f32; 4],
    line_color: [f32; 4],
) -> OpenTriangulation {
    OpenTriangulation {
        id: SOLID_PREVIEW_ID,
        state: crate::model::project::ProjectItemState::dirty(None).with_loaded(true),
        name,
        mesh,
        spatial,
        edges,
        surface_face_order,
        color,
        line_color,
        line_weight: None,
        raster_texture: None,
        raster_opacity: 1.0,
        flitch_style: None,
        cull_back_faces: false,
        always_show_edges: false,
        depth_shade: None,
    }
}

impl crate::app::App<'_> {
    /// Keep the Solids Setup page's preview in step with what it is showing.
    ///
    /// Called once per frame before rendering. A solid with only a design
    /// surface previews that surface directly - there is no volume to build
    /// from one sheet - and one with a topography as well previews the solid
    /// between them, built on a worker so a large pair of surfaces does not
    /// stall the page.
    /// Seed a plan covering the complete built solid once geometry is ready.
    fn seed_benching_plan(&mut self) {
        if self.editor.planning_solids_step != crate::ui::state::SolidsStep::Benching {
            return;
        }
        let Some(solid_id) = self.editor.planning_selected_solid else {
            return;
        };
        let Some(solid) = self.workspace.active_document().and_then(|document| document.solid(solid_id)) else {
            return;
        };
        if !solid.benching.intervals.is_empty() {
            return;
        }
        // The complete solid can extend above/below its design sheet. Wait
        // for both inputs and cover the built solid, even if they were evicted.
        let Some(preview) = self.solid_preview.as_ref() else {
            return;
        };
        let SolidPreviewStatus::Ready { mesh, volume: Some(_) } = &preview.status else {
            return;
        };
        let Some((lowest, highest)) = mesh_z_extent(&mesh.mesh) else {
            return;
        };
        let plan = crate::model::BenchingPlan::covering(lowest, highest);
        self.update_solid(solid_id, crate::model::SolidEdit::Benching(plan));
    }

    /// Cut the solid into the flitches its plan describes, so each is a solid
    /// of its own that can be coloured, and later measured, independently.
    ///
    /// Rebuilt only when the solid or its plan changes - selecting a different
    /// bench just recolours what is already cut. A solid with no plan, or one
    /// off the Benching step, is drawn whole.
    fn sync_bench_slabs(&mut self) {
        let benched = self.editor.planning_solids_step == crate::ui::state::SolidsStep::Benching;
        let Some(preview) = self.solid_preview.as_ref() else {
            return;
        };
        let Some(source) = preview.whole_mesh() else {
            if let Some(preview) = self.solid_preview.as_mut() {
                preview.body.clear();
                preview.bands.clear();
                preview.body_key = None;
            }
            return;
        };
        let plan = benched
            .then(|| {
                self.workspace
                    .active_document()
                    .and_then(|document| document.solid(preview.key.solid))
                    .map(|solid| solid.benching.clone())
            })
            .flatten()
            .unwrap_or_default();

        let key = {
            use std::hash::{DefaultHasher, Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            plan.top.to_bits().hash(&mut hasher);
            for interval in &plan.intervals {
                interval.base.to_bits().hash(&mut hasher);
                interval.bench.to_bits().hash(&mut hasher);
                interval.flitch.to_bits().hash(&mut hasher);
            }
            (Arc::as_ptr(&source.mesh) as usize, hasher.finish())
        };
        if preview.body_key == Some(key) {
            return;
        }

        // The finest division the plan describes: a bench with flitches is cut
        // into them, and one without is a band of its own. Each band carries
        // the style of the position it occupies, so a flitch is drawn the same
        // way in every bench of its range.
        let bands = plan_bands(&plan);

        let source = source.clone();
        let preview_key = preview.key;
        let cull_back_faces = !preview.is_partial();
        // Mark this request before scheduling so successive frames do not
        // enqueue the same cut. Keep the existing body until its replacement lands.
        self.solid_preview.as_mut().unwrap().body_key = Some(key);
        self.spawn_job(
            crate::i18n::tr!("planning-building-slabs"),
            vec![crate::app::jobs::JobKey::SolidPreview(preview_key.solid)],
            move |cancel| build_bench_body(&source, bands, cull_back_faces, cancel),
            move |app, result| {
                let Some(preview) = app.solid_preview.as_mut().filter(|preview| preview.key == preview_key && preview.body_key == Some(key)) else {
                    return;
                };
                match result {
                    Ok((body, bands)) => {
                        preview.body = body;
                        preview.bands = bands;
                    }
                    Err(error) => {
                        crate::userspace_error!("{}", error);
                    }
                }
            },
        );
    }

    /// Repaint the preview mesh in the solid's own colour.
    ///
    /// Applied to whatever mesh is on screen rather than baked in at build
    /// time, so recolouring a solid is immediate and never costs a rebuild.
    fn apply_solid_preview_color(&mut self) {
        let Some(preview) = self.solid_preview.as_ref() else {
            return;
        };
        let color = self
            .workspace
            .active_document()
            .and_then(|document| document.solid(preview.key.solid))
            .map(|solid| solid.color);
        let Some(color) = color else {
            return;
        };
        let selection = self.editor.planning_selected_bench;
        let plan = self
            .workspace
            .active_document()
            .and_then(|document| document.solid(preview.key.solid))
            .map(|solid| solid.benching.clone())
            .unwrap_or_default();
        if let Some(preview) = self.solid_preview.as_mut() {
            match &mut preview.status {
                SolidPreviewStatus::Ready { mesh, .. } => mesh.color = color,
                SolidPreviewStatus::Building { previous } | SolidPreviewStatus::WaitingForInputs { previous } => {
                    if let Some(mesh) = previous {
                        mesh.color = color;
                    }
                }
                SolidPreviewStatus::Failed(_) => {}
            }
            // Every flitch is drawn in its own style. With a bench picked out
            // the ones outside it step back to grey, which is the whole of the
            // highlight - nothing is drawn twice.
            for (index, mesh) in preview.body.iter_mut().enumerate() {
                let Some(band) = preview.bands.get(index) else {
                    mesh.color = color;
                    continue;
                };
                mesh.flitch_style = plan.intervals.get(band.interval).map(|interval| interval.style(band.position, color));
                mesh.color = if selection.is_some_and(|selection| !contains_band(selection, band.selection)) {
                    mesh.flitch_style = None;
                    UNSELECTED_SOLID_COLOR
                } else {
                    plan.intervals.get(band.interval).map_or(color, |interval| interval.style(band.position, color).color)
                };
            }
        }
    }

    pub(crate) fn sync_solid_preview(&mut self) {
        let runtime = self.workspace.active_project().map(|project| project.runtime_id);
        if self.solid_view_cache.values().any(|cache| Some(cache.runtime) != runtime) {
            self.solid_view_cache.clear();
            self.solid_view_body.clear();
            self.solid_view_body_key = None;
            self.editor.solid_view_bands.clear();
            // Identities belong to the project that issued them.
            self.dig_block_identities.clear();
            self.cancel_jobs(|job| matches!(job, crate::app::jobs::JobKey::SolidArtifact { .. }));
        }
        // Blasting reads the same per-solid geometry cache as View; it only
        // styles and frames it differently. Neither *starts* anything: the
        // artifacts are built when a stage is run, and these pages show what
        // that run committed. Opening a page is not a calculation.
        let displaying = self.editor.is_solids_view() || self.editor.is_planning_cut_step();
        let running = self.planning_pipeline.as_ref().and_then(crate::app::planning_pipeline::PlanningPipeline::demand).is_some();
        if displaying || running {
            self.sync_solids_view(displaying);
            if displaying {
                return;
            }
        }
        // Setup owns the shared inspector UI state until View is revisited.
        self.solid_view_body_key = None;
        self.editor.solid_preview_sources.clear();
        if let Some(solid) = self.editor.planning_selected_solid.and_then(|id| self.workspace.active_document()?.solid(id)) {
            self.editor.solid_preview_sources.extend([solid.surface, solid.topography].into_iter().flatten());
        }
        // Off the steps that show it, the cached solid is left exactly as it is:
        // nothing is built, and nothing is thrown away, so coming back to a
        // solid whose surfaces have since been unloaded still shows it. It is
        // one mesh, and only the step that displays it hands it to the
        // renderer, so no GPU cache entry is held while it is off screen.
        if !self.showing_solid_preview() {
            return;
        }
        self.sync_solid_preview_inner();
        self.seed_benching_plan();
        self.sync_bench_slabs();
        self.apply_solid_preview_color();
        // The solid names surfaces the page cannot show at all: none of them
        // are loaded, and nothing was built from them earlier.
        let nothing_loaded = self.solid_preview.is_none()
            && self
                .editor
                .planning_selected_solid
                .and_then(|id| self.workspace.active_document().and_then(|document| document.solid(id)))
                .is_some_and(|solid| solid.surface.is_some() || solid.topography.is_some());
        let partial = self.solid_preview.as_ref().is_some_and(SolidPreview::is_partial);
        self.editor.solid_preview_z_range = self.solid_preview.as_ref().and_then(SolidPreview::whole_mesh).and_then(|mesh| mesh_z_extent(&mesh.mesh));
        self.editor.solid_preview_summary = match self.solid_preview.as_ref().map(|preview| &preview.status) {
            None if nothing_loaded => crate::ui::state::SolidPreviewSummary::Unloaded,
            None => crate::ui::state::SolidPreviewSummary::Empty,
            Some(SolidPreviewStatus::Building { previous }) => crate::ui::state::SolidPreviewSummary::Building {
                showing_previous: previous.is_some(),
            },
            Some(SolidPreviewStatus::WaitingForInputs { previous }) => crate::ui::state::SolidPreviewSummary::LoadingInputs {
                showing_previous: previous.is_some(),
            },
            Some(SolidPreviewStatus::Ready { mesh, volume }) => crate::ui::state::SolidPreviewSummary::Ready {
                volume: *volume,
                faces: mesh.mesh.face_count(),
                waiting_on_unloaded: partial,
            },
            Some(SolidPreviewStatus::Failed(message)) => crate::ui::state::SolidPreviewSummary::Failed(message.clone()),
        };
    }

    fn sync_solid_preview_inner(&mut self) {
        let Some(key) = self.wanted_solid_preview_key() else {
            self.solid_preview = None;
            return;
        };
        let available = self.usable_solid_inputs(key);
        // A preview already built for these surfaces is kept unless the
        // geometry now on hand would build a better one - so unloading a
        // surface leaves the solid it produced on screen.
        if let Some(preview) = &self.solid_preview
            && preview.key == key
            && !available.improves_on(preview.built_from)
            && !preview.is_waiting()
        {
            return;
        }
        let previous = if self.solid_preview.as_ref().is_some_and(|preview| preview.key == key) {
            self.solid_preview.as_mut().and_then(SolidPreview::take_mesh)
        } else {
            None
        };

        // A surface whose geometry has been evicted is fetched back rather
        // than reported as missing: whether a surface is loaded says what the
        // viewport draws, and has nothing to say about whether this page can
        // show the solid built from it.
        if self.request_solid_input_restore(key, available) {
            self.solid_preview = Some(SolidPreview {
                key,
                built_from: available,
                status: SolidPreviewStatus::WaitingForInputs { previous },
                body: Vec::new(),
                bands: Vec::new(),
                body_key: None,
            });
            return;
        }

        match (key.surface.zip(available.surface), key.topography.zip(available.topography)) {
            // Both surfaces on hand: build the volume between them.
            (Some((surface, _)), Some((topography, _))) => self.spawn_solid_preview_build(key, available, key.kind, surface, topography, previous),
            // One of them: show it as it is. There is no volume to enclose
            // with a single sheet, and none is claimed for it.
            (Some((id, _)), None) | (None, Some((id, _))) => {
                let Some(source) = self.triangulations.iter().find(|item| item.id == id) else {
                    self.solid_preview = None;
                    return;
                };
                // The clone shares the source's mesh, so the preview keeps
                // that geometry alive even after the item is unloaded.
                let mesh = preview_triangulation(
                    source.name.clone(),
                    source.mesh.clone(),
                    source.spatial.clone(),
                    source.edges.clone(),
                    source.surface_face_order.clone(),
                    source.color,
                    source.line_color,
                );
                self.solid_preview = Some(SolidPreview {
                    key,
                    built_from: available,
                    status: SolidPreviewStatus::Ready {
                        mesh: Box::new(mesh),
                        volume: None,
                    },
                    body: Vec::new(),
                    bands: Vec::new(),
                    body_key: None,
                });
            }
            // No geometry to build from, none coming, and nothing built
            // earlier for this solid.
            (None, None) => self.solid_preview = None,
        }
    }

    /// Whether a step that shows the solid preview is the thing on screen.
    /// Only then is the cached solid built, or handed to the renderer.
    pub(crate) fn showing_solid_preview(&self) -> bool {
        use crate::ui::state::SolidsStep;
        self.editor.is_solids_view()
            || self.editor.is_planning_setup()
                && self.editor.planning_page == crate::ui::state::PlanningPage::Solids
                && self.editor.solids_subpage == crate::ui::state::PlanningSubpage::Setup
                && matches!(self.editor.planning_solids_step, SolidsStep::Solids | SolidsStep::Benching)
    }

    /// The preview the page is asking for, or `None` when no solid is
    /// selected to preview.
    fn wanted_solid_preview_key(&self) -> Option<SolidPreviewKey> {
        let solid = self.editor.planning_selected_solid?;
        let document = self.workspace.active_document()?;
        let solid = document.solid(solid)?;
        Some(SolidPreviewKey {
            runtime_id: self.workspace.active_project()?.runtime_id,
            solid: solid.id,
            kind: solid.kind,
            surface: solid.surface,
            topography: solid.topography,
        })
    }

    /// The geometry currently behind each of a solid's surfaces.
    ///
    /// The test is whether the mesh is actually there, not whether the item is
    /// loaded: unloading one evicts its geometry and leaves an empty mesh
    /// behind (see `App::evict_unloaded_items`), which a build would otherwise
    /// read as a surface with no faces. An unloaded item that has not been
    /// evicted yet is perfectly usable, and is used.
    fn usable_solid_inputs(&self, key: SolidPreviewKey) -> BuiltFrom {
        let geometry = |id: Option<TriangulationId>| {
            id.and_then(|id| self.triangulations.iter().find(|item| item.id == id))
                .filter(|item| item.mesh.face_count() > 0)
                .map(|item| Arc::as_ptr(&item.mesh) as usize)
        };
        BuiltFrom {
            surface: geometry(key.surface),
            topography: geometry(key.topography),
        }
    }

    /// Fetch back the geometry of any surface this solid names that has been
    /// evicted. Returns whether one is on its way, in which case the build
    /// waits for it and resumes from the restore's own continuation.
    ///
    /// Asked for once per solid: a restore that fails logs to the console, and
    /// retrying it every frame would do nothing but repeat the failure.
    fn request_solid_input_restore(&mut self, key: SolidPreviewKey, available: BuiltFrom) -> bool {
        let missing: Vec<crate::model::ItemRef> = [(key.surface, available.surface), (key.topography, available.topography)]
            .into_iter()
            .filter_map(|(named, present)| named.filter(|_| present.is_none()))
            .map(crate::model::ItemRef::Triangulation)
            .collect();
        if missing.is_empty() {
            return false;
        }
        if missing.iter().any(|item| self.item_load_pending(*item)) {
            return true;
        }
        if self.solid_preview_restore_requested == Some(key) {
            return false;
        }
        self.solid_preview_restore_requested = Some(key);
        self.restore_items_for(missing, |app| app.sync_solid_preview())
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_solid_preview_build(
        &mut self,
        key: SolidPreviewKey,
        built_from: BuiltFrom,
        kind: SolidKind,
        surface_id: TriangulationId,
        topography_id: TriangulationId,
        previous: Option<Box<OpenTriangulation>>,
    ) {
        let Some(surface) = self.triangulations.iter().find(|item| item.id == surface_id) else {
            self.solid_preview = None;
            return;
        };
        let Some(topography) = self.triangulations.iter().find(|item| item.id == topography_id) else {
            self.solid_preview = None;
            return;
        };
        // A pit is the ground cut away below its design; a dump or stockpile
        // is the material placed above it. Which of the two volumes the pair
        // of surfaces bounds is therefore the solid's own type's to say.
        let region = crate::ui::state::SolidRegion::of_solid(kind);
        let solid_color = self
            .workspace
            .active_document()
            .and_then(|document| document.solid(key.solid))
            .map_or_else(crate::model::default_solid_color, |solid| solid.color);
        let name = surface.name.clone();
        let color = solid_color;
        let line_color = surface.line_color;
        let surface_mesh = surface.mesh.clone();
        let topography_mesh = topography.mesh.clone();

        self.solid_preview = Some(SolidPreview {
            key,
            built_from,
            status: SolidPreviewStatus::Building { previous },
            body: Vec::new(),
            bands: Vec::new(),
            body_key: None,
        });

        let compute = move |cancel: &crate::app::jobs::CancelFlag,
                            progress: &crate::model::progress::Progress|
              -> anyhow::Result<(crate::model::triangulation::GeneratedTriangulation, f64)> {
            let (vertices, faces, volume) = super::triangulation::solid_between::build_solid_between_surfaces(&surface_mesh, &topography_mesh, region, &progress.phase(0.0, 0.85))?;
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            let generated = super::triangulation::session::build_generated_triangulation(
                name,
                vertices,
                faces,
                crate::ui::state::TriSurfaceType::SolidClosed,
                crate::model::triangulation::unique_edges,
            )?;
            Ok((generated, volume))
        };
        let apply = move |app: &mut crate::app::App, result: anyhow::Result<(crate::model::triangulation::GeneratedTriangulation, f64)>| {
            // The selection may have moved on while the build ran; only the
            // preview this job was started for accepts its result.
            if app.solid_preview.as_ref().is_none_or(|preview| preview.key != key || preview.built_from != built_from) {
                return;
            }
            let status = match result {
                Ok((generated, volume)) => SolidPreviewStatus::Ready {
                    mesh: Box::new(preview_triangulation(
                        generated.name,
                        generated.mesh,
                        generated.spatial,
                        generated.edges,
                        generated.surface_face_order,
                        color,
                        line_color,
                    )),
                    volume: Some(volume),
                },
                Err(error) => SolidPreviewStatus::Failed(format!("{error:#}")),
            };
            app.solid_preview = Some(SolidPreview {
                key,
                built_from,
                status,
                body: Vec::new(),
                bands: Vec::new(),
                body_key: None,
            });
        };
        self.spawn_job_reporting_progress(
            crate::i18n::tr!(literal = "Building solid preview…"),
            vec![crate::app::jobs::JobKey::SolidPreview(key.solid)],
            compute,
            apply,
        );
    }
}

impl crate::app::App<'_> {
    /// Promote the solid on the Solids page into a project triangulation.
    ///
    /// The preview itself is deliberately not a project item - it is rebuilt
    /// whenever its surfaces change and never saved - so this is how a solid
    /// worth keeping leaves the page: as a normal triangulation that renders
    /// in the viewport, exports, and saves with the project.
    pub(crate) fn save_solid_preview_to_project(&mut self) -> anyhow::Result<()> {
        let Some(preview) = self.solid_preview.as_ref() else {
            anyhow::bail!("No solid is being previewed");
        };
        let SolidPreviewStatus::Ready { mesh, volume } = &preview.status else {
            anyhow::bail!("The solid is still being built");
        };
        // Only a built volume is worth promoting; a lone surface preview is
        // already a project triangulation, and copying it would just duplicate it.
        let Some(volume) = *volume else {
            anyhow::bail!("This solid has no topography yet, so its preview is the design surface itself");
        };
        let solid_name = self
            .workspace
            .active_document()
            .and_then(|document| document.solid(preview.key.solid))
            .map(|solid| solid.name.clone())
            .unwrap_or_else(|| mesh.name.clone());
        let name = crate::app::canvas::derived_triangulation_name(&solid_name, &crate::i18n::tr!(literal = "Solid"));
        let generated = crate::model::triangulation::GeneratedTriangulation {
            name,
            mesh: mesh.mesh.clone(),
            spatial: mesh.spatial.clone(),
            edges: mesh.edges.clone(),
            surface_face_order: mesh.surface_face_order.clone(),
            surface_type: crate::ui::state::TriSurfaceType::SolidClosed,
        };
        crate::userspace_log!(
            "{}",
            crate::i18n::tr_format!(
                literal = "Saved solid '%name%' to the project · %volume% m³",
                name = &solid_name,
                volume = format!("{volume:.1}")
            )
        );
        self.insert_generated_triangulation(generated);
        Ok(())
    }
}

/// One band per bench, ignoring flitches.
///
/// The Blasting step divides benches, and a bench's ground is the union of
/// its flitches' - which are not nested, because the topography can fall
/// away over a bench. Cutting the bench whole gives its outline directly and
/// saves unioning them.
pub(super) fn bench_bands(plan: &crate::model::BenchingPlan) -> Vec<CutBand> {
    plan.benches()
        .into_iter()
        .map(|bench| CutBand {
            selection: crate::ui::state::BenchSelection {
                base: bench.base,
                top: bench.top(),
                is_flitch: false,
            },
            interval: bench.interval,
            position: 0,
        })
        .collect()
}

pub(super) fn plan_bands(plan: &crate::model::BenchingPlan) -> Vec<CutBand> {
    let mut bands: Vec<CutBand> = Vec::new();
    for bench in plan.benches() {
        if bench.flitches.is_empty() {
            bands.push(CutBand {
                selection: crate::ui::state::BenchSelection {
                    base: bench.base,
                    top: bench.top(),
                    is_flitch: false,
                },
                interval: bench.interval,
                position: 0,
            });
            continue;
        }
        // Flitches come bottom up; positions are counted from the top.
        let count = bench.flitches.len();
        for (index, flitch) in bench.flitches.iter().enumerate() {
            bands.push(CutBand {
                selection: crate::ui::state::BenchSelection {
                    base: flitch.base,
                    top: flitch.top(),
                    is_flitch: true,
                },
                interval: bench.interval,
                position: count - 1 - index,
            });
        }
    }

    bands
}

pub(super) fn build_bench_body(
    source: &OpenTriangulation,
    bands: Vec<CutBand>,
    cull_back_faces: bool,
    cancel: &crate::app::jobs::CancelFlag,
) -> anyhow::Result<(Vec<OpenTriangulation>, Vec<CutBand>)> {
    let name = source.name.clone();
    let line_color = source.line_color;
    let mut body = Vec::new();
    let mut cut_bands = Vec::new();
    if bands.is_empty() {
        body.push(source.clone());
    } else {
        let ranges: Vec<(f64, f64)> = bands.iter().map(|band| (band.selection.base, band.selection.top)).collect();
        let slabs = super::triangulation::solid_between::slabs_between_elevations(&source.mesh, &ranges, Some(cancel))?;
        for (band, slab) in bands.into_iter().zip(slabs) {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }
            if slab.1.is_empty() {
                continue;
            }
            let Ok(mesh) = crate::model::formats::mesh_data::Triangulation::from_vertices_and_faces(slab.0, slab.1) else {
                continue;
            };
            let mesh = Arc::new(mesh);
            let spatial = Arc::new(crate::model::spatial::TriangleBvh::build(&mesh));
            let edges = crate::model::triangulation::unique_edges(&mesh);
            let order = Arc::new(crate::model::triangulation::morton_surface_face_order(&mesh));
            let mut item = preview_triangulation(name.clone(), mesh, spatial, edges, order, [1.0; 4], line_color);
            // Each flitch needs an id of its own for the renderer's
            // per-surface cache, counted down from the preview's own.
            item.id = TriangulationId(SOLID_PREVIEW_ID.0 - 1 - cut_bands.len() as u64);
            item.cull_back_faces = cull_back_faces;
            body.push(item);
            cut_bands.push(band);
        }
    }

    Ok((body, cut_bands))
}
