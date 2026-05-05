//! Database-role-scoped writer service foundation.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use symdex_core::stable_id;
use symdex_store::{
    DatabaseRole, StoreConfig, WriterLease, WriterLeaseKind, WriterLeaseRequest, debug_db_lock_log,
    debug_db_locks_enabled,
};

const START_WAIT: Duration = Duration::from_secs(3);
const IDLE_EXIT_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriterIndexScope {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriterJob {
    Ping,
    Init,
    Index {
        repo: String,
        offline: bool,
        scope: WriterIndexScope,
    },
    IndexQuality {
        repo: String,
    },
    VectorRepair {
        repo: String,
        semantic_layer: String,
    },
    StartWatcher {
        repo: String,
        client_kind: String,
        client_id: String,
        pid: i32,
    },
    StopWatcher {
        repo: String,
    },
    AttachWatcherClient {
        repo: String,
        client_kind: String,
        client_id: String,
        pid: i32,
    },
    HeartbeatWatcherClient {
        repo: String,
        client_id: String,
    },
    DetachWatcherClient {
        repo: String,
        client_id: String,
    },
}

impl WriterJob {
    pub fn database_role(&self) -> Option<DatabaseRole> {
        match self {
            Self::Ping => None,
            Self::AttachWatcherClient { .. }
            | Self::HeartbeatWatcherClient { .. }
            | Self::DetachWatcherClient { .. } => Some(DatabaseRole::Watch),
            Self::Init
            | Self::Index { .. }
            | Self::IndexQuality { .. }
            | Self::VectorRepair { .. }
            | Self::StartWatcher { .. }
            | Self::StopWatcher { .. } => Some(DatabaseRole::Structural),
        }
    }

    pub fn operation(&self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Init => "init",
            Self::Index { .. } => "index",
            Self::IndexQuality { .. } => "index-quality",
            Self::VectorRepair { .. } => "vector-repair",
            Self::StartWatcher { .. } => "watch-start",
            Self::StopWatcher { .. } => "watch-stop",
            Self::AttachWatcherClient { .. } => "watch-attach",
            Self::HeartbeatWatcherClient { .. } => "watch-heartbeat",
            Self::DetachWatcherClient { .. } => "watch-detach",
        }
    }

    pub fn debug_details(&self) -> String {
        match self {
            Self::Ping => "operation=ping".to_owned(),
            Self::Init => "operation=init".to_owned(),
            Self::Index {
                repo,
                offline,
                scope,
            } => format!(
                "operation=index repo={} offline={} scope={scope:?}",
                repo, offline
            ),
            Self::IndexQuality { repo } => {
                format!("operation=index-quality repo={repo}")
            }
            Self::VectorRepair {
                repo,
                semantic_layer,
            } => {
                format!("operation=vector-repair repo={repo} semantic_layer={semantic_layer}")
            }
            Self::StartWatcher {
                repo,
                client_kind,
                client_id,
                pid,
            } => format!(
                "operation=watch-start repo={} client_kind={} client_id={} client_pid={}",
                repo, client_kind, client_id, pid
            ),
            Self::StopWatcher { repo } => format!("operation=watch-stop repo={repo}"),
            Self::AttachWatcherClient {
                repo,
                client_kind,
                client_id,
                pid,
            } => format!(
                "operation=watch-attach repo={} client_kind={} client_id={} client_pid={}",
                repo, client_kind, client_id, pid
            ),
            Self::HeartbeatWatcherClient { repo, client_id } => format!(
                "operation=watch-heartbeat repo={} client_id={}",
                repo, client_id
            ),
            Self::DetachWatcherClient { repo, client_id } => {
                format!("operation=watch-detach repo={repo} client_id={client_id}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriterProgress {
    pub phase: String,
    pub completed: usize,
    pub total: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriterJobResponse {
    pub ok: bool,
    pub message: String,
    pub progress: Vec<WriterProgress>,
    pub data: serde_json::Value,
}

impl WriterJobResponse {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            progress: Vec::new(),
            data: serde_json::Value::Null,
        }
    }

    pub fn ok_with_data(message: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            message: message.into(),
            progress: Vec::new(),
            data,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            progress: Vec::new(),
            data: serde_json::Value::Null,
        }
    }

    pub fn with_progress(mut self, progress: Vec<WriterProgress>) -> Self {
        self.progress = progress;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WriterStreamMessage {
    Progress { progress: WriterProgress },
    Result { response: WriterJobResponse },
}

#[derive(Debug, Clone)]
pub struct WriterClient {
    config: StoreConfig,
}

impl WriterClient {
    pub fn from_env() -> Self {
        Self {
            config: StoreConfig::from_env(),
        }
    }

    pub fn new(config: StoreConfig) -> Self {
        Self { config }
    }

    pub fn submit_and_wait(&self, job: &WriterJob) -> Result<WriterJobResponse, String> {
        self.submit_and_wait_with_progress(job, |_| {})
    }

    pub fn submit_and_wait_with_progress(
        &self,
        job: &WriterJob,
        mut on_progress: impl FnMut(WriterProgress),
    ) -> Result<WriterJobResponse, String> {
        let started = Instant::now();
        let role = job.database_role().unwrap_or(DatabaseRole::Structural);
        let endpoint = writer_endpoint_for_role(&self.config, role);
        debug_db_lock_log(
            "writer-client",
            format_args!(
                "submit_start role={} db={} endpoint={} {}",
                role.as_str(),
                self.config.database_path(role).display(),
                endpoint,
                job.debug_details()
            ),
        );
        self.ensure_daemon(role)?;
        let request = serde_json::to_string(job).map_err(|error| error.to_string())?;
        let mut streamed_progress = Vec::new();
        let mut final_response = None;
        ipc::send_request_stream(&endpoint, &request, |line| {
            match serde_json::from_str::<WriterStreamMessage>(line) {
                Ok(WriterStreamMessage::Progress { progress }) => {
                    on_progress(progress.clone());
                    streamed_progress.push(progress);
                    Ok(())
                }
                Ok(WriterStreamMessage::Result { response }) => {
                    final_response = Some(response);
                    Ok(())
                }
                Err(_) => {
                    let response = serde_json::from_str::<WriterJobResponse>(line)
                        .map_err(|error| error.to_string())?;
                    final_response = Some(response);
                    Ok(())
                }
            }
        })?;
        let mut response =
            final_response.ok_or_else(|| "writer daemon closed without response".to_owned())?;
        if response.progress.is_empty() {
            response.progress = streamed_progress;
        }
        debug_db_lock_log(
            "writer-client",
            format_args!(
                "submit_finish db={} endpoint={} operation={} ok={} elapsed_ms={} message={}",
                self.config.database_path(role).display(),
                endpoint,
                job.operation(),
                response.ok,
                started.elapsed().as_millis(),
                response.message
            ),
        );
        Ok(response)
    }

    fn ensure_daemon(&self, role: DatabaseRole) -> Result<(), String> {
        let endpoint = writer_endpoint_for_role(&self.config, role);
        let ping = serde_json::to_string(&WriterJob::Ping).unwrap_or_default();
        if ipc::send_request(&endpoint, &ping).is_ok() {
            debug_db_lock_log(
                "writer-client",
                format_args!(
                    "ensure_daemon_attach role={} db={} endpoint={}",
                    role.as_str(),
                    self.config.database_path(role).display(),
                    endpoint
                ),
            );
            return Ok(());
        }
        debug_db_lock_log(
            "writer-client",
            format_args!(
                "ensure_daemon_start role={} db={} endpoint={}",
                role.as_str(),
                self.config.database_path(role).display(),
                endpoint
            ),
        );
        ipc::cleanup_endpoint(&endpoint);
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let stderr = if debug_db_locks_enabled() {
            Stdio::inherit()
        } else {
            Stdio::null()
        };
        let mut command = Command::new(executable);
        command.arg("writer-daemon");
        if role != DatabaseRole::Structural {
            command.arg("--role").arg(role.as_str());
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("start writer daemon: {error}"))?;
        debug_db_lock_log(
            "writer-client",
            format_args!(
                "ensure_daemon_spawned role={} db={} endpoint={} daemon_pid={}",
                role.as_str(),
                self.config.database_path(role).display(),
                endpoint,
                child.id()
            ),
        );
        let started = Instant::now();
        while started.elapsed() < START_WAIT {
            if ipc::send_request(&endpoint, &ping).is_ok() {
                debug_db_lock_log(
                    "writer-client",
                    format_args!(
                        "ensure_daemon_ready role={} db={} endpoint={} wait_ms={}",
                        role.as_str(),
                        self.config.database_path(role).display(),
                        endpoint,
                        started.elapsed().as_millis()
                    ),
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        debug_db_lock_log(
            "writer-client",
            format_args!(
                "ensure_daemon_timeout role={} db={} endpoint={} wait_ms={}",
                role.as_str(),
                self.config.database_path(role).display(),
                endpoint,
                started.elapsed().as_millis()
            ),
        );
        Err("writer daemon did not become ready".to_owned())
    }
}

pub fn writer_endpoint_for_config(config: &StoreConfig) -> String {
    let sqlite_path = config.sqlite_path.display().to_string();
    let id = stable_id(&["writer", &sqlite_path]);
    ipc::endpoint_for_id(&id)
}

pub fn writer_endpoint_for_role(config: &StoreConfig, role: DatabaseRole) -> String {
    if role == DatabaseRole::Structural {
        return writer_endpoint_for_config(config);
    }
    let database_path = config.database_path(role).display().to_string();
    let id = stable_id(&["writer", role.as_str(), &database_path]);
    ipc::endpoint_for_id(&id)
}

pub fn run_daemon(
    handler: impl FnMut(WriterJob, &mut dyn FnMut(WriterProgress)) -> WriterJobResponse,
) -> Result<(), String> {
    run_daemon_for_role(DatabaseRole::Structural, handler)
}

pub fn run_daemon_for_role(
    role: DatabaseRole,
    mut handler: impl FnMut(WriterJob, &mut dyn FnMut(WriterProgress)) -> WriterJobResponse,
) -> Result<(), String> {
    let config = StoreConfig::from_env();
    let endpoint = writer_endpoint_for_role(&config, role);
    debug_db_lock_log(
        "writer-daemon",
        format_args!(
            "start role={} db={} endpoint={} lock={}",
            role.as_str(),
            config.database_path(role).display(),
            endpoint,
            writer_lock_path_for_log(&config, role)
        ),
    );
    let _lease = WriterLease::acquire_for_role(
        &config,
        role,
        WriterLeaseRequest::new(WriterLeaseKind::Maintenance, "writer-daemon"),
    )
    .map_err(|error| error.to_string())?;
    ipc::cleanup_endpoint(&endpoint);
    let listener = ipc::bind_listener(&endpoint)?;
    debug_db_lock_log(
        "writer-daemon",
        format_args!(
            "listening role={} db={} endpoint={}",
            role.as_str(),
            config.database_path(role).display(),
            endpoint
        ),
    );
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let mut next_job_id = 1usize;
    loop {
        match ipc::accept_with_timeout(&listener, Duration::from_millis(250)) {
            Ok(Some(stream)) => {
                if let Ok(mut last_activity) = last_activity.lock() {
                    *last_activity = Instant::now();
                }
                let job_id = next_job_id;
                next_job_id += 1;
                handle_stream(role, job_id, stream, &mut handler);
            }
            Ok(None) => {
                let idle_for = last_activity
                    .lock()
                    .map(|last_activity| last_activity.elapsed())
                    .unwrap_or(IDLE_EXIT_AFTER);
                if idle_for >= IDLE_EXIT_AFTER {
                    debug_db_lock_log(
                        "writer-daemon",
                        format_args!(
                            "idle_exit role={} db={} endpoint={} idle_ms={}",
                            role.as_str(),
                            config.database_path(role).display(),
                            endpoint,
                            idle_for.as_millis()
                        ),
                    );
                    break;
                }
            }
            Err(error) => {
                debug_db_lock_log(
                    "writer-daemon",
                    format_args!(
                        "accept_error role={} db={} endpoint={} error={}",
                        role.as_str(),
                        config.database_path(role).display(),
                        endpoint,
                        error
                    ),
                );
                return Err(error);
            }
        }
    }
    ipc::cleanup_endpoint(&endpoint);
    debug_db_lock_log(
        "writer-daemon",
        format_args!(
            "stop role={} db={} endpoint={} lock={}",
            role.as_str(),
            config.database_path(role).display(),
            endpoint,
            writer_lock_path_for_log(&config, role)
        ),
    );
    Ok(())
}

fn writer_lock_path_for_log(config: &StoreConfig, role: DatabaseRole) -> String {
    symdex_store::writer_lock_path_for_role(config, role)
        .display()
        .to_string()
}

fn handle_stream(
    role: DatabaseRole,
    job_id: usize,
    mut stream: impl Read + Write,
    handler: &mut impl FnMut(WriterJob, &mut dyn FnMut(WriterProgress)) -> WriterJobResponse,
) {
    let mut request = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        let _ = reader.read_line(&mut request);
    }
    let response = match serde_json::from_str::<WriterJob>(request.trim()) {
        Ok(job) => {
            let started = Instant::now();
            debug_db_lock_log(
                "writer-daemon",
                format_args!(
                    "job_start role={} job_id={} {}",
                    role.as_str(),
                    job_id,
                    job.debug_details()
                ),
            );
            let response = if job.database_role().is_some_and(|job_role| job_role != role) {
                WriterJobResponse::error(format!(
                    "writer job `{}` requires {} writer, but this daemon owns {}",
                    job.operation(),
                    job.database_role()
                        .map(DatabaseRole::as_str)
                        .unwrap_or("any"),
                    role.as_str()
                ))
            } else {
                let mut on_progress = |progress: WriterProgress| {
                    let message = WriterStreamMessage::Progress { progress };
                    if let Ok(encoded) = serde_json::to_string(&message) {
                        let _ = stream.write_all(encoded.as_bytes());
                        let _ = stream.write_all(b"\n");
                        let _ = stream.flush();
                    }
                };
                handler(job, &mut on_progress)
            };
            debug_db_lock_log(
                "writer-daemon",
                format_args!(
                    "job_finish job_id={} ok={} elapsed_ms={} message={}",
                    job_id,
                    response.ok,
                    started.elapsed().as_millis(),
                    response.message
                ),
            );
            response
        }
        Err(error) => {
            debug_db_lock_log(
                "writer-daemon",
                format_args!("job_invalid job_id={} error={}", job_id, error),
            );
            WriterJobResponse::error(format!("invalid writer job: {error}"))
        }
    };
    let encoded = serde_json::to_string(&WriterStreamMessage::Result { response })
        .unwrap_or_else(|error| {
            format!(
                r#"{{"kind":"result","response":{{"ok":false,"message":"{error}","progress":[],"data":null}}}}"#
            )
        });
    let _ = stream.write_all(encoded.as_bytes());
    let _ = stream.write_all(b"\n");
}

#[cfg(unix)]
mod ipc {
    use super::*;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::net::{UnixListener, UnixStream};

    pub type Listener = UnixListener;
    pub type Stream = UnixStream;

    pub fn endpoint_for_id(id: &str) -> String {
        std::path::Path::new("/tmp")
            .join(format!("symdex-writer-{id}.sock"))
            .display()
            .to_string()
    }

    pub fn cleanup_endpoint(endpoint: &str) {
        let _ = fs::remove_file(endpoint);
    }

    pub fn bind_listener(endpoint: &str) -> Result<Listener, String> {
        if let Some(parent) = std::path::Path::new(endpoint).parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        UnixListener::bind(endpoint)
            .map_err(|error| error.to_string())
            .and_then(|listener| {
                listener
                    .set_nonblocking(true)
                    .map_err(|error| error.to_string())?;
                Ok(listener)
            })
    }

    pub fn accept_with_timeout(
        listener: &Listener,
        timeout: Duration,
    ) -> Result<Option<Stream>, String> {
        let started = Instant::now();
        loop {
            match listener.accept() {
                Ok((stream, _)) => return Ok(Some(stream)),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    pub fn send_request(endpoint: &str, request: &str) -> Result<String, String> {
        let mut response = None;
        send_request_stream(endpoint, request, |line| {
            response = Some(line.to_owned());
            Ok(())
        })?;
        response.ok_or_else(|| "writer daemon closed without response".to_owned())
    }

    pub fn send_request_stream(
        endpoint: &str,
        request: &str,
        mut on_line: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut stream = UnixStream::connect(endpoint).map_err(|error| error.to_string())?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        stream.write_all(b"\n").map_err(|error| error.to_string())?;
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line.map_err(|error| error.to_string())?;
            on_line(&line)?;
        }
        Ok(())
    }
}

#[cfg(windows)]
mod ipc {
    use super::*;
    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, Stream as LocalSocketStream, ToNsName as _,
        traits::{Listener as LocalSocketListenerTrait, Stream as LocalSocketStreamTrait},
    };
    use std::io::ErrorKind;

    pub type Listener = interprocess::local_socket::Listener;
    pub type Stream = LocalSocketStream;

    pub fn endpoint_for_id(id: &str) -> String {
        format!("symdex-writer-{id}")
    }

    pub fn cleanup_endpoint(_endpoint: &str) {}

    pub fn bind_listener(endpoint: &str) -> Result<Listener, String> {
        let name = endpoint
            .to_ns_name::<GenericNamespaced>()
            .map_err(|error| error.to_string())?;

        ListenerOptions::new()
            .name(name)
            .create_sync()
            .map_err(|error| error.to_string())
    }

    pub fn accept_with_timeout(
        listener: &Listener,
        timeout: Duration,
    ) -> Result<Option<Stream>, String> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            match listener.accept() {
                Ok(stream) => return Ok(Some(stream)),
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(None)
    }

    pub fn send_request(endpoint: &str, request: &str) -> Result<String, String> {
        let mut response = None;
        send_request_stream(endpoint, request, |line| {
            response = Some(line.to_owned());
            Ok(())
        })?;
        response.ok_or_else(|| "writer daemon closed without response".to_owned())
    }

    pub fn send_request_stream(
        endpoint: &str,
        request: &str,
        mut on_line: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let name = endpoint
            .to_ns_name::<GenericNamespaced>()
            .map_err(|error| error.to_string())?;

        let mut stream = LocalSocketStream::connect(name).map_err(|error| error.to_string())?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        stream.write_all(b"\n").map_err(|error| error.to_string())?;
        stream.flush().map_err(|error| error.to_string())?;
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line.map_err(|error| error.to_string())?;
            on_line(&line)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WriterIndexScope, WriterJob, WriterJobResponse, writer_endpoint_for_config,
        writer_endpoint_for_role,
    };
    use symdex_store::{DatabaseRole, StoreConfig};

    #[test]
    fn writer_endpoint_is_stable_for_database_path() {
        let config = StoreConfig {
            sqlite_path: std::env::temp_dir().join("symdex-writer-a.sqlite"),
        };

        assert_eq!(
            writer_endpoint_for_config(&config),
            writer_endpoint_for_config(&config)
        );
    }

    #[test]
    fn writer_endpoint_is_database_file_scoped() {
        let first = StoreConfig {
            sqlite_path: std::env::temp_dir().join("symdex-writer-a.sqlite"),
        };
        let second = StoreConfig {
            sqlite_path: std::env::temp_dir().join("symdex-writer-b.sqlite"),
        };

        assert_ne!(
            writer_endpoint_for_config(&first),
            writer_endpoint_for_config(&second)
        );
    }

    #[test]
    fn writer_endpoint_is_database_role_scoped() {
        let config = StoreConfig {
            sqlite_path: std::env::temp_dir().join("symdex-writer-a.sqlite"),
        };

        assert_eq!(
            writer_endpoint_for_role(&config, DatabaseRole::Structural),
            writer_endpoint_for_config(&config)
        );
        assert_ne!(
            writer_endpoint_for_role(&config, DatabaseRole::FastSemantic),
            writer_endpoint_for_role(&config, DatabaseRole::QualitySemantic)
        );
        assert_ne!(
            writer_endpoint_for_config(&config),
            writer_endpoint_for_role(&config, DatabaseRole::Events)
        );
    }

    #[test]
    fn writer_jobs_select_database_roles() {
        assert_eq!(WriterJob::Ping.database_role(), None);
        assert_eq!(
            WriterJob::Init.database_role(),
            Some(DatabaseRole::Structural)
        );
        assert_eq!(
            (WriterJob::HeartbeatWatcherClient {
                repo: ".".to_owned(),
                client_id: "client".to_owned(),
            })
            .database_role(),
            Some(DatabaseRole::Watch)
        );
        assert_eq!(DatabaseRole::parse("watch"), Some(DatabaseRole::Watch));
        assert_eq!(DatabaseRole::parse("unknown"), None);
    }

    #[test]
    fn writer_jobs_round_trip_as_json() {
        let job = WriterJob::Index {
            repo: ".".to_owned(),
            offline: true,
            scope: WriterIndexScope::Incremental,
        };

        let encoded = serde_json::to_string(&job).expect("job should encode");
        let decoded: WriterJob = serde_json::from_str(&encoded).expect("job should decode");

        assert_eq!(decoded, job);
        assert_eq!(decoded.operation(), "index");
    }

    #[test]
    fn writer_response_reports_success_and_errors() {
        let success = WriterJobResponse::ok("done");
        let error = WriterJobResponse::error("failed");

        assert!(success.ok);
        assert_eq!(success.message, "done");
        assert!(!error.ok);
        assert_eq!(error.message, "failed");
    }
}
