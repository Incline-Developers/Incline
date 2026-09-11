//! Shared, background-built geometry for every solid listed in View.
//!
//! The artifacts built here are *presentation independent*. Which step is
//! open, which blast is selected and where the camera is pointing change what
//! is drawn out of them, never what is built: a solid is cut into its benches,
//! its flitches and its dig blocks once, and every consumer reads that one
//! result. Selecting a blast used to rebuild the whole body, which is exactly
//! what the pipeline is meant to make impossible.
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use glam::DVec2;

use super::solids::{CutBand, bench_bands, build_bench_body, plan_bands, preview_triangulation};
use crate::{
    model::{
        DigBlockId, Solid, SolidId,
        arrangement::{self, Face},
        formats::mesh_data::Triangulation,
        solid_reserves::{ReserveBand, ReserveOutcome, ReservePartition, ReservePiece},
        triangulation::{OpenTriangulation, TriangulationId},
    },
    ui::state::{BenchSelection, BlastShapeRef, SolidPreviewSummary, SolidsViewRow},
};

/// One built piece of a solid: the mesh, where it sits in the benching plan,
/// what it is worth, and which planning entity it is.
///
/// A record rather than an index into four parallel arrays, so a piece cannot
/// be measured against another piece's band or another piece's identity.
pub(crate) struct SolidPart {
    pub(crate) mesh: OpenTriangulation,
    pub(crate) band: CutBand,
    /// The bench this part lies in - a whole bench part names itself.
    pub(crate) bench: BenchSelection,
    /// Which blast of that bench holds it. A bench with no cuts drawn on it
    /// is one blast, so this is set for every part of a built solid.
    pub(crate) blast: Option<BlastShapeRef>,
    /// Which dig block this part is, for the flitch-level parts.
    pub(crate) block: Option<DigPiece>,
    /// `None` when the piece did not come out closed and so cannot be measured.
    pub(crate) volume: Option<f64>,
}

/// One blast face of one bench, derived once and read by every consumer.
#[derive(Clone)]
pub(crate) struct BlastFace {
    pub(crate) bench: BenchSelection,
    pub(crate) face: Face,
    pub(crate) anchor: [f64; 2],
    pub(crate) area: f64,
}

impl BlastFace {
    pub(super) fn shape_ref(&self, solid: SolidId) -> BlastShapeRef {
        BlastShapeRef::new(solid, self.bench.base, self.anchor)
    }
}

/// One flitch as it comes out of the Benching stage, before any cut divides
/// it. The mesh is shared, so the Dig Strips stage clips from it without
/// rebuilding the body it came out of.
pub(crate) struct FlitchPart {
    pub(crate) mesh: OpenTriangulation,
    pub(crate) band: CutBand,
    pub(crate) bench: BenchSelection,
    pub(crate) volume: Option<f64>,
}

/// What the Solids and Benching stages commit: the closed body, cut into
/// benches and flitches, with the ground each covers.
///
/// Deliberately free of anything a cut line decides. Drawing a blast cut has
/// to re-partition the solid, and must never rebuild it - which is exactly
/// what one artifact keyed on both would have done.
pub(crate) struct SolidBody {
    /// Whole-bench parts, never cut laterally. What Blasting draws on.
    pub(crate) bench_parts: Vec<SolidPart>,
    pub(crate) flitch_parts: Vec<FlitchPart>,
    /// Bench ground, by the bit pattern of the bench base RL.
    pub(crate) bench_footprints: HashMap<u64, Vec<Vec<DVec2>>>,
    /// Flitch ground, by the bit pattern of the flitch base RL.
    pub(crate) flitch_footprints: HashMap<u64, Vec<Vec<DVec2>>>,
    /// The flitch bands that actually hold material.
    pub(crate) occupied_bands: Vec<CutBand>,
}

/// What the Blasting stage commits: each bench's ground divided into blasts.
pub(crate) struct SolidBlasting {
    pub(crate) blast_faces: Vec<BlastFace>,
}

/// What the Dig Strips stage commits: the terminal blocks.
pub(crate) struct SolidPartition {
    /// Flitch-level parts, cut into dig blocks. What View draws.
    pub(crate) parts: Vec<SolidPart>,
}

/// Every committed half of one solid's geometry, for consumers that need more
/// than one of them together.
#[derive(Clone, Copy)]
pub(crate) struct SolidGeometry<'artifact> {
    pub(crate) body: &'artifact SolidBody,
    pub(crate) blasting: &'artifact SolidBlasting,
    pub(crate) partition: &'artifact SolidPartition,
}

impl SolidGeometry<'_> {
    pub(crate) fn bench_parts(&self) -> &[SolidPart] {
        &self.body.bench_parts
    }

    pub(crate) fn parts(&self) -> &[SolidPart] {
        &self.partition.parts
    }

    pub(crate) fn blast_faces(&self) -> &[BlastFace] {
        &self.blasting.blast_faces
    }

    pub(crate) fn occupied_bands(&self) -> &[CutBand] {
        &self.body.occupied_bands
    }

    pub(crate) fn flitch_footprints(&self) -> &HashMap<u64, Vec<Vec<DVec2>>> {
        &self.body.flitch_footprints
    }
}

/// One solid's artifacts, one per stage that owns one.
///
/// Four separate keys, products and errors rather than one of each. A bench
/// height changes the slabs and reuses the envelope; a blast cut changes the
/// blast faces and reuses the slabs; a strip changes the dig blocks and reuses
/// the blasts. And a fault in any of them fails only the stage that owns it.
/// One artifact's request state: what was asked for, and which attempt.
///
/// The attempt token is the point. A content fingerprint alone cannot tell a
/// retry from the cancelled attempt it replaces: identical inputs produce an
/// identical fingerprint, so a result already computed by the cancelled
/// attempt would satisfy the new one's validity check and publish silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    /// Content fingerprint of the inputs this attempt was made for.
    pub(crate) key: u64,
    /// Unique to this attempt, even for identical inputs.
    pub(crate) token: u64,
}

impl Request {
    fn new(key: u64) -> Self {
        Self {
            key,
            token: NEXT_REQUEST_TOKEN.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// One stage's slot: the attempt in flight, the product it committed, and why
/// it failed if it did.
///
/// Request and product are deliberately separate. A key that records only what
/// was *asked for* says a cancelled artifact is in hand, and the next sync
/// declines to ask again - a retry that waits for ever.
pub(crate) struct Stage<T> {
    request: Option<Request>,
    product: Option<T>,
    error: Option<String>,
    /// Waiting on deferred inputs to be restored, rather than on a worker.
    /// Not a failure: a run over unloaded surfaces has to be able to finish.
    awaiting_inputs: bool,
}

impl<T> Default for Stage<T> {
    fn default() -> Self {
        Self {
            request: None,
            product: None,
            error: None,
            awaiting_inputs: false,
        }
    }
}

impl<T> Stage<T> {
    /// Whether `token` names the attempt currently in flight. The one gate a
    /// finished result has to pass before it may publish.
    pub(crate) fn accepts(&self, token: u64) -> bool {
        self.request.is_some_and(|request| request.token == token)
    }

    /// Whether an attempt for these inputs is in hand - running or finished.
    fn is_current(&self, key: u64) -> bool {
        self.request.is_some_and(|request| request.key == key)
    }

    /// Start an attempt, discarding whatever the last one left behind.
    fn begin(&mut self, key: u64) -> u64 {
        let request = Request::new(key);
        self.request = Some(request);
        self.product = None;
        self.error = None;
        self.awaiting_inputs = false;
        request.token
    }

    fn settle(&mut self, product: T) {
        self.product = Some(product);
        self.error = None;
        self.awaiting_inputs = false;
    }

    fn fail(&mut self, error: String) {
        self.product = None;
        self.error = Some(error);
        self.awaiting_inputs = false;
    }

    /// Mark this attempt as waiting on deferred inputs rather than a worker.
    fn await_inputs(&mut self) {
        self.awaiting_inputs = true;
    }

    /// Retire an attempt that produced nothing, so the next sync asks again.
    fn retire_if_unfinished(&mut self) {
        if self.product.is_none() && self.error.is_none() {
            self.request = None;
        }
    }

    /// Retire a settled failure, so an explicit rerun tries once more. An
    /// unchanged-input retry is otherwise refused by design: a stage that
    /// failed must not re-fail on every refresh.
    fn retire_failure(&mut self) {
        if self.error.is_some() {
            self.request = None;
            self.error = None;
        }
    }

    fn product(&self) -> Option<&T> {
        self.product.as_ref()
    }

    fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn is_awaiting_inputs(&self) -> bool {
        self.awaiting_inputs
    }
}

/// What to do about a request whose inputs are not resident.
#[derive(Debug, PartialEq, Eq)]
enum RestoreDecision {
    /// Ask for them; this request has not asked yet.
    Request,
    /// A load is in flight - wait for it.
    Wait,
    /// This request already asked and nothing is in flight, so the load is not
    /// coming. A stated failure, not an indefinite wait.
    Failed,
}

fn restore_decision(attempted: bool, pending: bool) -> RestoreDecision {
    match (attempted, pending) {
        (false, _) => RestoreDecision::Request,
        (true, true) => RestoreDecision::Wait,
        (true, false) => RestoreDecision::Failed,
    }
}

/// What one completed reserve measurement produced.
pub(crate) struct ReserveProduct {
    /// Totals per entry of the partition it measured.
    totals: Vec<crate::model::solid_reserves::ReserveTotals>,
    resolution: Option<crate::model::solid_reserves::ReserveResolution>,
    availability: ReserveAvailability,
}

/// Why a completed reserve measurement holds the totals it does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReserveAvailability {
    Measured,
    /// A dump or stockpile with no model: geometry and capacity only.
    CapacityOnly,
    /// The project defines no reserve fields, so there is nothing to measure.
    NoSchema,
}

pub(crate) struct ViewSolid {
    /// The Solids stage: what the closed body is built from. The cache entry's
    /// own identity, so it is not inside a [`Stage`] like the rest.
    pub(crate) key: u64,
    pub(super) runtime: u32,
    solid: Solid,
    sources: [Option<Arc<Triangulation>>; 2],
    envelope: Stage<Arc<OpenTriangulation>>,
    /// The Benching stage: the envelope plus the plan that divides it.
    body: Stage<Arc<SolidBody>>,
    /// The Blasting stage: the benches plus the cuts across them.
    blasting: Stage<Arc<SolidBlasting>>,
    /// The Dig Strips stage: the blasts plus each flitch's own strips.
    partition: Stage<SolidPartition>,
    /// Reserves over whole benches, from the Benching stage. Independent of
    /// the dig-block figures, so the two can be reconciled against each other.
    bench_reserves: Stage<ReserveProduct>,
    reserves: Stage<ReserveProduct>,
}

impl ViewSolid {
    /// The source mesh this solid's envelope was built from, but only while it
    /// still stands for the content the caller is asking about.
    ///
    /// Keeping an unloaded surface's last mesh is what lets a solid survive an
    /// eviction. Keeping it across an *edit* would build the pit from the old
    /// geometry while the fingerprint claimed the new - so the key has to
    /// match, and the key carries the content versions.
    fn retained_source(&self, key: u64, index: usize) -> Option<Arc<Triangulation>> {
        (self.key == key).then(|| self.sources[index].clone()).flatten()
    }

    pub(crate) fn is_built(&self) -> bool {
        self.geometry().is_some()
    }

    /// Every committed half, once they are all there.
    pub(crate) fn geometry(&self) -> Option<SolidGeometry<'_>> {
        Some(SolidGeometry {
            body: self.body.product()?,
            blasting: self.blasting.product()?,
            partition: self.partition.product()?,
        })
    }

    /// The committed body, whatever the stages after it are doing.
    pub(crate) fn body(&self) -> Option<&SolidBody> {
        self.body.product().map(Arc::as_ref)
    }

    pub(crate) fn envelope_is_built(&self) -> bool {
        self.envelope.product().is_some()
    }

    pub(crate) fn blast_faces(&self) -> Option<&[BlastFace]> {
        self.blasting.product().map(|blasting| blasting.blast_faces.as_slice())
    }

    /// The first thing that went wrong, in stage order: a dig error on a solid
    /// whose envelope failed says nothing useful.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error_through(crate::app::planning_pipeline::GeometryDemand::Partition)
    }

