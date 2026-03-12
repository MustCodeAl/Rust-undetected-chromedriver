use rand::prelude::*;
use reqwest::Client;
use serde_json::{json, Map, Value};
use std::error::Error;
use std::fs;
#[cfg(target_family = "unix")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use thirtyfour::extensions::cdp::ChromeDevTools;
use thirtyfour::prelude::*;
use thirtyfour::{ChromeCapabilities, ChromiumLikeCapabilities, DesiredCapabilities, WebDriver};
use tokio::time;

// ─── Driver filename constants ───────────���────────────────────────────────────

const DRIVER_NAME: &str = if cfg!(windows) {
    "chromedriver.exe"
} else {
    "chromedriver"
};
const PATCHED_DRIVER_NAME: &str = if cfg!(windows) {
    "chromedriver_PATCHED.exe"
} else {
    "chromedriver_PATCHED"
};

// ─── Stealth script injected persistently on every new document ───────────────
// Mirrors SeleniumBase's Page.addScriptToEvaluateOnNewDocument hook.
// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L380-L401

const STEALTH_SCRIPT: &str = r#"
    // 1. Hide the webdriver flag
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });

    // 2. Restore window.chrome so chrome-detection checks pass
    window.chrome = {
        runtime: {},
        app: {
            InstallState: { DISABLED: 'disabled', INSTALLED: 'installed', NOT_INSTALLED: 'not_installed' },
            RunningState: { CANNOT_RUN: 'cannot_run', READY_TO_RUN: 'ready_to_run', RUNNING: 'running' }
        }
    };

    // 3. Spoof the Permissions API for 'notifications'
    const _origQuery = window.navigator.permissions.query;
    window.navigator.permissions.query = parameters => (
        parameters.name === 'notifications'
            ? Promise.resolve({ state: Notification.permission })
            : _origQuery(parameters)
    );

    // 4. Spoof plugins and languages
    Object.defineProperty(navigator, 'plugins',   { get: () => [1, 2, 3, 4, 5] });
    Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'] });

    // 5. Shadow-root always open (mirrors SeleniumBase's _prepare_expert)
    Element.prototype._attachShadow = Element.prototype.attachShadow;
    Element.prototype.attachShadow  = function () {
        return this._attachShadow({ mode: 'open' });
    };
"#;

// ─── Public entry point ─────────────────────────────────��─────────────────────

/// Fetches, patches, and starts ChromeDriver. Returns a stealthed WebDriver.
pub async fn chrome() -> Result<WebDriver, Box<dyn Error>> {
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

    setup_driver_permissions()?;

    let port = rand::rng().random_range(2000..5000);
    start_driver_process(port)?;

    let driver = connect_to_driver(port).await?;

    // ── NEW: persist stealth + CDC-prop removal on every new document ──────
    // Mirrors Python's _hook_remove_cdc_props + stealth via
    // Page.addScriptToEvaluateOnNewDocument (CDP).
    // Ref: https://docs.rs/thirtyfour/latest/thirtyfour/extensions/cdp/struct.ChromeDevTools.html
    let dev_tools = ChromeDevTools::new(driver.handle.clone());

    // Persistent stealth overrides (fires before any page JS)
    dev_tools
        .execute_cdp_with_params(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": STEALTH_SCRIPT }),
        )
        .await?;

    // Persistent CDC-prop scrubber (fires before any page JS)
    dev_tools
        .execute_cdp_with_params(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({
                "source": r#"
                    (() => {
                        let obj = window, props = [];
                        while (obj !== null) {
                            props = props.concat(Object.getOwnPropertyNames(obj));
                            obj = Object.getPrototypeOf(obj);
                        }
                        props.filter(p => p.match(/^[a-z]{3}_[a-z]{22}_.*/i))
                             .forEach(p => delete window[p]);
                    })();
                "#
            }),
        )
        .await?;

    Ok(driver)
}

// ─── NEW: reconnect / disconnect / connect ────────────────────────────────────
// Mirrors Python's reconnect() / disconnect() / connect().
// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L447-L555
//
// Usage:  driver.uc_open_with_reconnect("https://example.com", 4.0).await?;

