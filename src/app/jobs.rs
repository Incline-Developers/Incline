//! Reusable background-compute job plumbing.
//!
//! Generalizes the off-thread load/save pattern into a single queue so heavy,
//! discrete operations (include, cut, create) can run off the UI thread
//! without hand-rolling a
//! `pending_*` vec + poll fn per operation.
//!
//! The compute closure and its owned inputs live on a worker thread; the
//! apply closure stays App-side and runs on the UI thread when the result
//! arrives. Type information is captured inside each job's `poll` closure, so
//! one queue can hold jobs with heterogeneous result types.
//!
//! Compute runs on a small fixed worker pool (rather than one thread per
//! job), so rapidly re-triggering a heavy operation queues work instead of
//! stacking OS threads. Blocking file writes run on a separate small I/O pool
//! so a slow disk cannot starve compute jobs (or vice versa). Each job
//! carries a [`CancelFlag`] and a set of [`JobKey`] dependencies;
//! `cancel_jobs` sets the flag so a cancelled compute is skipped if it hasn't
//! started and can bail out early if it checks the flag mid-computation.
//!
//! A panicking task is converted into an error result and the worker loop
//! survives: repeated panics can never silently shrink the pool, and the
//! failed job is reported like any other error. Note this only holds for
//! builds with `panic = "unwind"` (all native builds); the wasm target
//! forces `panic = "abort"` for the web target, where any panic ends
//! execution before `catch_unwind` can intervene.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Mutex, OnceLock};

use super::App;
use crate::{
    i18n::tr_format,
    model::{progress::Progress, triangulation::TriangulationId},
};

/// Identifies the sources a job derives from, so stale in-flight jobs can be
/// cancelled when their source changes or is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKey {
    HistoryResidency {
        runtime_id: u32,
        revision: u64,
        restoring: bool,
    },
    LayerResidency {
        layer: crate::model::LayerId,
        runtime_id: u32,
        content_hash: u64,
        restoring: bool,
    },
    Residency {
        item: crate::model::ItemRef,
        runtime_id: u32,
        revision: u64,
        restoring: bool,
    },
    Triangulation(TriangulationId),
    PointCloud(crate::model::point_cloud::PointCloudId),
    BlockModel(crate::model::block_model::BlockModelId),
    DrillHole(crate::model::drill_hole::DrillHoleId),
    Project {
        runtime_id: u32,
        document_revision: u64,
    },
    /// A job not tied to any tracked source (always applied).
    #[allow(dead_code)]
    Anonymous,
}

/// Shared cancellation signal for one background job. Compute closures should
/// poll [`CancelFlag::is_cancelled`] at convenient points in long loops and
/// return early (any error is fine - the result is discarded anyway).
#[derive(Clone, Default)]
pub(crate) struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A heavy operation running on a background thread.
pub(crate) struct BackgroundJob<'a> {
    pub(crate) ticket: crate::app::BackgroundTaskTicket,
    /// Every source this job depends on; cancelling any of them cancels the job.
    pub(crate) keys: Vec<JobKey>,
    cancel: CancelFlag,
    /// Polls the worker channel. Returns `true` once the job has settled
    /// (result applied, or the worker vanished) and should be dropped.
    poll: Box<dyn FnMut(&mut App<'a>) -> bool + 'a>,
}

type PoolTask = Box<dyn FnOnce() + Send>;

#[cfg(not(target_arch = "wasm32"))]
fn build_pool(name: &'static str, workers: usize) -> mpsc::Sender<PoolTask> {
    let (tx, rx) = mpsc::channel::<PoolTask>();
    let rx = Arc::new(Mutex::new(rx));
    for index in 0..workers {
        let rx = Arc::clone(&rx);
        std::thread::Builder::new()
            .name(format!("{name}-{index}"))
            .spawn(move || {
                loop {
                    let task = match rx.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => break,
                    };
                    match task {
                        // The worker loop must survive task panics: the task
                        // wrapper reports the failure through its own result
                        // channel, and anything escaping it is contained here
                        // so the pool never silently loses capacity.
                        Ok(task) => {
                            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
                        }
                        Err(_) => break,
                    }
                }
            })
            .expect("failed to spawn worker thread");
    }
    tx
}

/// Fixed-size pool shared by all background compute jobs; bounds how many
/// heavy computes run at once no matter how fast jobs are spawned.
#[cfg(not(target_arch = "wasm32"))]
fn job_pool() -> &'static mpsc::Sender<PoolTask> {
    static POOL: OnceLock<mpsc::Sender<PoolTask>> = OnceLock::new();
    POOL.get_or_init(|| {
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
        build_pool("job-worker", workers)
    })
}

