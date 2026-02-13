use http::Request;
use isahc::prelude::*;
use log::{error, info};
use serde::Serialize;

use crate::error::RequestError;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DonationRequest {
    username: String,
    amount: i32,
    currency: String,
    post_chat: String,
}

/// Sends a donation to the API asynchronously
pub async fn send_donation(
    token: &str,
    fund_id: i32,
    username: &str,
    amount: i32,
) -> Result<(), RequestError> {
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

    let body = serde_json::to_vec(&request_body)?;

    let request = Request::post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(body)?;

    let mut response = isahc::send_async(request).await?;

    let status = response.status();
    if status.is_success() {
        info!("✅ Donation sent successfully!");
        Ok(())
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