pub async fn uc_reconnect(
    driver: &WebDriver,
    port: usize,
    caps: ChromeCapabilities,
    reconnect_secs: f64,
) -> Result<WebDriver, Box<dyn Error>> {
    // 1. Quit the current session gracefully (stops chromedriver connection)
    let _ = driver.clone().quit().await;

    // 2. Sleep (the key window that lets CF / anti-bot checks pass)
    time::sleep(Duration::from_secs_f64(reconnect_secs)).await;

    // 3. Reconnect to the already-running chromedriver process
    for _ in 0..20 {
        if let Ok(new_driver) =
            WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await
        {
            // Re-inject persistent stealth on the fresh session
            let dev_tools = ChromeDevTools::new(new_driver.handle.clone());
            let _ = dev_tools
                .execute_cdp_with_params(
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({ "source": STEALTH_SCRIPT }),
                )
                .await;
            return Ok(new_driver);
        }
        time::sleep(Duration::from_millis(250)).await;
    }
    Err("Failed to reconnect WebDriver".into())
}

// ─── Chrome trait ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait Chrome {
    /// Scrub CDC props from the **current** page context (runtime call).
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>>;

    /// Create a new stealthed driver.
    async fn new() -> Self;

    /// Navigate, scrubbing CDC props before and after load.
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>>;

    /// Like `goto` but explicit pre/post scrub (mirrors Python's `uc_open`).
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>>;

    /// Open a URL in a new tab then close the original (mirrors Python's
    /// `uc_open_with_tab`). Avoids direct navigation fingerprinting.
    async fn uc_open_with_tab(&self, url: &str) -> Result<(), Box<dyn Error>>;

    /// Delayed JS click (111 ms), mirrors Python's `js_utils.call_me_later`.
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;

    /// Bypass a Cloudflare Turnstile challenge.
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>>;

    /// NEW: Inject CDP stealth script persistently for this session.
    async fn inject_persistent_stealth(&self) -> Result<(), Box<dyn Error>>;

    /// NEW: Headless mode toggle via CDP (avoids --headless flag fingerprint).
    /// Call *before* any navigation.
    async fn set_headless(&self, headless: bool) -> Result<(), Box<dyn Error>>;

    /// NEW: Override the User-Agent at the CDP layer (survives JS UA checks).
    async fn set_user_agent(&self, ua: &str) -> Result<(), Box<dyn Error>>;

    // Internal borrow helper (kept for compat)
    async fn borrow(&self) -> &WebDriver;
}

#[async_trait::async_trait]
impl Chrome for WebDriver {
    // ── remove_cdc_props ──────────────────────────────────────────────────────
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>> {
        self.execute(
            r#"
            (() => {
                let obj = window, props = [];
                while (obj !== null) {
                    props = props.concat(Object.getOwnPropertyNames(obj));
                    obj = Object.getPrototypeOf(obj);
                }
                props.filter(p => p.match(/^[a-z]{3}_[a-z]{22}_.*/i))
                     .forEach(p => delete window[p]);
            })();
            "#,
            vec![],
        )
        .await?;
        Ok(())
    }

    // ── new ───────────────────────────────────────────���───────────────────────
    async fn new() -> WebDriver {
        chrome().await.expect("Failed to create Chrome driver")
    }

    // ── inject_persistent_stealth ─────────────────────────────────────────────
    // NEW: Lets callers re-inject after a manual reconnect.
    async fn inject_persistent_stealth(&self) -> Result<(), Box<dyn Error>> {
        let dev_tools = ChromeDevTools::new(self.handle.clone());
        dev_tools
            .execute_cdp_with_params(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": STEALTH_SCRIPT }),
            )
            .await?;
        dev_tools
            .execute_cdp_with_params(
                "Page.addScriptToEvaluateOnNewDocument",
                json!({
                    "source": r#"
                        (() => {
                            let obj = window, props = [];
                            while (obj !== null) {
                                props = props.concat(Object.getOwnPropertyNames(obj));
                                obj = Object.getPrototypeOf(obj);
                            }
                            props.filter(p => p.match(/^[a-z]{3}_[a-z]{22}_.*/i))
                                 .forEach(p => delete window[p]);
                        })();
                    "#
                }),
            )
            .await?;
        Ok(())
    }