/// Small pool for blocking file I/O (mesh exports and other long writes),
/// kept separate so disk-bound work and CPU-bound work cannot starve each
/// other.
#[cfg(not(target_arch = "wasm32"))]
fn io_pool() -> &'static mpsc::Sender<PoolTask> {
    static POOL: OnceLock<mpsc::Sender<PoolTask>> = OnceLock::new();
    POOL.get_or_init(|| build_pool("io-worker", 2))
}

/// Queue a CPU-bound task on the bounded compute pool. Prefer this over
/// `std::thread::spawn` for loads/parses: a folder of files queues instead of
/// creating one OS thread (and one peak allocation) per file.
pub(crate) fn spawn_pool_task(task: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = job_pool().send(Box::new(task));
    #[cfg(target_arch = "wasm32")]
    rayon::spawn(task);
}

/// Queue a blocking-I/O task (file writes/exports) on the bounded I/O pool.
/// The browser has no filesystem to write to, so every caller is desktop-only.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn spawn_io_task(task: impl FnOnce() + Send + 'static) {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = io_pool().send(Box::new(task));
    #[cfg(target_arch = "wasm32")]
    rayon::spawn(task);
}

/// Run `compute`, converting a panic into an error result so the caller's
/// channel always observes an outcome. Only effective under `panic =
/// "unwind"` (all native builds); on wasm, which forces `panic = "abort"`,
/// a panic ends execution instead.
pub(crate) fn run_compute_catching_panic<T>(compute: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(compute)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        Err(anyhow::anyhow!("background task panicked: {message}"))
    })
}

impl<'a> App<'a> {
    fn job_dependencies_are_current(&self, keys: &[JobKey]) -> bool {
        keys.iter().all(|key| match *key {
            JobKey::Triangulation(id) => self.triangulations.iter().any(|item| item.id == id),
            JobKey::PointCloud(id) => self.point_clouds.iter().any(|item| item.id == id),
            JobKey::BlockModel(id) => self.block_models.iter().any(|item| item.id == id),
            JobKey::DrillHole(id) => self.drill_holes.iter().any(|item| item.id == id),
            JobKey::Project { runtime_id, document_revision } => self
                .workspace
                .projects
                .iter()
                .find(|project| project.runtime_id == runtime_id)
                .is_some_and(|project| project.project.document.revision() == document_revision),
            JobKey::HistoryResidency { runtime_id, .. } => self.workspace.active_project().is_some_and(|project| project.runtime_id == runtime_id),
            JobKey::LayerResidency {
                layer, runtime_id, content_hash, ..
            } => self
                .workspace
                .active_project()
                .is_some_and(|project| project.runtime_id == runtime_id && project.current_layer_hashes().get(&(layer.0 & u64::from(u32::MAX))) == Some(&content_hash)),
            JobKey::Residency { item, runtime_id, revision, .. } => {
                self.workspace.active_project().is_some_and(|project| project.runtime_id == runtime_id)
                    && self.project_item_state(item).is_some_and(|state| state.revision() == revision)
            }
            JobKey::Anonymous => true,
        })
    }

