//! The Solids workspace's six-stage pipeline: what each stage needs, what it
//! produces, and whether what it produced is still current.
//!
//! The pipeline is *explicitly executed*. Opening a page, selecting a blast or
//! moving a camera never finishes a calculation; Run Step and Run All do. What
//! a page does is show the stage status and the artifacts that are already
//! there, so a scheduler can consume a completed run without any page having
//! been opened at all.
//!
//! Stages run in a strict order and no stage may read a later stage's output:
//!
//! | Stage | Reads | Produces |
//! | --- | --- | --- |
//! | Field List | The project's reserve fields | A validated schema |
//! | Block Models | The schema, each model's mapping and inclusion | Validated bindings and whole-model statistics |
//! | Solids | Those bindings, the solid definitions and their surfaces | Closed bodies, volumes and bounds |
//! | Benching | Completed bodies and the benching plan | Bench and flitch slabs, footprints, occupied bands |
//! | Blasting | Completed benches and their cut drawings | Blast partitions and committed names |
//! | Dig Strips | Completed blast and flitch partitions, strip drawings | Dig blocks with volumes and reserves |
//!
//! Each stage's input fingerprint includes the fingerprint of the stage before
//! it, so one edit marks exactly the suffix of stages it can reach. Camera,
//! hover and selection are not inputs to anything.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use crate::{
    i18n::tr,
    model::{ReserveAggregation, ReserveFieldIssue},
    ui::state::SolidsStep,
};

/// Where one stage stands for the inputs it has now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StageState {
    /// Never run for this project.
    #[default]
    NotRun,
    /// Ran, but an edit since has changed what it reads. The previous result
    /// is kept and labelled rather than thrown away.
    Stale,
    /// Cannot run: an earlier stage is not current.
    Blocked,
    /// Waiting its turn in a Run All.
    Queued,
    Running,
    /// Every required entity succeeded for the inputs it has now. Warnings and
    /// deliberate not-applicable values stay inspectable underneath.
    Complete,
    Failed,
    Cancelled,
}

impl StageState {
    /// Whether this stage's output can be used by the stage after it.
    pub(crate) fn is_current(self) -> bool {
        self == Self::Complete
    }

    pub(crate) fn is_busy(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    /// The icon the step tree marks this state with.
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Complete => "step_complete.svg",
            Self::Failed | Self::Blocked => "step_error.svg",
            _ => "step_pending.svg",
        }
    }

    pub(crate) fn label(self) -> String {
        match self {
            Self::NotRun => tr!("stage-state-not-run"),
            Self::Stale => tr!("stage-state-stale"),
            Self::Blocked => tr!("stage-state-blocked"),
            Self::Queued => tr!("stage-state-queued"),
            Self::Running => tr!("stage-state-running"),
            Self::Complete => tr!("stage-state-complete"),
            Self::Failed => tr!("stage-state-failed"),
            Self::Cancelled => tr!("stage-state-cancelled"),
        }
    }
}

/// One thing worth saying about a stage's result, beneath its overall state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StageDiagnostic {
    /// The entity it is about, where it is about one.
    pub(crate) entity: Option<String>,
    pub(crate) message: String,
    /// Whether it stops the stage completing, or merely qualifies its result.
    pub(crate) blocking: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StageStatus {
    pub(crate) state: StageState,
    /// The input fingerprint the last successful run saw. A stage is current
    /// when this equals what its inputs hash to now.
    pub(crate) completed_inputs: Option<u64>,
    /// The run this status belongs to, so a result arriving from a cancelled
    /// or superseded run can be rejected rather than published.
    pub(crate) run_generation: u64,
    pub(crate) message: Option<String>,
    pub(crate) diagnostics: Vec<StageDiagnostic>,
    /// How many entities the stage covered when it last completed.
    pub(crate) last_success: Option<StageSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StageSummary {
    pub(crate) entities: usize,
    /// Run number, so "last successful run" is something a user can name.
    pub(crate) generation: u64,
}

/// The pipeline's state for one open project.
#[derive(Debug)]
pub(crate) struct PlanningPipeline {
    /// Which project this belongs to; a different one starts over.
    pub(crate) runtime: u32,
    stages: [StageStatus; SolidsStep::ALL.len()],
    /// Input fingerprints as of the last refresh.
    fingerprints: [u64; SolidsStep::ALL.len()],
    /// Increments on every Run Step or Run All.
    generation: u64,
    /// Stages a Run All still has to reach, earliest first.
    queue: Vec<SolidsStep>,
    running: Option<SolidsStep>,
    /// How much geometry the running stage has asked for. `None` means no
    /// stage is running, and so nothing may build: displaying a page is not a
    /// reason to compute anything.
    demand: Option<GeometryDemand>,
}

/// How far down the geometry a running stage needs built.
///
/// Solids and Benching own the body; Blasting and Dig Strips own the
/// partition cut out of it. A stage never asks for work belonging to a stage
/// after it, which is what stops Run Solids waiting on a bad cut line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GeometryDemand {
    Envelope,
    Body,
    Blasting,
    Partition,
}

impl PlanningPipeline {
    pub(crate) fn new(runtime: u32) -> Self {
        Self {
            runtime,
            stages: Default::default(),
            fingerprints: [0; SolidsStep::ALL.len()],
            generation: 0,
            queue: Vec::new(),
            running: None,
            demand: None,
        }
    }

    pub(crate) fn status(&self, stage: SolidsStep) -> &StageStatus {
        &self.stages[stage.index()]
    }

    fn status_mut(&mut self, stage: SolidsStep) -> &mut StageStatus {
        &mut self.stages[stage.index()]
    }

    /// Whether anything is queued or running.
    pub(crate) fn is_running(&self) -> bool {
        self.running.is_some() || !self.queue.is_empty()
    }

    /// What the running stage has asked to be built, if anything.
    pub(crate) fn demand(&self) -> Option<GeometryDemand> {
        self.demand
    }

    /// Whether a stage's recorded inputs still match what they hash to now.
    pub(crate) fn stage_inputs_match(&self, stage: SolidsStep) -> bool {
        self.stages[stage.index()].completed_inputs == Some(self.fingerprints[stage.index()])
    }