    /// The first fault at or before `stage`'s own artifact, so a stage is only
    /// ever failed by its own half of the work or by one it depends on.
    pub(crate) fn error_through(&self, stage: crate::app::planning_pipeline::GeometryDemand) -> Option<&str> {
        use crate::app::planning_pipeline::GeometryDemand;
        [
            (GeometryDemand::Envelope, self.envelope.error()),
            (GeometryDemand::Body, self.body.error()),
            (GeometryDemand::Blasting, self.blasting.error()),
            (GeometryDemand::Partition, self.partition.error()),
        ]
        .into_iter()
        .take_while(|(demand, _)| *demand <= stage)
        .find_map(|(_, error)| error)
    }

    /// Whether any artifact up to `stage` is still waiting on deferred inputs
    /// coming back. Waiting, not failed: a run over unloaded surfaces has to
    /// be able to finish on its own.
    pub(crate) fn awaiting_inputs_through(&self, stage: crate::app::planning_pipeline::GeometryDemand) -> bool {
        use crate::app::planning_pipeline::GeometryDemand;
        [
            (GeometryDemand::Envelope, self.envelope.is_awaiting_inputs()),
            (GeometryDemand::Body, self.body.is_awaiting_inputs()),
            (GeometryDemand::Blasting, self.blasting.is_awaiting_inputs()),
            (GeometryDemand::Partition, self.partition.is_awaiting_inputs()),
        ]
        .into_iter()
        .take_while(|(demand, _)| *demand <= stage)
        .any(|(_, waiting)| waiting)
    }

    /// Everything about this solid's artifacts that changes what is drawn:
    /// which attempts are in hand, and what each produced.
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for (request, settled, error) in [
            (self.envelope.request, self.envelope.product().is_some(), self.envelope.error()),
            (self.body.request, self.body.product().is_some(), self.body.error()),
            (self.blasting.request, self.blasting.product().is_some(), self.blasting.error()),
            (self.partition.request, self.partition.product().is_some(), self.partition.error()),
            (self.bench_reserves.request, self.bench_reserves.product().is_some(), self.bench_reserves.error()),
            (self.reserves.request, self.reserves.product().is_some(), self.reserves.error()),
        ] {
            request.map(|request| (request.key, request.token)).hash(&mut hasher);
            settled.hash(&mut hasher);
            error.hash(&mut hasher);
        }
        hasher.finish()
    }

    #[cfg(test)]
    fn stage_mut(&mut self, kind: SolidArtifact) -> &mut dyn StageSlot {
        match kind {
            SolidArtifact::Envelope => &mut self.envelope,
            SolidArtifact::Body => &mut self.body,
            SolidArtifact::Blasting => &mut self.blasting,
            SolidArtifact::Partition => &mut self.partition,
            SolidArtifact::BenchReserves => &mut self.bench_reserves,
            SolidArtifact::DigReserves => &mut self.reserves,
        }
    }

    /// A cache entry with no artifacts, as one starts life.
    #[cfg(test)]
    pub(crate) fn empty_for_test(id: SolidId) -> Self {
        Self {
            key: 7,
            runtime: 1,
            solid: Solid {
                id,
                name: format!("Solid {}", id.0),
                kind: crate::model::SolidKind::Pit,
                surface: None,
                topography: None,
                block_model: None,
                color: [0.0; 4],
                benching: Default::default(),
                blasting: Default::default(),
            },
            sources: [None, None],
            envelope: Stage::default(),
            body: Stage::default(),
            blasting: Stage::default(),
            partition: Stage::default(),
            bench_reserves: Stage::default(),
            reserves: Stage::default(),
        }
    }

    /// Settle an artifact with a product built for a test.
    #[cfg(test)]
    pub(crate) fn settle_for_test(&mut self, kind: SolidArtifact, benches: usize, blocks: usize) {
        let bench_parts = (0..benches).map(|index| test_part(index as f64 * 12.0)).collect::<Vec<_>>();
        match kind {
            SolidArtifact::Envelope => {
                self.envelope.begin(1);
                // The envelope's own mesh is never read by the evaluator.
                self.envelope.settle(Arc::new(test_mesh()));
            }
            SolidArtifact::Body => {
                self.body.begin(1);
                self.body.settle(Arc::new(SolidBody {
                    bench_parts,
                    flitch_parts: Vec::new(),
                    bench_footprints: Default::default(),
                    flitch_footprints: Default::default(),
                    occupied_bands: Vec::new(),
                }));
            }
            SolidArtifact::Blasting => {
                self.blasting.begin(1);
                self.blasting.settle(Arc::new(SolidBlasting { blast_faces: Vec::new() }));
            }
            SolidArtifact::Partition => {
                self.partition.begin(1);
                self.partition.settle(SolidPartition {
                    parts: (0..blocks).map(|index| test_part(index as f64 * 4.0)).collect(),
                });
            }
            SolidArtifact::BenchReserves | SolidArtifact::DigReserves => {
                let scope = if kind == SolidArtifact::BenchReserves { ReserveScope::Bench } else { ReserveScope::Dig };
                let stage = scope.slot(self);
                stage.begin(1);
                stage.settle(ReserveProduct {
                    totals: vec![Default::default(); benches.max(blocks)],
                    resolution: None,
                    availability: ReserveAvailability::Measured,
                });
            }
        }
    }

    /// Settle a reserve scope as a deliberate capacity-only result.
    #[cfg(test)]
    pub(crate) fn settle_capacity_only(&mut self, scope: ReserveScope) {
        let stage = scope.slot(self);
        stage.begin(1);
        stage.settle(ReserveProduct {
            totals: Vec::new(),
            resolution: None,
            availability: ReserveAvailability::CapacityOnly,
        });
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(&mut self, kind: SolidArtifact, message: &str) {
        match kind {
            SolidArtifact::Envelope => {
                self.envelope.begin(1);
                self.envelope.fail(message.to_owned());
            }
            SolidArtifact::Body => {
                self.body.begin(1);
                self.body.fail(message.to_owned());
            }
            SolidArtifact::Blasting => {
                self.blasting.begin(1);
                self.blasting.fail(message.to_owned());
            }
            SolidArtifact::Partition => {
                self.partition.begin(1);
                self.partition.fail(message.to_owned());
            }
            SolidArtifact::BenchReserves | SolidArtifact::DigReserves => {
                let scope = if kind == SolidArtifact::BenchReserves { ReserveScope::Bench } else { ReserveScope::Dig };
                let stage = scope.slot(self);
                stage.begin(1);
                stage.fail(message.to_owned());
            }
        }
    }

    /// Leave an artifact waiting on deferred inputs.
    #[cfg(test)]
    pub(crate) fn await_inputs_for_test(&mut self, kind: SolidArtifact) {
        if kind == SolidArtifact::Envelope {
            self.envelope.begin(1);
            self.envelope.await_inputs();
        }
    }

    /// Does this solid's request for `kind` accept a result bearing `token`?
    pub(crate) fn accepts(&self, kind: SolidArtifact, token: u64) -> bool {
        match kind {
            SolidArtifact::Envelope => self.envelope.accepts(token),
            SolidArtifact::Body => self.body.accepts(token),
            SolidArtifact::Blasting => self.blasting.accepts(token),
            SolidArtifact::Partition => self.partition.accepts(token),
            SolidArtifact::BenchReserves => self.bench_reserves.accepts(token),
            SolidArtifact::DigReserves => self.reserves.accepts(token),
        }
    }
}

/// Begin an attempt on a slot without naming its product type, so one test
/// can walk every artifact kind through the same submission path.
#[cfg(test)]
pub(crate) trait StageSlot {
    fn begin(&mut self, key: u64) -> u64;
}

#[cfg(test)]
impl<T> StageSlot for Stage<T> {
    fn begin(&mut self, key: u64) -> u64 {
        Stage::begin(self, key)
    }
}

/// A one-triangle closed-enough mesh; nothing the evaluator reads looks at it.
#[cfg(test)]
fn test_mesh() -> OpenTriangulation {
    use crate::model::formats::mesh_data::Vertex;
    let mesh = Arc::new(
        Triangulation::from_vertices_and_faces(
            vec![Vertex { x: 0.0, y: 0.0, z: 0.0 }, Vertex { x: 1.0, y: 0.0, z: 0.0 }, Vertex { x: 0.0, y: 1.0, z: 0.0 }],
            vec![[0, 1, 2]],
        )
        .expect("test mesh"),
    );
    let spatial = Arc::new(crate::model::spatial::TriangleBvh::build(&mesh));
    let order = Arc::new(crate::model::triangulation::morton_surface_face_order(&mesh));
    preview_triangulation("test".to_owned(), mesh, spatial, Vec::new(), order, [1.0; 4], [0.0; 4])
}

/// A measured part at one elevation, for building committed artifacts.
#[cfg(test)]
fn test_part(base: f64) -> SolidPart {
    let band = CutBand {
        selection: BenchSelection {
            base,
            top: base + 12.0,
            is_flitch: false,
        },
        interval: 0,
        position: 0,
    };
    SolidPart {
        mesh: test_mesh(),
        band,
        bench: band.selection,
        blast: None,
        block: None,
        volume: Some(100.0),
    }
}

/// Which of a solid's artifacts a job belongs to.
///
/// Part of the job key, so cancellation can name one artifact of one solid
/// and validity can name one attempt at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SolidArtifact {
    Envelope,
    Body,
    Blasting,
    Partition,
    BenchReserves,
    DigReserves,
}

