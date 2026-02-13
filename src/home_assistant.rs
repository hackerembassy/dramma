use log::{error, info};
use std::process::{Child, Command};
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
            .arg("--window-size=1280,940")
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
                    .arg("--window-size=1280,940")
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
