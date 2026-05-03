//! Shared continuous-indexing watcher control plane.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use symdex_core::RepoRoot;
use symdex_index::{
    ContinuousIndexEvent, ContinuousIndexOptions, QualityIndexSummary, WatchChangeSet,
    run_continuous_index_until,
};
use symdex_store::{
    RepositoryRecord, SqliteStore, StoreConfig, WatcherStatusRecord, current_timestamp,
    sqlite_parent,
};

const HEARTBEAT_STALE_SECONDS: u64 = 10;
const START_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherStatus {
    pub repository_id: String,
    pub root_path: String,
    pub mode: String,
    pub owner_kind: String,
    pub owner_pid: Option<i32>,
    pub socket_path: Option<String>,
    pub state: String,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub files_seen: usize,
    pub queued_events: usize,
    pub last_indexed_path: Option<String>,
    pub last_error: Option<String>,
    pub active_layer: Option<String>,
    pub quality_status: Option<String>,
    pub quality_pending_jobs: usize,
    pub quality_running_jobs: usize,
    pub quality_failed_jobs: usize,
    pub quality_stale_jobs: usize,
}

impl WatcherStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self.state.as_str(),
            "starting" | "running" | "pending" | "indexing"
        )
    }

    pub fn from_record(record: WatcherStatusRecord) -> Self {
        let mut status = Self {
            repository_id: record.repository_id,
            root_path: record.root_path,
            mode: record.mode,
            owner_kind: record.owner_kind,
            owner_pid: record.owner_pid,
            socket_path: record.socket_path,
            state: record.state,
            started_at: record.started_at,
            updated_at: record.updated_at,
            heartbeat_at: record.heartbeat_at,
            files_seen: record.files_seen,
            queued_events: record.queued_events,
            last_indexed_path: record.last_indexed_path,
            last_error: record.last_error,
            active_layer: record.active_layer,
            quality_status: record.quality_status,
            quality_pending_jobs: record.quality_pending_jobs,
            quality_running_jobs: record.quality_running_jobs,
            quality_failed_jobs: record.quality_failed_jobs,
            quality_stale_jobs: record.quality_stale_jobs,
        };
        if status.is_active() && heartbeat_is_stale(status.heartbeat_at.as_deref()) {
            status.state = "stale".to_owned();
        }
        status
    }

    fn inactive(root: &RepoRoot) -> Self {
        Self {
            repository_id: root.id().to_owned(),
            root_path: root.path().display().to_string(),
            mode: "semantic".to_owned(),
            owner_kind: "none".to_owned(),
            owner_pid: None,
            socket_path: None,
            state: "inactive".to_owned(),
            started_at: None,
            updated_at: None,
            heartbeat_at: None,
            files_seen: 0,
            queued_events: 0,
            last_indexed_path: None,
            last_error: None,
            active_layer: None,
            quality_status: None,
            quality_pending_jobs: 0,
            quality_running_jobs: 0,
            quality_failed_jobs: 0,
            quality_stale_jobs: 0,
        }
    }

    fn record(&self) -> WatcherStatusRecord {
        WatcherStatusRecord {
            repository_id: self.repository_id.clone(),
            root_path: self.root_path.clone(),
            mode: self.mode.clone(),
            owner_kind: self.owner_kind.clone(),
            owner_pid: self.owner_pid,
            socket_path: self.socket_path.clone(),
            state: self.state.clone(),
            started_at: self.started_at.clone(),
            updated_at: self.updated_at.clone(),
            heartbeat_at: self.heartbeat_at.clone(),
            files_seen: self.files_seen,
            queued_events: self.queued_events,
            last_indexed_path: self.last_indexed_path.clone(),
            last_error: self.last_error.clone(),
            active_layer: self.active_layer.clone(),
            quality_status: self.quality_status.clone(),
            quality_pending_jobs: self.quality_pending_jobs,
            quality_running_jobs: self.quality_running_jobs,
            quality_failed_jobs: self.quality_failed_jobs,
            quality_stale_jobs: self.quality_stale_jobs,
        }
    }
}