/// A dig block cut out of a flitch: which block it is, what it is called, and
/// how much ground it covers.
#[derive(Clone)]
pub(crate) struct DigPiece {
    /// Stable across reruns that leave this ground alone.
    pub(crate) id: DigBlockId,
    pub(crate) key: BlastShapeRef,
    pub(crate) name: String,
    pub(crate) area: f64,
    /// The ground it covers, kept so the next run can recognise it.
    pub(crate) face: Face,
    /// Identities this block's ground used to be held under, where a split or
    /// a merge changed it. Recorded rather than left implicit so a scheduler
    /// can trace where material went instead of seeing a deletion and an
    /// unrelated creation.
    pub(crate) replaces: Vec<DigBlockId>,
}

/// One dig block's identity as the project remembers it between runs.
///
/// Held apart from the geometry caches on purpose. A body rebuild throws its
/// cache away, and a forced rerun throws all of them away; neither is a reason
/// for ground that did not move to be renamed underneath a schedule.
#[derive(Clone)]
pub(crate) struct DigBlockIdentity {
    pub(crate) id: DigBlockId,
    /// Base RL of the flitch the block was cut from.
    pub(crate) band: f64,
    pub(crate) anchor: [f64; 2],
    /// The ground it covered, so a split can be told from a merge.
    pub(crate) face: Face,
}

/// `amount` of the way from `color` towards `towards`, alpha untouched.
fn blended(color: [f32; 4], towards: [f32; 4], amount: f32) -> [f32; 4] {
    let mut blend = color;
    for (channel, target) in blend.iter_mut().zip(towards).take(3) {
        *channel += (target - *channel) * amount;
    }
    blend
}

/// A face's ground: its outer ring less its holes.
pub(super) fn face_plan_area(face: &Face) -> f64 {
    face.iter()
        .enumerate()
        .map(|(index, ring)| arrangement::signed_area(ring).abs() * if index == 0 { 1.0 } else { -1.0 })
        .sum()
}

/// What the body depends on: the solid's definition, its surfaces and its
/// benching plan. Deliberately free of the cut lines - those divide the body
/// rather than shaping it - and of the open step, the selected blast, the
/// camera and the selection, none of which may change what is built at all.
fn envelope_key(runtime: u32, solid: &Solid, versions: &[Option<u64>; 2]) -> u64 {
    let mut key = DefaultHasher::new();
    runtime.hash(&mut key);
    solid.id.hash(&mut key);
    (solid.kind as u8).hash(&mut key);
    solid.surface.hash(&mut key);
    solid.topography.hash(&mut key);
    // Content versions, not mesh pointers: unloading and restoring a surface
    // changes every pointer without changing a vertex, and rebuilding a pit
    // because of that would be work no edit asked for.
    versions.hash(&mut key);
    key.finish()
}

/// What the bench and flitch slabs depend on: the envelope, and the plan that
/// divides it. Changing a bench height re-slabs; it does not rebuild the
/// envelope, which is by far the most expensive thing here.
fn body_key(envelope: u64, solid: &Solid) -> u64 {
    let mut key = DefaultHasher::new();
    envelope.hash(&mut key);
    solid.benching.top.to_bits().hash(&mut key);
    for interval in &solid.benching.intervals {
        interval.base.to_bits().hash(&mut key);
        interval.bench.to_bits().hash(&mut key);
        interval.flitch.to_bits().hash(&mut key);
    }
    key.finish()
}

/// What the blast partition depends on: the benches, and the lines drawn
/// across them. Strip cuts belong to the stage after this one.
fn blasting_key(body: u64, solid: &Solid) -> u64 {
    let mut key = DefaultHasher::new();
    body.hash(&mut key);
    for bench in &solid.blasting.benches {
        bench.base.to_bits().hash(&mut key);
        for cut in &bench.cuts {
            cut.geometry_hash().hash(&mut key);
        }
    }
    key.finish()
}

/// What the dig blocks depend on: the blast partition above them, and each
/// flitch's own strips. Not the open step, not the selected blast, not the
/// camera.
fn partition_key(blasting: u64, solid: &Solid) -> u64 {
    let mut key = DefaultHasher::new();
    blasting.hash(&mut key);
    for bench in &solid.blasting.dig_strips {
        bench.base.to_bits().hash(&mut key);
        for cut in &bench.cuts {
            cut.geometry_hash().hash(&mut key);
        }
    }
    key.finish()
}

pub(super) fn mesh_volume(mesh: &Triangulation) -> f64 {
    let origin = mesh.bounds().min;
    mesh.face_vertex_indices_iter()
        .map(|face| {
            let [a, b, c] = face.map(|i| {
                let v = mesh.vertices()[i];
                glam::DVec3::new(v.x - origin.x, v.y - origin.y, v.z - origin.z)
            });
            a.dot(b.cross(c)) / 6.0
        })
        .sum::<f64>()
        .abs()
}

/// Whether any row of the selection names this solid at all. An empty
/// selection means everything, the same as [`selected`].
fn selected_solid(selection: &[SolidsViewRow], solid: SolidId) -> bool {
    selection.is_empty() || selection.iter().any(|row| row.solid == solid)
}

pub(super) fn selected(selection: &[SolidsViewRow], solid: SolidId, band: Option<BenchSelection>) -> bool {
    selection.is_empty()
        || selection.iter().any(|row| {
            row.solid == solid
                && row
                    .band
                    .is_none_or(|wanted| band.is_some_and(|band| band.base >= wanted.base - 1e-6 && band.top <= wanted.top + 1e-6))
        })
}

/// One job key naming one attempt at one artifact of one solid.
fn artifact_job(solid: SolidId, kind: SolidArtifact, token: u64) -> crate::app::jobs::JobKey {
    crate::app::jobs::JobKey::SolidArtifact { solid, kind, token }
}

/// Ids for the generated meshes, counted down from the top so nothing in the
/// project can ever take one.
static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(u64::MAX - (1 << 32));
static NEXT_DIG_BLOCK_ID: AtomicU64 = AtomicU64::new(1);
/// Unique per attempt at building any artifact, so a result can name the
/// attempt it belongs to rather than only the inputs it was computed from.
static NEXT_REQUEST_TOKEN: AtomicU64 = AtomicU64::new(1);

fn next_view_id() -> TriangulationId {
    TriangulationId(NEXT_VIEW_ID.fetch_sub(1, Ordering::Relaxed))
}

