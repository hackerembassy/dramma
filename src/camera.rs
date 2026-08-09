use log::{error, info};
use nokhwa::Camera;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{
    CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Captures a single frame from the default webcam and saves it as a JPEG
/// under `photos_dir`, running on a dedicated thread so it never blocks the UI.
pub fn capture_donation_photo(photos_dir: &str, username: &str) {
    let photos_dir = photos_dir.to_string();
    let username = username.to_string();

    thread::spawn(move || {
        if let Err(e) = capture_and_save(&photos_dir, &username) {
            error!("📷 Failed to take donation photo: {}", e);
        }
    });
}

fn open_camera() -> Result<Camera, String> {
    let index = CameraIndex::Index(0);
    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

    let mut camera =
        Camera::new(index, requested).map_err(|e| format!("failed to open webcam: {e}"))?;

    camera
        .open_stream()
        .map_err(|e| format!("failed to open webcam stream: {e}"))?;

    Ok(camera)
}

/// Opens the camera for the live diagnostics preview. Decoding a full-resolution
/// frame (e.g. 1920x1080, which many webcams default to) takes ~450ms — far too
/// slow for a live view — so this asks for a modest 640x480 mode first, which
/// virtually every UVC/AVFoundation webcam supports and decodes in under 100ms.
/// Falls back to whatever the camera actually offers if that request is rejected.
fn open_preview_camera() -> Result<Camera, String> {
    let small = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Exact(CameraFormat::new(
        Resolution::new(640, 480),
        FrameFormat::MJPEG,
        30,
    )));
    if let Ok(mut camera) = Camera::new(CameraIndex::Index(0), small)
        && camera.open_stream().is_ok()
    {
        return Ok(camera);
    }
    open_camera()
}

fn capture_and_save(photos_dir: &str, username: &str) -> Result<(), String> {
    let mut camera = open_camera()?;

    // Discard the first couple of frames to let auto-exposure/white-balance settle.
    for _ in 0..2 {
        let _ = camera.frame();
    }

    let frame = camera
        .frame()
        .map_err(|e| format!("failed to capture frame: {e}"))?;
    let image = frame
        .decode_image::<RgbFormat>()
        .map_err(|e| format!("failed to decode frame: {e}"))?;

    std::fs::create_dir_all(photos_dir)
        .map_err(|e| format!("failed to create photos directory {photos_dir}: {e}"))?;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let safe_username: String = username
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = PathBuf::from(photos_dir).join(format!("{timestamp}_{safe_username}.jpg"));

    image
        .save(&path)
        .map_err(|e| format!("failed to save photo to {path:?}: {e}"))?;

    info!("📷 Saved donation photo to {path:?}");
    Ok(())
}

/// Commands accepted by the [`spawn_preview`] thread.
pub enum PreviewCommand {
    Start,
    Stop,
}

/// A single decoded RGB8 frame, ready to hand to `slint::SharedPixelBuffer`.
pub struct PreviewFrame {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const PREVIEW_FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// Spawns a thread that owns the webcam for as long as the diagnostics page's
/// live preview is active. `Start`/`Stop` on `cmd_rx` open/close the device;
/// while open, frames are pushed to `frame_tx` on a best-effort basis (a full
/// channel just means the consumer hasn't caught up, so the frame is dropped).
pub fn spawn_preview(cmd_rx: Receiver<PreviewCommand>, frame_tx: SyncSender<PreviewFrame>) {
    thread::spawn(move || {
        let mut camera: Option<Camera> = None;

        loop {
            let cmd = if camera.is_some() {
                match cmd_rx.try_recv() {
                    Ok(cmd) => Some(cmd),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            } else {
                match cmd_rx.recv() {
                    Ok(cmd) => Some(cmd),
                    Err(_) => break,
                }
            };

            match cmd {
                Some(PreviewCommand::Start) if camera.is_none() => match open_preview_camera() {
                    Ok(cam) => camera = Some(cam),
                    Err(e) => error!("📷 Failed to start preview: {}", e),
                },
                Some(PreviewCommand::Stop) => camera = None,
                _ => {}
            }

            let Some(cam) = camera.as_mut() else {
                continue;
            };

            let frame_start = Instant::now();
            match cam.frame().and_then(|f| f.decode_image::<RgbFormat>()) {
                Ok(image) => {
                    let _ = frame_tx.try_send(PreviewFrame {
                        width: image.width(),
                        height: image.height(),
                        rgb: image.into_raw(),
                    });
                }
                Err(e) => error!("📷 Preview frame capture failed: {}", e),
            }
            // Capture+decode already ate into the budget; only sleep the remainder
            // so the preview holds close to its target cadence instead of drifting.
            if let Some(remaining) = PREVIEW_FRAME_INTERVAL.checked_sub(frame_start.elapsed()) {
                thread::sleep(remaining);
            }
        }
    });
}
