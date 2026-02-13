use http::Request;
use isahc::prelude::*;
use log::{error, info};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FundsError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] isahc::Error),
    #[error("HTTP error: {0}")]
    HttpError(#[from] http::Error),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("API returned error status {status}: {message}")]
    ApiError { status: u16, message: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fund {
    pub id: i32,
    pub name: String,
    #[allow(dead_code)]
    pub target_value: i32,
    #[allow(dead_code)]
    pub target_currency: String,
    #[allow(dead_code)]
    pub status: String,
}

/// Fetches available open funds from the API asynchronously
pub async fn fetch_funds(token: &str) -> Result<Vec<Fund>, FundsError> {
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
        Err(FundsError::ApiError {
            status: status.as_u16(),
            message,
        })
    }
}