    /// The earliest stage that is not current, which is where Run All starts.
    pub(crate) fn first_stale(&self) -> Option<SolidsStep> {
        SolidsStep::ALL.into_iter().find(|stage| !self.stages[stage.index()].state.is_current())
    }

    /// The stage that has to run before `stage` can, if any.
    pub(crate) fn blocked_by(&self, stage: SolidsStep) -> Option<SolidsStep> {
        SolidsStep::ALL
            .into_iter()
            .take(stage.index())
            .find(|earlier| !self.stages[earlier.index()].state.is_current())
    }

    /// What the runner should do next. The scheduling half of a run, with no
    /// reference to jobs, geometry or the app - so it can be driven straight
    /// through in a test.
    pub(crate) fn next_step(&mut self) -> RunStep {
        if let Some(stage) = self.running {
            return RunStep::Evaluate(stage);
        }
        let Some(next) = (!self.queue.is_empty()).then(|| self.queue.remove(0)) else {
            self.demand = None;
            return RunStep::Idle;
        };
        // Already current from an earlier run: skip it rather than repeating
        // work whose inputs have not moved.
        if self.status(next).state.is_current() && self.stage_inputs_match(next) {
            return RunStep::Skip;
        }
        RunStep::Start(next)
    }

    pub(crate) fn start(&mut self, stage: SolidsStep) {
        self.running = Some(stage);
        self.demand = stage.geometry_demand();
        self.status_mut(stage).state = StageState::Running;
    }

    /// Apply one stage's outcome. Returns whether the run should stop here -
    /// either because the stage is still working, or because it failed.
    pub(crate) fn settle(&mut self, stage: SolidsStep, outcome: StageOutcome) -> bool {
        let generation = self.generation;
        let fingerprint = self.fingerprints[stage.index()];
        match outcome {
            StageOutcome::Working { message } => {
                let status = self.status_mut(stage);
                status.state = StageState::Running;
                status.message = message;
                true
            }
            StageOutcome::Settled { diagnostics, entities } => {
                let blocking = diagnostics.iter().filter(|entry| entry.blocking).count();
                let status = self.status_mut(stage);
                status.diagnostics = diagnostics;
                status.run_generation = generation;
                status.message = None;
                if blocking > 0 {
                    status.state = StageState::Failed;
                    status.message = Some(tr!("stage-failed-count", count = blocking.to_string()));
                    self.running = None;
                    // A failed stage stops everything after it.
                    for later in std::mem::take(&mut self.queue) {
                        let status = self.status_mut(later);
                        status.state = StageState::Blocked;
                        status.message = Some(tr!("stage-blocked-by", stage = stage.label()));
                    }
                    self.demand = None;
                    return true;
                }
                status.state = StageState::Complete;
                status.completed_inputs = Some(fingerprint);
                status.last_success = Some(StageSummary { entities, generation });
                self.running = None;
                false
            }
        }
    }

    /// Whether a completed run stands for these exact inputs, and which run it
    /// was. The readiness half of the public snapshot gate, with no reference
    /// to the app - so the refusals can be exercised directly.
    pub(crate) fn readiness(&self, fingerprints: &[u64; SolidsStep::ALL.len()]) -> Result<u64, StageNotReady> {
        // Every stage, not only the last: a terminal stage cannot be current
        // over inputs an earlier one has since been marked stale for.
        for stage in SolidsStep::ALL {
            let status = self.status(stage);
            match status.state {
                StageState::Complete if status.completed_inputs == Some(fingerprints[stage.index()]) => {}
                StageState::Complete | StageState::Stale => return Err(StageNotReady::Stale(stage)),
                StageState::Queued | StageState::Running => return Err(StageNotReady::Running(stage)),
                StageState::Failed | StageState::Blocked => {
                    return Err(StageNotReady::Failed {
                        stage,
                        message: status.message.clone().unwrap_or_else(|| status.state.label()),
                    });
                }
                StageState::NotRun | StageState::Cancelled => return Err(StageNotReady::NotRun(stage)),
            }
            // A stage that completed with a blocking diagnostic did not
            // reconcile; its numbers are not schedulable inventory.
            if let Some(blocking) = status.diagnostics.iter().find(|entry| entry.blocking) {
                return Err(StageNotReady::Failed {
                    stage,
                    message: blocking
                        .entity
                        .clone()
                        .map_or_else(|| blocking.message.clone(), |entity| format!("{entity}: {}", blocking.message)),
                });
            }
        }
        Ok(self.status(SolidsStep::DigStrips).run_generation)
    }

    /// Mark this stage and every stage after it as no longer current.
    ///
    /// Applied to every committed edit, not only to ones whose numbers change:
    /// a metadata edit the user can see has to mark its stage stale too, or
    /// the markers stop meaning anything. Reusing an unchanged artifact during
    /// a run is an internal matter for the fingerprints.
    fn invalidate_from(&mut self, stage: SolidsStep) -> bool {
        for later in SolidsStep::ALL.into_iter().skip(stage.index()) {
            let status = self.status_mut(later);
            match status.state {
                StageState::Complete => status.state = StageState::Stale,
                // A run in flight over inputs that just changed is stopped
                // rather than allowed to publish a mixed generation.
                StageState::Running | StageState::Queued => status.state = StageState::Cancelled,
                _ => {}
            }
        }
        self.queue.retain(|queued| queued.index() < stage.index());
        let stopped = self.running.is_some_and(|running| running.index() >= stage.index());
        if stopped {
            self.running = None;
            self.demand = None;
        }
        stopped
    }
}