impl crate::app::App<'_> {
    /// `displaying` says whether a page that shows these artifacts is open.
    /// When it is not, the artifacts are still built and measured - only the
    /// display list, the pick and the selection pruning are skipped.
    pub(crate) fn sync_solids_view(&mut self, displaying: bool) {
        let Some(project) = self.workspace.active_project() else {
            return;
        };
        let runtime = project.runtime_id;
        let solids = self.workspace.active_document().map(|doc| doc.solids().to_vec()).unwrap_or_default();
        self.solid_view_cache
            .retain(|id, cache| cache.runtime == runtime && solids.iter().any(|solid| solid.id == *id));
        self.editor.solid_view_bands.retain(|id, _| solids.iter().any(|solid| solid.id == *id));
        self.dig_block_identities.retain(|id, _| solids.iter().any(|solid| solid.id == *id));
        // Only a running stage may start authoritative work. Display reads
        // what the last run committed and nothing else, so opening View on a
        // project that has never been run finishes no calculation.
        let demand = self.planning_pipeline.as_ref().and_then(crate::app::planning_pipeline::PlanningPipeline::demand);
        if let Some(demand) = demand {
            use crate::app::planning_pipeline::GeometryDemand;
            for solid in &solids {
                // Each stage asks only for its own artifact, and only once the
                // one it reads is committed. Nothing here reaches forward.
                self.sync_solid_envelope(runtime, solid);
                if demand >= GeometryDemand::Body {
                    self.sync_solid_body(runtime, solid);
                }
                if demand >= GeometryDemand::Blasting {
                    self.sync_solid_blasting(runtime, solid);
                }
                if demand >= GeometryDemand::Partition {
                    self.sync_solid_partition(runtime, solid);
                }
            }
            // Bench reserves are the Benching stage's own output, and the
            // reference the dig blocks are later reconciled against.
            if demand >= GeometryDemand::Body {
                self.sync_solid_reserves(&solids, ReserveScope::Bench);
            }
            if demand >= GeometryDemand::Partition {
                self.sync_solid_reserves(&solids, ReserveScope::Dig);
            }
        }
        if !displaying {
            return;
        }
        self.prune_solids_view_selection(&solids);
        self.resolve_solid_preview_pick();
        self.rebuild_solid_view_body(&solids);
    }

    /// Solids: build the closed body between the solid's two surfaces.
    ///
    /// The single most expensive thing in the pipeline, and the one keyed
    /// least often: only the solid's own definition and its source meshes
    /// reach it. A benching, blasting or strip edit never gets this far.
    fn sync_solid_envelope(&mut self, runtime: u32, solid: &Solid) {
        let ids = [solid.surface, solid.topography];
        let versions = ids.map(|id| self.surface_content_version(id));
        let key = envelope_key(runtime, solid, &versions);
        let sources: [Option<Arc<Triangulation>>; 2] = std::array::from_fn(|index| {
            ids[index]
                .and_then(|id| {
                    self.triangulations
                        .iter()
                        .find(|item| item.id == id && item.mesh.face_count() > 0)
                        .map(|item| item.mesh.clone())
                })
                .or_else(|| self.solid_view_cache.get(&solid.id).and_then(|cache| cache.retained_source(key, index)))
        });
        let attempted = self
            .solid_view_cache
            .get(&solid.id)
            .is_some_and(|cache| cache.key == key && cache.envelope.is_awaiting_inputs());
        if self.solid_view_cache.get(&solid.id).is_some_and(|cache| cache.key == key) {
            // The solid itself has not moved, but its name, colour or model
            // may have; keep the snapshot the later stages read.
            if let Some(cache) = self.solid_view_cache.get_mut(&solid.id) {
                cache.solid = solid.clone();
                cache.sources = sources.clone();
            }
            // An attempt that is waiting on a restore has to be re-decided
            // every pass: the restore does not change the key - it is the same
            // content coming back - so nothing else would ever wake it.
            let cache = self.solid_view_cache.get(&solid.id).expect("checked above");
            if cache.envelope.is_current(key) && !cache.envelope.is_awaiting_inputs() {
                return;
            }
        } else {
            self.cancel_solid_jobs(solid.id, None);
            self.editor.solid_view_bands.remove(&solid.id);
            self.solid_view_cache.insert(
                solid.id,
                ViewSolid {
                    key,
                    runtime,
                    solid: solid.clone(),
                    sources: sources.clone(),
                    envelope: Stage::default(),
                    body: Stage::default(),
                    blasting: Stage::default(),
                    partition: Stage::default(),
                    bench_reserves: Stage::default(),
                    reserves: Stage::default(),
                },
            );
        }
        let [Some(design), Some(topo)] = sources else {
            // Unconfigured is a fault; still loading is not. Failing a run
            // because a surface has yet to come back would make a project with
            // deferred inputs impossible to run without visiting Setup first.
            let missing: Vec<_> = ids
                .into_iter()
                .flatten()
                .filter(|id| self.triangulations.iter().any(|item| item.id == *id && item.mesh.face_count() == 0))
                .map(crate::model::ItemRef::Triangulation)
                .collect();
            let unconfigured = ids.iter().any(Option::is_none);
            // A solid that names no surface at all is misconfigured, not
            // loading: there is nothing to wait for.
            let pending = !unconfigured && self.restore_solid_inputs(missing, attempted);
            let Some(cache) = self.solid_view_cache.get_mut(&solid.id) else { return };
            cache.envelope.begin(key);
            if pending {
                cache.envelope.await_inputs();
            } else {
                cache.envelope.fail(crate::i18n::tr!("planning-solid-needs-surfaces"));
            }
            return;
        };
        let id = solid.id;
        let token = self.solid_view_cache.get_mut(&id).expect("just inserted").envelope.begin(key);
        let snapshot = solid.clone();
        self.spawn_job_reporting_progress(
            crate::i18n::tr!("planning-building-view"),
            vec![artifact_job(id, SolidArtifact::Envelope, token)],
            move |cancel, progress| build_solid_envelope(&snapshot, &design, &topo, cancel, progress),
            move |app, result| {
                let Some(cache) = app.accepting_solid(runtime, id, SolidArtifact::Envelope, token) else {
                    return;
                };
                match result {
                    Ok(envelope) => cache.envelope.settle(Arc::new(envelope)),
                    Err(error) => cache.envelope.fail(format!("{error:#}")),
                }
                app.solid_view_body_key = None;
            },
        );
    }

    /// Ask for this request's deferred inputs, at most once.
    ///
    /// Re-deciding a waiting attempt every pass is what lets a restore that
    /// lands wake the work behind it - but it also means an unconditional
    /// request here would spawn a fresh residency job on every pass for ever
    /// once one failed. `attempted` says this request has already asked, so a
    /// pass that finds nothing in flight has its answer: the load is not
    /// coming, and the stage fails rather than waiting on it.
    fn restore_solid_inputs(&mut self, needed: Vec<crate::model::ItemRef>, attempted: bool) -> bool {
        let pending = needed.iter().any(|item| self.item_load_pending(*item));
        match restore_decision(attempted, pending) {
            RestoreDecision::Wait => true,
            RestoreDecision::Failed => false,
            RestoreDecision::Request => {
                let missing = needed.into_iter().filter(|item| !self.item_load_pending(*item)).collect();
                // The restore that lands has to wake the build waiting on it:
                // a run keeps the artifacts in sync each frame, but coming
                // back through here immediately is what turns a completed
                // load into started work rather than another frame of waiting.
                let started = self.restore_items_for(missing, |app| app.sync_solid_preview());
                pending || started
            }
        }
    }

    /// Benching: cut a committed envelope into its benches and flitches.
    fn sync_solid_body(&mut self, runtime: u32, solid: &Solid) {
        let Some(cache) = self.solid_view_cache.get(&solid.id) else { return };
        let Some(envelope) = cache.envelope.product().cloned() else { return };
        let key = body_key(cache.key, solid);
        if cache.body.is_current(key) {
            return;
        }
        let id = solid.id;
        self.cancel_solid_jobs(id, Some(SolidArtifact::Body));
        let cache = self.solid_view_cache.get_mut(&id).expect("checked above");
        let token = cache.body.begin(key);
        // Re-slabbing invalidates everything cut out of the old slabs.
        cache.blasting = Stage::default();
        cache.partition = Stage::default();
        cache.bench_reserves = Stage::default();
        cache.reserves = Stage::default();
        let snapshot = solid.clone();
        self.spawn_job_reporting_progress(
            crate::i18n::tr!("planning-building-slabs"),
            vec![artifact_job(id, SolidArtifact::Body, token)],
            move |cancel, progress| build_solid_body(&snapshot, &envelope, cancel, progress),
            move |app, result| {
                let Some(cache) = app.accepting_solid(runtime, id, SolidArtifact::Body, token) else {
                    return;
                };
                match result {
                    Ok(body) => cache.body.settle(Arc::new(body)),
                    Err(error) => cache.body.fail(format!("{error:#}")),
                }
                app.solid_view_body_key = None;
            },
        );
    }

    /// Blasting: divide each committed bench's ground by the cuts across it.
    fn sync_solid_blasting(&mut self, runtime: u32, solid: &Solid) {
        let Some(cache) = self.solid_view_cache.get(&solid.id) else { return };
        let Some(body) = cache.body.product().cloned() else { return };
        let Some(body_request) = cache.body.request else { return };
        let key = blasting_key(body_request.key, solid);
        if cache.blasting.is_current(key) {
            return;
        }
        let id = solid.id;
        self.cancel_solid_jobs(id, Some(SolidArtifact::Blasting));
        let cache = self.solid_view_cache.get_mut(&id).expect("checked above");
        let token = cache.blasting.begin(key);
        cache.partition = Stage::default();
        let snapshot = solid.clone();
        self.spawn_job_reporting_progress(
            crate::i18n::tr!("planning-building-view"),
            vec![artifact_job(id, SolidArtifact::Blasting, token)],
            move |cancel, _| build_solid_blasting(&snapshot, &body, cancel),
            move |app, result| {
                let Some(cache) = app.accepting_solid(runtime, id, SolidArtifact::Blasting, token) else {
                    return;
                };
                match result {
                    Ok(blasting) => cache.blasting.settle(Arc::new(blasting)),
                    Err(error) => cache.blasting.fail(format!("{error:#}")),
                }
                app.solid_view_body_key = None;
            },
        );
    }

    /// Dig Strips: cut each flitch into the blocks its strips and the blast
    /// boundaries above it divide it into.
    fn sync_solid_partition(&mut self, runtime: u32, solid: &Solid) {
        let Some(cache) = self.solid_view_cache.get(&solid.id) else { return };
        let Some(body) = cache.body.product().cloned() else { return };
        let Some(blasting) = cache.blasting.product().cloned() else { return };
        let Some(blasting_request) = cache.blasting.request else { return };
        let key = partition_key(blasting_request.key, solid);
        if cache.partition.is_current(key) {
            return;
        }
        let id = solid.id;
        self.cancel_solid_jobs(id, Some(SolidArtifact::Partition));
        let cache = self.solid_view_cache.get_mut(&id).expect("checked above");
        let token = cache.partition.begin(key);
        cache.reserves = Stage::default();
        let snapshot = solid.clone();
        self.spawn_job_reporting_progress(
            crate::i18n::tr!("planning-building-view"),
            vec![artifact_job(id, SolidArtifact::Partition, token)],
            move |cancel, progress| build_solid_partition(&snapshot, &body, &blasting, cancel, progress),
            move |app, result| {
                if app.accepting_solid(runtime, id, SolidArtifact::Partition, token).is_none() {
                    return;
                }
                match result {
                    Ok(mut partition) => {
                        adopt_dig_block_ids(&mut partition, app.dig_block_identities.get(&id).map_or(&[][..], Vec::as_slice));
                        app.solid_view_cache.get_mut(&id).expect("checked above").partition.settle(partition);
                        app.record_dig_block_identities(id);
                    }
                    Err(error) => app.solid_view_cache.get_mut(&id).expect("checked above").partition.fail(format!("{error:#}")),
                }
                app.solid_view_body_key = None;
            },
        );
    }

    /// The cache entry for a result that is still wanted, or `None` when the
    /// project moved on or the attempt was superseded.
    fn accepting_solid(&mut self, runtime: u32, solid: SolidId, kind: SolidArtifact, token: u64) -> Option<&mut ViewSolid> {
        if self.workspace.active_project().is_none_or(|project| project.runtime_id != runtime) {
            return None;
        }
        self.solid_view_cache.get_mut(&solid).filter(|cache| cache.accepts(kind, token))
    }

    /// Every job belonging to one solid, or to one artifact of it and the
    /// artifacts cut out of that one.
    fn cancel_solid_jobs(&mut self, solid: SolidId, from: Option<SolidArtifact>) {
        let cancelled = match from {
            None => vec![
                SolidArtifact::Envelope,
                SolidArtifact::Body,
                SolidArtifact::Blasting,
                SolidArtifact::Partition,
                SolidArtifact::BenchReserves,
                SolidArtifact::DigReserves,
            ],
            Some(SolidArtifact::Body) => vec![
                SolidArtifact::Body,
                SolidArtifact::Blasting,
                SolidArtifact::Partition,
                SolidArtifact::BenchReserves,
                SolidArtifact::DigReserves,
            ],
            Some(SolidArtifact::Blasting) => vec![SolidArtifact::Blasting, SolidArtifact::Partition, SolidArtifact::DigReserves],
            Some(SolidArtifact::Partition) => vec![SolidArtifact::Partition, SolidArtifact::DigReserves],
            Some(kind) => vec![kind],
        };
        self.cancel_jobs(|job| matches!(job, crate::app::jobs::JobKey::SolidArtifact { solid: job_solid, kind, .. } if *job_solid == solid && cancelled.contains(kind)));
    }

    /// Retire every attempt that produced nothing, so a retry asks again.
    ///
    /// A cancelled attempt leaves a request behind with no product; without
    /// this the next sync sees a request for the current inputs and declines
    /// to ask, which is a retry that waits for ever.
    pub(crate) fn discard_incomplete_solid_requests(&mut self) {
        self.solid_view_cache.retain(|_, cache| {
            cache.body.retire_if_unfinished();
            cache.blasting.retire_if_unfinished();
            cache.partition.retire_if_unfinished();
            cache.bench_reserves.retire_if_unfinished();
            cache.reserves.retire_if_unfinished();
            cache.envelope.retire_if_unfinished();
            // An envelope that never arrived cannot be retired in place - its
            // key is what the whole entry is identified by - so the entry
            // goes, and the next run asks for it from scratch.
            cache.envelope.product().is_some() || cache.envelope.error().is_some()
        });
    }

    /// Retire settled failures so an explicit rerun tries them once more.
    ///
    /// Kept apart from the refresh path on purpose: a failed stage must not
    /// re-fail on every frame, and only Run or a changed input retires it.
    pub(crate) fn retry_failed_solid_requests(&mut self) {
        for cache in self.solid_view_cache.values_mut() {
            cache.envelope.retire_failure();
            cache.body.retire_failure();
            cache.blasting.retire_failure();
            cache.partition.retire_failure();
            cache.bench_reserves.retire_failure();
            cache.reserves.retire_failure();
        }
        for model in &mut self.block_models {
            model.reserve_totals_error = None;
            model.reserve_totals_error_key = None;
        }
    }

    /// Remember the identities a completed partition established, so the next
    /// run can hand the same ground the same ids.
    fn record_dig_block_identities(&mut self, solid: SolidId) {
        let Some(partition) = self.solid_view_cache.get(&solid).and_then(|cache| cache.partition.product()) else {
            return;
        };
        let ledger: Vec<DigBlockIdentity> = partition
            .parts
            .iter()
            .filter_map(|part| {
                part.block.as_ref().map(|block| DigBlockIdentity {
                    id: block.id,
                    band: part.band.selection.base,
                    anchor: block.key.anchor(),
                    face: block.face.clone(),
                })
            })
            .collect();
        self.dig_block_identities.insert(solid, ledger);
    }

    /// Drop selection rows whose band no longer exists in the built geometry.
    fn prune_solids_view_selection(&mut self, solids: &[Solid]) {
        self.editor.solids_view_selection.retain(|row| {
            let Some(solid) = solids.iter().find(|solid| solid.id == row.solid) else {
                return false;
            };
            let Some(band) = row.band else {
                return true;
            };
            let Some(geometry) = self.solid_view_cache.get(&row.solid).and_then(ViewSolid::geometry) else {
                return true;
            };
            if band.is_flitch {
                geometry.occupied_bands().iter().any(|part| part.selection == band)
            } else {
                solid.benching.benches().iter().any(|bench| bench.base == band.base && bench.top() == band.top)
                    && geometry.bench_parts().iter().any(|part| part.band.selection == band)
            }
        });
    }

    /// Apply a completed pick from the preview renderer.
    ///
    /// A completed miss is as meaningful as a hit: it clears the selection,
    /// the way clicking empty space in the main viewport does. Nothing is
    /// rebuilt either way - the pick only changes what is highlighted.
    fn resolve_solid_preview_pick(&mut self) {
        let Some(picked) = self.editor.solid_preview_picked.take() else {
            return;
        };
        self.redraw_requested = true;
        self.solid_view_body_key = None;
        let hit = match picked {
            crate::ui::state::SolidPick::Miss => None,
            crate::ui::state::SolidPick::Hit(id) => self
                .solid_view_cache
                .values()
                .filter_map(|cache| {
                    let geometry = cache.geometry()?;
                    geometry
                        .parts()
                        .iter()
                        .chain(geometry.bench_parts())
                        .find(|part| part.mesh.id == id)
                        .and_then(|part| part.block.as_ref())
                        .map(|block| block.key)
                })
                .next(),
        };
        self.editor.selected_dig_block = hit;
    }

    /// Rebuild the display list from the artifacts. Pure presentation: which
    /// step is open, what is selected and how things are coloured all live
    /// here, and none of them reach the geometry.
    fn rebuild_solid_view_body(&mut self, solids: &[Solid]) {
        let selected_block = self.editor.selected_dig_block;
        let selected_blast = self.editor.selected_blast;
        let benches_only = self.editor.is_blasting_step();
        let blasting = self.editor.is_planning_cut_step();
        let runtime = self.workspace.active_project().map(|project| project.runtime_id);

        let mut hasher = DefaultHasher::new();
        runtime.hash(&mut hasher);
        selected_block.hash(&mut hasher);
        selected_blast.hash(&mut hasher);
        benches_only.hash(&mut hasher);
        blasting.hash(&mut hasher);
        serde_json::to_vec(&solids).unwrap_or_default().hash(&mut hasher);
        for row in &self.editor.solids_view_selection {
            row.solid.hash(&mut hasher);
            row.band.map(|band| (band.base.to_bits(), band.top.to_bits(), band.is_flitch)).hash(&mut hasher);
        }
        for solid in solids {
            if let Some(cache) = self.solid_view_cache.get(&solid.id) {
                cache.key.hash(&mut hasher);
                cache.is_built().hash(&mut hasher);
                cache.fingerprint().hash(&mut hasher);
            }
        }
        let key = hasher.finish();
        if self.solid_view_body_key == Some(key) {
            return;
        }
        self.solid_view_body_key = Some(key);
        self.solid_view_body.clear();
        self.editor.solid_view_reserves = None;
        self.editor.solid_view_reserve_status = None;
        self.editor.solid_view_reserve_issues.clear();
        self.editor.solid_view_coverage = None;
        self.editor.selected_dig_block_info = None;
        self.editor.solid_preview_sources.clear();

        // Ramp across what is actually on screen rather than the whole pit,
        // so one bench is not a single flat shade of its solid's colour.
        let depth_shade = blasting
            .then(|| {
                let mut range: Option<[f64; 2]> = None;
                for solid in solids {
                    let Some(geometry) = self
                        .solid_view_cache
                        .get(&solid.id)
                        .filter(|_| selected_solid(&self.editor.solids_view_selection, solid.id))
                        .and_then(ViewSolid::geometry)
                    else {
                        continue;
                    };
                    for part in displayed_parts(geometry, benches_only) {
                        if !selected(&self.editor.solids_view_selection, solid.id, Some(part.band.selection)) {
                            continue;
                        }
                        let bounds = part.mesh.mesh.bounds();
                        range = Some(range.map_or([bounds.min.z, bounds.max.z], |[low, high]| [low.min(bounds.min.z), high.max(bounds.max.z)]));
                    }
                }
                range
            })
            .flatten();

        let mut volume = Some(0.0);
        let mut covered = 0.0;
        let mut pending = false;
        let mut not_run = false;
        let mut reserve_complete = true;
        let mut errors = Vec::new();
        let has_fields = self.workspace.active_document().is_some_and(|doc| !doc.reserve_fields().is_empty());
        for solid in solids {
            let wanted = selected_solid(&self.editor.solids_view_selection, solid.id);
            let Some(cache) = self.solid_view_cache.get(&solid.id) else {
                // Nothing has been built for this solid. Opening a page is not
                // a calculation, so this says so rather than starting one.
                not_run |= wanted;
                continue;
            };
            let Some(geometry) = cache.geometry() else {
                if cache.error().is_some() {
                    self.editor.solid_view_bands.insert(solid.id, Vec::new());
                }
                if wanted {
                    match cache.error() {
                        Some(error) => errors.push(error.to_owned()),
                        None => pending = true,
                    }
                }
                continue;
            };
            self.editor
                .solid_view_bands
                .insert(solid.id, geometry.occupied_bands().iter().map(|band| band.selection).collect());
            if !wanted {
                continue;
            }
            if has_fields {
                let settled = cache.reserves.product().is_some();
                if !settled {
                    reserve_complete = false;
                    self.editor.solid_view_reserve_status = Some(cache.reserves.error().map(str::to_owned).unwrap_or_else(|| crate::i18n::tr!("planning-computing-reserves")));
                }
                if let Some(resolution) = cache.reserves.product().and_then(|product| product.resolution.as_ref()) {
                    for (field, issue) in &resolution.issues {
                        self.editor.solid_view_reserve_issues.entry(*field).or_insert_with(|| issue.describe());
                    }
                }
                if cache.reserves.product().is_some_and(|product| product.availability == ReserveAvailability::CapacityOnly) {
                    self.editor.solid_view_reserve_status = Some(crate::i18n::tr!("planning-reserve-capacity-only"));
                }
            }
            self.editor.solid_preview_sources.extend([solid.surface, solid.topography].into_iter().flatten());
            let reserves = cache.reserves.product().map(|product| &product.totals);
            for (index, part) in displayed_parts(geometry, benches_only).enumerate() {
                if !selected(&self.editor.solids_view_selection, solid.id, Some(part.band.selection)) {
                    continue;
                }
                // Selecting a blast narrows what is shown, not what is built.
                if !benches_only
                    && let Some(blast) = selected_blast.filter(|blast| blast.solid == solid.id)
                    && part.blast != Some(blast)
                {
                    continue;
                }
                let is_selected_block = part.block.as_ref().is_some_and(|piece| Some(piece.key) == selected_block);
                if is_selected_block && let Some(piece) = &part.block {
                    self.editor.selected_dig_block_info = Some(crate::ui::state::DigBlockInfo {
                        id: piece.id,
                        name: piece.name.clone(),
                        plan_area: piece.area,
                        bench: part.bench,
                        flitch: part.band.selection,
                        blast: part.blast,
                        volume: part.volume,
                        replaces: piece.replaces.clone(),
                    });
                }
                let counted = selected_block.is_none() || is_selected_block;
                let mut mesh = part.mesh.clone();
                if blasting {
                    // A plan view flattens everything a 3/4 view says with
                    // shading, so height carries the shape instead. Flitch
                    // hatching would only read as noise under the blast lines.
                    mesh.flitch_style = None;
                    mesh.color = solid.color;
                    mesh.depth_shade = depth_shade;
                } else {
                    mesh.flitch_style = solid
                        .benching
                        .intervals
                        .get(part.band.interval)
                        .map(|interval| interval.style(part.band.position, solid.color));
                    mesh.color = mesh.flitch_style.map_or(solid.color, |style| style.color);
                }
                if is_selected_block {
                    // Both blocks either side of a seam draw it, so an edge
                    // colour alone is a coin toss over which one is on top.
                    // Tinting the faces is what actually shows the pick.
                    mesh.line_color = crate::ui::SELECTION_COLOR_F32;
                    mesh.color = blended(mesh.color, crate::ui::SELECTION_COLOR_F32, 0.55);
                    mesh.flitch_style = None;
                }
                if counted {
                    // Reserves are measured over the flitch-level partition,
                    // so a bench figure is its own parts added back up rather
                    // than a second measurement of the same ground.
                    if !benches_only && let Some(values) = reserves.and_then(|values| values.get(index)) {
                        covered += values.all.covered_volume;
                        match self.editor.solid_view_reserves.as_mut() {
                            Some(total) => total.merge(values),
                            None => self.editor.solid_view_reserves = Some(values.clone()),
                        }
                    }
                    volume = volume.zip(part.volume).map(|(total, item)| total + item);
                }
                self.solid_view_body.push(mesh);
            }
        }
        if !reserve_complete || pending || !errors.is_empty() {
            self.editor.solid_view_reserves = None;
        }
        // Coverage says how much of the shown ground the block model actually
        // reaches, so a partly covered solid cannot read as fully measured.
        self.editor.solid_view_coverage = volume.filter(|volume| *volume > 0.0).map(|volume| covered / volume);
        self.editor.solid_preview_summary = if not_run && self.solid_view_body.is_empty() {
            SolidPreviewSummary::NotRun
        } else if pending {
            SolidPreviewSummary::Building {
                showing_previous: !self.solid_view_body.is_empty(),
            }
        } else if !errors.is_empty() {
            SolidPreviewSummary::Failed(errors.join("\n"))
        } else if self.solid_view_body.is_empty() {
            SolidPreviewSummary::Empty
        } else {
            SolidPreviewSummary::Ready {
                volume,
                faces: self.solid_view_body.iter().map(|mesh| mesh.mesh.face_count()).sum(),
                waiting_on_unloaded: false,
            }
        };
    }
}

