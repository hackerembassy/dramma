use http::Request;
use image::{GenericImageView, RgbImage};
use isahc::prelude::*;
use log::info;
use serde::Deserialize;

#[derive(Deserialize)]
struct CatResponseItem {
    url: String,
}

/// Fetch a random cat image from thecatapi.com, decode it, and resize to fit the receipt printer (max width 500px)
pub async fn fetch_cat_image() -> Result<RgbImage, Box<dyn std::error::Error + Send + Sync>> {
    info!("Fetching random cat info from API...");
    let request = Request::get("https://api.thecatapi.com/v1/images/search")
        .header("User-Agent", "dramma/0.4.1")
        .body(())?;

    let mut response = isahc::send_async(request).await?;
    if !response.status().is_success() {
        return Err(format!("Cat API returned status: {}", response.status()).into());
    }

    let items: Vec<CatResponseItem> = response.json().await?;
    let item = items.first().ok_or("No cat images returned from API")?;
    let cat_url = &item.url;
    info!("Cat image URL: {}", cat_url);

    // Download the cat image
    let img_request = Request::get(cat_url)
        .header("User-Agent", "dramma/0.4.1")
        .body(())?;

    let mut img_response = isahc::send_async(img_request).await?;
    if !img_response.status().is_success() {
        return Err(format!("Failed to download cat image: {}", img_response.status()).into());
    }

    let bytes = img_response.bytes().await?;
    info!("Cat image downloaded ({} bytes). Decoding...", bytes.len());

    // Decode and resize
    let dyn_img = image::load_from_memory(&bytes)?;
    let (width, height) = dyn_img.dimensions();

    // Scale to max width 500px to fit receipt canvas comfortably
    let target_width = 500;
    let scaled_img = if width > target_width {
        let ratio = target_width as f32 / width as f32;
        let target_height = (height as f32 * ratio) as u32;
        info!("Resizing cat image from {}x{} to {}x{}", width, height, target_width, target_height);
        dyn_img.resize(target_width, target_height, image::imageops::FilterType::Lanczos3)
    } else {
        dyn_img
    };

    Ok(scaled_img.to_rgb8())
}