/// One stage's own inputs, hashed. The chained fingerprints are built from
/// these in [`crate::app::App::planning_fingerprints`].
fn hash_of(value: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

impl crate::app::App<'_> {
    /// Refresh the pipeline against the project as it stands, without running
    /// anything: recompute the fingerprints, mark the stages an edit has
    /// reached as stale, and advance a Run All that is part way through.
    ///
    /// Cheap enough to call every frame - it hashes configuration, never
    /// geometry or block data.
    pub(crate) fn sync_planning_pipeline(&mut self) {
        let Some(runtime) = self.workspace.active_project().map(|project| project.runtime_id) else {
            self.planning_pipeline = None;
            // Closing the project clears the markers too; a stage status left
            // over from the last one would describe geometry that is gone.
            self.mirror_planning_stages();
            return;
        };
        if self.planning_pipeline.as_ref().is_none_or(|pipeline| pipeline.runtime != runtime) {
            self.planning_pipeline = Some(PlanningPipeline::new(runtime));
        }
        let fingerprints = self.planning_fingerprints();
        let mut earliest_change = None;
        {
            let pipeline = self.planning_pipeline.as_mut().expect("just ensured");
            for stage in SolidsStep::ALL {
                if pipeline.fingerprints[stage.index()] != fingerprints[stage.index()] {
                    earliest_change = Some(earliest_change.map_or(stage, |first: SolidsStep| if stage.index() < first.index() { stage } else { first }));
                }
            }
            pipeline.fingerprints = fingerprints;
        }
        if let Some(stage) = earliest_change {
            // A first refresh on a project that has never run changes nothing
            // that was current, so this is a no-op there.
            let was_running = self.planning_pipeline.as_mut().expect("just ensured").invalidate_from(stage);
            if was_running {
                // Stopping a stage has to stop its workers too, and retire the
                // requests they were serving; leaving those in place would let
                // a superseded result publish, or strand a retry behind a
                // request that will never land.
                self.cancel_jobs(|job| matches!(job, crate::app::jobs::JobKey::SolidArtifact { .. }));
                self.discard_incomplete_solid_requests();
            }
        }
        // A stage whose recorded inputs no longer match is stale even if no
        // edit passed through `invalidate_from` - a reopened project, say.
        {
            let pipeline = self.planning_pipeline.as_mut().expect("just ensured");
            for stage in SolidsStep::ALL {
                let current = pipeline.fingerprints[stage.index()];
                let status = pipeline.status_mut(stage);
                if status.state == StageState::Complete && status.completed_inputs != Some(current) {
                    status.state = StageState::Stale;
                }
            }
            if !pipeline.is_running() {
                pipeline.demand = None;
            }
        }
        self.advance_planning_run();
        self.mirror_planning_stages();
    }

    /// Copy the pipeline's status into the editor state the panels read.
    ///
    /// Derived every frame from the pipeline itself, so nothing in the UI ever
    /// holds a stage status that outlived the run it came from.
    fn mirror_planning_stages(&mut self) {
        let Some(pipeline) = self.planning_pipeline.as_ref() else {
            self.editor.planning_stages = Default::default();
            self.editor.planning_run_active = false;
            return;
        };
        let views = SolidsStep::ALL.map(|stage| {
            let status = pipeline.status(stage);
            crate::ui::state::PlanningStageView {
                state: status.state,
                message: status.message.clone(),
                diagnostics: status.diagnostics.clone(),
                last_success: status.last_success.clone(),
                blocked_by: pipeline.blocked_by(stage),
            }
        });
        let running = pipeline.is_running();
        // What a scheduler would get if it asked right now, shown where the
        // run controls are: the readiness gate is the point of the pipeline,
        // so it should not be invisible until something consumes it.
        let snapshot = match self.planning_snapshot() {
            Ok(snapshot) => tr!(
                "planning-snapshot-ready",
                generation = snapshot.generation.to_string(),
                blocks = snapshot.blocks.len().to_string()
            ),
            Err(reason) => reason.describe(),
        };
        if self.editor.planning_stages != views || self.editor.planning_run_active != running || self.editor.planning_snapshot_status != snapshot {
            self.editor.planning_stages = views;
            self.editor.planning_run_active = running;
            self.editor.planning_snapshot_status = snapshot;
            self.redraw_requested = true;
        }
    }

    /// Retake every stage's input fingerprint without changing any status.
    ///
    /// For the one case where running a stage edits that stage's own inputs.
    fn refresh_planning_fingerprints(&mut self) {
        let fingerprints = self.planning_fingerprints();
        if let Some(pipeline) = self.planning_pipeline.as_mut() {
            pipeline.fingerprints = fingerprints;
        }
    }

    /// Input fingerprints for all six stages, each chained onto the one before
    /// it so an edit invalidates exactly the suffix it can reach.
    pub(crate) fn planning_fingerprints(&self) -> [u64; SolidsStep::ALL.len()] {
        let document = self.workspace.active_document();
        let fields = document.map(|document| document.reserve_fields().to_vec()).unwrap_or_default();
        let solids = document.map(|document| document.solids().to_vec()).unwrap_or_default();

        let field_list = hash_of(serde_json::to_vec(&fields).unwrap_or_default());

        // Block Models: the bindings and the data behind them. Attribute
        // arrays are immutable and shared, so their identity stands in for
        // their contents; a colour or rendering edit touches neither.
        /// One block model's binding fingerprint: id, name, inclusion and the
        /// authoritative content version of everything a scan reads.
        type ModelBinding = (u64, String, bool, u64);
        let mut models: Vec<ModelBinding> = self
            .block_models
            .iter()
            // The same content version the measurement requests use, so
            // staleness and measurement cannot disagree about what changed.
            .map(|model| (model.id.0, model.name.clone(), model.included_in_reserves, self.model_content_version(model)))
            .collect();
        models.sort_by_key(|entry| entry.0);
        let block_models = hash_of((field_list, models));

        // Solids: what each one is and which geometry it is built from.
        let definitions: Vec<_> = solids
            .iter()
            .map(|solid| {
                // Content versions, not mesh pointers, and taken from the
                // project rather than from whatever the cache happens to hold:
                // an edit to a surface under its own id has to be visible
                // here, and an unload without an edit must not be.
                let sources = [solid.surface, solid.topography].map(|id| self.surface_content_version(id));
                // Name and colour are in here on purpose. They change nothing
                // numerical - `envelope_key` leaves them out, so a run reuses
                // the built body - but the plan's rule is that a visible edit
                // marks its stage stale, or the markers stop meaning anything.
                let color = solid.color.map(f32::to_bits);
                (solid.id.0, solid.name.clone(), solid.kind as u8, solid.block_model.map(|id| id.0), sources, color)
            })
            .collect();
        let solids_stage = hash_of((block_models, definitions));

        let benching: Vec<_> = solids.iter().map(|solid| (solid.id.0, serde_json::to_vec(&solid.benching).unwrap_or_default())).collect();
        let benching_stage = hash_of((solids_stage, benching));

        let blasting: Vec<_> = solids
            .iter()
            .map(|solid| {
                let cuts: Vec<_> = solid
                    .blasting
                    .benches
                    .iter()
                    .map(|bench| {
                        let names: Vec<_> = bench.blasts.iter().map(|blast| (blast.name.clone(), blast.anchor.map(f64::to_bits))).collect();
                        (bench.base.to_bits(), bench.cuts.iter().map(crate::model::Object::geometry_hash).collect::<Vec<_>>(), names)
                    })
                    .collect();
                (solid.id.0, cuts)
            })
            .collect();
        let blasting_stage = hash_of((benching_stage, blasting));

        let strips: Vec<_> = solids
            .iter()
            .map(|solid| {
                let cuts: Vec<_> = solid
                    .blasting
                    .dig_strips
                    .iter()
                    .map(|bench| (bench.base.to_bits(), bench.cuts.iter().map(crate::model::Object::geometry_hash).collect::<Vec<_>>()))
                    .collect();
                (solid.id.0, cuts)
            })
            .collect();
        let dig_stage = hash_of((blasting_stage, strips));

        [field_list, block_models, solids_stage, benching_stage, blasting_stage, dig_stage]
    }

    /// Run one stage, if the stages before it are current.
    pub(crate) fn run_planning_stage(&mut self, stage: SolidsStep) {
        self.sync_planning_pipeline();
        let Some(pipeline) = self.planning_pipeline.as_mut() else {
            return;
        };
        if let Some(blocker) = pipeline.blocked_by(stage) {
            let status = pipeline.status_mut(stage);
            status.state = StageState::Blocked;
            status.message = Some(tr!("stage-blocked-by", stage = blocker.label()));
            crate::userspace_warn!("{}", tr!("stage-blocked-by", stage = blocker.label()));
            return;
        }
        pipeline.generation += 1;
        pipeline.queue = vec![stage];
        self.retry_failed_solid_requests();
        self.advance_planning_run();
    }

    /// Run every stage from the earliest that is not current through Dig
    /// Strips, skipping the ones whose artifacts are already current.
    pub(crate) fn run_all_planning_stages(&mut self) {
        self.sync_planning_pipeline();
        let Some(pipeline) = self.planning_pipeline.as_mut() else {
            return;
        };
        let Some(first) = pipeline.first_stale() else {
            crate::userspace_log!("{}", tr!("stage-all-current"));
            return;
        };
        pipeline.generation += 1;
        pipeline.queue = SolidsStep::ALL.into_iter().skip(first.index()).collect();
        for stage in pipeline.queue.clone() {
            pipeline.status_mut(stage).state = StageState::Queued;
        }
        self.retry_failed_solid_requests();
        self.advance_planning_run();
    }

    /// Rebuild every stage from the Field List down, whatever their artifacts
    /// currently say. Separate from Run All, which skips current stages.
    pub(crate) fn force_rebuild_planning(&mut self) {
        self.solid_view_cache.clear();
        self.solid_view_body.clear();
        self.solid_view_body_key = None;
        self.cancel_jobs(|job| matches!(job, crate::app::jobs::JobKey::SolidArtifact { .. }));
        self.recompute_all_reserve_totals();
        if let Some(pipeline) = self.planning_pipeline.as_mut() {
            for stage in SolidsStep::ALL {
                *pipeline.status_mut(stage) = StageStatus::default();
            }
        }
        self.run_all_planning_stages();
    }

    pub(crate) fn cancel_planning_run(&mut self) {
        let Some(pipeline) = self.planning_pipeline.as_mut() else {
            return;
        };
        let affected: Vec<_> = std::mem::take(&mut pipeline.queue).into_iter().chain(pipeline.running.take()).collect();
        for stage in affected {
            let status = pipeline.status_mut(stage);
            if status.state.is_busy() {
                status.state = StageState::Cancelled;
                status.message = Some(tr!("stage-state-cancelled"));
            }
        }
        pipeline.demand = None;
        self.cancel_jobs(|job| matches!(job, crate::app::jobs::JobKey::SolidArtifact { .. } | crate::app::jobs::JobKey::ReserveStats(..)));
        // A cancelled request leaves a cache key that claims the work is in
        // hand when its product never arrived. Clearing those is what lets a
        // retry ask again instead of waiting on a job that will never land.
        self.discard_incomplete_solid_requests();
    }

    /// Start the next queued stage, and finish the running one when its work
    /// has landed. Called every refresh, so a stage completes as soon as the
    /// jobs it is waiting on apply - no page needs to be open.
    fn advance_planning_run(&mut self) {
        // Each stage takes at most two passes - one to start it, one to settle
        // it - plus a pass to find the queue empty.
        for _ in 0..SolidsStep::ALL.len() * 2 + 1 {
            let Some(pipeline) = self.planning_pipeline.as_mut() else { return };
            let step = pipeline.next_step();
            match step {
                RunStep::Idle => return,
                RunStep::Start(stage) => {
                    pipeline.start(stage);
                    self.begin_planning_stage(stage);
                }
                RunStep::Skip => continue,
                RunStep::Evaluate(stage) => {
                    let outcome = self.evaluate_planning_stage(stage);
                    let Some(pipeline) = self.planning_pipeline.as_mut() else { return };
                    if pipeline.settle(stage, outcome) {
                        return;
                    }
                }
            }
        }
    }

    /// Kick off whatever a stage needs before it can settle.
    fn begin_planning_stage(&mut self, stage: SolidsStep) {
        match stage {
            // Validation only; nothing to start.
            SolidsStep::FieldList => {}
            SolidsStep::BlockModels => self.recompute_all_reserve_totals(),
            // The geometry artifacts are built by one job per solid, which
            // every geometric stage reads a different facet of. Asking for it
            // here is what makes a run independent of any page being open.
            SolidsStep::Solids | SolidsStep::Benching | SolidsStep::Blasting | SolidsStep::DigStrips => {
                self.sync_solid_preview();
            }
        }
    }

    /// Whether a stage's work has landed, and what it has to say about it.
    fn evaluate_planning_stage(&mut self, stage: SolidsStep) -> StageOutcome {
        match stage {
            SolidsStep::FieldList => self.evaluate_field_list_stage(),
            SolidsStep::BlockModels => self.evaluate_block_models_stage(),
            SolidsStep::Solids | SolidsStep::Benching | SolidsStep::DigStrips => self.evaluate_geometry_stage(stage),
            SolidsStep::Blasting => {
                let outcome = self.evaluate_geometry_stage(stage);
                // Names are the one thing about a blast that cannot be
                // re-derived, so committing them is part of running the stage
                // rather than of drawing the page. Committing them is also an
                // edit to this stage's own inputs, so the fingerprint has to
                // be taken again before the result is published - otherwise
                // the stage would mark itself stale the moment it completed.
                if matches!(outcome, StageOutcome::Settled { .. }) && self.commit_blast_names() > 0 {
                    self.refresh_planning_fingerprints();
                }
                outcome
            }
        }
    }

    fn evaluate_field_list_stage(&mut self) -> StageOutcome {
        let fields = self.workspace.active_document().map(|document| document.reserve_fields().to_vec()).unwrap_or_default();
        let mut diagnostics = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for field in &fields {
            *seen.entry(field.name.to_lowercase()).or_default() += 1;
            if let ReserveAggregation::WeightedAverage { weight_field } = field.aggregation {
                match fields.iter().find(|entry| entry.id == weight_field) {
                    None => diagnostics.push(StageDiagnostic {
                        entity: Some(field.name.clone()),
                        message: ReserveFieldIssue::WeightFieldMissing.describe(),
                        blocking: true,
                    }),
                    Some(weight) if weight.aggregation != ReserveAggregation::Sum => diagnostics.push(StageDiagnostic {
                        entity: Some(field.name.clone()),
                        message: ReserveFieldIssue::WeightNotSummed(weight.name.clone()).describe(),
                        blocking: true,
                    }),
                    Some(_) => {}
                }
            }
        }
        for (name, count) in seen {
            if count > 1 {
                diagnostics.push(StageDiagnostic {
                    entity: Some(name),
                    message: tr!("stage-duplicate-field"),
                    blocking: true,
                });
            }
        }
        StageOutcome::Settled {
            entities: fields.len(),
            diagnostics,
        }
    }

    fn evaluate_block_models_stage(&mut self) -> StageOutcome {
        let fields = self.workspace.active_document().map(|document| document.reserve_fields().to_vec()).unwrap_or_default();
        let mut diagnostics = Vec::new();
        let mut waiting = 0;
        let ids: Vec<_> = self.block_models.iter().map(|model| model.id).collect();
        for id in &ids {
            // Statistics are computed for every configured model, not only the
            // one the page happens to have selected. A model whose last scan
            // failed for these very inputs is left alone - the request is
            // terminal until the inputs change or Recompute retires it.
            self.request_reserve_stats(*id);
        }
        for id in &ids {
            let Some(model) = self.block_models.iter().find(|model| model.id == *id) else {
                continue;
            };
            if let Some(error) = &model.reserve_totals_error {
                diagnostics.push(StageDiagnostic {
                    entity: Some(model.name.clone()),
                    message: error.clone(),
                    blocking: model.included_in_reserves,
                });
                continue;
            }
            if model.reserve_totals_key.is_none() || (model.reserve_totals.is_empty() && !fields.is_empty()) {
                waiting += 1;
                continue;
            }
            for field in &fields {
                let Some(stats) = model.reserve_totals.get(&field.id) else { continue };
                if let Some(issue) = &stats.issue {
                    diagnostics.push(StageDiagnostic {
                        entity: Some(format!("{} · {}", model.name, field.name)),
                        message: issue.describe(),
                        // A model deliberately left out of reserves, or a field
                        // it genuinely has no data for, qualifies the result
                        // rather than stopping the stage.
                        blocking: model.included_in_reserves && matches!(issue, ReserveFieldIssue::LengthMismatch { .. } | ReserveFieldIssue::WrongColumnKind { .. }),
                    });
                } else if stats.missing_values > 0 || stats.unusable_weights > 0 {
                    diagnostics.push(StageDiagnostic {
                        entity: Some(format!("{} · {}", model.name, field.name)),
                        message: tr!(
                            "stage-model-data-gaps",
                            missing = stats.missing_values.to_string(),
                            weights = stats.unusable_weights.to_string()
                        ),
                        blocking: false,
                    });
                }
            }
        }
        if waiting > 0 {
            return StageOutcome::Working {
                message: Some(tr!("stage-waiting-models", count = waiting.to_string())),
            };
        }
        StageOutcome::Settled { entities: ids.len(), diagnostics }
    }

    /// The geometric stages, each judged on the facet it owns.
    ///
    /// Solids and Benching settle on the body alone; a bad cut line cannot
    /// fail them, and they never wait on the partitioning that follows them.
    /// Blasting and Dig Strips settle on the partition cut out of that body.
    fn evaluate_geometry_stage(&mut self, stage: SolidsStep) -> StageOutcome {
        let solids = self.workspace.active_document().map(|document| document.solids().to_vec()).unwrap_or_default();
        let outcome = PlanningInputs {
            solids: &solids,
            caches: &self.solid_view_cache,
            has_schema: self.workspace.active_document().is_some_and(|document| !document.reserve_fields().is_empty()),
        }
        .evaluate_geometry_stage(stage);
        // Only the Dig Strips summary needs the app: it reports through the
        // record API and reconciles against the bench measurements.
        match outcome {
            StageOutcome::Settled { mut diagnostics, entities } if stage == SolidsStep::DigStrips && !diagnostics.iter().any(|entry| entry.blocking) => {
                let records = match self.planning_dig_blocks() {
                    Ok(records) => records,
                    // Reached only with no blocking diagnostic recorded above,
                    // so a refusal here is work that has yet to arrive.
                    Err(reason) => {
                        return StageOutcome::Working { message: Some(reason.describe()) };
                    }
                };
                let measured: f64 = records.iter().filter_map(|record| record.volume).sum();
                let unmeasured = records.iter().filter(|record| record.volume.is_none()).count();
                let solid_count = records.iter().map(|record| record.solid).collect::<std::collections::HashSet<_>>().len();
                diagnostics.extend(self.reconcile_dig_blocks(&records));
                let relineaged = records.iter().filter(|record| !record.replaces.is_empty()).count();
                if relineaged > 0 {
                    diagnostics.push(StageDiagnostic {
                        entity: None,
                        message: tr!("stage-blocks-relineaged", count = relineaged.to_string()),
                        blocking: false,
                    });
                }
                crate::userspace_log!(
                    "{}",
                    tr!(
                        "stage-dig-blocks-summary",
                        blocks = records.len().to_string(),
                        solids = solid_count.to_string(),
                        volume = format!("{measured:.1}"),
                        unmeasured = unmeasured.to_string()
                    )
                );
                let _ = entities;
                StageOutcome::Settled {
                    entities: records.len(),
                    diagnostics,
                }
            }
            outcome => outcome,
        }
    }

    /// Check that the dig blocks add back up to the benches they came out of.
    ///
    /// A subdivision that loses or duplicates material is a real error even
    /// when every child looks closed on its own, so the parent's own measured
    /// volume - and its reserve accumulators, not its grades - are the thing
    /// the children are reconciled against.
    fn reconcile_dig_blocks(&self, records: &[crate::app::commands::solids_view::DigBlockRecord]) -> Vec<StageDiagnostic> {
        use crate::app::commands::solids_view::MaterialState;

        // Scale aware: a tolerance in metres would be meaningless across a
        // 50 m³ block and a 5,000,000 m³ bench, so it is relative to the
        // parent, with a floor that covers a single clip's own conditioning.
        const RELATIVE: f64 = 1e-6;
        const FLOOR: f64 = 1e-3;

        let solids = self.workspace.active_document().map(|document| document.solids().to_vec()).unwrap_or_default();
        let mut diagnostics = Vec::new();
        for solid in &solids {
            for parent in self.bench_reserve_references(solid.id) {
                let here = |message: String, blocking: bool| StageDiagnostic {
                    entity: Some(format!("{} · RL {:.2}", solid.name, parent.bench.base)),
                    message,
                    blocking,
                };
                let Some(expected) = parent.volume else { continue };
                let children: Vec<_> = records
                    .iter()
                    .filter(|record| record.solid == solid.id && (record.bench.base - parent.bench.base).abs() < 1e-6)
                    .collect();
                let tolerance = (expected.abs() * RELATIVE).max(FLOOR);
                if children.is_empty() {
                    // A bench that holds material and produced no dig blocks
                    // has lost its whole subdivision, which is not an empty
                    // result - it is material that fell out of the schedule.
                    if expected > tolerance {
                        diagnostics.push(here(tr!("stage-bench-no-children", volume = format!("{expected:.3}")), true));
                    }
                    continue;
                }
                if let Some(open) = children.iter().find(|record| record.volume.is_none()) {
                    diagnostics.push(StageDiagnostic {
                        entity: Some(format!("{} · RL {:.2}", solid.name, open.flitch.base)),
                        message: tr!("stage-block-not-closed", block = open.name.clone(), area = format!("{:.1}", open.plan_area)),
                        blocking: true,
                    });
                    continue;
                }
                let total: f64 = children.iter().filter_map(|record| record.volume).sum();
                if (total - expected).abs() > tolerance {
                    diagnostics.push(here(
                        tr!(
                            "stage-volume-mismatch",
                            children = format!("{total:.3}"),
                            parent = format!("{expected:.3}"),
                            blocks = children.len().to_string()
                        ),
                        true,
                    ));
                }
                // Reserves reconcile against the bench's own independent
                // measurement, on the accumulators - summed quantities, and
                // weighted numerators and denominators. Comparing grades would
                // compare two ratios that can agree while their contents do
                // not, and summing the children to compare them with
                // themselves would prove nothing at all.
                //
                // A missing reference is an invariant violation, not a reason
                // to skip: Benching is a completed predecessor by the time
                // this runs, so it owes this bench a measurement.
                let Some(reference) = parent.totals.as_ref() else {
                    if parent.measured {
                        diagnostics.push(here(tr!("stage-missing-reference"), true));
                    }
                    continue;
                };
                let mut summed = crate::model::solid_reserves::ReserveTotals::default();
                let mut measured = 0usize;
                for child in &children {
                    match &child.material {
                        MaterialState::Measured(totals) => {
                            if measured == 0 {
                                summed = totals.clone();
                            } else {
                                summed.merge(totals);
                            }
                            measured += 1;
                        }
                        MaterialState::CapacityOnly | MaterialState::NoSchema => {}
                        MaterialState::Unavailable => {
                            diagnostics.push(here(tr!("stage-child-unmeasured", block = child.name.clone()), true));
                        }
                    }
                }
                if measured == 0 {
                    continue;
                }
                let relative = |child: f64, parent: f64| {
                    let scale = parent.abs().max(child.abs()).max(1.0);
                    (child - parent).abs() / scale
                };
                if relative(summed.all.equivalent_blocks, reference.all.equivalent_blocks) > 1e-6 {
                    diagnostics.push(here(
                        tr!(
                            "stage-blocks-mismatch",
                            children = format!("{:.6}", summed.all.equivalent_blocks),
                            parent = format!("{:.6}", reference.all.equivalent_blocks)
                        ),
                        true,
                    ));
                }
                if relative(summed.all.covered_volume, reference.all.covered_volume) > 1e-6 {
                    diagnostics.push(here(
                        tr!(
                            "stage-coverage-mismatch",
                            children = format!("{:.3}", summed.all.covered_volume),
                            parent = format!("{:.3}", reference.all.covered_volume)
                        ),
                        true,
                    ));
                }
                let fields: std::collections::BTreeSet<_> = reference.all.numeric.keys().chain(summed.all.numeric.keys()).copied().collect();
                for field in &fields {
                    let Some(parent_total) = reference.all.numeric.get(field) else {
                        diagnostics.push(here(tr!("stage-field-invented"), true));
                        continue;
                    };
                    let Some(child_total) = summed.all.numeric.get(field) else {
                        diagnostics.push(here(tr!("stage-field-lost"), true));
                        continue;
                    };
                    let name = self
                        .workspace
                        .active_document()
                        .and_then(|document| document.reserve_field(*field))
                        .map_or_else(|| tr!(literal = "?"), |field| field.name.clone());
                    // Numerator and denominator separately: a weighted average
                    // can match while both halves are wrong.
                    if relative(child_total.sum, parent_total.sum) > 1e-6 || relative(child_total.weight, parent_total.weight) > 1e-6 {
                        diagnostics.push(here(
                            tr!(
                                "stage-field-mismatch",
                                field = name,
                                children = format!("{:.6}", child_total.sum),
                                parent = format!("{:.6}", parent_total.sum)
                            ),
                            true,
                        ));
                    }
                }
                // Categories compare on the union of keys, and on the same
                // accumulators as the totals above. Block counts alone can
                // match while the quantities inside them do not, and a
                // category the children invented would not appear at all.
                let category_fields: std::collections::BTreeSet<_> = reference.categories.keys().chain(summed.categories.keys()).copied().collect();
                for field in category_fields {
                    let empty = std::collections::BTreeMap::new();
                    let parent_groups = reference.categories.get(&field).unwrap_or(&empty);
                    let child_groups = summed.categories.get(&field).unwrap_or(&empty);
                    let labels: std::collections::BTreeSet<_> = parent_groups.keys().chain(child_groups.keys()).cloned().collect();
                    for label in labels {
                        let zero = crate::model::solid_reserves::ReserveGroup::default();
                        let parent_group = parent_groups.get(&label).unwrap_or(&zero);
                        let child_group = child_groups.get(&label).unwrap_or(&zero);
                        let mut disagrees = relative(child_group.equivalent_blocks, parent_group.equivalent_blocks) > 1e-6
                            || relative(child_group.covered_volume, parent_group.covered_volume) > 1e-6;
                        let numeric: std::collections::BTreeSet<_> = parent_group.numeric.keys().chain(child_group.numeric.keys()).copied().collect();
                        for id in numeric {
                            let parent_total = parent_group.numeric.get(&id);
                            let child_total = child_group.numeric.get(&id);
                            let (child_sum, child_weight) = child_total.map_or((0.0, 0.0), |total| (total.sum, total.weight));
                            let (parent_sum, parent_weight) = parent_total.map_or((0.0, 0.0), |total| (total.sum, total.weight));
                            disagrees |= relative(child_sum, parent_sum) > 1e-6 || relative(child_weight, parent_weight) > 1e-6;
                        }
                        if disagrees {
                            diagnostics.push(here(
                                tr!(
                                    "stage-category-mismatch",
                                    category = label.clone(),
                                    children = format!("{:.6}", child_group.equivalent_blocks),
                                    parent = format!("{:.6}", parent_group.equivalent_blocks)
                                ),
                                true,
                            ));
                        }
                    }
                }
            }
        }
        // Every dig block belongs to exactly one blast. A block whose ground
        // lies in no blast face means the two partitions disagree, which no
        // amount of re-inferring parentage downstream would fix.
        for record in records.iter().filter(|record| record.blast.is_none()) {
            diagnostics.push(StageDiagnostic {
                entity: Some(format!("{} · RL {:.2}", record.solid_name, record.flitch.base)),
                message: tr!("stage-block-no-blast", block = record.name.clone()),
                blocking: true,
            });
        }
        // Identities must be unique across a run: two blocks sharing one id
        // would silently merge in anything that keys on it.
        let mut seen = std::collections::HashSet::new();
        for record in records {
            if !seen.insert(record.id) {
                diagnostics.push(StageDiagnostic {
                    entity: Some(record.solid_name.clone()),
                    message: tr!("stage-duplicate-block-id", id = record.id.0.to_string()),
                    blocking: true,
                });
            }
        }
        diagnostics
    }
}

