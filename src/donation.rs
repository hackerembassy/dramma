use log::{error, info};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DonationError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("API returned error status {status}: {message}")]
    ApiError { status: u16, message: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DonationRequest {
    username: String,
    amount: i32,
    currency: String,
    post_chat: String,
}

/// Sends a donation to the API
pub fn send_donation(
    token: &str,
    fund_id: i32,
    username: &str,
    amount: i32,
) -> Result<(), DonationError> {
    let url = format!("https://gateway.hackem.cc/api/funds/{}/donations", fund_id);

    let request_body = DonationRequest {
        username: username.to_string(),
        amount,
        currency: "AMD".to_string(),
        post_chat: "main".to_string(),
    };

    info!(
        "Sending donation: {} AMD from {} to fund {}",
        amount, username, fund_id
    );

    let client = reqwest::blocking::Client::new();
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .json(&request_body)
        .send()?;

    let status = response.status();
    if status.is_success() {
        info!("✅ Donation sent successfully!");
        Ok(())
    } else {
        let message = response
            .text()
            .unwrap_or_else(|_| "Unknown error".to_string());
        error!("❌ API error {}: {}", status.as_u16(), message);
        Err(DonationError::ApiError {
            status: status.as_u16(),
            message,
        })
    }
}
