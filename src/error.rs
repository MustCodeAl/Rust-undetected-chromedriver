/// Errors produced by the undetected-chromedriver library.
use thiserror::Error;

/// Unified error type for all fallible operations in this crate.
#[derive(Debug, Error)]
pub enum ChromeError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WebDriver error: {0}")]
    WebDriver(#[from] thirtyfour::error::WebDriverError),

    #[error("HTTP request error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("ZIP extraction error: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("Base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("UTF-8 decode error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("{0}")]
    Other(String),
}

impl From<&str> for ChromeError {
    fn from(s: &str) -> Self {
        Self::Other(s.to_string())
    }
}

impl From<String> for ChromeError {
    fn from(s: String) -> Self {
        Self::Other(s)
    }
}

/// Convenience alias used throughout the crate.
pub type ChromeResult<T> = Result<T, ChromeError>;
