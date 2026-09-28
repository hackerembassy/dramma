use crate::error::RequestError;
use crate::health::{HealthError, HealthSnapshot};
use http::Request;
use isahc::prelude::*;
use log::{error, info};
use serde::Serialize;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

const HEALTH_ENTITY_ID: &str = "sensor.dramma_health";

#[derive(Serialize)]
struct HealthState<'a> {
    state: &'a str,
    attributes: HealthAttributes<'a>,
}

#[derive(Serialize)]
struct HealthAttributes<'a> {
    friendly_name: &'static str,
    errors: &'a [HealthError],
    restart_in_secs: Option<u64>,
    restart_requested: bool,
}

/// Pushes acceptor health into Home Assistant as `sensor.dramma_health` via the
/// states API, creating the entity on first use and updating it afterwards.
pub async fn push_health(
    api_url: &str,
    token: &str,
    snapshot: &HealthSnapshot,
) -> Result<(), RequestError> {
    let url = format!(
        "{}/api/states/{}",
        api_url.trim_end_matches('/'),
        HEALTH_ENTITY_ID
    );
    let payload = HealthState {
        state: snapshot.status,
        attributes: HealthAttributes {
            friendly_name: "Dramma Health",
            errors: &snapshot.errors,
            restart_in_secs: snapshot.restart_in_secs,
            restart_requested: snapshot.restart_requested,
        },
    };
    let body = serde_json::to_vec(&payload)?;

    let request = Request::post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(body)?;

    let mut response = isahc::send_async(request).await?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        Err(RequestError::Api {
            status: status.as_u16(),
            message,
        })
    }
}

/// Manages a Chromium subprocess for displaying Home Assistant
pub struct ChromiumManager {
    process: Arc<Mutex<Option<Child>>>,
}

impl ChromiumManager {
    pub fn new() -> Self {
        Self {
            process: Arc::new(Mutex::new(None)),
        }
    }

    /// Launch Chromium in app mode with the given URL
    pub fn launch(&self, url: &str) -> Result<(), String> {
        let mut process_guard = self.process.lock().unwrap();

        // If there's already a process running, kill it first
        if let Some(ref mut child) = *process_guard {
            info!("Killing existing Chromium process");
            let _ = child.kill();
            let _ = child.wait();
        }

        info!("Launching Chromium with URL: {}", url);

        // Try chromium first, then chromium-browser as fallback (different Debian versions)
        let command_result = Command::new("chromium")
            .arg("--app=".to_string() + url)
            .arg("--start-fullscreen")
            .arg("--window-position=0,0")
            .arg("--disable-infobars")
            .arg("--noerrdialogs")
            .arg("--disable-session-crashed-bubble")
            .arg("--disable-pinch")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--enable-native-gpu-memory-buffers")
            .arg("--ozone-platform-hint=auto")
            .arg("--enable-features=AcceleratedVideoEncoder,VaapiOnNvidiaGPUs,VaapiIgnoreDriverChecks,Vulkan,DefaultANGLEVulkan,VulkanFromANGLE,VaapiVideoDecoder,PlatformHEVCDecoderSupport,UseMultiPlaneFormatForHardwareVideo,OverlayScrollbar")
            .arg("--ignore-gpu-blocklist")
            .arg("--enable-zero-copy")
            .arg("--autoplay-policy=no-user-gesture-required")
            .arg("--disable-restore-session-state")
            .spawn()
            .or_else(|_| {
                // Fallback to chromium-browser
                Command::new("chromium-browser")
                    .arg("--app=".to_string() + url)
                    .arg("--start-fullscreen")
                    .arg("--window-position=0,0")
                    .arg("--disable-infobars")
                    .arg("--noerrdialogs")
                    .arg("--disable-session-crashed-bubble")
                    .arg("--disable-pinch")
                    .arg("--no-first-run")
                    .arg("--no-default-browser-check")
                    .arg("--enable-native-gpu-memory-buffers")
                    .arg("--ozone-platform-hint=auto")
                    .arg("--enable-features=AcceleratedVideoEncoder,VaapiOnNvidiaGPUs,VaapiIgnoreDriverChecks,Vulkan,DefaultANGLEVulkan,VulkanFromANGLE,VaapiVideoDecoder,PlatformHEVCDecoderSupport,UseMultiPlaneFormatForHardwareVideo,OverlayScrollbar")
                    .arg("--ignore-gpu-blocklist")
                    .arg("--enable-zero-copy")
                    .arg("--autoplay-policy=no-user-gesture-required")
                    .arg("--disable-restore-session-state")
                    .spawn()
            });

        match command_result {
            Ok(child) => {
                info!("Chromium launched successfully with PID: {}", child.id());
                *process_guard = Some(child);
                Ok(())
            }
            Err(e) => {
                let err_msg = format!(
                    "Failed to launch Chromium. Make sure chromium is installed: {}",
                    e
                );
                error!("{}", err_msg);
                Err(err_msg)
            }
        }
    }