pub fn start_daemon(repo: &str) -> Result<WatcherStatus, String> {
    let root = open_repo_and_migrate(repo)?;
    let status = status_for_root(&root)?;
    if status.is_active() {
        return Ok(status);
    }
    if let Some(socket_path) = status.socket_path.as_deref() {
        let _ = fs::remove_file(socket_path);
    }
    let store = open_store()?;
    let starting = WatcherStatus {
        repository_id: root.id().to_owned(),
        root_path: root.path().display().to_string(),
        mode: "semantic".to_owned(),
        owner_kind: "launcher".to_owned(),
        owner_pid: Some(std::process::id() as i32),
        socket_path: None,
        state: "starting".to_owned(),
        started_at: Some(current_timestamp()),
        updated_at: None,
        heartbeat_at: None,
        files_seen: 0,
        queued_events: 0,
        last_indexed_path: None,
        last_error: None,
        active_layer: None,
        quality_status: None,
        quality_pending_jobs: 0,
        quality_running_jobs: 0,
        quality_failed_jobs: 0,
        quality_stale_jobs: 0,
    };
    store
        .upsert_watcher_status(&starting.record())
        .map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    if let Err(error) = Command::new(executable)
        .arg("watch-daemon")
        .arg(root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let _ = store.mark_watcher_failed(root.id(), &error.to_string());
        return Err(format!("start watcher daemon: {error}"));
    }

    let started = Instant::now();
    while started.elapsed() < START_WAIT {
        let current = status_for_root(&root)?;
        if current.is_active() {
            return Ok(current);
        }
        thread::sleep(Duration::from_millis(50));
    }
    status_for_root(&root)
}

pub fn stop_daemon(repo: &str) -> Result<WatcherStatus, String> {
    let root = open_repo_and_migrate(repo)?;
    let status = status_for_root(&root)?;
    if let Some(socket_path) = status.socket_path.as_deref()
        && UnixStream::connect(socket_path)
            .and_then(|mut stream| {
                stream.write_all(b"stop\n")?;
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let mut response = String::new();
                let _ = BufReader::new(stream).read_line(&mut response);
                Ok(())
            })
            .is_ok()
    {
        wait_for_stopped(&root)?;
        return status_for_root(&root);
    }
    let store = open_store()?;
    store
        .mark_watcher_stopped(root.id())
        .map_err(|error| error.to_string())?;
    status_for_root(&root)
}

pub fn status(repo: &str) -> Result<WatcherStatus, String> {
    let root = open_repo_and_migrate(repo)?;
    status_for_root(&root)
}

pub fn run_daemon(repo: &str) -> Result<(), String> {
    let root = open_repo_and_migrate(repo)?;
    let socket_path = socket_path_for_repo(root.id())?;
    let _ = fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .map_err(|error| format!("bind watcher socket {}: {error}", socket_path.display()))?;
    let should_continue = Arc::new(AtomicBool::new(true));
    start_control_listener(listener, Arc::clone(&should_continue));

    let store = open_store()?;
    let initial = WatcherStatus {
        repository_id: root.id().to_owned(),
        root_path: root.path().display().to_string(),
        mode: "semantic".to_owned(),
        owner_kind: "daemon".to_owned(),
        owner_pid: Some(std::process::id() as i32),
        socket_path: Some(socket_path.display().to_string()),
        state: "starting".to_owned(),
        started_at: Some(current_timestamp()),
        updated_at: None,
        heartbeat_at: None,
        files_seen: 0,
        queued_events: 0,
        last_indexed_path: None,
        last_error: None,
        active_layer: None,
        quality_status: None,
        quality_pending_jobs: 0,
        quality_running_jobs: 0,
        quality_failed_jobs: 0,
        quality_stale_jobs: 0,
    };
    store
        .upsert_watcher_status(&initial.record())
        .map_err(|error| error.to_string())?;

    let mut current = initial;
    let result = run_continuous_index_until(
        &ContinuousIndexOptions::new(root.path().display().to_string(), false),
        |event| {
            apply_event(&mut current, &event);
            let _ = store.upsert_watcher_status(&current.record());
        },
        || should_continue.load(Ordering::SeqCst),
    );
    let _ = fs::remove_file(&socket_path);
    match result {
        Ok(()) => store
            .mark_watcher_stopped(root.id())
            .map_err(|error| error.to_string()),
        Err(error) => {
            let _ = store.mark_watcher_failed(root.id(), &error);
            Err(error)
        }
    }
}