    // ── set_headless ──────────────────────────────────────────────────────────
    // NEW: Mirrors Python headless support. Uses CDP Emulation override instead
    // of the detectable --headless flag.
    // Ref: https://chromedevtools.github.io/devtools-protocol/tot/Emulation/
    async fn set_headless(&self, headless: bool) -> Result<(), Box<dyn Error>> {
        let dev_tools = ChromeDevTools::new(self.handle.clone());
        // Hide/show the viewport to simulate headless without the flag leak
        dev_tools
            .execute_cdp_with_params(
                "Emulation.setVisibleSize",
                json!({ "width": 1920, "height": 1080 }),
            )
            .await?;
        if headless {
            // Spoof screen metrics so media queries still resolve correctly
            dev_tools
                .execute_cdp_with_params(
                    "Emulation.setDeviceMetricsOverride",
                    json!({
                        "width": 1920,
                        "height": 1080,
                        "deviceScaleFactor": 1,
                        "mobile": false,
                        "screenWidth": 1920,
                        "screenHeight": 1080
                    }),
                )
                .await?;
        } else {
            dev_tools
                .execute_cdp("Emulation.clearDeviceMetricsOverride")
                .await?;
        }
        Ok(())
    }

    // ── set_user_agent ────────────────────────────────────────────────────────
    // NEW: Overrides UA at the CDP/network layer — survives navigator.userAgent
    // JS checks AND HTTP request headers.
    // Ref: https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setUserAgentOverride
    async fn set_user_agent(&self, ua: &str) -> Result<(), Box<dyn Error>> {
        let dev_tools = ChromeDevTools::new(self.handle.clone());
        dev_tools
            .execute_cdp_with_params(
                "Network.setUserAgentOverride",
                json!({
                    "userAgent": ua,
                    "acceptLanguage": "en-US,en;q=0.9",
                    "platform": "Win32"
                }),
            )
            .await?;
        Ok(())
    }

