use http::Request;
use isahc::prelude::*;
use log::{error, info, warn};
use serde::Deserialize;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::error::RequestError;

#[derive(Debug, Clone, Deserialize)]
pub struct Fund {
    pub id: i32,
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_flexible_f64")]
    pub target_value: f64,
    #[serde(default)]
    pub target_currency: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FundDetailResponse {
    pub fund: FundDetail,
    #[serde(default)]
    pub donations: Vec<FundDonation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FundDetail {
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_flexible_f64")]
    pub target_value: f64,
    #[serde(default, deserialize_with = "deserialize_flexible_f64")]
    pub collected_value: f64,
    #[serde(default)]
    pub target_currency: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FundDonation {
    #[allow(dead_code)]
    pub rank: Option<i32>,
    #[allow(dead_code)]
    pub username: Option<String>,
    #[serde(default, deserialize_with = "deserialize_flexible_f64_opt")]
    pub converted_value: Option<f64>,
}

fn deserialize_flexible_f64<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Ok(s.parse::<f64>().unwrap_or(0.0)),
        _ => Ok(0.0),
    }
}

fn deserialize_flexible_f64_opt<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(deserializer)?;
    match v {
        serde_json::Value::Number(n) => Ok(n.as_f64()),
        serde_json::Value::String(s) => Ok(s.parse::<f64>().ok()),
        _ => Ok(None),
    }
}

#[derive(Debug, Clone)]
pub struct DetailedFund {
    pub id: i32,
    pub name: String,
    pub target_value: f64,
    pub collected_value: f64,
    pub target_currency: String,
    pub donations: Vec<FundDonation>,
}

/// Fetches available open funds from the API asynchronously
pub async fn fetch_open_funds(token: &str) -> Result<Vec<Fund>, RequestError> {
    let url = "https://gateway.hackem.cc/api/funds?status=open";

    info!("Fetching open funds from API...");

    let request = Request::get(url)
        .header("Authorization", format!("Bearer {}", token))
        .body(())?;

    let mut response = isahc::send_async(request).await?;

    let status = response.status();
    if status.is_success() {
        let funds: Vec<Fund> = response.json().await?;
        info!("✅ Fetched {} open funds", funds.len());
        Ok(funds)
    } else {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());

        error!("❌ API error {}: {}", status.as_u16(), message);
        Err(RequestError::Api {
            status: status.as_u16(),
            message,
        })
    }
}

