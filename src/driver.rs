//! Driver lifecycle: fetch, patch, launch, and connect to chromedriver.

use crate::config::{
    ChromeConfig, AD_BLOCK_PATTERNS, BROWSER_PERMISSIONS, DRIVER_NAME, PATCHED_DRIVER_NAME,
};
use crate::error::ChromeError;
use crate::stealth::inject_all_persistent_stealth;
use crate::utils::{insert_nested_pref, merge_json};

use base64::{engine::general_purpose, Engine};
use rand::prelude::*;
use reqwest::Client;
use serde_json::{json, Map, Value};
use std::fs;
#[cfg(target_family = "unix")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use thirtyfour::extensions::cdp::ChromeDevTools;
use thirtyfour::{ChromiumLikeCapabilities, DesiredCapabilities, WebDriver};

// ─── Public entry points ──────────────────────────────────────────────────────

/// Full-featured entry point with config.
///
/// Downloads/patches chromedriver if needed, starts the process, connects,
/// and applies all stealth hooks and CDP overrides from the supplied config.
///
/// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/cdp_util.py#L278-L294>
pub async fn chrome_with_config(config: ChromeConfig) -> Result<WebDriver, ChromeError> {
    ensure_driver_binary().await?;
    setup_driver_permissions()?;

    let port = rand::rng().random_range(2000..5000);
    start_driver_process(port)?;

    let driver = connect_to_driver(port, &config).await?;
    let dt = ChromeDevTools::new(driver.handle.clone());

    inject_all_persistent_stealth(&driver).await?;
    grant_startup_permissions(&dt).await;
    apply_cdp_overrides(&dt, &config).await;

    Ok(driver)
}

/// Zero-config entry point — backward-compatible convenience wrapper.
pub async fn chrome() -> Result<WebDriver, ChromeError> {
    chrome_with_config(ChromeConfig::default()).await
}

// ─── Startup helpers ──────────────────────────────────────────────────────────

/// Download and patch chromedriver binary if either is missing.
async fn ensure_driver_binary() -> Result<(), ChromeError> {
    if !Path::new(DRIVER_NAME).exists() {
        println!("ChromeDriver does not exist! Fetching...");
        fetch_chromedriver().await?;
    } else {
        println!("ChromeDriver already exists!");
    }
    if !Path::new(PATCHED_DRIVER_NAME).exists() {
        patch_chromedriver()?;
    } else {
        println!("Detected patched chromedriver executable!");
    }
    Ok(())
}

/// Grant all permissions so prompts never fire (a bot-detection signal).
///
/// Ref: <https://chromedevtools.github.io/devtools-protocol/tot/Browser/#method-grantPermissions>
async fn grant_startup_permissions(dt: &ChromeDevTools) {
    let _ = dt
        .execute_cdp_with_params(
            "Browser.grantPermissions",
            json!({ "permissions": BROWSER_PERMISSIONS }),
        )
        .await;
}

/// Apply optional CDP overrides from config (mobile, ad-block, CSP, proxy).
async fn apply_cdp_overrides(dt: &ChromeDevTools, config: &ChromeConfig) {
    // Mobile device metrics
    // Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L6058-L6074
    if config.mobile {
        let (w, h, dpr) = config.mobile_metrics.unwrap_or((412, 732, 3.0));
        let _ = dt
            .execute_cdp_with_params(
                "Emulation.setDeviceMetricsOverride",
                json!({"width":w,"height":h,"deviceScaleFactor":dpr,"mobile":true}),
            )
            .await;
        let _ = dt
            .execute_cdp_with_params(
                "Emulation.setTouchEmulationEnabled",
                json!({"enabled":true,"maxTouchPoints":5}),
            )
            .await;
        if let Some(ua) = &config.mobile_user_agent {
            let _ = dt
                .execute_cdp_with_params("Network.setUserAgentOverride", json!({"userAgent": ua}))
                .await;
        }
    }

    // Ad/tracker URL blocking
    if config.ad_block {
        let _ = dt.execute_cdp("Network.enable").await;
        let _ = dt
            .execute_cdp_with_params("Network.setBlockedURLs", json!({"urls": AD_BLOCK_PATTERNS}))
            .await;
    }

    // Bypass CSP
    if config.disable_csp {
        let _ = dt
            .execute_cdp_with_params("Page.setBypassCSP", json!({"enabled": true}))
            .await;
    }

    // Authenticated proxy header injection
    if let Some(ref proxy) = config.proxy {
        if let Some(creds) = proxy.split('@').next().filter(|_| proxy.contains('@')) {
            let encoded = general_purpose::STANDARD.encode(creds);
            let _ = dt.execute_cdp("Network.enable").await;
            let _ = dt
                .execute_cdp_with_params(
                    "Network.setExtraHTTPHeaders",
                    json!({"headers": {"Proxy-Authorization": format!("Basic {encoded}")}}),
                )
                .await;
        }
    }
}

