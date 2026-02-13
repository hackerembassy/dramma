use log::{error, info};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FundsError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("API returned error status {status}: {message}")]
    ApiError { status: u16, message: String },
}

#[derive(Debug, Clone, Deserialize)]
pub struct Fund {
    pub id: i32,
    pub name: String,
    pub target_value: i32,
    pub target_currency: String,
    pub status: String,
}

/// Fetches available open funds from the API
pub fn fetch_funds(token: &str) -> Result<Vec<Fund>, FundsError> {
    let url = "https://gateway.hackem.cc/api/funds?status=open";

    info!("Fetching open funds from API...");

    let client = reqwest::blocking::Client::new();
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {}", token))
        .send()?;

    let status = response.status();
    if status.is_success() {
        let funds: Vec<Fund> = response.json()?;
        info!("✅ Fetched {} open funds", funds.len());
        Ok(funds)
    } else {
        let message = response
            .text()
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("❌ API error {}: {}", status.as_u16(), message);
        Err(FundsError::ApiError {
            status: status.as_u16(),
            message,
        })
    }
}