pub fn run_foreground(
    repo: &str,
    offline: bool,
    mut on_event: impl FnMut(ContinuousIndexEvent),
) -> Result<(), String> {
    let root = open_repo_and_migrate(repo)?;
    let status = status_for_root(&root)?;
    if status.is_active() {
        return Err(format!(
            "continuous watcher already active for `{}`; use `symdex watch status {}`",
            root.path().display(),
            root.path().display()
        ));
    }
    let mut current = WatcherStatus {
        repository_id: root.id().to_owned(),
        root_path: root.path().display().to_string(),
        mode: if offline { "offline" } else { "semantic" }.to_owned(),
        owner_kind: "foreground-cli".to_owned(),
        owner_pid: Some(std::process::id() as i32),
        socket_path: None,
        state: "starting".to_owned(),
        started_at: Some(current_timestamp()),
        updated_at: None,
        heartbeat_at: None,
        files_seen: 0,
        queued_events: 0,
        last_indexed_path: None,
        last_error: None,
        active_layer: None,
        quality_status: None,
        quality_pending_jobs: 0,
        quality_running_jobs: 0,
        quality_failed_jobs: 0,
        quality_stale_jobs: 0,
    };
    let store = open_store()?;
    store
        .upsert_watcher_status(&current.record())
        .map_err(|error| error.to_string())?;
    run_continuous_index_until(
        &ContinuousIndexOptions::new(root.path().display().to_string(), offline),
        |event| {
            apply_event(&mut current, &event);
            let _ = store.upsert_watcher_status(&current.record());
            on_event(event);
        },
        || true,
    )?;
    store
        .mark_watcher_stopped(root.id())
        .map_err(|error| error.to_string())
}

fn open_repo_and_migrate(repo: &str) -> Result<RepoRoot, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let store = open_store()?;
    store.migrate().map_err(|error| error.to_string())?;
    store
        .upsert_repository(&RepositoryRecord {
            id: root.id().to_owned(),
            root_path: root.path().display().to_string(),
        })
        .map_err(|error| error.to_string())?;
    Ok(root)
}

fn open_store() -> Result<SqliteStore, String> {
    SqliteStore::open(&StoreConfig::from_env()).map_err(|error| error.to_string())
}

fn status_for_root(root: &RepoRoot) -> Result<WatcherStatus, String> {
    let store = open_store()?;
    store
        .watcher_status(root.id())
        .map_err(|error| error.to_string())
        .map(|status| {
            status
                .map(WatcherStatus::from_record)
                .unwrap_or_else(|| WatcherStatus::inactive(root))
        })
}

fn socket_path_for_repo(repository_id: &str) -> Result<PathBuf, String> {
    let config = StoreConfig::from_env();
    let parent = sqlite_parent(&config).unwrap_or_else(|| PathBuf::from(".symdex"));
    fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
    Ok(parent.join(format!("watch-{repository_id}.sock")))
}

fn start_control_listener(listener: UnixListener, should_continue: Arc<AtomicBool>) {
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let mut command = String::new();
            let _ = BufReader::new(&stream).read_line(&mut command);
            if command.trim() == "stop" {
                should_continue.store(false, Ordering::SeqCst);
                let _ = stream.write_all(b"stopping\n");
                break;
            }
            let _ = stream.write_all(b"ok\n");
        }
    });
}

