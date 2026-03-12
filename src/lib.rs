//! # undetected-chromedriver
//!
//! A Rust implementation of
//! [undetected-chromedriver](https://github.com/ultrafunkamsterdam/undetected-chromedriver)
//! built on top of [thirtyfour](https://github.com/stevepryde/thirtyfour).
//!
//! Provides stealth Chrome automation that bypasses common bot-detection
//! systems (Cloudflare, reCAPTCHA, DataDome, etc.) by patching the
//! chromedriver binary, injecting CDP stealth scripts, and spoofing
//! browser fingerprints.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use undetected_chromedriver::{chrome, Chrome};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let driver = chrome().await?;
//!     driver.goto("https://www.rust-lang.org/").await?;
//!     let title = driver.get_title().await?;
//!     println!("Title: {title}");
//!     driver.quit().await?;
//!     Ok(())
//! }
//! ```

mod chrome;
mod config;
mod driver;
mod error;
mod stealth;
mod utils;

// Re-export the public API
pub use chrome::{uc_open_with_reconnect, Chrome};
pub use config::ChromeConfig;
pub use driver::{chrome, chrome_with_config};
pub use error::{ChromeError, ChromeResult};