/// The parts one step draws: whole benches for Blasting, dig blocks elsewhere.
fn displayed_parts<'artifact>(geometry: SolidGeometry<'artifact>, benches_only: bool) -> impl Iterator<Item = &'artifact SolidPart> {
    if benches_only {
        geometry.body.bench_parts.iter()
    } else {
        geometry.partition.parts.iter()
    }
}

/// Carry dig block identities across a rebuild, and record what changed.
///
/// A new block inherits the identity of any remembered block whose ground it
/// still is: either the old anchor falls inside the new face (the old block
/// was merged into this one) or the new anchor falls inside the old face (the
/// old block was split, and this is one of the pieces). The first unclaimed
/// candidate is inherited so one predecessor cannot name two successors; the
/// rest are recorded as replaced.
fn adopt_dig_block_ids(partition: &mut SolidPartition, previous: &[DigBlockIdentity]) {
    let mut taken: Vec<DigBlockId> = Vec::new();
    for part in &mut partition.parts {
        let base = part.band.selection.base;
        let Some(block) = part.block.as_mut() else { continue };
        let anchor = DVec2::from(block.key.anchor());
        let (inherited, replaces) = match_identity(base, &block.face, anchor, previous, &taken);
        if let Some(id) = inherited {
            taken.push(id);
            block.id = id;
        }
        block.replaces = replaces;
    }
}

