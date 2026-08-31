//! Structured program logging and the in-app activity console.

// The Windows-GNU `thread_local!` expansion triggers this lint inside the
// standard-library macro even though the initializer is already const.
#![cfg_attr(all(target_os = "windows", target_env = "gnu"), allow(clippy::missing_const_for_thread_local))]

#[cfg(not(target_arch = "wasm32"))]
use std::backtrace::Backtrace;
use std::{
    cell::Cell,
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, OnceLock},
};

use chrono::Local;
#[cfg(not(target_arch = "wasm32"))]
use tracing_subscriber::{
    filter::EnvFilter,
    layer::{Layer as _, SubscriberExt as _},
    util::SubscriberInitExt as _,
};

/// Length of the rolling log buffer.
const MAX_BUFFER_BYTES: usize = 2 * 1024 * 1024;
const MAX_SUMMARY_CHARS: usize = 180;

static RUNTIME_LOG: OnceLock<Mutex<RuntimeLog>> = OnceLock::new();

thread_local! {
    static ACTIVE_REPORT: Cell<Option<ConsoleEntryId>> = const { Cell::new(None) };
    static PANIC_HOOK_SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

/// Stable identity for one console activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConsoleEntryId(u64);

/// Severity shown by the console activity icons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ConsoleSeverity {
    Info,
    Success,
    Warn,
    Error,
}

impl ConsoleSeverity {
    const fn rank(self) -> u8 {
        match self {
            Self::Info => 0,
            Self::Success => 1,
            Self::Warn => 2,
            Self::Error => 3,
        }
    }

    fn escalate(self, other: Self) -> Self {
        if other.rank() > self.rank() { other } else { self }
    }
}

/// Whether an activity is still waiting for async children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsoleEntryState {
    Pending,
    Complete,
}

/// One structured row in the activity console.
#[derive(Clone, Debug)]
pub(crate) struct ConsoleEntry {
    pub(crate) id: ConsoleEntryId,
    pub(crate) timestamp: String,
    pub(crate) severity: ConsoleSeverity,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) details: Vec<String>,
    pub(crate) state: ConsoleEntryState,
    #[cfg(not(target_arch = "wasm32"))]
    started_at: web_time::Instant,
    dispatch_complete: bool,
    pending_children: usize,
    cancelled: bool,
}

impl ConsoleEntry {
    fn byte_len(&self) -> usize {
        self.timestamp.len() + self.title.len() + self.summary.len() + self.details.iter().map(|detail| detail.len()).sum::<usize>() + self.details.len() + 32
    }
}

pub(crate) type ConsoleSnapshot = (u64, Arc<[ConsoleEntry]>);

struct RuntimeLog {
    entries: VecDeque<ConsoleEntry>,
    bytes: usize,
    revision: u64,
    next_id: u64,
    snapshot: Option<(u64, Arc<[ConsoleEntry]>)>,
}

impl RuntimeLog {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            revision: 0,
            next_id: 1,
            snapshot: None,
        }
    }

    fn next_id(&mut self) -> ConsoleEntryId {
        let id = ConsoleEntryId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn push(&mut self, entry: ConsoleEntry) {
        self.bytes += entry.byte_len();
        self.entries.push_back(entry);
        self.trim();
        self.changed();
    }

    fn update(&mut self, id: ConsoleEntryId, update: impl FnOnce(&mut ConsoleEntry)) {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return;
        };
        let old_len = self.entries[index].byte_len();
        update(&mut self.entries[index]);
        let new_len = self.entries[index].byte_len();
        self.bytes = self.bytes.saturating_sub(old_len).saturating_add(new_len);
        self.trim();
        self.changed();
    }

    fn trim(&mut self) {
        while self.bytes > MAX_BUFFER_BYTES && self.entries.len() > 1 {
            if let Some(removed) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.byte_len());
            }
        }
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.snapshot = None;
    }

    fn snapshot(&mut self) -> Arc<[ConsoleEntry]> {
        if let Some((revision, snapshot)) = &self.snapshot
            && *revision == self.revision
        {
            return Arc::clone(snapshot);
        }
        let snapshot: Arc<[ConsoleEntry]> = self.entries.iter().cloned().collect();
        self.snapshot = Some((self.revision, Arc::clone(&snapshot)));
        snapshot
    }
}