/// Why the pipeline is not holding a completed run for the current inputs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StageNotReady {
    NotRun(SolidsStep),
    Stale(SolidsStep),
    Running(SolidsStep),
    Failed { stage: SolidsStep, message: String },
}

/// One turn of the runner.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunStep {
    /// Nothing queued and nothing running.
    Idle,
    /// Begin this stage's work.
    Start(SolidsStep),
    /// Its artifacts are already current; take the next one.
    Skip,
    /// Ask whether this stage's work has landed.
    Evaluate(SolidsStep),
}

/// Everything the stage evaluator and the readiness gate read.
///
/// A borrowed view rather than the app itself: the two paths the reviews kept
/// finding defects in are decisions about committed artifacts, and holding a
/// window and a GPU device is no part of making them. Assembled by `App` each
/// time, and constructible directly in a test.
pub(crate) struct PlanningInputs<'a> {
    pub(crate) solids: &'a [crate::model::Solid],
    pub(crate) caches: &'a HashMap<crate::model::SolidId, crate::app::commands::solids_view::ViewSolid>,
    /// Whether the project defines any reserve fields at all.
    pub(crate) has_schema: bool,
}

impl PlanningInputs<'_> {
    /// Where one solid's reserve measurement stands for one scope.
    pub(crate) fn reserve_state(&self, solid: &crate::model::Solid, scope: crate::app::commands::solids_view::ReserveScope) -> ReserveState {
        let _ = self.has_schema;
        self.caches.get(&solid.id).map_or(ReserveState::Waiting, |cache| cache.reserve_state(scope))
    }
}