/// Which remembered identity one new block should take, and which others its
/// ground came out of.
///
/// Split out from the walk above so the rule can be exercised on plain data:
/// the meshes around it are expensive to build and say nothing about it.
fn match_identity(band: f64, face: &Face, anchor: DVec2, previous: &[DigBlockIdentity], taken: &[DigBlockId]) -> (Option<DigBlockId>, Vec<DigBlockId>) {
    let mut candidates: Vec<DigBlockId> = previous
        .iter()
        .filter(|entry| (entry.band - band).abs() < 1e-6)
        .filter(|entry| arrangement::point_in_face(face, DVec2::from(entry.anchor)) || arrangement::point_in_face(&entry.face, anchor))
        .map(|entry| entry.id)
        .collect();
    candidates.sort_unstable();
    match candidates.iter().position(|id| !taken.contains(id)) {
        Some(position) => {
            let inherited = candidates.remove(position);
            (Some(inherited), candidates)
        }
        // Every candidate already named a successor, so this is new ground
        // that came out of them: keep the fresh id and record the lineage.
        None => (None, candidates),
    }
}

/// The Solids stage: the closed body between a solid's two surfaces.
fn build_solid_envelope(
    solid: &Solid,
    design: &Arc<Triangulation>,
    topo: &Arc<Triangulation>,
    cancel: &crate::app::jobs::CancelFlag,
    progress: &crate::model::progress::Progress,
) -> anyhow::Result<OpenTriangulation> {
    let (vertices, faces, _) =
        super::triangulation::solid_between::build_solid_between_surfaces(design, topo, crate::ui::state::SolidRegion::of_solid(solid.kind), &progress.phase(0.0, 0.9))?;
    anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
    let generated = super::triangulation::session::build_generated_triangulation(
        solid.name.clone(),
        vertices,
        faces,
        crate::ui::state::TriSurfaceType::SolidClosed,
        crate::model::triangulation::unique_edges,
    )?;
    Ok(preview_triangulation(
        solid.name.clone(),
        generated.mesh,
        generated.spatial,
        generated.edges,
        generated.surface_face_order,
        solid.color,
        [0.0, 0.0, 0.0, 1.0],
    ))
}

/// The Benching stage: cut a committed envelope into benches and flitches.
///
/// Reads the envelope, never the surfaces it came from, so editing a bench
/// height costs one re-slab rather than a whole rebuild.
fn build_solid_body(solid: &Solid, source: &OpenTriangulation, cancel: &crate::app::jobs::CancelFlag, progress: &crate::model::progress::Progress) -> anyhow::Result<SolidBody> {
    progress.set_fraction(0.0);
    let (bench_body, bench_bands_out) = build_bench_body(source, bench_bands(&solid.benching), true, cancel)?;
    progress.set_fraction(0.4);
    let (flitch_body, flitch_bands) = build_bench_body(source, plan_bands(&solid.benching), true, cancel)?;
    progress.set_fraction(0.8);
    let bench_footprints = plan_footprints(&bench_body, &bench_bands_out, cancel)?;
    let flitch_footprints = plan_footprints(&flitch_body, &flitch_bands, cancel)?;
    let occupied_bands = flitch_bands.clone();

    let benches = solid.benching.benches();
    let bench_of = |band: BenchSelection| -> BenchSelection {
        benches.iter().find(|bench| band.base >= bench.base - 1e-6 && band.top <= bench.top() + 1e-6).map_or(
            BenchSelection {
                base: band.base,
                top: band.top,
                is_flitch: false,
            },
            |bench| BenchSelection {
                base: bench.base,
                top: bench.top(),
                is_flitch: false,
            },
        )
    };

    let mut bench_parts = Vec::with_capacity(bench_body.len());
    for (mut mesh, band) in bench_body.into_iter().zip(bench_bands_out) {
        mesh.id = next_view_id();
        let volume = closed_volume(&mesh);
        bench_parts.push(SolidPart {
            bench: band.selection,
            band,
            blast: None,
            block: None,
            volume,
            mesh,
        });
    }
    let mut flitch_parts = Vec::with_capacity(flitch_body.len());
    for (mesh, band) in flitch_body.into_iter().zip(flitch_bands) {
        let volume = closed_volume(&mesh);
        flitch_parts.push(FlitchPart {
            bench: bench_of(band.selection),
            band,
            volume,
            mesh,
        });
    }
    progress.set_fraction(1.0);

    Ok(SolidBody {
        bench_parts,
        flitch_parts,
        bench_footprints,
        flitch_footprints,
        occupied_bands,
    })
}

/// The Blasting stage: each bench's ground cut by the lines drawn across it.
///
/// A bench with no cuts is one blast covering the whole of it, which is what
/// makes "no cuts drawn" a valid finished result rather than pending work.
fn build_solid_blasting(solid: &Solid, body: &SolidBody, cancel: &crate::app::jobs::CancelFlag) -> anyhow::Result<SolidBlasting> {
    let mut blast_faces = Vec::new();
    for part in &body.bench_parts {
        anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
        let Some(outline) = body.bench_footprints.get(&part.band.selection.base.to_bits()) else {
            continue;
        };
        let cuts = solid
            .blasting
            .bench(part.band.selection.base)
            .map(|bench| arrangement::cut_lines(&bench.cuts))
            .unwrap_or_default();
        for face in arrangement::subdivide(outline, &cuts) {
            let Some(anchor) = arrangement::representative_point(&face) else { continue };
            blast_faces.push(BlastFace {
                bench: part.band.selection,
                area: face_plan_area(&face),
                face,
                anchor: anchor.to_array(),
            });
        }
    }
    Ok(SolidBlasting { blast_faces })
}

/// The Dig Strips stage: each flitch cut by its own strips and by the blast
/// boundaries of the bench above it.
///
/// Reuses the committed flitch meshes and the committed blast faces; nothing
/// here rebuilds the envelope or re-derives a blast.
fn build_solid_partition(
    solid: &Solid,
    body: &SolidBody,
    blasting: &SolidBlasting,
    cancel: &crate::app::jobs::CancelFlag,
    progress: &crate::model::progress::Progress,
) -> anyhow::Result<SolidPartition> {
    let blast_for = |bench: BenchSelection, point: DVec2| -> Option<BlastShapeRef> {
        blasting
            .blast_faces
            .iter()
            .find(|blast| blast.bench == bench && arrangement::point_in_face(&blast.face, point))
            .map(|blast| blast.shape_ref(solid.id))
    };

    // With neither strips nor blast boundaries, the flitch is one dig block -
    // the plan's terminal output for an uncut blast.
    let mut parts = Vec::new();
    for (index, flitch) in body.flitch_parts.iter().enumerate() {
        anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
        progress.set_items(index as u64, body.flitch_parts.len() as u64);
        let band = flitch.band;
        let bench = flitch.bench;
        let strips = solid.blasting.drawing(band.selection.base, true).map_or(&[][..], |drawing| &drawing.cuts);
        let bench_cuts = solid.blasting.bench(bench.base).map_or(&[][..], |entry| &entry.cuts);
        let footprint = body.flitch_footprints.get(&band.selection.base.to_bits());
        let faces = match footprint {
            Some(footprint) if !strips.is_empty() || !bench_cuts.is_empty() => super::dig_strips::dig_block_faces(footprint, strips, bench_cuts),
            // Nothing cuts this flitch, so it is one block and needs no clip.
            _ => Vec::new(),
        };
        if faces.len() < 2 {
            let (anchor, plan) = match faces.into_iter().next() {
                Some((face, anchor)) => (anchor, face),
                None => {
                    let rings = footprint.cloned().unwrap_or_default();
                    let anchor = arrangement::representative_point(&rings).unwrap_or_default();
                    (anchor, rings)
                }
            };
            let mut mesh = flitch.mesh.clone();
            mesh.id = next_view_id();
            mesh.cull_back_faces = true;
            parts.push(SolidPart {
                bench,
                blast: blast_for(bench, anchor),
                block: Some(DigPiece {
                    id: DigBlockId(NEXT_DIG_BLOCK_ID.fetch_add(1, Ordering::Relaxed)),
                    key: BlastShapeRef::new(solid.id, band.selection.base, anchor.to_array()),
                    name: "1".to_owned(),
                    area: face_plan_area(&plan),
                    face: plan,
                    replaces: Vec::new(),
                }),
                band,
                volume: flitch.volume,
                mesh,
            });
            continue;
        }
        // A clipped body's own edge count is meaningless - see
        // `clip_solid_to_plan` - so a piece's volume is trusted on whether the
        // band it was cut from was closed, and measured inside the clip.
        let source_closed = flitch.volume.is_some();
        for (number, (face, anchor)) in faces.into_iter().enumerate() {
            anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
            let clipped = super::triangulation::solid_between::clip_solid_to_plan(&flitch.mesh.mesh, &face, cancel)?;
            if clipped.slab.1.is_empty() {
                continue;
            }
            // The clip knows which faces it cut along the block's boundary, so
            // the outline is their rim. Deriving it from the finished mesh
            // instead cannot tell that rim from the chords across a flat cap,
            // whose ends sit on the boundary too.
            let edges = super::triangulation::solid_between::boundary_wall_outline(&clipped.slab, &clipped.boundary_wall);
            let volume = clipped.volume;
            let (vertices, faces) = clipped.slab;
            let mesh = Arc::new(Triangulation::from_vertices_and_faces(vertices, faces)?);
            let spatial = Arc::new(crate::model::spatial::TriangleBvh::build(&mesh));
            let order = Arc::new(crate::model::triangulation::morton_surface_face_order(&mesh));
            let mut piece = preview_triangulation(flitch.mesh.name.clone(), mesh, spatial, edges, order, flitch.mesh.color, flitch.mesh.line_color);
            piece.cull_back_faces = true;
            piece.always_show_edges = true;
            piece.line_weight = Some(1.5);
            piece.id = next_view_id();
            parts.push(SolidPart {
                bench,
                blast: blast_for(bench, anchor),
                block: Some(DigPiece {
                    id: DigBlockId(NEXT_DIG_BLOCK_ID.fetch_add(1, Ordering::Relaxed)),
                    key: BlastShapeRef::new(solid.id, band.selection.base, anchor.to_array()),
                    name: (number + 1).to_string(),
                    area: face_plan_area(&face),
                    face,
                    replaces: Vec::new(),
                }),
                band,
                volume: source_closed.then_some(volume),
                mesh: piece,
            });
        }
    }
    progress.set_items(body.flitch_parts.len() as u64, body.flitch_parts.len() as u64);

    Ok(SolidPartition { parts })
}