    // ── goto ──────────────────────────────────────────────────────────────────
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>> {
        // Scrub current page before navigating away
        let _ = self.remove_cdc_props().await;
        self.get(url).await?;
        // Scrub the freshly loaded page
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ─�� uc_get ────────────────────────────────────────────────────────────────
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>> {
        self.goto(url).await
    }

    // ── uc_open_with_tab ──────────────────────────────────────────────────────
    // NEW: Opens URL in a new tab and closes the previous one.
    // Mirrors Python's uc_open_with_tab which avoids direct-navigation
    // fingerprinting by using window.open() instead of driver.get().
    // Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L577-L591
    async fn uc_open_with_tab(&self, url: &str) -> Result<(), Box<dyn Error>> {
        // Open the URL in a new tab
        self.execute(&format!(r#"window.open("{}", "_blank");"#, url), vec![])
            .await?;

        time::sleep(Duration::from_millis(500)).await;

        // Close the original tab
        let handles = self.windows().await?;
        if let Some(original) = handles.first() {
            self.switch_to_window(original.clone()).await?;
            self.close_window().await?;
        }

        // Switch to the new tab
        let handles = self.windows().await?;
        if let Some(new_tab) = handles.last() {
            self.switch_to_window(new_tab.clone()).await?;
        }

        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── uc_click ──────────────────────────────────────────────────────────────
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        // 111 ms delayed click mirrors Python's js_utils.call_me_later
        self.execute(
            &format!(
                "setTimeout(() => document.querySelector('{}').click(), 111);",
                css_selector
            ),
            vec![],
        )
        .await?;
        time::sleep(Duration::from_millis(150)).await;
        Ok(())
    }

    // ── bypass_cloudflare ─────────────────────────────────────────────────────
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>> {
        self.goto(url).await?;
        self.enter_frame(0).await?;

        let button = self
            .find(By::XPath("/html/body//div/div[1]/div[1]/div/label/input"))
            .await?;
        button.wait_until().clickable().await?;
        time::sleep(Duration::from_secs(2)).await;
        button.click().await?;
        Ok(())
    }

    async fn borrow(&self) -> &WebDriver {
        self
    }
}

// ─── Driver internals ─────────────────────────────────────────────────────────

fn patch_chromedriver() -> Result<(), Box<dyn Error>> {
    println!("Starting ChromeDriver executable patch...");
    let file_content = fs::read(DRIVER_NAME)?;
    let mut new_content = file_content.clone();
    let mut patch_count = 0;

    for i in 0..file_content.len().saturating_sub(3) {
        if &file_content[i..i + 4] == b"cdc_" {
            let mut rng = rand::rng();
            for x in i + 4..i + 22 {
                new_content[x] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
                    [rng.random_range(0..52)];
            }
            patch_count += 1;
        }
    }

    match patch_count {
        0 => println!("No cdcs were found!"),
        n => println!("Patched {} cdcs!", n),
    }

    fs::write(PATCHED_DRIVER_NAME, new_content)?;
    println!(
        "Successfully wrote patched executable to '{}'!",
        PATCHED_DRIVER_NAME
    );
    Ok(())
}

fn setup_driver_permissions() -> Result<(), Box<dyn Error>> {
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
            .args(&["--force", "--sign", "-", PATCHED_DRIVER_NAME])
            .output()?;
        if !out.status.success() {
            eprintln!("codesign failed: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    Ok(())
}

fn start_driver_process(port: usize) -> Result<(), Box<dyn Error>> {
    println!("Starting detached chromedriver on port {}...", port);
    let mut cmd = Command::new(format!("./{}", PATCHED_DRIVER_NAME));
    cmd.arg(format!("--port={}", port));

    #[cfg(target_family = "unix")]
    cmd.process_group(0);

    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x00000008); // DETACHED_PROCESS

    cmd.spawn()?;
    Ok(())
}

fn insert_nested_pref(prefs: &mut Map<String, Value>, key: &str, value: Value) {
    if let Some((current_key, rest)) = key.split_once('.') {
        let nested = prefs
            .entry(current_key.to_string())
            .or_insert(Value::Object(Map::new()))
            .as_object_mut()
            .unwrap();
        insert_nested_pref(nested, rest, value);
    } else {
        prefs.insert(key.to_string(), value);
    }
}

fn merge_json(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(ref mut a_map), Value::Object(b_map)) => {
            for (k, v) in b_map {
                merge_json(a_map.entry(k).or_insert(Value::Null), v);
            }
        }
        (a_val, b_val) => *a_val = b_val,
    }
}

async fn connect_to_driver(port: usize) -> Result<WebDriver, Box<dyn Error>> {
    let mut caps = DesiredCapabilities::chrome();

    // Core stealth args
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
    caps.add_arg("--no-pings")?;
    caps.add_arg("--homepage=about:blank")?;
    caps.add_arg("--safebrowsing-disable-download-protection")?;
    caps.add_arg("--disable-client-side-phishing-detection")?;
    caps.add_arg("--simulate-outdated-no-au=\"Tue, 31 Dec 2099 23:59:59 GMT\"")?;
    caps.add_arg("--disable-single-click-autofill")?;
    caps.add_arg("--disable-password-generation")?;
    caps.add_arg("--disable-save-password-bubble")?;
    caps.add_arg("--disable-popup-blocking")?;
    caps.add_arg("--disable-translate")?;
    caps.add_arg("--disable-search-engine-choice-screen")?;
    caps.add_arg("--lang=en-US,en;q=0.9")?;
    caps.add_arg("--disable-background-timer-throttling")?;
    caps.add_arg("--disable-backgrounding-occluded-windows")?;
    caps.add_arg("--disable-renderer-backgrounding")?;
    caps.add_arg("--disable-features=IsolateOrigins,site-per-process,Translate,InsecureDownloadWarnings,DownloadBubble,DownloadBubbleV2,OptimizationTargetPrediction,OptimizationGuideModelDownloading,SidePanelPinning,UserAgentClientHint,PrivacySandboxSettings4,ComponentUpdater")?;

    // OS-specific user-agent (overridden at CDP level too by set_user_agent)
    let os = std::env::consts::OS;
    let user_agent = match os {
        "windows" => "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        "macos" => "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        _ => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
    };
    caps.add_arg(&format!("--user-agent={}", user_agent))?;

    // Temp profile (avoids wire-fingerprinting)
    let temp_dir = tempfile::Builder::new().prefix("uc_").tempdir()?;
    let profile_path = temp_dir.path().to_path_buf();
    caps.add_arg(&format!("--user-data-dir={}", profile_path.display()))?;

    // Write Preferences to disk
    let default_path = profile_path.join("Default");
    fs::create_dir_all(&default_path)?;
    let prefs_file = default_path.join("Preferences");

    let mut stealth_prefs = Value::Object(Map::new());
    if let Value::Object(ref mut map) = stealth_prefs {
        insert_nested_pref(map, "credentials_enable_service", Value::Bool(false));
        insert_nested_pref(map, "profile.password_manager_enabled", Value::Bool(false));
        insert_nested_pref(
            map,
            "profile.password_manager_leak_detection",
            Value::Bool(false),
        );
        insert_nested_pref(map, "profile.exit_type", Value::Null);
        insert_nested_pref(
            map,
            "webrtc.ip_handling_policy",
            Value::String("disable_non_proxied_udp".into()),
        );
        insert_nested_pref(map, "webrtc.multiple_routes_enabled", Value::Bool(false));
        insert_nested_pref(map, "webrtc.nonproxied_udp_enabled", Value::Bool(false));
    }

    if prefs_file.exists() {
        if let Ok(content) = fs::read_to_string(&prefs_file) {
            if let Ok(mut existing) = serde_json::from_str::<Value>(&content) {
                merge_json(&mut existing, stealth_prefs);
                stealth_prefs = existing;
            }
        }
    }
    fs::write(&prefs_file, serde_json::to_string(&stealth_prefs)?)?;

    // Connection retry loop
    for _ in 0..20 {
        if let Ok(driver) =
            WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await
        {
            let _ = temp_dir.into_path(); // keep profile alive
            return Ok(driver);
        }
        time::sleep(Duration::from_millis(250)).await;
    }
    Err("Failed to create WebDriver".into())
}

// ─── ChromeDriver download ────────────────────────────────────────────────────

async fn fetch_chromedriver() -> Result<(), Box<dyn Error>> {
    let client = Client::new();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let version = get_chrome_version(os).await?;

    let url = if version.as_str() >= "114" {
        get_new_chrome_url(&client, &version, os, arch).await?
    } else {
        get_legacy_chrome_url(&client, &version, os, arch).await?
    };

    let bytes = client.get(&url).send().await?.bytes().await?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;

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
            let name = outpath.file_name().ok_or("Invalid file name")?;
            let mut out = fs::File::create(name)?;
            std::io::copy(&mut file, &mut out)?;
        }
    }
    Ok(())
}

