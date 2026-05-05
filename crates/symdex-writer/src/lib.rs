//! Database-file-scoped single writer service.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use symdex_core::stable_id;
use symdex_store::{
    StoreConfig, WriterLease, WriterLeaseKind, WriterLeaseRequest, writer_lock_path,
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
        self.ensure_daemon()?;
        let endpoint = writer_endpoint_for_config(&self.config);
        let request = serde_json::to_string(job).map_err(|error| error.to_string())?;
        let response = ipc::send_request(&endpoint, &request)?;
        serde_json::from_str(&response).map_err(|error| error.to_string())
    }

    fn ensure_daemon(&self) -> Result<(), String> {
        let endpoint = writer_endpoint_for_config(&self.config);
        let ping = serde_json::to_string(&WriterJob::Ping).unwrap_or_default();
        if ipc::send_request(&endpoint, &ping).is_ok() {
            return Ok(());
        }
        ipc::cleanup_endpoint(&endpoint);
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        Command::new(executable)
            .arg("writer-daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start writer daemon: {error}"))?;
        let started = Instant::now();
        while started.elapsed() < START_WAIT {
            if ipc::send_request(&endpoint, &ping).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err("writer daemon did not become ready".to_owned())
    }
}

pub fn writer_endpoint_for_config(config: &StoreConfig) -> String {
    let sqlite_path = config.sqlite_path.display().to_string();
    let id = stable_id(&["writer", &sqlite_path]);
    ipc::endpoint_for_id(&id)
}

pub fn run_daemon(mut handler: impl FnMut(WriterJob) -> WriterJobResponse) -> Result<(), String> {
    let config = StoreConfig::from_env();
    let endpoint = writer_endpoint_for_config(&config);
    let _lease = WriterLease::acquire(
        &config,
        WriterLeaseRequest::new(WriterLeaseKind::Maintenance, "writer-daemon"),
    )
    .map_err(|error| error.to_string())?;
    ipc::cleanup_endpoint(&endpoint);
    let listener = ipc::bind_listener(&endpoint)?;
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    loop {
        match ipc::accept_with_timeout(&listener, Duration::from_millis(250)) {
            Ok(Some(stream)) => {
                if let Ok(mut last_activity) = last_activity.lock() {
                    *last_activity = Instant::now();
                }
                handle_stream(stream, &mut handler);
            }
            Ok(None) => {
                let idle_for = last_activity
                    .lock()
                    .map(|last_activity| last_activity.elapsed())
                    .unwrap_or(IDLE_EXIT_AFTER);
                if idle_for >= IDLE_EXIT_AFTER {
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    ipc::cleanup_endpoint(&endpoint);
    let _ = writer_lock_path(&config);
    Ok(())
}

fn handle_stream(
    mut stream: impl Read + Write,
    handler: &mut impl FnMut(WriterJob) -> WriterJobResponse,
) {
    let mut request = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        let _ = reader.read_line(&mut request);
    }
    let response = match serde_json::from_str::<WriterJob>(request.trim()) {
        Ok(job) => handler(job),
        Err(error) => WriterJobResponse::error(format!("invalid writer job: {error}")),
    };
    let encoded = serde_json::to_string(&response).unwrap_or_else(|error| {
        format!(r#"{{"ok":false,"message":"{error}","progress":[],"data":null}}"#)
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
        let mut stream = UnixStream::connect(endpoint).map_err(|error| error.to_string())?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        stream.write_all(b"\n").map_err(|error| error.to_string())?;
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut response = String::new();
        BufReader::new(stream)
            .read_line(&mut response)
            .map_err(|error| error.to_string())?;
        Ok(response)
    }
}

#[cfg(windows)]
mod ipc {
    use super::*;
    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, Stream as LocalSocketStream,
        traits::Stream as LocalSocketStreamTrait,
    };
    use std::io::ErrorKind;

    pub type Listener = interprocess::local_socket::Listener;
    pub type Stream = LocalSocketStream;

    pub fn endpoint_for_id(id: &str) -> String {
        format!("symdex-writer-{id}")
    }

    pub fn cleanup_endpoint(_endpoint: &str) {}

    pub fn bind_listener(endpoint: &str) -> Result<Listener, String> {
        ListenerOptions::new()
            .name(
                endpoint
                    .to_ns_name::<GenericNamespaced>()
                    .map_err(|error| error.to_string())?,
            )
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
        let mut stream = LocalSocketStream::connect(
            endpoint
                .to_ns_name::<GenericNamespaced>()
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        stream
            .write_all(request.as_bytes())
            .map_err(|error| error.to_string())?;
        stream.write_all(b"\n").map_err(|error| error.to_string())?;
        stream.flush().map_err(|error| error.to_string())?;
        let mut response = String::new();
        BufReader::new(stream)
            .read_line(&mut response)
            .map_err(|error| error.to_string())?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::{WriterIndexScope, WriterJob, WriterJobResponse, writer_endpoint_for_config};
    use symdex_store::StoreConfig;

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
