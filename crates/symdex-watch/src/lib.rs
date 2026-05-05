//! Shared continuous-indexing watcher control plane.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;
use symdex_core::RepoRoot;
use symdex_index::{
    ContinuousIndexEvent, ContinuousIndexOptions, QualityIndexSummary, WatchChangeSet,
    run_continuous_index_until_with_write_gate,
};
use symdex_store::{
    RepositoryRecord, SqliteStore, StoreConfig, WatcherClientRecord, WatcherStatusRecord,
    current_timestamp, debug_db_lock_log,
};
use symdex_writer::{WriterClient, WriterJob};

const HEARTBEAT_STALE_SECONDS: u64 = 10;
const CLIENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const NO_CLIENT_GRACE: Duration = Duration::from_secs(10);
static CLIENT_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatcherClientKind {
    Tui,
    Mcp,
    Cli,
}

impl WatcherClientKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Mcp => "mcp",
            Self::Cli => "cli",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherClientStatus {
    pub client_id: String,
    pub client_kind: String,
    pub pid: Option<i32>,
    pub started_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub last_seen_at: Option<String>,
}

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
    pub attached_clients: usize,
    pub client_kinds: Vec<String>,
    pub clients: Vec<WatcherClientStatus>,
    pub shutdown_after_seconds: Option<u64>,
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
            attached_clients: 0,
            client_kinds: Vec::new(),
            clients: Vec::new(),
            shutdown_after_seconds: None,
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
            attached_clients: 0,
            client_kinds: Vec::new(),
            clients: Vec::new(),
            shutdown_after_seconds: None,
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

pub struct WatcherAttachment {
    repository_id: String,
    client_id: String,
    root_path: String,
    endpoint: Option<String>,
    stop_heartbeat: Option<Sender<()>>,
    heartbeat_thread: Option<JoinHandle<()>>,
}

impl WatcherAttachment {
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub fn status(&self) -> Result<WatcherStatus, String> {
        let store = open_store_read_only()?;
        let root_path = store
            .watcher_status(&self.repository_id)
            .map_err(|error| error.to_string())?
            .map(|status| status.root_path)
            .ok_or_else(|| "watcher status missing for attachment".to_owned())?;
        status(&root_path)
    }
}

impl Drop for WatcherAttachment {
    fn drop(&mut self) {
        if let Some(stop_heartbeat) = self.stop_heartbeat.take() {
            let _ = stop_heartbeat.send(());
        }
        if let Some(handle) = self.heartbeat_thread.take() {
            let _ = handle.join();
        }
        if let Some(endpoint) = &self.endpoint {
            let _ =
                control_ipc::send_control_command(endpoint, &format!("detach {}", self.client_id));
        } else {
            let _ = WriterClient::from_env().submit_and_wait(&WriterJob::DetachWatcherClient {
                repo: self.root_path.clone(),
                client_id: self.client_id.clone(),
            });
        }
    }
}

pub fn start_or_attach(
    repo: &str,
    client_kind: WatcherClientKind,
) -> Result<WatcherAttachment, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let client_id = format!(
        "{}-{}-{}",
        client_kind.as_str(),
        std::process::id(),
        CLIENT_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    debug_db_lock_log(
        "watch-client",
        format_args!(
            "start_or_attach repo={} client_kind={} client_id={}",
            root.path().display(),
            client_kind.as_str(),
            client_id
        ),
    );
    WriterClient::from_env().submit_and_wait(&WriterJob::StartWatcher {
        repo: root.path().display().to_string(),
        client_kind: client_kind.as_str().to_owned(),
        client_id: client_id.clone(),
        pid: std::process::id() as i32,
    })?;
    heartbeat_attachment(&root, client_kind, client_id)
}

pub fn start_daemon(repo: &str) -> Result<WatcherStatus, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    let client_id = format!(
        "cli-{}-{}",
        std::process::id(),
        CLIENT_COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    debug_db_lock_log(
        "watch-client",
        format_args!(
            "start_daemon repo={} client_id={}",
            root.path().display(),
            client_id
        ),
    );
    WriterClient::from_env().submit_and_wait(&WriterJob::StartWatcher {
        repo: root.path().display().to_string(),
        client_kind: WatcherClientKind::Cli.as_str().to_owned(),
        client_id,
        pid: std::process::id() as i32,
    })?;
    status_for_root(&root)
}

