use log::{error, info};
use nokhwa::Camera;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

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

fn capture_and_save(photos_dir: &str, username: &str) -> Result<(), String> {
    let index = CameraIndex::Index(0);
    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate);

    let mut camera =
        Camera::new(index, requested).map_err(|e| format!("failed to open webcam: {e}"))?;

    camera
        .open_stream()
        .map_err(|e| format!("failed to open webcam stream: {e}"))?;

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
