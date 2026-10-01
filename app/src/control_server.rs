//! Local named-pipe / Unix-socket listener. Never runs on the audio thread.
use std::io::{self, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use interprocess::local_socket::{
    prelude::*, GenericNamespaced, ListenerNonblockingMode, ListenerOptions,
};
use mydaw_control_protocol::{
    read_message, write_message, Reply, Request, SessionEndpoint, PROTOCOL_VERSION,
};
use tauri::Manager;

// A connected local client must finish framing its request and reading its
// reply. Nonblocking IO also lets application shutdown release stalled clients.
struct DeadlineStream {
    stream: LocalSocketStream,
    deadline: Instant,
    alive: Arc<AtomicBool>,
}
impl DeadlineStream {
    fn retry(&self) -> io::Result<()> {
        if !self.alive.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "local control IO timed out",
            ));
        }
        thread::sleep(Duration::from_millis(2));
        Ok(())
    }
}
impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.stream.read(bytes) {
                // PIPE_NOWAIT's ERROR_NO_DATA is downgraded to EOF by
                // interprocess on Windows. Treat it as not ready until deadline.
                #[cfg(windows)]
                Ok(0) if !bytes.is_empty() => self.retry()?,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.retry()?,
                result => return result,
            }
        }
    }
}
impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            match self.stream.write(bytes) {
                #[cfg(windows)]
                Ok(0) if !bytes.is_empty() => self.retry()?,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.retry()?,
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        // Framing completes with write_all; flushing Windows pipes can wait for
        // a disconnected or stalled reader indefinitely and is unnecessary.
        Ok(())
    }
}

pub struct ControlServer {
    running: Arc<AtomicBool>,
    path: std::path::PathBuf,
}
impl ControlServer {
    pub fn start(app: tauri::AppHandle) -> std::io::Result<Self> {
        let state = app.state::<crate::AppState>();
        let endpoint = SessionEndpoint {
            version: PROTOCOL_VERSION,
            session_id: state.control().session_id.clone(),
            socket: format!("mydaw-{}", state.control().session_id),
            token: uuid::Uuid::new_v4().to_string(),
            pid: std::process::id(),
        };
        let name = endpoint.socket.as_str().to_ns_name::<GenericNamespaced>()?;
        let listener = ListenerOptions::new()
            .name(name)
            .nonblocking(ListenerNonblockingMode::Accept)
            .create_sync()?;
        let directory = mydaw_control_protocol::session_dir();
        std::fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = directory.join(format!("{}.json", endpoint.session_id));
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, serde_json::to_vec(&endpoint)?)?;
        std::fs::rename(&temporary, &path)?;
        let running = Arc::new(AtomicBool::new(true));
        let alive = Arc::clone(&running);
        thread::Builder::new()
            .name("mydaw-control".into())
            .spawn(move || {
                let clients = Arc::new(AtomicUsize::new(0));
                while alive.load(Ordering::Acquire) {
                    let stream = match listener.accept() {
                        Ok(stream) => stream,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(10));
                            continue;
                        }
                        Err(e) => {
                            log::warn!("local control listener: {e}");
                            break;
                        }
                    };
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    if clients.fetch_add(1, Ordering::AcqRel) >= 8 {
                        clients.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    let app = app.clone();
                    let token = endpoint.token.clone();
                    let clients = Arc::clone(&clients);
                    let client_alive = Arc::clone(&alive);
                    thread::spawn(move || {
                        let mut stream = DeadlineStream {
                            stream,
                            deadline: Instant::now() + Duration::from_secs(5),
                            alive: client_alive,
                        };
                        if let Ok(mut request) = read_message::<Request>(&mut stream) {
                            if request.token.as_deref() == Some(&token) {
                                request.origin = Some("cli".into());
                                let state = app.state::<crate::AppState>();
                                let reply = tauri::async_runtime::block_on(
                                    state.control().execute(&app, request),
                                );
                                stream.deadline = Instant::now() + Duration::from_secs(5);
                                let _ = write_message(&mut stream, &reply);
                            } else {
                                let reply = Reply {
                                    version: PROTOCOL_VERSION,
                                    session_id: String::new(),
                                    revision: 0,
                                    data: serde_json::Value::Null,
                                    error: Some(mydaw_control_protocol::ControlError::new(
                                        "UNAUTHORIZED",
                                        "invalid session token",
                                    )),
                                };
                                let _ = write_message(&mut stream, &reply);
                            }
                        }
                        clients.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            })?;
        Ok(Self { running, path })
    }
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
    }
}
impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop();
    }
}