pub fn stop_daemon(repo: &str) -> Result<WatcherStatus, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    debug_db_lock_log(
        "watch-client",
        format_args!("stop_daemon repo={}", root.path().display()),
    );
    WriterClient::from_env().submit_and_wait(&WriterJob::StopWatcher {
        repo: root.path().display().to_string(),
    })?;
    status_for_root(&root)
}

pub fn status(repo: &str) -> Result<WatcherStatus, String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    status_for_root(&root)
}

pub fn spawn_writer_managed_daemon(repo: String, write_gate: Arc<Mutex<()>>) {
    debug_db_lock_log(
        "watcher-managed",
        format_args!("spawn_thread repo={}", repo),
    );
    thread::spawn(move || {
        if let Err(error) = run_writer_managed_daemon(&repo, write_gate) {
            debug_db_lock_log(
                "watcher-managed",
                format_args!("thread_error repo={} error={}", repo, error),
            );
        }
    });
}

pub fn run_writer_managed_daemon(repo: &str, write_gate: Arc<Mutex<()>>) -> Result<(), String> {
    let root = RepoRoot::open(repo).map_err(|error| error.to_string())?;
    debug_db_lock_log(
        "watcher-managed",
        format_args!("start repo={}", root.path().display()),
    );
    let store_config = StoreConfig::from_env();
    let initial = WatcherStatus {
        repository_id: root.id().to_owned(),
        root_path: root.path().display().to_string(),
        mode: "semantic".to_owned(),
        owner_kind: "writer-daemon".to_owned(),
        owner_pid: Some(std::process::id() as i32),
        socket_path: None,
        state: "running".to_owned(),
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
        attached_clients: 0,
        client_kinds: Vec::new(),
        clients: Vec::new(),
        shutdown_after_seconds: None,
    };
    let store = {
        let wait_started = Instant::now();
        debug_db_lock_log(
            "watcher-managed",
            format_args!("setup_gate_wait repo={}", root.path().display()),
        );
        let _guard = write_gate
            .lock()
            .map_err(|_| "writer gate lock poisoned".to_owned())?;
        debug_db_lock_log(
            "watcher-managed",
            format_args!(
                "setup_gate_acquired repo={} wait_ms={}",
                root.path().display(),
                wait_started.elapsed().as_millis()
            ),
        );
        let setup_started = Instant::now();
        let store = SqliteStore::open(&store_config).map_err(|error| error.to_string())?;
        store.migrate().map_err(|error| error.to_string())?;
        store
            .upsert_repository(&RepositoryRecord {
                id: root.id().to_owned(),
                root_path: root.path().display().to_string(),
            })
            .map_err(|error| error.to_string())?;
        store
            .upsert_watcher_status(&initial.record())
            .map_err(|error| error.to_string())?;
        debug_db_lock_log(
            "watcher-managed",
            format_args!(
                "setup_gate_release repo={} held_ms={}",
                root.path().display(),
                setup_started.elapsed().as_millis()
            ),
        );
        store
    };

    let mut current = initial;
    let mut no_clients_since: Option<Instant> = None;
    let result = run_continuous_index_until_with_write_gate(
        &ContinuousIndexOptions::new(root.path().display().to_string(), false),
        |event| {
            apply_event(&mut current, &event);
            let _ = store.upsert_watcher_status(&current.record());
        },
        || clients_allow_continuing(root.id(), &mut no_clients_since),
        |job| {
            let wait_started = Instant::now();
            debug_db_lock_log(
                "watcher-managed",
                format_args!("write_gate_wait repo={}", root.path().display()),
            );
            let _guard = write_gate
                .lock()
                .map_err(|_| "writer gate lock poisoned".to_owned())?;
            debug_db_lock_log(
                "watcher-managed",
                format_args!(
                    "write_gate_acquired repo={} wait_ms={}",
                    root.path().display(),
                    wait_started.elapsed().as_millis()
                ),
            );
            let run_started = Instant::now();
            let result = job();
            debug_db_lock_log(
                "watcher-managed",
                format_args!(
                    "write_gate_release repo={} ok={} held_ms={}",
                    root.path().display(),
                    result.is_ok(),
                    run_started.elapsed().as_millis()
                ),
            );
            result
        },
    );
    match result {
        Ok(()) => {
            debug_db_lock_log(
                "watcher-managed",
                format_args!("stop repo={}", root.path().display()),
            );
            let _guard = write_gate
                .lock()
                .map_err(|_| "writer gate lock poisoned".to_owned())?;
            store
                .mark_watcher_stopped(root.id())
                .map_err(|error| error.to_string())
        }
        Err(error) => {
            debug_db_lock_log(
                "watcher-managed",
                format_args!("fail repo={} error={}", root.path().display(), error),
            );
            let _guard = write_gate
                .lock()
                .map_err(|_| "writer gate lock poisoned".to_owned())?;
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
    let attachment = start_or_attach(repo, WatcherClientKind::Cli)?;
    let status = attachment.status()?;
    on_event(ContinuousIndexEvent::Started {
        repository_id: status.repository_id,
        files_seen: status.files_seen,
    });
    if offline {
        on_event(ContinuousIndexEvent::BatchFailed {
            changes: WatchChangeSet::default(),
            error: "offline foreground watch is now managed by the writer service semantic watcher"
                .to_owned(),
        });
    }
    loop {
        thread::sleep(CLIENT_HEARTBEAT_INTERVAL);
        if let Ok(status) = attachment.status()
            && !status.is_active()
        {
            break;
        }
    }
    Ok(())
}

fn heartbeat_attachment(
    root: &RepoRoot,
    _client_kind: WatcherClientKind,
    client_id: String,
) -> Result<WatcherAttachment, String> {
    let repo = root.path().display().to_string();
    let heartbeat_repo = repo.clone();
    let heartbeat_client_id = client_id.clone();
    let (stop_heartbeat, heartbeat_stop) = mpsc::channel();
    let heartbeat_thread = thread::spawn(move || {
        while heartbeat_stop
            .recv_timeout(CLIENT_HEARTBEAT_INTERVAL)
            .is_err()
        {
            if let Err(error) =
                WriterClient::from_env().submit_and_wait(&WriterJob::HeartbeatWatcherClient {
                    repo: heartbeat_repo.clone(),
                    client_id: heartbeat_client_id.clone(),
                })
            {
                debug_db_lock_log(
                    "watch-client",
                    format_args!(
                        "heartbeat_error repo={} client_id={} error={}",
                        heartbeat_repo, heartbeat_client_id, error
                    ),
                );
            }
        }
    });

    Ok(WatcherAttachment {
        repository_id: root.id().to_owned(),
        client_id,
        root_path: repo,
        endpoint: None,
        stop_heartbeat: Some(stop_heartbeat),
        heartbeat_thread: Some(heartbeat_thread),
    })
}

fn open_store() -> Result<SqliteStore, String> {
    SqliteStore::open(&StoreConfig::from_env()).map_err(|error| error.to_string())
}

fn open_store_read_only() -> Result<SqliteStore, String> {
    SqliteStore::open_read_only(&StoreConfig::from_env()).map_err(|error| error.to_string())
}

fn status_for_root(root: &RepoRoot) -> Result<WatcherStatus, String> {
    let store = match open_store_read_only() {
        Ok(store) => store,
        Err(error) if read_state_missing(&error) => return Ok(WatcherStatus::inactive(root)),
        Err(error) => return Err(error),
    };
    let mut status = match store.watcher_status(root.id()) {
        Ok(Some(status)) => WatcherStatus::from_record(status),
        Ok(None) => WatcherStatus::inactive(root),
        Err(error) if read_state_missing(&error.to_string()) => WatcherStatus::inactive(root),
        Err(error) => return Err(error.to_string()),
    };
    attach_clients(&store, root.id(), &mut status)?;
    Ok(status)
}

fn attach_clients(
    store: &SqliteStore,
    repository_id: &str,
    status: &mut WatcherStatus,
) -> Result<(), String> {
    let stale_before = timestamp_minus(HEARTBEAT_STALE_SECONDS)
        .parse::<u64>()
        .unwrap_or(0);
    let clients = match store.watcher_clients(repository_id) {
        Ok(clients) => clients,
        Err(error) if read_state_missing(&error.to_string()) => Vec::new(),
        Err(error) => return Err(error.to_string()),
    }
    .into_iter()
    .filter(|client| watcher_client_is_live(client, stale_before))
    .collect::<Vec<_>>();
    status.attached_clients = clients.len();
    status.client_kinds = clients
        .iter()
        .map(|client| client.client_kind.clone())
        .collect::<Vec<_>>();
    status.client_kinds.sort();
    status.client_kinds.dedup();
    status.clients = clients
        .into_iter()
        .map(|client| WatcherClientStatus {
            client_id: client.client_id,
            client_kind: client.client_kind,
            pid: client.pid,
            started_at: client.started_at,
            heartbeat_at: client.heartbeat_at,
            last_seen_at: client.last_seen_at,
        })
        .collect();
    status.shutdown_after_seconds = if status.is_active() && status.attached_clients == 0 {
        Some(NO_CLIENT_GRACE.as_secs())
    } else {
        None
    };
    Ok(())
}

fn read_state_missing(error: &str) -> bool {
    error.contains("unable to open database file")
        || error.contains("no such table: watchers")
        || error.contains("no such table: watcher_clients")
}

fn watcher_client_is_live(client: &WatcherClientRecord, stale_before: u64) -> bool {
    client
        .heartbeat_at
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|heartbeat| heartbeat >= stale_before)
        .unwrap_or(true)
}

fn clients_allow_continuing(repository_id: &str, no_clients_since: &mut Option<Instant>) -> bool {
    let Ok(store) = open_store() else {
        return true;
    };
    let _ = prune_stale_clients(&store, repository_id);
    let clients = store
        .watcher_clients(repository_id)
        .unwrap_or_default()
        .len();
    clients_allow_continuing_with_count(clients, no_clients_since)
}

fn clients_allow_continuing_with_count(
    clients: usize,
    no_clients_since: &mut Option<Instant>,
) -> bool {
    if clients > 0 {
        *no_clients_since = None;
        return true;
    }
    let since = no_clients_since.get_or_insert_with(Instant::now);
    since.elapsed() < NO_CLIENT_GRACE
}

fn prune_stale_clients(store: &SqliteStore, repository_id: &str) -> Result<(), String> {
    let stale_before = timestamp_minus(HEARTBEAT_STALE_SECONDS);
    store
        .prune_stale_watcher_clients(repository_id, &stale_before)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn timestamp_minus(seconds: u64) -> String {
    current_timestamp()
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_sub(seconds)
        .to_string()
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

#[allow(dead_code)]
fn handle_control_stream(
    mut stream: impl Read + Write,
    should_continue: &AtomicBool,
    repository_id: &str,
) -> bool {
    let mut command = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        let _ = reader.read_line(&mut command);
    }
    let trimmed = command.trim();
    if trimmed == "stop" {
        should_continue.store(false, Ordering::SeqCst);
        let _ = stream.write_all(b"stopping\n");
        return true;
    }
    let response = handle_control_command(trimmed, repository_id);
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(b"\n");
    false
}

#[allow(dead_code)]
fn handle_control_command(command: &str, repository_id: &str) -> String {
    let mut parts = command.split_whitespace();
    match parts.next() {
        Some("attach") => {
            let Some(client_kind) = parts.next() else {
                return "error missing client kind".to_owned();
            };
            let pid = parts.next().and_then(|value| value.parse::<i32>().ok());
            let Some(client_id) = parts.next() else {
                return "error missing client id".to_owned();
            };
            let now = current_timestamp();
            match open_store().and_then(|store| {
                store
                    .upsert_watcher_client(&WatcherClientRecord {
                        repository_id: repository_id.to_owned(),
                        client_id: client_id.to_owned(),
                        client_kind: client_kind.to_owned(),
                        pid,
                        started_at: Some(now.clone()),
                        heartbeat_at: Some(now),
                        last_seen_at: None,
                    })
                    .map_err(|error| error.to_string())
            }) {
                Ok(()) => format!("attached {client_id}"),
                Err(error) => format!("error {error}"),
            }
        }
        Some("heartbeat") => {
            let Some(client_id) = parts.next() else {
                return "error missing client id".to_owned();
            };
            match open_store().and_then(|store| {
                store
                    .heartbeat_watcher_client(repository_id, client_id)
                    .map_err(|error| error.to_string())
                    .map(|_| ())
            }) {
                Ok(()) => "ok".to_owned(),
                Err(error) => format!("error {error}"),
            }
        }
        Some("detach") => {
            let Some(client_id) = parts.next() else {
                return "error missing client id".to_owned();
            };
            match open_store().and_then(|store| {
                store
                    .remove_watcher_client(repository_id, client_id)
                    .map_err(|error| error.to_string())
            }) {
                Ok(()) => "ok".to_owned(),
                Err(error) => format!("error {error}"),
            }
        }
        Some("status") | None => "ok".to_owned(),
        Some(other) => format!("error unsupported command {other}"),
    }
}

#[cfg(any(test, windows))]
fn watcher_endpoint_name(repository_id: &str) -> String {
    let suffix = repository_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("symdex-watch-{suffix}")
}

#[cfg(unix)]
#[allow(dead_code)]
mod control_ipc {
    use std::fs;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use super::{BufRead, BufReader, StoreConfig, Write, handle_control_stream};
    use symdex_store::sqlite_parent;

    pub type ControlListener = UnixListener;

    pub fn control_endpoint_for_repo(repository_id: &str) -> Result<String, String> {
        let config = StoreConfig::from_env();
        let parent = sqlite_parent(&config).unwrap_or_else(|| PathBuf::from(".symdex"));
        fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
        Ok(parent
            .join(format!("watch-{repository_id}.sock"))
            .display()
            .to_string())
    }

    pub fn bind_control_listener(endpoint: &str) -> Result<ControlListener, String> {
        cleanup_control_endpoint(endpoint);
        UnixListener::bind(endpoint)
            .map_err(|error| format!("bind watcher socket {endpoint}: {error}"))
    }

    pub fn cleanup_control_endpoint(endpoint: &str) {
        let _ = fs::remove_file(endpoint);
    }

    pub fn send_control_command(endpoint: &str, command: &str) -> bool {
        UnixStream::connect(endpoint)
            .and_then(|mut stream| {
                stream.write_all(format!("{command}\n").as_bytes())?;
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let mut response = String::new();
                let _ = BufReader::new(stream).read_line(&mut response);
                if response.starts_with("error ") {
                    Err(std::io::Error::other(response))
                } else {
                    Ok(())
                }
            })
            .is_ok()
    }

    pub fn start_control_listener(
        listener: ControlListener,
        should_continue: Arc<AtomicBool>,
        repository_id: String,
    ) {
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                if handle_control_stream(stream, &should_continue, &repository_id) {
                    break;
                }
                if !should_continue.load(Ordering::SeqCst) {
                    break;
                }
            }
        });
    }
}