    /// Close the Chromium process
    pub fn close(&self) {
        let mut process_guard = self.process.lock().unwrap();

        if let Some(ref mut child) = *process_guard {
            info!("Closing Chromium process");
            if let Err(e) = child.kill() {
                error!("Failed to kill Chromium process: {}", e);
            } else {
                let _ = child.wait();
                info!("Chromium process closed");
            }
        }

        *process_guard = None;
    }
}

impl Drop for ChromiumManager {
    fn drop(&mut self) {
        self.close();
    }
}

/// Starts a simple HTTP listener for remote control from Home Assistant.
/// When a `POST /close-hass` request is received, sends a signal through `tx`.
#[allow(dead_code)]
pub fn start_close_listener(port: u16, tx: Sender<()>) {
    let addr = format!("0.0.0.0:{}", port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind HASS close listener on {}: {}", addr, e);
            return;
        }
    };
    info!("🏠 Home Assistant close listener on port {}", port);

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            continue;
        };
        let mut buf = [0u8; 512];
        let Ok(n) = stream.read(&mut buf) else {
            continue;
        };
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or("");

        if first_line.starts_with("POST /close-hass") {
            info!("🏠 Received remote close-hass request");
            let _ = tx.send(());
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 2\r\n\r\nOK",
            );
        } else if first_line.starts_with("OPTIONS") {
            // CORS preflight
            let _ = stream.write_all(
                b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n\r\n",
            );
        } else {
            let _ =
                stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nNot Found");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{Acceptor, Health};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    /// Accepts a single request, replies with `status`, and returns (headers, body).
    fn accept_one(listener: TcpListener, status: &str) -> (String, String) {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 512];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let content_length: usize = headers
            .to_ascii_lowercase()
            .lines()
            .find_map(|line| line.strip_prefix("content-length:").map(str::trim))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let body_start = header_end + 4;
        while buf.len() < body_start + content_length {
            let n = stream.read(&mut chunk).unwrap();
            buf.extend_from_slice(&chunk[..n]);
        }
        let body =
            String::from_utf8_lossy(&buf[body_start..body_start + content_length]).into_owned();
        stream
            .write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").as_bytes())
            .unwrap();
        (headers, body)
    }

    fn block_on(
        future: impl std::future::Future<Output = Result<(), RequestError>>,
    ) -> Result<(), RequestError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn pushes_status_and_component_errors_with_bearer_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || accept_one(listener, "200 OK"));

        let health = Health::new(Duration::from_secs(300));
        health
            .device(Acceptor::CcTalk)
            .unavailable("USB adapter missing");
        let snapshot = health.snapshot();

        block_on(push_health(&api_url, "test-token", &snapshot)).unwrap();

        let (headers, body) = server.join().unwrap();
        assert!(headers.starts_with("POST /api/states/sensor.dramma_health HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token")
        );
        assert!(body.contains("\"state\":\"error\""));
        assert!(body.contains("USB adapter missing"));
    }

    #[test]
    fn non_success_status_is_reported_as_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || accept_one(listener, "401 Unauthorized"));

        let snapshot = Health::new(Duration::ZERO).snapshot();
        let result = block_on(push_health(&api_url, "bad-token", &snapshot));

        server.join().unwrap();
        assert!(matches!(result, Err(RequestError::Api { status: 401, .. })));
    }
}