    /// Run `compute` on the background worker pool; when it finishes, `apply`
    /// runs on the UI thread with the result. Increments the shared progress
    /// counter (progress cursor + status bar) for the job's lifetime.
    ///
    /// `compute` must capture only owned data / `Arc`s - never `&self`, and
    /// should poll the passed [`CancelFlag`] in long loops. `apply` runs later
    /// with `&mut App`, so it must re-resolve anything by stable id: sources
    /// may have been edited or removed while the job ran.
    pub(crate) fn spawn_job<T, C, A>(&mut self, label: impl Into<String>, keys: Vec<JobKey>, compute: C, apply: A)
    where
        T: Send + 'static,
        C: FnOnce(&CancelFlag) -> anyhow::Result<T> + Send + 'static,
        A: FnOnce(&mut App<'a>, anyhow::Result<T>) + 'a,
    {
        self.spawn_job_reporting_progress(label, keys, move |cancel, _progress| compute(cancel), apply);
    }

    /// As [`App::spawn_job`], but the compute closure also gets a
    /// [`Progress`] to report through, so the status bar shows a real
    /// percentage instead of a marquee. Use it whenever the work knows how
    /// much it has to do.
    pub(crate) fn spawn_job_reporting_progress<T, C, A>(&mut self, label: impl Into<String>, keys: Vec<JobKey>, compute: C, apply: A)
    where
        T: Send + 'static,
        C: FnOnce(&CancelFlag, &Progress) -> anyhow::Result<T> + Send + 'static,
        A: FnOnce(&mut App<'a>, anyhow::Result<T>) + 'a,
    {
        let label = label.into();
        let (ticket, progress) = self.begin_reported_task(label.clone());
        let mut console_report = crate::logging::retain_current_report();

        let (tx, rx) = mpsc::channel();
        let window = self.window.clone();
        let cancel = CancelFlag::default();
        let worker_cancel = cancel.clone();
        let worker_console_report = console_report.as_ref().map(crate::logging::ConsoleReportHandle::child);
        let task: PoolTask = Box::new(move || {
            // A job cancelled while still queued never starts computing;
            // dropping `tx` settles its poll via Disconnected.
            if !worker_cancel.is_cancelled() {
                let run = || run_compute_catching_panic(|| compute(&worker_cancel, &progress));
                let result = if let Some(report) = worker_console_report.as_ref() { report.scope(run) } else { run() };
                let _ = tx.send(result);
            }
            if let Some(window) = window {
                window.request_redraw();
            }
        });
        spawn_pool_task(task);

        let mut apply = Some(apply);
        let poll_label = label.clone();
        let poll_cancel = cancel.clone();
        let poll_keys = keys.clone();
        let poll = Box::new(move |app: &mut App<'a>| -> bool {
            let mut poll_once = || match rx.try_recv() {
                Ok(result) => {
                    if !app.job_dependencies_are_current(&poll_keys) {
                        crate::userspace_warn!(
                            "{}",
                            tr_format!(
                                literal = "Discarded stale background result for '%poll_label%' because a source changed or closed",
                                poll_label = poll_label.clone()
                            )
                        );
                    } else if let Some(apply) = apply.take() {
                        apply(app, result);
                    }
                    true
                }
                Err(mpsc::TryRecvError::Empty) => false,
                // Worker dropped the sender without a value. A cancelled job
                // settles silently; anything else is an unexpected loss and
                // must be visible rather than quietly treated as done.
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !poll_cancel.is_cancelled() {
                        crate::userspace_error!(
                            "{}",
                            tr_format!(literal = "Background task '%poll_label%' ended without a result", poll_label = poll_label.clone())
                        );
                    }
                    true
                }
            };
            let settled = if let Some(report) = console_report.as_ref() {
                report.scope(&mut poll_once)
            } else {
                poll_once()
            };
            if settled {
                if poll_cancel.is_cancelled() {
                    if let Some(report) = console_report.take() {
                        report.cancel();
                    }
                } else {
                    drop(console_report.take());
                }
            }
            settled
        });

        self.pending_jobs.push(BackgroundJob { ticket, keys, cancel, poll });
    }

    /// Drain finished background jobs, running their apply closures on the UI
    /// thread. Call once per frame alongside the other polls.
    pub(crate) fn poll_jobs(&mut self) {
        #[cfg(target_arch = "wasm32")]
        crate::model::asset_storage::poll_browser();
        if self.pending_jobs.is_empty() {
            return;
        }

        let mut residency_settled = false;
        let mut still_pending = Vec::with_capacity(self.pending_jobs.len());
        for mut job in std::mem::take(&mut self.pending_jobs) {
            if (job.poll)(self) {
                residency_settled |= job
                    .keys
                    .iter()
                    .any(|key| matches!(key, JobKey::Residency { .. } | JobKey::LayerResidency { .. } | JobKey::HistoryResidency { .. }));
                // Settled: balance the begin_topology_load() from spawn_job.
                self.finish_background_task(job.ticket, false);
                self.redraw_requested = true;
            } else {
                still_pending.push(job);
            }
        }
        // An apply closure may itself spawn a follow-up job; keep those
        // (currently in self.pending_jobs) after the ones still running.
        still_pending.append(&mut self.pending_jobs);
        self.pending_jobs = still_pending;
        // A rename/edit can invalidate a pending eviction. Reconcile the
        // remaining payloads after stale workers settle as well as on edits.
        if residency_settled {
            self.evict_unloaded_items();
            self.evict_unloaded_layers();
        }
        // The status bar is driven from the ticket registry (see
        // `App::refresh_status_message`), which each job joined when it began.
    }

    /// Cancel in-flight jobs where any dependency key matches `pred`: their
    /// cancel flag is set (so queued computes never start and running ones
    /// can bail) and their results are discarded. Settles the progress
    /// counter for each so the progress cursor doesn't stick.
    pub(crate) fn cancel_jobs(&mut self, pred: impl Fn(&JobKey) -> bool) {
        let mut cancelled_tickets = Vec::new();
        self.pending_jobs.retain(|job| {
            if job.keys.iter().any(&pred) {
                job.cancel.cancel();
                cancelled_tickets.push(job.ticket);
                false
            } else {
                true
            }
        });
        for ticket in cancelled_tickets {
            self.cancel_background_task(ticket);
        }
    }
}
