use log::{error, info};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

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
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nNot Found",
            );
        }
    }
}