/// A measured volume, or `None` when the piece did not come out closed.
fn closed_volume(mesh: &OpenTriangulation) -> Option<f64> {
    let faces: Vec<_> = mesh.mesh.face_vertex_indices_iter().map(|face| face.map(|i| i as u32)).collect();
    (super::triangulation::solid_between::open_edge_count(mesh.mesh.vertices(), &faces) == 0).then(|| mesh_volume(&mesh.mesh))
}

fn plan_footprints(body: &[OpenTriangulation], bands: &[CutBand], cancel: &crate::app::jobs::CancelFlag) -> anyhow::Result<HashMap<u64, Vec<Vec<DVec2>>>> {
    let mut footprints = HashMap::new();
    for (mesh, band) in body.iter().zip(bands) {
        anyhow::ensure!(!cancel.is_cancelled(), "Cancelled");
        let rings = super::triangulation::solid_between::plan_footprint_rings(&mesh.mesh, band.selection.top)
            .into_iter()
            .map(|ring| ring.into_iter().map(|point| DVec2::new(point.x, point.y)).collect())
            .collect();
        footprints.insert(band.selection.base.to_bits(), rings);
    }
    Ok(footprints)
}

impl crate::app::App<'_> {
    /// Measure reserves over one scope's partition of every solid.
    ///
    /// Run twice over a completed solid: once at Benching over whole benches,
    /// and once at Dig Strips over the blocks those benches are cut into. The
    /// bench figures are an *independent* measurement, not the blocks added
    /// up, which is the only thing a subdivision can honestly be reconciled
    /// against - summing the children and comparing them to themselves would
    /// prove nothing.
    fn sync_solid_reserves(&mut self, solids: &[Solid], scope: ReserveScope) {
        let fields = self.workspace.active_document().map(|doc| doc.reserve_fields().to_vec()).unwrap_or_default();
        for solid in solids {
            let Some(cache) = self.solid_view_cache.get(&solid.id) else { continue };
            let Some(parts) = scope.parts(cache) else { continue };
            let model = solid.block_model.and_then(|id| self.block_models.iter().find(|model| model.id == id));
            // The inclusion checkbox is enforced here, not merely displayed:
            // an excluded model contributes nothing to project reserves.
            let included = model.is_some_and(|model| model.included_in_reserves);
            let resident = model.is_some_and(|model| model.blocks.len() == model.model.metadata.n_blocks);
            let mut config = DefaultHasher::new();
            scope.hash_into(&mut config);
            scope.source_key(cache).hash(&mut config);
            solid.block_model.hash(&mut config);
            included.hash(&mut config);
            serde_json::to_vec(&fields).unwrap_or_default().hash(&mut config);
            if let Some(model) = model {
                serde_json::to_vec(&model.reserve_mapping).unwrap_or_default().hash(&mut config);
                self.model_content_version(model).hash(&mut config);
            }
            let key = config.finish();
            let slots = parts.len();
            // As for the envelope: an attempt waiting on a restore keeps the
            // same key when that restore succeeds - it is the same content
            // coming back - so it has to be re-decided rather than skipped.
            let stage = scope.slot_ref(cache);
            let attempted = stage.is_current(key) && stage.is_awaiting_inputs();
            if stage.is_current(key) && !stage.is_awaiting_inputs() {
                continue;
            }
            let id = solid.id;
            // An empty schema is a completed result of its own. Totals
            // measured against a schema that no longer exists must not stay
            // readable - the record API would export their field ids as
            // current.
            if fields.is_empty() {
                let stage = scope.slot(self.solid_view_cache.get_mut(&id).unwrap());
                stage.begin(key);
                stage.settle(ReserveProduct {
                    totals: vec![Default::default(); slots],
                    resolution: None,
                    availability: ReserveAvailability::NoSchema,
                });
                continue;
            }
            // A dump or stockpile with no model is a legitimate geometry-only
            // result: it has a capacity, and no measured content. Turning that
            // into a zero tonnage, or inferring tonnes from cubic metres,
            // would be an invented number.
            let capacity_only = solid.block_model.is_none() && !solid.kind.requires_block_model();
            let error = if capacity_only {
                None
            } else if model.is_none() {
                Some(crate::i18n::tr!("planning-reserve-needs-model"))
            } else if !included {
                Some(crate::model::ReserveFieldIssue::ModelExcluded.describe())
            } else if parts.iter().any(|part| part.volume.is_none()) {
                Some(crate::i18n::tr!("planning-reserve-open-solid"))
            } else {
                None
            };
            let inputs = model
                .filter(|_| resident && included && error.is_none())
                .map(|model| (model.model.clone(), model.blocks.clone(), model.reserve_mapping.clone(), reserve_partition(parts)));
            let runtime = cache.runtime;
            self.cancel_solid_jobs(id, Some(scope.artifact()));
            let token = scope.slot(self.solid_view_cache.get_mut(&id).unwrap()).begin(key);
            if capacity_only {
                scope.slot(self.solid_view_cache.get_mut(&id).unwrap()).settle(ReserveProduct {
                    totals: vec![Default::default(); slots],
                    resolution: None,
                    availability: ReserveAvailability::CapacityOnly,
                });
                continue;
            }
            if let Some(error) = error {
                scope.slot(self.solid_view_cache.get_mut(&id).unwrap()).fail(error);
                continue;
            }
            let Some((model, blocks, mapping, partition)) = inputs else {
                // Not resident: a restore is a wait, not a failure.
                let item = crate::model::ItemRef::BlockModel(solid.block_model.expect("a model was resolved"));
                let pending = self.restore_solid_inputs(vec![item], attempted);
                let stage = scope.slot(self.solid_view_cache.get_mut(&id).unwrap());
                if pending {
                    stage.await_inputs();
                } else {
                    stage.fail(crate::i18n::tr!("planning-reserve-unavailable"));
                }
                continue;
            };
            let fields = fields.clone();
            self.spawn_job_reporting_progress(
                crate::i18n::tr!("planning-computing-reserves"),
                vec![artifact_job(id, scope.artifact(), token)],
                move |cancel, progress| crate::model::solid_reserves::compute(&model, &blocks, &mapping, &fields, &partition, cancel, progress),
                move |app, result: anyhow::Result<ReserveOutcome>| {
                    if app.accepting_solid(runtime, id, scope.artifact(), token).is_none() {
                        return;
                    }
                    let stage = scope.slot(app.solid_view_cache.get_mut(&id).expect("checked above"));
                    match result {
                        Ok(outcome) => stage.settle(ReserveProduct {
                            totals: outcome.totals,
                            resolution: Some(outcome.resolution),
                            availability: ReserveAvailability::Measured,
                        }),
                        Err(error) => stage.fail(format!("{error:#}")),
                    }
                    app.solid_view_body_key = None;
                },
            );
        }
    }
}

/// Which partition of a solid a reserve measurement covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReserveScope {
    /// Whole benches, measured at the Benching stage.
    Bench,
    /// Dig blocks, measured at the Dig Strips stage.
    Dig,
}

impl ReserveScope {
    fn hash_into(self, hasher: &mut DefaultHasher) {
        (self as u8).hash(hasher);
    }

    fn artifact(self) -> SolidArtifact {
        match self {
            Self::Bench => SolidArtifact::BenchReserves,
            Self::Dig => SolidArtifact::DigReserves,
        }
    }

    fn parts(self, cache: &ViewSolid) -> Option<&[SolidPart]> {
        match self {
            Self::Bench => cache.body().map(|body| body.bench_parts.as_slice()),
            Self::Dig => cache.partition.product().map(|partition| partition.parts.as_slice()),
        }
    }

    /// The fingerprint of the geometry this scope measures, so a re-slab or a
    /// re-partition is an input change to the measurement over it.
    fn source_key(self, cache: &ViewSolid) -> Option<u64> {
        match self {
            Self::Bench => cache.body.request.map(|request| request.key),
            Self::Dig => cache.partition.request.map(|request| request.key),
        }
    }

    fn slot(self, cache: &mut ViewSolid) -> &mut Stage<ReserveProduct> {
        match self {
            Self::Bench => &mut cache.bench_reserves,
            Self::Dig => &mut cache.reserves,
        }
    }

    pub(crate) fn slot_ref(self, cache: &ViewSolid) -> &Stage<ReserveProduct> {
        match self {
            Self::Bench => &cache.bench_reserves,
            Self::Dig => &cache.reserves,
        }
    }
}

impl crate::app::App<'_> {
    /// One authoritative version of everything a reserve scan reads from a
    /// block model.
    ///
    /// Built on the item's own content epoch, which advances when the content
    /// is edited and is deliberately preserved across eviction and
    /// restoration (see `replace_residency`). Allocation identity is not a
    /// content version: restoring identical bytes into fresh `Arc`s changes
    /// every pointer without changing a single value, and a fingerprint built
    /// on pointers reads that as an edit - invalidating, and cancelling, the
    /// very run that asked for the restore.
    ///
    /// The transform and the mapping are folded in as well. Both survive
    /// eviction, and both change what a scan measures.
    pub(crate) fn model_content_version(&self, model: &crate::model::block_model::OpenBlockModel) -> u64 {
        let mut version = DefaultHasher::new();
        model.state.epoch().hash(&mut version);
        model.model.metadata.n_blocks.hash(&mut version);
        for value in model.model.origin().to_array().into_iter().chain(model.model.rotation().to_cols_array()) {
            value.to_bits().hash(&mut version);
        }
        serde_json::to_vec(&model.reserve_mapping).unwrap_or_default().hash(&mut version);
        version.finish()
    }

    /// The same, for one of a solid's source surfaces. `None` when the solid
    /// does not name one; `Some` whether or not it is currently resident.
    pub(crate) fn surface_content_version(&self, id: Option<crate::model::triangulation::TriangulationId>) -> Option<u64> {
        let id = id?;
        let epoch = self.triangulations.iter().find(|item| item.id == id).map(|item| item.state.epoch());
        let mut version = DefaultHasher::new();
        id.hash(&mut version);
        // A named surface that is not in the project at all is distinct from
        // one that is present but unloaded.
        epoch.hash(&mut version);
        Some(version.finish())
    }
}