impl PlanningInputs<'_> {
    /// Whether each stage's own artifacts have landed, and what they have to
    /// say. Pure: it reads committed artifacts and starts nothing, which is
    /// what lets it be driven directly in a test.
    pub(crate) fn evaluate_geometry_stage(&self, stage: SolidsStep) -> StageOutcome {
        let demand = stage.geometry_demand().expect("a geometric stage");
        let needs_partition = demand >= GeometryDemand::Partition;
        let solids = self.solids;
        let mut diagnostics = Vec::new();
        let mut waiting = 0;
        let mut entities = 0;
        for solid in solids {
            let Some(cache) = self.caches.get(&solid.id) else {
                waiting += 1;
                continue;
            };
            // Deferred inputs coming back is a wait, not a fault: a run over a
            // project whose surfaces are not yet loaded has to be able to
            // finish without anyone visiting a Setup page first.
            if cache.awaiting_inputs_through(demand) {
                waiting += 1;
                continue;
            }
            // Only faults at or before this stage's own artifact count against
            // it: a bad strip cannot fail Solids, and a bad cut cannot fail
            // Benching.
            if let Some(error) = cache.error_through(demand) {
                diagnostics.push(StageDiagnostic {
                    entity: Some(solid.name.clone()),
                    message: error.to_owned(),
                    blocking: true,
                });
                continue;
            }
            // The Solids stage owns the envelope alone.
            if demand == GeometryDemand::Envelope {
                match cache.envelope_is_built() {
                    true => entities += 1,
                    false => waiting += 1,
                }
                continue;
            }
            let Some(body) = cache.body() else {
                waiting += 1;
                continue;
            };
            if demand == GeometryDemand::Blasting {
                match cache.blast_faces() {
                    Some(faces) => entities += faces.len(),
                    None => waiting += 1,
                }
                continue;
            }
            // An explicitly empty occupied region is a successful empty
            // output, not pending work: a plan that reaches no material has
            // finished having nothing to report.
            if body.bench_parts.is_empty() {
                diagnostics.push(StageDiagnostic {
                    entity: Some(solid.name.clone()),
                    message: tr!("stage-no-occupied-bands"),
                    blocking: false,
                });
            }
            if !needs_partition {
                // Benching owns the independent bench measurement the dig
                // blocks are later reconciled against. Completing without it
                // would leave that reconciliation quietly optional.
                match self.reserve_state(solid, crate::app::commands::solids_view::ReserveScope::Bench) {
                    ReserveState::Waiting => waiting += 1,
                    ReserveState::Failed(message) => diagnostics.push(StageDiagnostic {
                        entity: Some(solid.name.clone()),
                        message,
                        blocking: measurement_required(solid),
                    }),
                    ReserveState::CapacityOnly | ReserveState::Complete => entities += body.bench_parts.len(),
                }
                continue;
            }
            let Some(geometry) = cache.geometry() else {
                waiting += 1;
                continue;
            };
            entities += geometry.parts().len();
            if geometry.parts().iter().any(|part| part.volume.is_none()) {
                diagnostics.push(StageDiagnostic {
                    entity: Some(solid.name.clone()),
                    message: tr!("planning-reserve-open-solid"),
                    blocking: stage == SolidsStep::DigStrips,
                });
            }
            if stage == SolidsStep::DigStrips {
                match self.reserve_state(solid, crate::app::commands::solids_view::ReserveScope::Dig) {
                    ReserveState::Waiting => waiting += 1,
                    ReserveState::CapacityOnly => diagnostics.push(StageDiagnostic {
                        entity: Some(solid.name.clone()),
                        message: tr!("planning-reserve-capacity-only"),
                        blocking: false,
                    }),
                    ReserveState::Failed(message) => diagnostics.push(StageDiagnostic {
                        entity: Some(solid.name.clone()),
                        message,
                        blocking: measurement_required(solid),
                    }),
                    ReserveState::Complete => {}
                }
            }
        }
        // A fault settles the stage. Waiting only means work that can still
        // arrive; a solid that failed has nothing left to deliver, and calling
        // it Working would leave the run unfinishable and Retry unreachable.
        let blocking = diagnostics.iter().any(|entry| entry.blocking);
        if waiting > 0 && !blocking {
            return StageOutcome::Working {
                message: Some(tr!("stage-waiting-solids", count = waiting.to_string())),
            };
        }
        if blocking {
            return StageOutcome::Settled { entities, diagnostics };
        }
        StageOutcome::Settled { entities, diagnostics }
    }
}