/// Fetches single fund details from the API asynchronously
pub async fn fetch_fund_detail(
    token: &str,
    fund_id: i32,
) -> Result<FundDetailResponse, RequestError> {
    let url = format!("https://gateway.hackem.cc/api/funds/{}", fund_id);

    let request = Request::get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .body(())?;

    let mut response = isahc::send_async(request).await?;

    let status = response.status();
    if status.is_success() {
        let detail: FundDetailResponse = response.json().await?;
        Ok(detail)
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

/// Fetches all open funds and their detailed donation stats
pub async fn fetch_detailed_funds(token: &str) -> Result<Vec<DetailedFund>, RequestError> {
    let open_funds = fetch_open_funds(token).await?;

    let mut detailed_funds = Vec::with_capacity(open_funds.len());
    for fund in open_funds {
        match fetch_fund_detail(token, fund.id).await {
            Ok(detail) => {
                detailed_funds.push(DetailedFund {
                    id: fund.id,
                    name: detail.fund.name,
                    target_value: detail.fund.target_value,
                    collected_value: detail.fund.collected_value,
                    target_currency: detail.fund.target_currency,
                    donations: detail.donations,
                });
            }
            Err(e) => {
                warn!("⚠️ Could not fetch details for fund {}: {}", fund.id, e);
                detailed_funds.push(DetailedFund {
                    id: fund.id,
                    name: fund.name,
                    target_value: fund.target_value,
                    collected_value: 0.0,
                    target_currency: fund.target_currency,
                    donations: Vec::new(),
                });
            }
        }
    }

    Ok(detailed_funds)
}

const PALETTE: &[[u8; 4]] = &[
    [192, 41, 43, 255],   // Deep Red
    [231, 76, 60, 255],   // Coral Red
    [230, 126, 34, 255],  // Warm Orange
    [243, 156, 18, 255],  // Golden Amber
    [30, 55, 153, 255],   // Deep Navy Blue
    [41, 128, 185, 255],  // Cobalt Blue
    [52, 152, 219, 255],  // Sky Blue
    [0, 206, 201, 255],   // Turquoise / Cyan
    [39, 174, 96, 255],   // Forest Green
    [46, 204, 113, 255],  // Emerald Green
    [186, 220, 88, 255],  // Lime Green
    [246, 229, 141, 255], // Soft Yellow
    [109, 76, 65, 255],   // Cocoa Brown
    [108, 92, 231, 255],  // Lavender Violet
    [142, 68, 173, 255],  // Royal Purple
    [127, 140, 141, 255], // Steel Gray
];

pub fn render_donut_chart(
    target: f64,
    collected: f64,
    donations: &[FundDonation],
    width: u32,
    height: u32,
) -> Image {
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    let base = if target > 0.0 {
        target.max(collected)
    } else if collected > 0.0 {
        collected
    } else {
        1.0
    };

    let mut slices: Vec<(f64, f64, [u8; 4])> = Vec::new();
    let mut current_frac = 0.0;

    if !donations.is_empty() {
        for (i, d) in donations.iter().enumerate() {
            let val = d.converted_value.unwrap_or(0.0).max(0.0);
            if val <= 0.0 {
                continue;
            }
            let frac_len = val / base;
            let next_frac = (current_frac + frac_len).min(1.0);
            if next_frac > current_frac {
                slices.push((current_frac, next_frac, PALETTE[i % PALETTE.len()]));
                current_frac = next_frac;
            }
            if current_frac >= 1.0 {
                break;
            }
        }
    } else if collected > 0.0 {
        let frac_len = (collected / base).min(1.0);
        slices.push((0.0, frac_len, [46, 204, 113, 255]));
        current_frac = frac_len;
    }

    // Remained slice (uncollected portion of target)
    if current_frac < 1.0 {
        let remained_color = [44, 52, 64, 255]; // #2c3440
        slices.push((current_frac, 1.0, remained_color));
    }

    let r_outer = (width as f64) * 0.46;
    let r_inner = (width as f64) * 0.30;
    let r_outer_sq = r_outer * r_outer;
    let r_inner_sq = r_inner * r_inner;
    let cx = (width as f64) / 2.0;
    let cy = (height as f64) / 2.0;

    // 2x supersampling for smooth antialiased circles
    let samples = 2;
    let subpixel_step = 1.0 / (samples as f64);
    let num_subpixels = (samples * samples) as f64;

    for y in 0..height {
        for x in 0..width {
            let mut r_acc = 0.0;
            let mut g_acc = 0.0;
            let mut b_acc = 0.0;
            let mut a_acc = 0.0;

            for sy in 0..samples {
                for sx in 0..samples {
                    let px = (x as f64) + ((sx as f64) + 0.5) * subpixel_step;
                    let py = (y as f64) + ((sy as f64) + 0.5) * subpixel_step;

                    let dx = px - cx;
                    let dy = py - cy;
                    let dist_sq = dx * dx + dy * dy;

                    if dist_sq >= r_inner_sq && dist_sq <= r_outer_sq {
                        let mut angle = dy.atan2(dx) + std::f64::consts::FRAC_PI_2;
                        if angle < 0.0 {
                            angle += 2.0 * std::f64::consts::PI;
                        }
                        let frac = angle / (2.0 * std::f64::consts::PI);

                        let mut c = [0u8, 0, 0, 0];
                        for &(s_start, s_end, color) in &slices {
                            if frac >= s_start && frac < s_end {
                                c = color;
                                break;
                            }
                        }
                        if c[3] == 0 && !slices.is_empty() {
                            c = slices.last().unwrap().2;
                        }

                        // Thin dark separator line between adjacent slices
                        let arc_dist_to_boundary = slices
                            .iter()
                            .map(|s| (frac - s.1).abs())
                            .fold(1.0, f64::min);
                        let separator_threshold = 0.75 / (2.0 * std::f64::consts::PI * r_outer);
                        if slices.len() > 1 && arc_dist_to_boundary < separator_threshold {
                            c = [18, 20, 24, 255];
                        }

                        r_acc += c[0] as f64;
                        g_acc += c[1] as f64;
                        b_acc += c[2] as f64;
                        a_acc += c[3] as f64;
                    }
                }
            }

            let idx = ((y * width + x) * 4) as usize;
            pixels[idx] = (r_acc / num_subpixels).round() as u8;
            pixels[idx + 1] = (g_acc / num_subpixels).round() as u8;
            pixels[idx + 2] = (b_acc / num_subpixels).round() as u8;
            pixels[idx + 3] = (a_acc / num_subpixels).round() as u8;
        }
    }

    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels, width, height);
    Image::from_rgba8(buffer)
}

pub fn format_amount(val: f64) -> String {
    let int_val = val.round() as i64;
    let s = int_val.to_string();
    let mut result = String::new();
    let len = s.len();
    for (i, c) in s.chars().enumerate() {
        result.push(c);
        let rem = len - 1 - i;
        if rem > 0 && rem.is_multiple_of(3) {
            result.push(' ');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_amount() {
        assert_eq!(format_amount(0.0), "0");
        assert_eq!(format_amount(500.0), "500");
        assert_eq!(format_amount(15000.0), "15 000");
        assert_eq!(format_amount(473539.34), "473 539");
        assert_eq!(format_amount(800000.0), "800 000");
        assert_eq!(format_amount(1234567.0), "1 234 567");
    }

    #[test]
    fn test_flexible_f64_deserialization() {
        let json_with_strings = r#"{
            "fund": {
                "name": "Test Fund",
                "target_value": "800000",
                "collected_value": "473539.34",
                "target_currency": "AMD",
                "status": "open"
            },
            "donations": [
                { "rank": 1, "username": "alice", "converted_value": 40000 },
                { "rank": 2, "username": "bob", "converted_value": "20000" }
            ]
        }"#;

        let res: FundDetailResponse = serde_json::from_str(json_with_strings).unwrap();
        assert_eq!(res.fund.name, "Test Fund");
        assert_eq!(res.fund.target_value, 800000.0);
        assert!((res.fund.collected_value - 473539.34).abs() < 0.01);
        assert_eq!(res.donations.len(), 2);
        assert_eq!(res.donations[0].converted_value, Some(40000.0));
        assert_eq!(res.donations[1].converted_value, Some(20000.0));
    }

    #[test]
    fn test_render_donut_chart() {
        let donations = vec![
            FundDonation {
                rank: Some(1),
                username: Some("user1".into()),
                converted_value: Some(40000.0),
            },
            FundDonation {
                rank: Some(2),
                username: Some("user2".into()),
                converted_value: Some(20000.0),
            },
        ];
        let img = render_donut_chart(100000.0, 60000.0, &donations, 200, 200);
        let size = img.size();
        assert_eq!(size.width, 200);
        assert_eq!(size.height, 200);
    }
}