// ─── ChromeDriver connection ──────────────────────────────────────────────────

/// Build capabilities and connect to a running chromedriver instance.
async fn connect_to_driver(port: usize, config: &ChromeConfig) -> Result<WebDriver, ChromeError> {
    let mut caps = DesiredCapabilities::chrome();

    add_stealth_args(&mut caps)?;
    add_user_agent_arg(&mut caps)?;
    add_ui_suppression_args(&mut caps)?;
    add_language_arg(&mut caps, config)?;
    add_disable_features_arg(&mut caps)?;
    add_anti_throttling_args(&mut caps)?;
    add_headless_args(&mut caps, config)?;
    add_proxy_arg(&mut caps, config)?;
    add_extension_args(&mut caps, config)?;
    let _profile_path = write_stealth_profile(&mut caps)?;

    // Retry loop — chromedriver needs a moment after spawning
    for _ in 0..20 {
        if let Ok(driver) = WebDriver::new(&format!("http://localhost:{port}"), caps.clone()).await
        {
            return Ok(driver);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(ChromeError::Other(
        "Failed to create WebDriver after 20 attempts".into(),
    ))
}

fn add_stealth_args(caps: &mut thirtyfour::ChromeCapabilities) -> Result<(), ChromeError> {
    caps.set_no_sandbox()?;
    caps.set_disable_dev_shm_usage()?;
    caps.add_arg("--disable-blink-features=AutomationControlled")?;
    caps.add_arg("window-size=960,540")?;
    caps.add_arg("--disable-infobars")?;
    caps.add_arg("--no-default-browser-check")?;
    caps.add_arg("--no-first-run")?;
    caps.add_arg("--no-service-autorun")?;
    caps.add_arg("--password-store=basic")?;
    caps.add_arg("--profile-directory=Default")?;

    // Stop phoning home
    caps.add_arg("--no-pings")?;
    caps.add_arg("--homepage=about:blank")?;
    caps.add_arg("--safebrowsing-disable-download-protection")?;
    caps.add_arg("--disable-client-side-phishing-detection")?;
    caps.add_arg("--simulate-outdated-no-au=\"Tue, 31 Dec 2099 23:59:59 GMT\"")?;
    caps.add_arg("--disable-single-click-autofill")?;
    caps.add_arg("--disable-password-generation")?;
    caps.add_arg("--disable-save-password-bubble")?;
    Ok(())
}

fn add_user_agent_arg(caps: &mut thirtyfour::ChromeCapabilities) -> Result<(), ChromeError> {
    let ua = match std::env::consts::OS {
        "windows" => "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        "macos"   => "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        _         => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
    };
    caps.add_arg(&format!("--user-agent={ua}"))?;
    Ok(())
}

fn add_ui_suppression_args(caps: &mut thirtyfour::ChromeCapabilities) -> Result<(), ChromeError> {
    caps.add_arg("--disable-popup-blocking")?;
    caps.add_arg("--disable-translate")?;
    caps.add_arg("--disable-search-engine-choice-screen")?;
    caps.add_arg("--enable-unsafe-extension-debugging")?;
    Ok(())
}

fn add_language_arg(
    caps: &mut thirtyfour::ChromeCapabilities,
    config: &ChromeConfig,
) -> Result<(), ChromeError> {
    let lang = config.lang.as_deref().unwrap_or("en-US,en;q=0.9");
    caps.add_arg(&format!("--lang={lang}"))?;
    Ok(())
}

fn add_disable_features_arg(caps: &mut thirtyfour::ChromeCapabilities) -> Result<(), ChromeError> {
    caps.add_arg(
        "--disable-features=IsolateOrigins,site-per-process,Translate,\
        InsecureDownloadWarnings,DownloadBubble,DownloadBubbleV2,\
        OptimizationTargetPrediction,OptimizationGuideModelDownloading,\
        SafeBrowsingEnhancedProtection,PrivacySandboxSettings4,\
        AutofillEnableAccountWalletStorage",
    )?;
    Ok(())
}

fn add_anti_throttling_args(caps: &mut thirtyfour::ChromeCapabilities) -> Result<(), ChromeError> {
    caps.add_arg("--disable-background-timer-throttling")?;
    caps.add_arg("--disable-backgrounding-occluded-windows")?;
    caps.add_arg("--disable-renderer-backgrounding")?;
    Ok(())
}

fn add_headless_args(
    caps: &mut thirtyfour::ChromeCapabilities,
    config: &ChromeConfig,
) -> Result<(), ChromeError> {
    if config.headless {
        caps.add_arg("--headless=new")?;
        caps.add_arg("--window-size=1920,1080")?;
    }
    Ok(())
}

fn add_proxy_arg(
    caps: &mut thirtyfour::ChromeCapabilities,
    config: &ChromeConfig,
) -> Result<(), ChromeError> {
    if let Some(proxy) = &config.proxy {
        let host_port = if proxy.contains('@') {
            proxy.split('@').next_back().unwrap_or(proxy)
        } else {
            proxy
        };
        caps.add_arg(&format!("--proxy-server={host_port}"))?;
    }
    Ok(())
}

fn add_extension_args(
    caps: &mut thirtyfour::ChromeCapabilities,
    config: &ChromeConfig,
) -> Result<(), ChromeError> {
    if !config.extensions.is_empty() {
        let paths = config
            .extensions
            .iter()
            .map(|p| {
                fs::canonicalize(p)
                    .unwrap_or_else(|_| p.into())
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(",");
        caps.add_arg(&format!("--load-extension={paths}"))?;
    }
    Ok(())
}

/// Create a temp Chrome profile with stealth preferences, return the path
/// (ownership is deliberately leaked so Chrome can keep using it).
fn write_stealth_profile(
    caps: &mut thirtyfour::ChromeCapabilities,
) -> Result<std::path::PathBuf, ChromeError> {
    let temp_dir = tempfile::Builder::new().prefix("uc_").tempdir()?;
    let profile_path = temp_dir.path().to_path_buf();
    caps.add_arg(&format!("--user-data-dir={}", profile_path.display()))?;

    let default_path = profile_path.join("Default");
    fs::create_dir_all(&default_path)?;
    let prefs_file = default_path.join("Preferences");

    let mut prefs = Map::new();
    insert_nested_pref(&mut prefs, "credentials_enable_service", Value::Bool(false));
    insert_nested_pref(
        &mut prefs,
        "profile.password_manager_enabled",
        Value::Bool(false),
    );
    insert_nested_pref(
        &mut prefs,
        "profile.password_manager_leak_detection",
        Value::Bool(false),
    );
    insert_nested_pref(&mut prefs, "profile.exit_type", Value::Null);
    insert_nested_pref(
        &mut prefs,
        "webrtc.ip_handling_policy",
        Value::String("disable_non_proxied_udp".into()),
    );
    insert_nested_pref(
        &mut prefs,
        "webrtc.multiple_routes_enabled",
        Value::Bool(false),
    );
    insert_nested_pref(
        &mut prefs,
        "webrtc.nonproxied_udp_enabled",
        Value::Bool(false),
    );

    let mut stealth_prefs = Value::Object(prefs);

    if prefs_file.exists() {
        if let Ok(content) = fs::read_to_string(&prefs_file) {
            if let Ok(mut existing) = serde_json::from_str::<Value>(&content) {
                merge_json(&mut existing, stealth_prefs);
                stealth_prefs = existing;
            }
        }
    }
    fs::write(&prefs_file, serde_json::to_string(&stealth_prefs)?)?;

    // Prevent temp dir cleanup — Chrome needs the profile directory
    let _ = temp_dir.keep();
    Ok(profile_path)
}

// ─── Binary patching ──────────────────────────────────────────────────────────

/// Replace `cdc_` marker bytes in the chromedriver binary with random chars.
fn patch_chromedriver() -> Result<(), ChromeError> {
    println!("Starting ChromeDriver executable patch...");
    let file_content = fs::read(DRIVER_NAME)?;
    let mut new_content = file_content.clone();
    let mut patch_count = 0u32;

    for i in 0..file_content.len().saturating_sub(3) {
        if file_content[i..i + 4] == *b"cdc_" {
            let mut rng = rand::rng();
            let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
            for byte in &mut new_content[i + 4..i + 22] {
                *byte = alphabet[rng.random_range(0..alphabet.len())];
            }
            patch_count += 1;
        }
    }

    if patch_count > 0 {
        println!("Patched {patch_count} cdcs!");
    } else {
        println!("No cdcs were found!");
    }
    fs::write(PATCHED_DRIVER_NAME, new_content)?;
    println!("Successfully wrote patched executable to '{PATCHED_DRIVER_NAME}'!");
    Ok(())
}

/// Set executable permission and codesign on macOS.
fn setup_driver_permissions() -> Result<(), ChromeError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(PATCHED_DRIVER_NAME)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(PATCHED_DRIVER_NAME, perms)?;
    }
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("codesign")
            .args(["--force", "--sign", "-", PATCHED_DRIVER_NAME])
            .output()?;
        if !out.status.success() {
            eprintln!("codesign failed: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    Ok(())
}

/// Spawn chromedriver as a detached background process.
fn start_driver_process(port: usize) -> Result<(), ChromeError> {
    println!("Starting detached chromedriver on port {port}...");
    let mut cmd = Command::new(format!("./{PATCHED_DRIVER_NAME}"));
    cmd.arg(format!("--port={port}"));
    #[cfg(target_family = "unix")]
    cmd.process_group(0);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x00000008);
    cmd.spawn()?;
    Ok(())
}

// ─── ChromeDriver download ───────────────────────────────────────────────────

/// Fetch and extract the correct chromedriver binary for the current platform.
async fn fetch_chromedriver() -> Result<(), ChromeError> {
    let client = Client::new();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let version = get_chrome_version(os)?;

    let url = if version.as_str() >= "114" {
        get_new_chrome_url(&client, &version, os, arch).await?
    } else {
        get_legacy_chrome_url(&client, &version, os, arch).await?
    };

    let resp = client.get(&url).send().await?.bytes().await?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(resp))?;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = file.mangled_name();
        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    fs::create_dir_all(p)?;
                }
            }
            let mut outfile = fs::File::create(outpath.file_name().ok_or("bad zip filename")?)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }
    Ok(())
}