impl crate::app::App<'_> {}

/// Whether this solid is configured to be measured against a block model.
///
/// A dump or stockpile with no model is a deliberate geometry-only result. One
/// that *names* a model has asked for a measurement, and failing to produce it
/// is a failure whatever kind the solid is - degrading that to a warning on
/// the strength of the kind alone is how a stage came to read Complete while
/// the snapshot refused to export it.
fn measurement_required(solid: &crate::model::Solid) -> bool {
    solid.block_model.is_some() || solid.kind.requires_block_model()
}

pub(crate) enum StageOutcome {
    /// Still waiting on background work.
    Working { message: Option<String> },
    /// The work has landed; these are its findings.
    Settled { diagnostics: Vec<StageDiagnostic>, entities: usize },
}

pub(crate) enum ReserveState {
    Waiting,
    CapacityOnly,
    Complete,
    Failed(String),
}

impl SolidsStep {
    pub(crate) fn index(self) -> usize {
        SolidsStep::ALL.iter().position(|stage| *stage == self).unwrap_or(0)
    }

    /// How much geometry this stage needs built to do its own work.
    pub(crate) fn geometry_demand(self) -> Option<GeometryDemand> {
        match self {
            Self::FieldList | Self::BlockModels => None,
            Self::Solids => Some(GeometryDemand::Envelope),
            Self::Benching => Some(GeometryDemand::Body),
            Self::Blasting => Some(GeometryDemand::Blasting),
            Self::DigStrips => Some(GeometryDemand::Partition),
        }
    }

    /// Stable egui id for this step's tree row.
    pub(crate) fn tree_id(self) -> &'static str {
        match self {
            Self::FieldList => "solids_field_list_step",
            Self::BlockModels => "solids_block_models_step",
            Self::Solids => "solids_solids_step",
            Self::Benching => "solids_benching_step",
            Self::Blasting => "solids_blasting_step",
            Self::DigStrips => "solids_dig_strips_step",
        }
    }

    pub(crate) fn label(self) -> String {
        match self {
            Self::FieldList => tr!("planning-field-list"),
            Self::BlockModels => tr!("planning-block-models"),
            Self::Solids => tr!("planning-solids"),
            Self::Benching => tr!("planning-benching"),
            Self::Blasting => tr!("planning-blasting"),
            Self::DigStrips => tr!("planning-dig-strips"),
        }
    }
}