fn runtime_log() -> &'static Mutex<RuntimeLog> {
    RUNTIME_LOG.get_or_init(|| Mutex::new(RuntimeLog::new()))
}

fn with_runtime_log<T>(f: impl FnOnce(&mut RuntimeLog) -> T) -> T {
    let mut log = runtime_log().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut log)
}

fn timestamp() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

fn concise(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_SUMMARY_CHARS {
        return one_line;
    }
    let mut shortened: String = one_line.chars().take(MAX_SUMMARY_CHARS.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

fn standalone_title(target: Option<&str>, severity: ConsoleSeverity) -> String {
    let target = target.unwrap_or_default();
    if target.contains("render") || target.contains("wgpu") {
        "Renderer".to_owned()
    } else if target.contains("job") || target.contains("load") {
        "Background".to_owned()
    } else if severity == ConsoleSeverity::Error {
        "System Error".to_owned()
    } else {
        "System".to_owned()
    }
}

fn adjacent_standalone_group_id(log: &RuntimeLog, title: &str) -> Option<ConsoleEntryId> {
    log.entries
        .back()
        .filter(|entry| entry.title == title && entry.state == ConsoleEntryState::Complete)
        .map(|entry| entry.id)
}

fn append_record(message: String, severity: ConsoleSeverity, target: Option<&str>) {
    let active = ACTIVE_REPORT.get();
    with_runtime_log(|log| {
        if let Some(id) = active {
            log.update(id, |entry| {
                entry.severity = entry.severity.escalate(severity);
                entry.summary = concise(&message);
                entry.details.push(message);
            });
        } else {
            let timestamp = timestamp();
            let title = standalone_title(target, severity);
            let grouped_id = adjacent_standalone_group_id(log, &title);
            if let Some(id) = grouped_id {
                log.update(id, |entry| {
                    entry.severity = entry.severity.escalate(severity);
                    if entry.details.is_empty() {
                        entry.details.push(entry.summary.clone());
                    }
                    entry.details.push(message);
                    entry.summary = format!("{} messages", entry.details.len());
                });
                return;
            }
            let id = log.next_id();
            log.push(ConsoleEntry {
                id,
                timestamp,
                severity,
                title,
                summary: concise(&message),
                details: Vec::new(),
                state: ConsoleEntryState::Complete,
                #[cfg(not(target_arch = "wasm32"))]
                started_at: web_time::Instant::now(),
                dispatch_complete: true,
                pending_children: 0,
                cancelled: false,
            });
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct LoggingGuard {
    _file_writer: Option<tracing_appender::non_blocking::WorkerGuard>,
}

#[cfg(not(target_arch = "wasm32"))]
fn diagnostic_filter() -> EnvFilter {
    let mut filter = EnvFilter::builder()
        .with_env_var("INCLINE_DESIGN_LOG")
        .with_default_directive(tracing::Level::INFO.into())
        .from_env_lossy();
    if std::env::var_os("PI_INCLUDE_DIAG").is_some() {
        filter = filter.add_directive("incline_design::include=debug".parse().expect("static diagnostic filter must be valid"));
    }
    filter
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn init() -> LoggingGuard {
    let _ = runtime_log();

    let logs_dir = crate::app::io::data_path("logs").ok();
    let (file_layer, file_writer_guard) = if let Some(logs_dir) = logs_dir {
        let appender = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("incline_design")
            .filename_suffix("jsonl")
            .max_log_files(10)
            .build(logs_dir);
        match appender {
            Ok(appender) => {
                let (writer, guard) = tracing_appender::non_blocking(appender);
                let layer = tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(true)
                    .with_writer(writer)
                    .with_filter(diagnostic_filter());
                (Some(layer), Some(guard))
            }
            Err(error) => {
                eprintln!("Failed to initialise diagnostic file logging: {error}");
                (None, None)
            }
        }
    } else {
        eprintln!("Failed to determine the diagnostic log directory");
        (None, None)
    };

    let terminal_layer = cfg!(debug_assertions).then(|| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_target(true)
            .with_writer(std::io::stderr)
            .with_filter(diagnostic_filter())
    });

    tracing_subscriber::registry().with(file_layer).with(terminal_layer).init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if PANIC_HOOK_SUPPRESSED.get() {
            return;
        }
        let backtrace = Backtrace::force_capture();
        tracing::error!(target: "incline_design::panic", panic = %panic_info, backtrace = %backtrace, "Incline Design panicked");
        default_hook(panic_info);
    }));

    LoggingGuard { _file_writer: file_writer_guard }
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn init() {
    let _ = runtime_log();
    let _ = console_log::init_with_level(log::Level::Info);
}

/// Run `operation`, converting a panic into `None` without invoking the panic hook.
pub(crate) fn catch_panic_quietly<T>(operation: impl FnOnce() -> T) -> Option<T> {
    PANIC_HOOK_SUPPRESSED.set(true);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
    PANIC_HOOK_SUPPRESSED.set(false);
    result.ok()
}

/// Friendly metadata supplied by a meaningful [`UiCommand`](crate::ui::state::UiCommand).
#[derive(Clone, Debug)]
pub(crate) struct CommandReportSpec {
    pub(crate) title: String,
    pub(crate) summary: String,
}

impl CommandReportSpec {
    pub(crate) fn new(title: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            summary: summary.into(),
        }
    }
}

/// Begin a command activity and make it visible immediately as pending.
pub(crate) fn begin_command_report(spec: CommandReportSpec) -> ConsoleEntryId {
    let title = spec.title.clone();
    let summary = spec.summary.clone();
    let id = with_runtime_log(|log| {
        let id = log.next_id();
        log.push(ConsoleEntry {
            id,
            timestamp: timestamp(),
            severity: ConsoleSeverity::Info,
            title: spec.title,
            summary: spec.summary,
            details: Vec::new(),
            state: ConsoleEntryState::Pending,
            #[cfg(not(target_arch = "wasm32"))]
            started_at: web_time::Instant::now(),
            dispatch_complete: false,
            pending_children: 0,
            cancelled: false,
        });
        id
    });
    trace_activity_started(id, &title, &summary);
    id
}

/// Run work with logging routed into the given command entry.
pub(crate) fn with_report_scope<T>(id: ConsoleEntryId, operation: impl FnOnce() -> T) -> T {
    ACTIVE_REPORT.with(|active| {
        let previous = active.replace(Some(id));
        let result = operation();
        active.set(previous);
        result
    })
}

fn complete_if_settled(entry: &mut ConsoleEntry) -> bool {
    if !entry.dispatch_complete || entry.pending_children != 0 || entry.state == ConsoleEntryState::Complete {
        return false;
    }
    entry.state = ConsoleEntryState::Complete;
    if !entry.cancelled && entry.severity == ConsoleSeverity::Info {
        entry.severity = ConsoleSeverity::Success;
        if entry.summary.is_empty() || entry.summary == "Working…" {
            entry.summary = "Completed".to_owned();
        }
    }
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn trace_activity_started(id: ConsoleEntryId, title: &str, summary: &str) {
    tracing::info!(
        target: "incline_design::activity",
        activity_id = id.0,
        phase = "started",
        user_visible = true,
        title,
        summary,
        "Activity started"
    );
}

#[cfg(target_arch = "wasm32")]
fn trace_activity_started(_id: ConsoleEntryId, _title: &str, _summary: &str) {}

/// Compact completion metadata; detailed messages are emitted individually,
/// so completing an activity never clones its potentially large detail buffer.
#[cfg(not(target_arch = "wasm32"))]
struct CompletedActivity {
    id: ConsoleEntryId,
    severity: ConsoleSeverity,
    title: String,
    summary: String,
    outcome: &'static str,
    elapsed_ms: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl CompletedActivity {
    fn from_entry(entry: &ConsoleEntry) -> Self {
        let outcome = if entry.cancelled {
            "cancelled"
        } else {
            match entry.severity {
                ConsoleSeverity::Error => "error",
                ConsoleSeverity::Warn => "warning",
                ConsoleSeverity::Info | ConsoleSeverity::Success => "success",
            }
        };
        Self {
            id: entry.id,
            severity: entry.severity,
            title: entry.title.clone(),
            summary: entry.summary.clone(),
            outcome,
            elapsed_ms: u64::try_from(entry.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn trace_activity_completed(activity: &CompletedActivity) {
    match activity.severity {
        ConsoleSeverity::Error => tracing::error!(
            target: "incline_design::activity",
            activity_id = activity.id.0,
            phase = "completed",
            outcome = activity.outcome,
            elapsed_ms = activity.elapsed_ms,
            user_visible = true,
            title = %activity.title,
            summary = %activity.summary,
            "Activity completed"
        ),
        ConsoleSeverity::Warn => tracing::warn!(
            target: "incline_design::activity",
            activity_id = activity.id.0,
            phase = "completed",
            outcome = activity.outcome,
            elapsed_ms = activity.elapsed_ms,
            user_visible = true,
            title = %activity.title,
            summary = %activity.summary,
            "Activity completed"
        ),
        ConsoleSeverity::Info | ConsoleSeverity::Success => tracing::info!(
            target: "incline_design::activity",
            activity_id = activity.id.0,
            phase = "completed",
            outcome = activity.outcome,
            elapsed_ms = activity.elapsed_ms,
            user_visible = true,
            title = %activity.title,
            summary = %activity.summary,
            "Activity completed"
        ),
    }
}

/// Finish synchronous dispatch. Async children keep the entry pending.
pub(crate) fn finish_command_report(id: ConsoleEntryId, error: Option<&anyhow::Error>) {
    let completed = with_runtime_log(|log| {
        let mut settled = false;
        log.update(id, |entry| {
            entry.dispatch_complete = true;
            if let Some(error) = error {
                let detail = format!("{error:#}");
                entry.severity = ConsoleSeverity::Error;
                entry.summary = concise(&detail);
                entry.details.push(detail);
            }
            settled = complete_if_settled(entry);
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            settled
                .then(|| log.entries.iter().find(|entry| entry.id == id).map(CompletedActivity::from_entry))
                .flatten()
        }
        #[cfg(target_arch = "wasm32")]
        {
            settled
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(activity) = completed {
        trace_activity_completed(&activity);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = completed;
}

/// Record a completed action that was triggered outside the [`UiCommand`]
/// dispatcher, such as a canvas-tool commit. If a command report is already
/// active, the result is attached to it instead of creating a duplicate row.
pub(crate) fn report_completed_action(spec: CommandReportSpec, detail: impl Into<String>) {
    let detail = detail.into();
    if ACTIVE_REPORT.get().is_some() {
        log_to_distributor(format_args!("{detail}"), ConsoleSeverity::Info, module_path!());
        return;
    }

    let id = begin_command_report(spec);
    with_report_scope(id, || {
        log_to_distributor(format_args!("{detail}"), ConsoleSeverity::Info, module_path!());
    });
    finish_command_report(id, None);
}

/// An async child tied to the command that was active when it was created.
///
/// Dropping the handle settles that child. Run completion/apply work through
/// [`scope`](Self::scope) so its messages stay attached to the same row.
#[derive(Debug)]
pub(crate) struct ConsoleReportHandle {
    id: ConsoleEntryId,
    cancelled: bool,
}

impl ConsoleReportHandle {
    pub(crate) fn scope<T>(&self, operation: impl FnOnce() -> T) -> T {
        with_report_scope(self.id, operation)
    }

    pub(crate) fn child(&self) -> Self {
        with_runtime_log(|log| {
            log.update(self.id, |entry| entry.pending_children = entry.pending_children.saturating_add(1));
        });
        Self { id: self.id, cancelled: false }
    }

    pub(crate) fn cancel(mut self) {
        self.cancelled = true;
    }
}

impl Drop for ConsoleReportHandle {
    fn drop(&mut self) {
        let completed = with_runtime_log(|log| {
            let mut settled = false;
            log.update(self.id, |entry| {
                entry.pending_children = entry.pending_children.saturating_sub(1);
                if self.cancelled {
                    entry.cancelled = true;
                    entry.summary = "Cancelled".to_owned();
                    entry.severity = ConsoleSeverity::Info;
                }
                settled = complete_if_settled(entry);
            });
            #[cfg(not(target_arch = "wasm32"))]
            {
                settled
                    .then(|| log.entries.iter().find(|entry| entry.id == self.id).map(CompletedActivity::from_entry))
                    .flatten()
            }
            #[cfg(target_arch = "wasm32")]
            {
                settled
            }
        });
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(activity) = completed {
            trace_activity_completed(&activity);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = completed;
    }
}

/// Retain the current command report for a file dialog or background task.
pub(crate) fn retain_current_report() -> Option<ConsoleReportHandle> {
    let id = ACTIVE_REPORT.get()?;
    with_runtime_log(|log| {
        log.update(id, |entry| entry.pending_children = entry.pending_children.saturating_add(1));
    });
    Some(ConsoleReportHandle { id, cancelled: false })
}

/// Immutable revision-cached data for the UI.
pub(crate) fn console_snapshot() -> ConsoleSnapshot {
    with_runtime_log(|log| (log.revision, log.snapshot()))
}

pub(crate) fn log_to_distributor(args: fmt::Arguments<'_>, severity: ConsoleSeverity, target: &'static str) {
    let message = format!("{args}");

    #[cfg(not(target_arch = "wasm32"))]
    match (severity, ACTIVE_REPORT.get().map(|id| id.0)) {
        (ConsoleSeverity::Error, activity_id) => tracing::error!(target: "incline_design::activity", source_target = target, activity_id, user_visible = true, message = %message),
        (ConsoleSeverity::Warn, activity_id) => tracing::warn!(target: "incline_design::activity", source_target = target, activity_id, user_visible = true, message = %message),
        (ConsoleSeverity::Info | ConsoleSeverity::Success, activity_id) => {
            tracing::info!(target: "incline_design::activity", source_target = target, activity_id, user_visible = true, message = %message)
        }
    }

    #[cfg(target_arch = "wasm32")]
    match severity {
        ConsoleSeverity::Error => log::error!("{message}"),
        ConsoleSeverity::Warn => log::warn!("{message}"),
        ConsoleSeverity::Info | ConsoleSeverity::Success => log::info!("{message}"),
    }

    append_record(message, severity, Some(target));
}

/// Log an informational message visible in the activity console.
#[macro_export]
macro_rules! userspace_log {
    ($($arg:tt)*) => {
        $crate::logging::log_to_distributor(format_args!($($arg)*), $crate::logging::ConsoleSeverity::Info, module_path!())
    };
}

/// Log a warning visible in the activity console.
#[macro_export]
macro_rules! userspace_warn {
    ($($arg:tt)*) => {
        $crate::logging::log_to_distributor(format_args!($($arg)*), $crate::logging::ConsoleSeverity::Warn, module_path!())
    };
}

/// Log an error visible in the activity console.
#[macro_export]
macro_rules! userspace_error {
    ($($arg:tt)*) => {
        $crate::logging::log_to_distributor(format_args!($($arg)*), $crate::logging::ConsoleSeverity::Error, module_path!())
    };
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn startup_log() {
    let id = begin_command_report(CommandReportSpec::new("Application Startup", "Initialising Incline Design"));
    with_report_scope(id, || {
        userspace_log!("APPLICATION NAME: {}", crate::APP_NAME);
        userspace_log!("Application ID: {}", crate::APP_ID);
        userspace_log!("Release Version: {}", crate::APP_RELEASE);
        userspace_log!("Build Target: {}-{}", std::env::consts::OS, std::env::consts::ARCH);
        userspace_log!("Pointer Width: {}-bit", usize::BITS);
        userspace_log!("Rustc Host: {}", std::env::var("HOST").unwrap_or_else(|_| "unknown".to_string()));
        userspace_log!("Process ID: {}", std::process::id());
        userspace_log!(
            "Locale Env: LANG={:?}, LC_ALL={:?}, TZ={:?}",
            std::env::var_os("LANG"),
            std::env::var_os("LC_ALL"),
            std::env::var_os("TZ")
        );

        #[cfg(target_os = "linux")]
        {
            userspace_log!("Operating System: GNU / Linux");
            userspace_log!(
                "Desktop Session: XDG_SESSION_TYPE={:?}, XDG_CURRENT_DESKTOP={:?}, WAYLAND_DISPLAY={:?}, DISPLAY={:?}",
                std::env::var_os("XDG_SESSION_TYPE"),
                std::env::var_os("XDG_CURRENT_DESKTOP"),
                std::env::var_os("WAYLAND_DISPLAY"),
                std::env::var_os("DISPLAY")
            );
        }

        #[cfg(target_os = "windows")]
        {
            userspace_log!("Operating System: Microsoft Windows");
            userspace_log!(
                "Windows Session: SESSIONNAME={:?}, USERNAME={:?}",
                std::env::var_os("SESSIONNAME"),
                std::env::var_os("USERNAME")
            );
        }

        #[cfg(target_os = "macos")]
        {
            userspace_log!("Operating System: MacOS");
            userspace_log!("macOS Session: USER={:?}, SHELL={:?}", std::env::var_os("USER"), std::env::var_os("SHELL"));
        }
    });
    finish_command_report(id, None);
}