#[cfg(windows)]
mod control_ipc {
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, ToNsName as _,
        prelude::{LocalSocketListener, LocalSocketStream},
        traits::{ListenerExt as _, Stream as _},
    };

    use super::{BufRead, BufReader, handle_control_stream, watcher_endpoint_name};

    pub type ControlListener = LocalSocketListener;

    pub fn control_endpoint_for_repo(repository_id: &str) -> Result<String, String> {
        Ok(watcher_endpoint_name(repository_id))
    }

    pub fn bind_control_listener(endpoint: &str) -> Result<ControlListener, String> {
        let name = endpoint
            .to_ns_name::<GenericNamespaced>()
            .map_err(|error| format!("watcher local socket name {endpoint}: {error}"))?;
        ListenerOptions::new()
            .name(name)
            .create_sync()
            .map_err(|error| format!("bind watcher local socket {endpoint}: {error}"))
    }

    pub fn cleanup_control_endpoint(_endpoint: &str) {}

    pub fn send_control_command(endpoint: &str, command: &str) -> bool {
        let name = match endpoint.to_ns_name::<GenericNamespaced>() {
            Ok(name) => name,
            Err(_) => return false,
        };
        let Ok(mut stream) = LocalSocketStream::connect(name) else {
            return false;
        };
        if stream.write_all(format!("{command}\n").as_bytes()).is_err() {
            return false;
        }
        let _ = stream.flush();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).is_ok() && !response.starts_with("error ")
    }

    pub fn start_control_listener(
        listener: ControlListener,
        should_continue: Arc<AtomicBool>,
        repository_id: String,
    ) {
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                if handle_control_stream(stream, &should_continue, &repository_id) {
                    break;
                }
                if !should_continue.load(Ordering::SeqCst) {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::{
        HEARTBEAT_STALE_SECONDS, NO_CLIENT_GRACE, WatcherStatus,
        clients_allow_continuing_with_count, heartbeat_is_stale, timestamp_minus,
        watcher_client_is_live, watcher_endpoint_name,
    };
    use symdex_store::WatcherClientRecord;

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

    #[test]
    fn timestamp_minus_saturates() {
        let stale_before = timestamp_minus(HEARTBEAT_STALE_SECONDS);

        assert!(stale_before.parse::<u64>().is_ok());
    }

    #[test]
    fn no_clients_grace_eventually_stops() {
        let mut no_clients_since = Some(Instant::now() - NO_CLIENT_GRACE);

        assert!(!clients_allow_continuing_with_count(
            0,
            &mut no_clients_since
        ));
    }

    #[test]
    fn active_clients_keep_watcher_running() {
        let mut no_clients_since = Some(Instant::now() - NO_CLIENT_GRACE);

        assert!(clients_allow_continuing_with_count(
            1,
            &mut no_clients_since
        ));
        assert!(no_clients_since.is_none());
    }

    #[test]
    fn stale_clients_are_hidden_from_read_only_status() {
        let stale_before = 100;
        let mut client = sample_client();

        client.heartbeat_at = Some("99".to_owned());
        assert!(!watcher_client_is_live(&client, stale_before));

        client.heartbeat_at = Some("100".to_owned());
        assert!(watcher_client_is_live(&client, stale_before));

        client.heartbeat_at = None;
        assert!(watcher_client_is_live(&client, stale_before));
    }

    #[test]
    fn watcher_endpoint_name_is_deterministic_and_namespaced() {
        assert_eq!(watcher_endpoint_name("repo/id"), "symdex-watch-repo_id");
        assert_eq!(
            watcher_endpoint_name("abc-123_DEF"),
            "symdex-watch-abc-123_DEF"
        );
    }

    #[test]
    #[cfg(unix)]
    fn unix_control_endpoint_uses_state_socket_path() {
        let endpoint = super::control_ipc::control_endpoint_for_repo("repo")
            .unwrap_or_else(|error| panic!("control endpoint should be generated: {error}"));

        assert!(endpoint.ends_with("watch-repo.sock"));
    }

    #[test]
    #[cfg(windows)]
    fn windows_control_endpoint_uses_namespaced_socket_name() {
        let endpoint = super::control_ipc::control_endpoint_for_repo("repo")
            .unwrap_or_else(|error| panic!("control endpoint should be generated: {error}"));

        assert_eq!(endpoint, "symdex-watch-repo");
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

    fn sample_client() -> WatcherClientRecord {
        WatcherClientRecord {
            repository_id: "repo".to_owned(),
            client_id: "tui-1".to_owned(),
            client_kind: "tui".to_owned(),
            pid: Some(1),
            started_at: Some("1".to_owned()),
            heartbeat_at: Some("100".to_owned()),
            last_seen_at: Some("100".to_owned()),
        }
    }
}
