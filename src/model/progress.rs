//! Progress shared between a background worker and the status bar.
//!
//! A [`Progress`] is created on the UI thread, cloned into the worker, and
//! sampled once per frame by the status bar. Reports are plain atomic stores,
//! so a worker can call them inside a tight loop without coordinating with the
//! UI thread; the reader only ever needs the latest value, never the history.
//!
//! Work whose size is known ahead of time reports item counts
//! ([`Progress::set_items`]) and drives an exact percentage. Work that runs in
//! stages of differing cost splits the bar into [`Phase`]s, each reporting
//! exactly within its own slice - the percentage is then exact inside a stage
//! and approximate between stages, which is the best a staged pipeline can do.
//! Work that can't measure itself at all stays indeterminate and the status bar
//! shows a marquee.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// `fraction` value meaning "no percentage available".
const INDETERMINATE: u64 = u64::MAX;
/// Fixed-point scale for the stored fraction: 1.0 == `FRACTION_SCALE`.
const FRACTION_SCALE: f64 = 1_000_000.0;

/// Latest progress of one background operation.
#[derive(Clone)]
pub(crate) struct Progress(Arc<State>);

struct State {
    /// Completion in millionths, or [`INDETERMINATE`].
    fraction: AtomicU64,
    /// Item counts behind the fraction. `total == 0` means "not countable".
    done: AtomicU64,
    total: AtomicU64,
}

/// One sample of a [`Progress`], as shown in the status bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProgressSnapshot {
    pub(crate) fraction: f32,
    /// `(done, total)` when the work counts discrete items.
    pub(crate) units: Option<(u64, u64)>,
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

impl Progress {
    /// A new counter, indeterminate until the worker reports something.
    pub(crate) fn new() -> Self {
        Self(Arc::new(State {
            fraction: AtomicU64::new(INDETERMINATE),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }))
    }

    /// Report a bare percentage, with no item counts behind it.
    pub(crate) fn set_fraction(&self, fraction: f32) {
        self.0.total.store(0, Ordering::Relaxed);
        self.0.fraction.store(encode(fraction), Ordering::Relaxed);
    }

    /// Report `done` of `total` items; the status bar shows both the derived
    /// percentage and the counts.
    pub(crate) fn set_items(&self, done: u64, total: u64) {
        self.0.done.store(done, Ordering::Relaxed);
        self.0.total.store(total, Ordering::Relaxed);
        self.0.fraction.store(encode(done as f32 / total.max(1) as f32), Ordering::Relaxed);
    }

    /// A reporter for one stage of a staged operation, occupying
    /// `start..=end` of the overall bar. Item counts reported to a phase drive
    /// its slice of the percentage but are not shown as counts: they describe
    /// the stage, not the operation.
    pub(crate) fn phase(&self, start: f32, end: f32) -> Phase {
        Phase {
            progress: self.clone(),
            start,
            span: (end - start).max(0.0),
        }
    }

    /// Latest report, or `None` while the work is indeterminate. The three
    /// fields are stored separately, so a sample taken mid-report can pair a
    /// fraction with the previous counts - a one-frame cosmetic skew that no
    /// consumer of a progress bar can act on.
    pub(crate) fn snapshot(&self) -> Option<ProgressSnapshot> {
        let fraction = self.0.fraction.load(Ordering::Relaxed);
        if fraction == INDETERMINATE {
            return None;
        }
        let total = self.0.total.load(Ordering::Relaxed);
        Some(ProgressSnapshot {
            fraction: (fraction as f64 / FRACTION_SCALE) as f32,
            units: (total > 0).then(|| (self.0.done.load(Ordering::Relaxed), total)),
        })
    }
}

fn encode(fraction: f32) -> u64 {
    (fraction.clamp(0.0, 1.0) as f64 * FRACTION_SCALE) as u64
}

/// Reports into one slice of an operation's overall progress. Cloneable and
/// thread-safe so parallel stages can share one phase.
#[derive(Clone)]
pub(crate) struct Phase {
    progress: Progress,
    start: f32,
    span: f32,
}

impl Phase {
    /// A sub-stage occupying `start..=end` *of this stage*, so a stage built
    /// from smaller steps can subdivide itself without knowing where it sits
    /// in the overall bar.
    pub(crate) fn phase(&self, start: f32, end: f32) -> Self {
        Self {
            progress: self.progress.clone(),
            start: self.start + self.span * start,
            span: self.span * (end - start).max(0.0),
        }
    }

    /// Report this stage as `fraction` complete (0..=1 within the stage).
    pub(crate) fn set_fraction(&self, fraction: f32) {
        self.progress.set_fraction(self.start + self.span * fraction.clamp(0.0, 1.0));
    }

    /// Report `done` of `total` items within this stage.
    pub(crate) fn set_items(&self, done: u64, total: u64) {
        self.set_fraction(done as f32 / total.max(1) as f32);
    }

    /// Mark the stage complete: the bar jumps to the end of its slice.
    pub(crate) fn finish(&self) {
        self.set_fraction(1.0);
    }

    /// A counter for a stage whose items are finished by several threads at
    /// once. Sharing one counter reports what has actually been done, rather
    /// than how far any one worker has walked.
    pub(crate) fn counter(&self, total: usize) -> Counter {
        Counter {
            phase: self.clone(),
            done: AtomicU64::new(0),
            total: total as u64,
        }
    }
}

/// Shared item counter for a parallel stage. Report through
/// [`Counter::advance_by`]; the phase is updated every
/// [`REPORT_STRIDE`](Counter::REPORT_STRIDE) items so a tight loop isn't paced
/// by its own bookkeeping.
pub(crate) struct Counter {
    phase: Phase,
    done: AtomicU64,
    total: u64,
}

impl Counter {
    /// Items between reports.
    const REPORT_STRIDE: u64 = 4096;

    pub(crate) fn advance_by(&self, items: usize) {
        let before = self.done.fetch_add(items as u64, Ordering::Relaxed);
        let done = before + items as u64;
        if done / Self::REPORT_STRIDE != before / Self::REPORT_STRIDE || done >= self.total {
            self.phase.set_items(done, self.total);
        }
    }
}