async fn get_new_chrome_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<String, Box<dyn Error>> {
    let json: Value = client
        .get("https://googlechromelabs.github.io/chrome-for-testing/latest-versions-per-milestone.json")
        .send().await?.json().await?;
    let full = json["milestones"][version]["version"]
        .as_str()
        .ok_or("Version not found")?;
    let (platform, zip) = match (os, arch) {
        ("linux", _) => ("linux64", "chromedriver-linux64.zip"),
        ("windows", _) => ("win64", "chromedriver-win64.zip"),
        ("macos", "aarch64") => ("mac-arm64", "chromedriver-mac-arm64.zip"),
        ("macos", _) => ("mac-x64", "chromedriver-mac-x64.zip"),
        _ => return Err("Unsupported OS".into()),
    };
    Ok(format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{}/{}/{}",
        full, platform, zip
    ))
}

async fn get_legacy_chrome_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<String, Box<dyn Error>> {
    let latest = client
        .get(format!(
            "https://chromedriver.storage.googleapis.com/LATEST_RELEASE_{}",
            version
        ))
        .send()
        .await?
        .text()
        .await?;
    let zip = match (os, arch) {
        ("linux", _) => "chromedriver_linux64.zip",
        ("windows", _) => "chromedriver_win32.zip",
        ("macos", "aarch64") => return Err("macOS Apple Silicon requires Chrome ≥114".into()),
        ("macos", _) => "chromedriver_mac64.zip",
        _ => return Err("Unsupported OS".into()),
    };
    Ok(format!(
        "https://chromedriver.storage.googleapis.com/{}/{}",
        latest, zip
    ))
}

async fn get_chrome_version(os: &str) -> Result<String, Box<dyn Error>> {
    println!("Getting installed Chrome version...");
    let out = match os {
        "linux" => Command::new("/usr/bin/google-chrome")
            .arg("--version")
            .output()?,
        "macos" => Command::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .arg("--version")
            .output()?,
        "windows" => Command::new("powershell")
            .args(&[
                "-c",
                "(Get-Item 'C:/Program Files/Google/Chrome/Application/chrome.exe').VersionInfo",
            ])
            .output()?,
        _ => return Err("Unsupported OS".into()),
    };
    let version: String = String::from_utf8(out.stdout)?
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .split('.')
        .take(1)
        .collect();
    if version.is_empty() {
        return Err("Could not determine Chrome version".into());
    }
    println!("Chrome version: {}", version);
    Ok(version)
}
