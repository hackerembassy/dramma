use thiserror::Error;

#[derive(Debug, Error)]
pub enum RequestError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] isahc::Error),
    #[error("HTTP error: {0}")]
    Http(#[from] http::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("API returned error status {status}: {message}")]
    Api { status: u16, message: String },
}