fn wait_for_stopped(root: &RepoRoot) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < START_WAIT {
        let status = status_for_root(root)?;
        if !status.is_active() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn heartbeat_is_stale(heartbeat_at: Option<&str>) -> bool {
    let Some(heartbeat_at) = heartbeat_at.and_then(|value| value.parse::<u64>().ok()) else {
        return false;
    };
    let now = current_timestamp().parse::<u64>().unwrap_or(0);
    now.saturating_sub(heartbeat_at) > HEARTBEAT_STALE_SECONDS
}

fn apply_event(status: &mut WatcherStatus, event: &ContinuousIndexEvent) {
    status.heartbeat_at = Some(current_timestamp());
    status.last_error = None;
    match event {
        ContinuousIndexEvent::Started { files_seen, .. } => {
            status.state = "running".to_owned();
            status.files_seen = *files_seen;
            status.queued_events = 0;
        }
        ContinuousIndexEvent::Idle { files_seen } => {
            status.state = "running".to_owned();
            status.files_seen = *files_seen;
            status.queued_events = 0;
        }
        ContinuousIndexEvent::ChangesPending { changes } => {
            status.state = "pending".to_owned();
            status.queued_events = changes.event_count();
        }
        ContinuousIndexEvent::ChangesDetected { changes } => {
            status.state = "indexing".to_owned();
            status.queued_events = changes.event_count();
        }
        ContinuousIndexEvent::BatchCompleted { changes, .. } => {
            status.state = "running".to_owned();
            status.queued_events = 0;
            status.last_indexed_path = latest_changed_path(changes);
        }
        ContinuousIndexEvent::BatchFailed { changes, error } => {
            status.state = "failed".to_owned();
            status.queued_events = changes.event_count();
            status.last_error = Some(error.clone());
        }
        ContinuousIndexEvent::QualityState { state }
        | ContinuousIndexEvent::QualityStarted { state } => {
            status.active_layer = Some(state.active_layer.clone());
            status.quality_status = Some(state.quality_status.clone());
            status.quality_pending_jobs = state.pending_jobs;
            status.quality_running_jobs = state.running_jobs;
            status.quality_failed_jobs = state.failed_jobs;
            status.quality_stale_jobs = state.skipped_stale_jobs;
        }
        ContinuousIndexEvent::QualityProgress { .. } => {}
        ContinuousIndexEvent::QualityCompleted { summary } => {
            apply_quality_summary(status, summary);
        }
        ContinuousIndexEvent::QualityFailed { state, error } => {
            if let Some(state) = state {
                status.active_layer = Some(state.active_layer.clone());
                status.quality_status = Some(state.quality_status.clone());
                status.quality_pending_jobs = state.pending_jobs;
                status.quality_running_jobs = state.running_jobs;
                status.quality_failed_jobs = state.failed_jobs;
                status.quality_stale_jobs = state.skipped_stale_jobs;
            }
            status.last_error = Some(error.clone());
        }
    }
}

fn apply_quality_summary(status: &mut WatcherStatus, summary: &QualityIndexSummary) {
    status.active_layer = Some(summary.active_layer.clone());
    status.quality_status = Some(summary.quality_status.clone());
    status.quality_pending_jobs = summary.progress.pending_jobs;
    status.quality_running_jobs = summary.progress.running_jobs;
    status.quality_failed_jobs = summary.progress.failed_jobs;
    status.quality_stale_jobs = summary.progress.skipped_stale_jobs;
}

fn latest_changed_path(changes: &WatchChangeSet) -> Option<String> {
    changes
        .modified
        .iter()
        .chain(changes.created.iter())
        .chain(changes.deleted.iter())
        .next()
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::{WatcherStatus, heartbeat_is_stale};

    #[test]
    fn stale_heartbeat_marks_active_status_stale() {
        let mut record = sample_status("running");
        record.heartbeat_at = Some("1".to_owned());

        let status = WatcherStatus::from_record(record);

        assert_eq!(status.state, "stale");
    }

    #[test]
    fn stopped_status_is_not_active() {
        let status = WatcherStatus::from_record(sample_status("stopped"));

        assert!(!status.is_active());
    }

    #[test]
    fn missing_heartbeat_is_not_stale() {
        assert!(!heartbeat_is_stale(None));
    }

    fn sample_status(state: &str) -> symdex_store::WatcherStatusRecord {
        symdex_store::WatcherStatusRecord {
            repository_id: "repo".to_owned(),
            root_path: ".".to_owned(),
            mode: "semantic".to_owned(),
            owner_kind: "daemon".to_owned(),
            owner_pid: Some(1),
            socket_path: Some("watch.sock".to_owned()),
            state: state.to_owned(),
            started_at: Some("1".to_owned()),
            updated_at: Some("1".to_owned()),
            heartbeat_at: Some(symdex_store::current_timestamp()),
            files_seen: 0,
            queued_events: 0,
            last_indexed_path: None,
            last_error: None,
            active_layer: None,
            quality_status: None,
            quality_pending_jobs: 0,
            quality_running_jobs: 0,
            quality_failed_jobs: 0,
            quality_stale_jobs: 0,
        }
    }
}