impl ViewSolid {
    /// Where this solid's reserve measurement stands for one scope. Reading
    /// the committed artifacts, never starting work.
    pub(crate) fn reserve_state(&self, scope: ReserveScope) -> crate::app::planning_pipeline::ReserveState {
        use crate::app::planning_pipeline::ReserveState;
        let stage = scope.slot_ref(self);
        if let Some(error) = stage.error() {
            return ReserveState::Failed(error.to_owned());
        }
        match stage.product() {
            Some(product) => match product.availability {
                ReserveAvailability::Measured | ReserveAvailability::NoSchema => ReserveState::Complete,
                ReserveAvailability::CapacityOnly => ReserveState::CapacityOnly,
            },
            None => ReserveState::Waiting,
        }
    }
}

/// One bench's independently measured reserves, as the reconciliation reads
/// them. `None` where the bench was not measured at all.
pub(crate) struct BenchReserveReference {
    pub(crate) bench: BenchSelection,
    pub(crate) volume: Option<f64>,
    pub(crate) totals: Option<crate::model::solid_reserves::ReserveTotals>,
    /// Whether the Benching stage actually measured this solid. A capacity-only
    /// or schema-free solid owes no reference; a measured one does.
    pub(crate) measured: bool,
}

impl crate::app::App<'_> {
    /// The Benching stage's own measurement of each bench of one solid.
    pub(crate) fn bench_reserve_references(&self, solid: SolidId) -> Vec<BenchReserveReference> {
        let Some(cache) = self.solid_view_cache.get(&solid) else { return Vec::new() };
        let Some(body) = cache.body() else { return Vec::new() };
        let product = cache.bench_reserves.product();
        let measured = product.is_some_and(|product| product.availability == ReserveAvailability::Measured);
        body.bench_parts
            .iter()
            .enumerate()
            .map(|(slot, part)| BenchReserveReference {
                bench: part.band.selection,
                volume: part.volume,
                totals: product.and_then(|product| product.totals.get(slot)).cloned(),
                measured,
            })
            .collect()
    }
}

/// One finished dig block, as a scheduler consumes it.
///
/// The terminal output of a completed run: identity, parentage, geometry and
/// measured content in one record, readable without opening any page. Parents
/// are named directly rather than left to be re-inferred from anchor points.
pub(crate) struct DigBlockRecord {
    pub(crate) id: DigBlockId,
    pub(crate) solid: SolidId,
    pub(crate) solid_name: String,
    /// RL range of the bench this block belongs to.
    pub(crate) bench: BenchSelection,
    /// RL range of the flitch it was cut from.
    pub(crate) flitch: BenchSelection,
    pub(crate) blast: Option<BlastShapeRef>,
    /// The block's number within its flitch, as the panels show it.
    pub(crate) name: String,
    pub(crate) plan_area: f64,
    /// `None` when the block did not come out closed, so it has no volume
    /// rather than a volume of zero.
    pub(crate) volume: Option<f64>,
    /// Identities this block's ground was previously held under.
    pub(crate) replaces: Vec<DigBlockId>,
    /// What is known about the material in this block. Deliberately not an
    /// `Option`: "no block model by design" and "the reserve run has not
    /// finished" are different answers, and a scheduler must not read either
    /// as an absent tonnage.
    pub(crate) material: MaterialState,
}

/// What a completed run can say about one block's contents.
pub(crate) enum MaterialState {
    Measured(crate::model::solid_reserves::ReserveTotals),
    /// A dump or stockpile with no block model: geometry and capacity only.
    CapacityOnly,
    /// The project defines no reserve fields, so there is nothing to measure.
    NoSchema,
    /// A measurement was expected and is not there.
    Unavailable,
}

/// Why a completed scheduling snapshot could not be produced.
pub(crate) enum PlanningNotReady {
    NoProject,
    /// No run has produced this stage yet, or a run was cancelled.
    NotRun {
        stage: String,
    },
    /// A run produced it, and an edit since has retired it.
    Stale {
        stage: String,
    },
    Running {
        stage: String,
    },
    /// One solid's geometry has not been built for the current inputs.
    Incomplete {
        solid: String,
    },
    Failed {
        solid: String,
        message: String,
    },
}

impl PlanningNotReady {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::NoProject => crate::i18n::tr!("planning-snapshot-no-project"),
            Self::NotRun { stage } => crate::i18n::tr!("planning-snapshot-not-run", stage = stage.clone()),
            Self::Stale { stage } => crate::i18n::tr!("planning-snapshot-stale", stage = stage.clone()),
            Self::Running { stage } => crate::i18n::tr!("planning-snapshot-running", stage = stage.clone()),
            Self::Incomplete { solid } => crate::i18n::tr!("planning-snapshot-incomplete", solid = solid.clone()),
            Self::Failed { solid, message } => crate::i18n::tr!("planning-snapshot-failed", solid = solid.clone(), message = message.clone()),
        }
    }
}

impl crate::app::App<'_> {
    /// Every dig block of every solid whose geometry has been built.
    ///
    /// Reads committed artifacts only: it starts no work and depends on no
    /// page being open, which is what lets a scheduler consume a completed
    /// run through the model rather than through the UI.
    pub(crate) fn planning_dig_blocks(&self) -> Result<Vec<DigBlockRecord>, PlanningNotReady> {
        let solids = self.workspace.active_document().map(|document| document.solids().to_vec()).unwrap_or_default();
        let mut records = Vec::new();
        for solid in &solids {
            let Some(cache) = self.solid_view_cache.get(&solid.id) else {
                return Err(PlanningNotReady::Incomplete { solid: solid.name.clone() });
            };
            if let Some(error) = cache.error() {
                return Err(PlanningNotReady::Failed {
                    solid: solid.name.clone(),
                    message: error.to_owned(),
                });
            }
            let Some(geometry) = cache.geometry() else {
                return Err(PlanningNotReady::Incomplete { solid: solid.name.clone() });
            };
            let totals = cache.reserves.product().map(|product| &product.totals);
            for (slot, part) in geometry.partition.parts.iter().enumerate() {
                let Some(block) = &part.block else { continue };
                records.push(DigBlockRecord {
                    id: block.id,
                    solid: solid.id,
                    solid_name: solid.name.clone(),
                    bench: part.bench,
                    flitch: part.band.selection,
                    blast: part.blast,
                    name: block.name.clone(),
                    plan_area: block.area,
                    volume: part.volume,
                    replaces: block.replaces.clone(),
                    material: match cache.reserves.product().map(|product| product.availability) {
                        Some(ReserveAvailability::CapacityOnly) => MaterialState::CapacityOnly,
                        Some(ReserveAvailability::NoSchema) => MaterialState::NoSchema,
                        Some(ReserveAvailability::Measured) => match totals.and_then(|totals| totals.get(slot)) {
                            Some(totals) => MaterialState::Measured(totals.clone()),
                            None => MaterialState::Unavailable,
                        },
                        None => MaterialState::Unavailable,
                    },
                });
            }
        }
        Ok(records)
    }

    /// The completed, validated scheduling snapshot, or why there is not one.
    ///
    /// The public contract, and deliberately not the collector above. That one
    /// gathers whatever the artifacts hold, because Dig Strips reconciles
    /// against it *before* it can be Complete - gating it on completion would
    /// deadlock the stage that produces it. This one gates: a caller gets one
    /// coherent generation of a run that finished, or an explicit reason.
    pub(crate) fn planning_snapshot(&self) -> Result<PlanningSnapshot, PlanningNotReady> {
        use crate::app::planning_pipeline::StageNotReady;

        let Some(project) = self.workspace.active_project() else {
            return Err(PlanningNotReady::NoProject);
        };
        let Some(pipeline) = self.planning_pipeline.as_ref().filter(|pipeline| pipeline.runtime == project.runtime_id) else {
            return Err(PlanningNotReady::NoProject);
        };
        // Taken here, not read from the frame-synchronised cache. A caller can
        // edit configuration and ask before the next frame runs; answering
        // from last frame's fingerprints would hand out an old artifact
        // wearing current metadata.
        let fingerprints = self.planning_fingerprints();
        let generation = pipeline.readiness(&fingerprints).map_err(|reason| match reason {
            StageNotReady::NotRun(stage) => PlanningNotReady::NotRun { stage: stage.label() },
            StageNotReady::Stale(stage) => PlanningNotReady::Stale { stage: stage.label() },
            StageNotReady::Running(stage) => PlanningNotReady::Running { stage: stage.label() },
            StageNotReady::Failed { stage, message } => PlanningNotReady::Failed { solid: stage.label(), message },
        })?;
        let blocks = self.planning_dig_blocks()?;
        // Unavailable material is not a schedulable quantity. Capacity-only
        // and no-schema are deliberate answers and pass.
        if let Some(block) = blocks.iter().find(|block| matches!(block.material, MaterialState::Unavailable)) {
            return Err(PlanningNotReady::Failed {
                solid: block.solid_name.clone(),
                message: crate::i18n::tr!("planning-snapshot-unmeasured", block = block.name.clone()),
            });
        }
        Ok(PlanningSnapshot {
            runtime: project.runtime_id,
            generation,
            blocks,
        })
    }
}

/// One coherent generation of a completed run, as a scheduler consumes it.
pub(crate) struct PlanningSnapshot {
    /// The project run this belongs to, so a consumer can tell that a snapshot
    /// it is holding has outlived the project that produced it.
    #[allow(dead_code, reason = "part of the published contract; read by consumers, not by the app")]
    pub(crate) runtime: u32,
    /// The run that produced it, for provenance.
    pub(crate) generation: u64,
    pub(crate) blocks: Vec<DigBlockRecord>,
}

/// Group the flitch-level parts into the ordered bands the reserve scan needs.
///
/// This is the fix for the calculator's input contract: dig blocks in one
/// flitch share that flitch's elevations exactly, so they can never be
/// flattened into one ordered, non-overlapping list. They are pieces of one
/// band instead, and the band carries the ordering.
fn reserve_partition(parts: &[SolidPart]) -> ReservePartition {
    let mut bands: Vec<ReserveBand> = Vec::new();
    for (slot, part) in parts.iter().enumerate() {
        let piece = ReservePiece {
            mesh: part.mesh.mesh.clone(),
            spatial: part.mesh.spatial.clone(),
            slot,
        };
        let band = part.band.selection;
        match bands.iter_mut().find(|entry| entry.base == band.base && entry.top == band.top) {
            Some(entry) => entry.pieces.push(piece),
            None => bands.push(ReserveBand {
                base: band.base,
                top: band.top,
                pieces: vec![piece],
            }),
        }
    }
    bands.sort_by(|a, b| a.base.total_cmp(&b.base));
    ReservePartition { bands, slots: parts.len() }
}