/// Resolve download URL for Chrome ≥ 114 via the JSON endpoint.
async fn get_new_chrome_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<String, ChromeError> {
    let json: Value = client
        .get("https://googlechromelabs.github.io/chrome-for-testing/latest-versions-per-milestone.json")
        .send()
        .await?
        .json()
        .await?;

    let full = json["milestones"][version]["version"]
        .as_str()
        .ok_or("Version not found in milestone JSON")?;

    let (platform, zip_name) = match (os, arch) {
        ("linux", _) => ("linux64", "chromedriver-linux64.zip"),
        ("windows", _) => ("win64", "chromedriver-win64.zip"),
        ("macos", "aarch64") => ("mac-arm64", "chromedriver-mac-arm64.zip"),
        ("macos", _) => ("mac-x64", "chromedriver-mac-x64.zip"),
        _ => return Err("Unsupported OS/arch combination".into()),
    };
    Ok(format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{full}/{platform}/{zip_name}"
    ))
}

/// Resolve download URL for Chrome < 114 via the legacy LATEST_RELEASE endpoint.
async fn get_legacy_chrome_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<String, ChromeError> {
    let latest = client
        .get(format!(
            "https://chromedriver.storage.googleapis.com/LATEST_RELEASE_{version}"
        ))
        .send()
        .await?
        .text()
        .await?;

    let zip_name = match (os, arch) {
        ("linux", _) => "chromedriver_linux64.zip",
        ("windows", _) => "chromedriver_win32.zip",
        ("macos", "aarch64") => return Err("macOS Apple Silicon + Chrome <114 unsupported".into()),
        ("macos", _) => "chromedriver_mac64.zip",
        _ => return Err("Unsupported OS/arch combination".into()),
    };
    Ok(format!(
        "https://chromedriver.storage.googleapis.com/{latest}/{zip_name}"
    ))
}

/// Detect the installed Chrome major version via platform-specific commands.
fn get_chrome_version(os: &str) -> Result<String, ChromeError> {
    println!("Getting installed Chrome version...");
    let output = match os {
        "linux" => Command::new("/usr/bin/google-chrome")
            .arg("--version")
            .output()?,
        "macos" => Command::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .arg("--version")
            .output()?,
        "windows" => Command::new("powershell")
            .args([
                "-c",
                "(Get-Item 'C:/Program Files/Google/Chrome/Application/chrome.exe').VersionInfo",
            ])
            .output()?,
        _ => return Err("Unsupported OS for Chrome version detection".into()),
    };
    let version: String = String::from_utf8(output.stdout)?
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .split('.')
        .take(1)
        .collect();
    if version.is_empty() {
        return Err("Could not determine Chrome version".into());
    }
    println!("Installed Chrome version: {version}");
    Ok(version)
}
