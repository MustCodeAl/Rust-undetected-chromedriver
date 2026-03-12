use base64::{engine::general_purpose, Engine};
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
use thirtyfour::{ChromiumLikeCapabilities, DesiredCapabilities, WebDriver};
use tokio::time;

// ─── Driver filename constants ────────────────────────────────────────────────

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

// ─── Ad-block URL patterns ────────────────────────────────────────────────────
// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454

const AD_BLOCK_PATTERNS: &[&str] = &[
    "*.googlesyndication.com*",
    "*.googletagmanager.com*",
    "*.google-analytics.com*",
    "*.amazon-adsystem.com*",
    "*.adsafeprotected.com*",
    "*.doubleclick.net*",
    "*.fastclick.net*",
    "*.snigelweb.com*",
    "*.2mdn.net*",
    "*.casalemedia.com*",
    "*.admanmedia.com*",
    "*.quantserve.com*",
    "*.bidswitch.net*",
    "*.360yield.com*",
    "*.adthrive.com*",
    "*.pubmatic.com*",
    "*.id5-sync.com*",
    "*.moatads.com*",
    "*.dotomi.com*",
    "*.adsrvr.org*",
    "*.adnxs.com*",
    "*.openx.net*",
    "*.tapad.com*",
    "*.3lift.com*",
];

// ─── Persistent CDP stealth scripts ──────────────────────────────────────────
// Registered via Page.addScriptToEvaluateOnNewDocument — fires before ANY page
// JS on every navigation, including cross-origin ones.

/// Hides webdriver, restores window.chrome, spoofs permissions/plugins/languages,
/// and forces shadow roots open.
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L380-L401
const STEALTH_SCRIPT: &str = r#"
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
    window.chrome = {
        runtime: {},
        app: {
            InstallState: { DISABLED:'disabled', INSTALLED:'installed', NOT_INSTALLED:'not_installed' },
            RunningState: { CANNOT_RUN:'cannot_run', READY_TO_RUN:'ready_to_run', RUNNING:'running' }
        }
    };
    const _origQuery = window.navigator.permissions.query;
    window.navigator.permissions.query = p => (
        p.name === 'notifications'
            ? Promise.resolve({ state: Notification.permission })
            : _origQuery(p)
    );
    Object.defineProperty(navigator, 'plugins',   { get: () => [1,2,3,4,5] });
    Object.defineProperty(navigator, 'languages', { get: () => ['en-US','en'] });
    Element.prototype._attachShadow = Element.prototype.attachShadow;
    Element.prototype.attachShadow  = function() { return this._attachShadow({ mode:'open' }); };
"#;

/// Scrubs all chromedriver CDC artefact properties from window.
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L388-L394
const CDC_SCRUB_SCRIPT: &str = r#"
    (() => {
        let o = window, p = [];
        while (o !== null) { p = p.concat(Object.getOwnPropertyNames(o)); o = Object.getPrototypeOf(o); }
        p.filter(x => x.match(/^[a-z]{3}_[a-z]{22}_.*/i)).forEach(x => delete window[x]);
    })();
"#;

/// Canvas noise, AudioContext noise, WebGL vendor/renderer spoof,
/// hardwareConcurrency, deviceMemory, and screen metrics normalization.
const ADVANCED_FINGERPRINT_SCRIPT: &str = r#"
    (() => {
        const _toDataURL = HTMLCanvasElement.prototype.toDataURL;
        HTMLCanvasElement.prototype.toDataURL = function(type, q) {
            const ctx = this.getContext('2d');
            if (ctx) {
                const id = ctx.getImageData(0,0,this.width,this.height);
                for (let i=0;i<id.data.length;i+=4){
                    id.data[i]  +=Math.floor(Math.random()*2);
                    id.data[i+1]+=Math.floor(Math.random()*2);
                    id.data[i+2]+=Math.floor(Math.random()*2);
                }
                ctx.putImageData(id,0,0);
            }
            return _toDataURL.apply(this,arguments);
        };
        const AC = window.AudioContext||window.webkitAudioContext;
        if (AC) {
            const _ca = AC.prototype.createAnalyser;
            AC.prototype.createAnalyser = function() {
                const n=_ca.apply(this,arguments);
                const _g=n.getFloatFrequencyData.bind(n);
                n.getFloatFrequencyData=function(a){_g(a);for(let i=0;i<a.length;i++)a[i]+=(Math.random()*0.0002)-0.0001;};
                return n;
            };
        }
        const _gp=WebGLRenderingContext.prototype.getParameter;
        WebGLRenderingContext.prototype.getParameter=function(p){
            if(p===37445)return 'Intel Inc.';
            if(p===37446)return 'Intel Iris OpenGL Engine';
            return _gp.apply(this,arguments);
        };
        if(typeof WebGL2RenderingContext!=='undefined'){
            const _gp2=WebGL2RenderingContext.prototype.getParameter;
            WebGL2RenderingContext.prototype.getParameter=function(p){
                if(p===37445)return 'Intel Inc.';
                if(p===37446)return 'Intel Iris OpenGL Engine';
                return _gp2.apply(this,arguments);
            };
        }
        Object.defineProperty(navigator,'hardwareConcurrency',{get:()=>8});
        Object.defineProperty(navigator,'deviceMemory',       {get:()=>8});
        Object.defineProperty(screen,'width',      {get:()=>1920});
        Object.defineProperty(screen,'height',     {get:()=>1080});
        Object.defineProperty(screen,'availWidth', {get:()=>1920});
        Object.defineProperty(screen,'availHeight',{get:()=>1040});
        Object.defineProperty(screen,'colorDepth', {get:()=>24});
        Object.defineProperty(screen,'pixelDepth', {get:()=>24});
    })();
"#;

// ─── ChromeConfig ────────────��────────────────────────────────────────────────

/// Builder-style config for [`chrome_with_config`].
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/cdp_util.py#L278-L294
#[derive(Debug, Clone, Default)]
pub struct ChromeConfig {
    /// Run in `--headless=new` mode (Chrome ≥ 112).
    pub headless: bool,
    /// Enable mobile device emulation. Default metrics: 412×732 @ 3×.
    pub mobile: bool,
    /// Override mobile metrics: `(css_width, css_height, pixel_ratio)`.
    pub mobile_metrics: Option<(u32, u32, f64)>,
    /// Override mobile user-agent string.
    pub mobile_user_agent: Option<String>,
    /// Block ad/tracker URLs via `Network.setBlockedURLs` on startup.
    pub ad_block: bool,
    /// Bypass Content Security Policy (`Page.setBypassCSP`).
    pub disable_csp: bool,
    /// Proxy: `"host:port"` or `"user:pass@host:port"`.
    pub proxy: Option<String>,
    /// Language locale, e.g. `"en-US"`.
    pub lang: Option<String>,
    /// Paths to unpacked extension dirs loaded via `--load-extension`.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L2240
    pub extensions: Vec<String>,
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

async fn inject_all_persistent_stealth(driver: &WebDriver) -> Result<(), Box<dyn Error>> {
    let dt = ChromeDevTools::new(driver.handle.clone());
    for src in [
        STEALTH_SCRIPT,
        CDC_SCRUB_SCRIPT,
        ADVANCED_FINGERPRINT_SCRIPT,
    ] {
        dt.execute_cdp_with_params(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": src }),
        )
        .await?;
    }
    Ok(())
}

fn insert_nested_pref(prefs: &mut Map<String, Value>, key: &str, value: Value) {
    if let Some((cur, rest)) = key.split_once('.') {
        let nested = prefs
            .entry(cur.to_string())
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
        (Value::Object(ref mut am), Value::Object(bm)) => {
            for (k, v) in bm {
                merge_json(am.entry(k).or_insert(Value::Null), v);
            }
        }
        (av, bv) => *av = bv,
    }
}

// ─── Public entry points ──────────────────────────────────────────────────────

/// Full-featured entry point with config.
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/cdp_util.py#L278-L294
pub async fn chrome_with_config(config: ChromeConfig) -> Result<WebDriver, Box<dyn Error>> {
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
    let driver = connect_to_driver(port, &config).await?;
    let dt = ChromeDevTools::new(driver.handle.clone());

    // Persistent stealth hooks fire before every page's own JS
    inject_all_persistent_stealth(&driver).await?;

    // Grant all permissions — prompts are bot-detection signals
    // Ref: https://chromedevtools.github.io/devtools-protocol/tot/Browser/#method-grantPermissions
    let _ = dt
        .execute_cdp_with_params(
            "Browser.grantPermissions",
            json!({
                "permissions": [
                    "geolocation","notifications","audioCapture","videoCapture",
                    "clipboardReadWrite","clipboardSanitizedWrite","midi","midiSysex",
                    "sensors","backgroundSync","backgroundFetch","nfc","displayCapture",
                    "storageAccess","protectedMediaIdentifier","idleDetection"
                ]
            }),
        )
        .await;

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
    // Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454
    if config.ad_block {
        let _ = dt.execute_cdp("Network.enable").await;
        let _ = dt
            .execute_cdp_with_params("Network.setBlockedURLs", json!({"urls": AD_BLOCK_PATTERNS}))
            .await;
    }

    // Bypass CSP
    // Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setBypassCSP
    if config.disable_csp {
        let _ = dt
            .execute_cdp_with_params("Page.setBypassCSP", json!({"enabled": true}))
            .await;
    }

    // Authenticated proxy header injection
    if let Some(ref proxy) = config.proxy {
        if proxy.contains('@') {
            let creds = proxy.split('@').next().unwrap_or("");
            let encoded = general_purpose::STANDARD.encode(creds);
            let _ = dt.execute_cdp("Network.enable").await;
            let _ = dt
                .execute_cdp_with_params(
                    "Network.setExtraHTTPHeaders",
                    json!({"headers": {"Proxy-Authorization": format!("Basic {}", encoded)}}),
                )
                .await;
        }
    }

    Ok(driver)
}

/// Zero-config entry point — backward-compatible with all existing callers.
pub async fn chrome() -> Result<WebDriver, Box<dyn Error>> {
    chrome_with_config(ChromeConfig::default()).await
}

/// Canonical CF/bot bypass: open URL in new tab → quit → sleep → reconnect.
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L587-L633
pub async fn uc_open_with_reconnect(
    driver: &WebDriver,
    url: &str,
    port: usize,
    caps: thirtyfour::ChromeCapabilities,
    reconnect_secs: f64,
) -> Result<WebDriver, Box<dyn Error>> {
    let _ = driver
        .execute(&format!(r#"window.open("{}","_blank");"#, url), vec![])
        .await;
    let _ = driver.clone().quit().await;
    time::sleep(Duration::from_secs_f64(reconnect_secs)).await;
    for _ in 0..20 {
        if let Ok(d) = WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await {
            inject_all_persistent_stealth(&d).await?;
            let handles = d.windows().await.unwrap_or_default();
            if let Some(last) = handles.last() {
                let _ = d.switch_to_window(last.clone()).await;
            }
            return Ok(d);
        }
        time::sleep(Duration::from_millis(250)).await;
    }
    Err("Failed to reconnect WebDriver".into())
}

// ─── connect_to_driver ────────────────────────────────────────────────────────

async fn connect_to_driver(
    port: usize,
    config: &ChromeConfig,
) -> Result<WebDriver, Box<dyn Error>> {
    let mut caps = DesiredCapabilities::chrome();

    // 1. Core stealth
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

    // 1.5 Stop phoning home
    caps.add_arg("--no-pings")?;
    caps.add_arg("--homepage=about:blank")?;
    caps.add_arg("--safebrowsing-disable-download-protection")?;
    caps.add_arg("--disable-client-side-phishing-detection")?;
    caps.add_arg("--simulate-outdated-no-au=\"Tue, 31 Dec 2099 23:59:59 GMT\"")?;
    caps.add_arg("--disable-single-click-autofill")?;
    caps.add_arg("--disable-password-generation")?;
    caps.add_arg("--disable-save-password-bubble")?;

    // 2. Dynamic user-agent
    let os = std::env::consts::OS;
    let user_agent = match os {
        "windows" => "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        "macos"   => "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        _         => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
    };
    caps.add_arg(&format!("--user-agent={}", user_agent))?;

    // 3. UI suppression
    caps.add_arg("--disable-popup-blocking")?;
    caps.add_arg("--disable-translate")?;
    caps.add_arg("--disable-search-engine-choice-screen")?;
    caps.add_arg("--enable-unsafe-extension-debugging")?;

    // 4. Language
    let lang = config.lang.as_deref().unwrap_or("en-US,en;q=0.9");
    caps.add_arg(&format!("--lang={}", lang))?;

    // 5. Mega disable-features
    caps.add_arg(
        "--disable-features=IsolateOrigins,site-per-process,Translate,\
        InsecureDownloadWarnings,DownloadBubble,DownloadBubbleV2,\
        OptimizationTargetPrediction,OptimizationGuideModelDownloading,\
        SafeBrowsingEnhancedProtection,PrivacySandboxSettings4,\
        AutofillEnableAccountWalletStorage",
    )?;

    // 6. Anti-throttling
    caps.add_arg("--disable-background-timer-throttling")?;
    caps.add_arg("--disable-backgrounding-occluded-windows")?;
    caps.add_arg("--disable-renderer-backgrounding")?;

    // 7. Headless
    if config.headless {
        caps.add_arg("--headless=new")?;
        caps.add_arg("--window-size=1920,1080")?;
    }

    // 8. Proxy
    if let Some(proxy) = &config.proxy {
        let host_port = if proxy.contains('@') {
            proxy.split('@').last().unwrap_or(proxy)
        } else {
            proxy
        };
        caps.add_arg(&format!("--proxy-server={}", host_port))?;
    }

    // 9. Extensions
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
        caps.add_arg(&format!("--load-extension={}", paths))?;
    }

    // 10. Temp profile + Preferences
    let temp_dir = tempfile::Builder::new().prefix("uc_").tempdir()?;
    let profile_path = temp_dir.path().to_path_buf();
    caps.add_arg(&format!("--user-data-dir={}", profile_path.display()))?;

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

    // 11. Retry loop
    for _ in 0..20 {
        if let Ok(driver) =
            WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await
        {
            let _ = temp_dir.into_path();
            return Ok(driver);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("Failed to create WebDriver after 20 attempts".into())
}

// ─── Patching / permissions / process ────────────────────────────────────────

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
    if patch_count > 0 {
        println!("Patched {} cdcs!", patch_count);
    } else {
        println!("No cdcs were found!");
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
    cmd.creation_flags(0x00000008);
    cmd.spawn()?;
    Ok(())
}

// ─── ChromeDriver download helpers ───────────────────────────────────────────

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
            let mut outfile = fs::File::create(outpath.file_name().ok_or("bad filename")?)?;
            std::io::copy(&mut file, &mut outfile)?;
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
    let (platform, zip_name) = match (os, arch) {
        ("linux", _) => ("linux64", "chromedriver-linux64.zip"),
        ("windows", _) => ("win64", "chromedriver-win64.zip"),
        ("macos", "aarch64") => ("mac-arm64", "chromedriver-mac-arm64.zip"),
        ("macos", _) => ("mac-x64", "chromedriver-mac-x64.zip"),
        _ => return Err("Unsupported OS".into()),
    };
    Ok(format!(
        "https://storage.googleapis.com/chrome-for-testing-public/{}/{}/{}",
        full, platform, zip_name
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
    let zip_name = match (os, arch) {
        ("linux", _) => "chromedriver_linux64.zip",
        ("windows", _) => "chromedriver_win32.zip",
        ("macos", "aarch64") => return Err("macOS Apple Silicon + Chrome <114 unsupported".into()),
        ("macos", _) => "chromedriver_mac64.zip",
        _ => return Err("Unsupported OS".into()),
    };
    Ok(format!(
        "https://chromedriver.storage.googleapis.com/{}/{}",
        latest, zip_name
    ))
}

async fn get_chrome_version(os: &str) -> Result<String, Box<dyn Error>> {
    println!("Getting installed Chrome version...");
    let output = match os {
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
    println!("Installed Chrome version: {}", version);
    Ok(version)
}

// ─── Chrome trait — complete public API ──────────────────────────────────────

#[async_trait::async_trait]
pub trait Chrome {
    // ── Stealth ───────────────────────────────────────────────────────────────
    /// Scrub CDC props from the current page context (runtime call).
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>>;
    /// Re-inject all persistent CDP stealth hooks (call after manual reconnect).
    async fn inject_persistent_stealth(&self) -> Result<(), Box<dyn Error>>;

    // ── Lifecycle ─────────────────────────────────────────────────────────────
    async fn new() -> Self;
    async fn borrow(&self) -> &WebDriver;

    // ── Navigation ────────────────────────────────────────────────────────────
    /// Navigate with pre/post CDC scrub.
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>>;
    /// Double-scrub alias for goto.
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>>;
    /// Reload with pre/post CDC scrub.
    async fn refresh(&self) -> Result<(), Box<dyn Error>>;
    /// Navigate back with CDC scrub.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L576
    async fn go_back(&self) -> Result<(), Box<dyn Error>>;
    /// Navigate forward with CDC scrub.
    async fn go_forward(&self) -> Result<(), Box<dyn Error>>;
    /// Open URL in a new tab, close the original.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L561-L576
    async fn uc_open_with_tab(&self, url: &str) -> Result<(), Box<dyn Error>>;
    /// Open url in new tab → quit session → sleep → reconnect.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L634-L662
    async fn uc_open_with_disconnect(
        &self,
        url: &str,
        timeout_secs: f64,
        port: usize,
        caps: thirtyfour::ChromeCapabilities,
    ) -> Result<WebDriver, Box<dyn Error>>;
    async fn get_current_url(&self) -> Result<String, Box<dyn Error>>;
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/examples/raw_google.py#L8
    async fn get_title(&self) -> Result<String, Box<dyn Error>>;
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L1336
    async fn get_page_source(&self) -> Result<String, Box<dyn Error>>;

    // ── Cloudflare bypass ─────────────────────────────────────────────────────
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>>;

    // ── Clicking ──────────────────────────────────────────────────────────────
    /// 111 ms delayed JS click. Mirrors Python's `js_utils.call_me_later`.
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
    /// Click without panicking if element is not visible.
    async fn click_if_visible(&self, css_selector: &str);

    // ── Form / typing ─────────────────────────────────────────────────────────
    /// Focus + clear + native `send_keys`.
    async fn send_keys(&self, css_selector: &str, text: &str) -> Result<(), Box<dyn Error>>;
    /// Set `.value` via React-compatible native setter + input/change events.
    async fn set_value(&self, css_selector: &str, value: &str) -> Result<(), Box<dyn Error>>;
    async fn clear_input(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
    /// Dispatch Enter keydown — submits forms without clicking a button.
    async fn submit(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;

    // ── Scroll ────────────────────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L3096
    async fn scroll_to_top(&self) -> Result<(), Box<dyn Error>>;
    async fn scroll_to_bottom(&self) -> Result<(), Box<dyn Error>>;
    async fn scroll_to_y(&self, y: i64) -> Result<(), Box<dyn Error>>;
    async fn scroll_by_y(&self, y: i64) -> Result<(), Box<dyn Error>>;
    async fn scroll_into_view(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;

    // ── DOM mutation ──────────────────────────────────────────────────────────
    async fn remove_element(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
    async fn remove_elements(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
    async fn set_attribute(
        &self,
        css_selector: &str,
        attr: &str,
        val: &str,
    ) -> Result<(), Box<dyn Error>>;
    async fn get_attribute(
        &self,
        css_selector: &str,
        attr: &str,
    ) -> Result<Option<String>, Box<dyn Error>>;
    async fn get_text(&self, css_selector: &str) -> Result<String, Box<dyn Error>>;
    /// Rewrite all target="_blank" links to target="_self".
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L1736
    async fn internalize_links(&self) -> Result<(), Box<dyn Error>>;
    /// Open a new browser window at URL.
    async fn window_new(&self, url: &str) -> Result<(), Box<dyn Error>>;

    // ── Visibility helpers ────────────────────────��───────────────────────────
    async fn is_element_visible(&self, css_selector: &str) -> bool;
    async fn is_element_present(&self, css_selector: &str) -> bool;

    // ── Checkbox helpers ──────────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L2562
    async fn is_checked(&self, css_selector: &str) -> bool;
    async fn check_if_unchecked(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
    async fn uncheck_if_checked(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;

    // ── Mouse / gesture ───────────────────────────────────────────────────────
    /// CDP `Input.dispatchMouseEvent` hover — works headless.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L2507
    async fn hover_element(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
    /// CDP drag from one selector to another.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L2412
    async fn drag_and_drop(&self, drag_sel: &str, drop_sel: &str) -> Result<(), Box<dyn Error>>;
    /// CDP drag between raw viewport coordinates.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L2376
    async fn drag_and_drop_points(
        &self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> Result<(), Box<dyn Error>>;

    // ── Window geometry ───────────────────────────────────────────────────────
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Browser/#method-getWindowForTarget
    async fn get_window_rect(&self) -> Result<Value, Box<dyn Error>>;
    async fn set_window_rect(&self, x: i64, y: i64, w: i64, h: i64) -> Result<(), Box<dyn Error>>;
    async fn maximize(&self) -> Result<(), Box<dyn Error>>;
    async fn minimize(&self) -> Result<(), Box<dyn Error>>;

    // ── localStorage ──────────────────────────────────────────────────────────
    async fn get_local_storage_item(&self, key: &str) -> Result<Option<String>, Box<dyn Error>>;
    async fn set_local_storage_item(&self, key: &str, val: &str) -> Result<(), Box<dyn Error>>;
    async fn remove_local_storage_item(&self, key: &str) -> Result<(), Box<dyn Error>>;
    async fn clear_local_storage(&self) -> Result<(), Box<dyn Error>>;

    // ── sessionStorage ────────────────────────────────────────────────────────
    async fn get_session_storage_item(&self, key: &str) -> Result<Option<String>, Box<dyn Error>>;
    async fn set_session_storage_item(&self, key: &str, val: &str) -> Result<(), Box<dyn Error>>;
    async fn remove_session_storage_item(&self, key: &str) -> Result<(), Box<dyn Error>>;
    async fn clear_session_storage(&self) -> Result<(), Box<dyn Error>>;

    // ── Cookies ───────────────────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/help_docs/cdp_mode_methods.md#L44
    async fn get_all_cookies(&self) -> Result<Value, Box<dyn Error>>;
    /// Restore cookies from a `Value` returned by `get_all_cookies`.
    async fn set_all_cookies(&self, cookies: &Value) -> Result<(), Box<dyn Error>>;
    async fn get_cookie_string(&self) -> Result<String, Box<dyn Error>>;
    async fn clear_cookies(&self) -> Result<(), Box<dyn Error>>;

    // ── Page capture ──────────────────────────────────────────────────────────
    /// Lossless PNG screenshot via CDP. Returns raw bytes.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-captureScreenshot
    async fn cdp_screenshot(&self) -> Result<Vec<u8>, Box<dyn Error>>;
    /// Save screenshot PNG to `path`.
    async fn save_screenshot(&self, path: &str) -> Result<(), Box<dyn Error>>;
    /// Print page to PDF bytes via CDP.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-printToPDF
    async fn print_to_pdf(&self) -> Result<Vec<u8>, Box<dyn Error>>;
    /// Save PDF to `path`.
    async fn save_pdf(&self, path: &str) -> Result<(), Box<dyn Error>>;

    // ── CDP overrides ─────────────────────────────────────────────────────────
    /// Override User-Agent at the CDP Network layer.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setUserAgentOverride
    async fn set_user_agent(&self, ua: &str) -> Result<(), Box<dyn Error>>;
    /// Spoof timezone + geolocation.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/connection.py#L330-L348
    async fn set_timezone_and_geolocation(
        &self,
        timezone_id: &str,
        latitude: f64,
        longitude: f64,
        accuracy: f64,
    ) -> Result<(), Box<dyn Error>>;
    /// Grant all common browser permissions so prompts never appear.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Browser/#method-grantPermissions
    async fn grant_all_permissions(&self) -> Result<(), Box<dyn Error>>;
    /// Enable CDP Network + Log domains for request/event capture.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L175-L183
    async fn enable_cdp_log_capture(&self) -> Result<(), Box<dyn Error>>;

    // ── Mobile emulation ──────────────────────────────────────────────────────
    /// Enable mobile device emulation at runtime (post-startup).
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride
    async fn set_mobile_emulation(
        &self,
        width: u32,
        height: u32,
        pixel_ratio: f64,
    ) -> Result<(), Box<dyn Error>>;

    // ── Network control ───────────────────────────────────────────────────────
    /// Block ad/tracker URLs via CDP `Network.setBlockedURLs`.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454
    async fn enable_ad_block(&self) -> Result<(), Box<dyn Error>>;
    /// Block a custom list of URL glob patterns via CDP.
    async fn block_urls(&self, patterns: &[&str]) -> Result<(), Box<dyn Error>>;
    /// JS-layer fetch/XHR override injected via `Page.addScriptToEvaluateOnNewDocument`.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-addScriptToEvaluateOnNewDocument
    async fn block_url_patterns(&self, patterns: &[&str]) -> Result<(), Box<dyn Error>>;
    /// Bypass CSP — needed for JS injection on strict sites.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setBypassCSP
    async fn disable_csp(&self) -> Result<(), Box<dyn Error>>;
    /// Inject Proxy-Authorization at the CDP Network layer.
    async fn set_proxy(&self, proxy: &str) -> Result<(), Box<dyn Error>>;

    // ── Wait / poll ───────────────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L240
    async fn wait_for_element(
        &self,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), Box<dyn Error>>;
    async fn wait_for_text(
        &self,
        text: &str,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), Box<dyn Error>>;

    // ── Text assertions ───────────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L2970
    async fn assert_text(&self, text: &str, css_selector: &str) -> Result<(), Box<dyn Error>>;
    async fn assert_exact_text(&self, text: &str, css_selector: &str)
        -> Result<(), Box<dyn Error>>;
    async fn assert_text_not_visible(
        &self,
        text: &str,
        css_selector: &str,
    ) -> Result<(), Box<dyn Error>>;

    // ── Title / URL assertions ────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L2940
    async fn assert_title(&self, title: &str) -> Result<(), Box<dyn Error>>;
    async fn assert_title_contains(&self, s: &str) -> Result<(), Box<dyn Error>>;
    async fn assert_url(&self, url: &str) -> Result<(), Box<dyn Error>>;
    async fn assert_url_contains(&self, s: &str) -> Result<(), Box<dyn Error>>;

    // ── Text search ───────────────────────────────────────────────────────────
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L304
    async fn find_elements_by_text(
        &self,
        text: &str,
        tag: Option<&str>,
    ) -> Result<Vec<String>, Box<dyn Error>>;

    // ── Alert handling ────────────────────────────────────────────────────────
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-handleJavaScriptDialog
    async fn wait_for_and_accept_alert(&self, timeout_secs: f64) -> Result<(), Box<dyn Error>>;
    async fn wait_for_and_dismiss_alert(&self, timeout_secs: f64) -> Result<(), Box<dyn Error>>;

    // ── Ergonomics ────────────────────────────────────────────────────────────
    async fn sleep(&self, secs: f64);
}

// ─── impl Chrome for WebDriver ───────────────────────────────────────────────

#[async_trait::async_trait]
impl Chrome for WebDriver {
    // ── remove_cdc_props ──────────────────────────────────────────────────────
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>> {
        let _ = self.execute(CDC_SCRUB_SCRIPT, vec![]).await;
        Ok(())
    }

    // ── inject_persistent_stealth ─────────────────────────────────────────────
    async fn inject_persistent_stealth(&self) -> Result<(), Box<dyn Error>> {
        inject_all_persistent_stealth(self).await
    }

    // ── new ───────────────────────────────────────────────────────────────────
    async fn new() -> WebDriver {
        chrome().await.expect("Failed to create Chrome driver")
    }

    async fn borrow(&self) -> &WebDriver {
        self
    }

    // ── goto ──────────────────────────────────────────────────────────────────
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let check = r#"
            let o=window,r=[];
            while(o!==null){r=r.concat(Object.getOwnPropertyNames(o));o=Object.getPrototypeOf(o);}
            return r.filter(i=>i.match(/^[a-z]{3}_[a-z]{22}_.*/i));
        "#;
        let props = self.execute(check, vec![]).await?;
        let arr = props.json().as_array().cloned().unwrap_or_default();
        if !arr.is_empty() {
            let js = format!(
                "{}.forEach(p=>delete window[p]);",
                serde_json::to_string(&arr)?
            );
            self.execute(&js, vec![]).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.get(url).await?;
        self.remove_cdc_props().await?;
        Ok(())
    }

    // ── uc_get ────────────────────────────────────────────────────────────────
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        self.goto(url).await?;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── refresh ───────────────────────────────────────────────────────────────
    async fn refresh(&self) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        self.execute("window.location.reload();", vec![]).await?;
        time::sleep(Duration::from_millis(300)).await;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── go_back ───────────────────────────────────────────────────────────────
    async fn go_back(&self) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        self.execute("window.history.back();", vec![]).await?;
        time::sleep(Duration::from_millis(300)).await;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── go_forward ────────────────────────────────────────────────────────────
    async fn go_forward(&self) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        self.execute("window.history.forward();", vec![]).await?;
        time::sleep(Duration::from_millis(300)).await;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── uc_open_with_tab ──────────────────────────────────────────────────────
    async fn uc_open_with_tab(&self, url: &str) -> Result<(), Box<dyn Error>> {
        self.execute(&format!(r#"window.open("{}","_blank");"#, url), vec![])
            .await?;
        time::sleep(Duration::from_secs(1)).await;
        // Close original tab and switch to new one
        let wins = self.windows().await?;
        if let Some(first) = wins.first() {
            self.switch_to_window(first.clone()).await?;
            self.close_window().await?;
        }
        if let Some(last) = self.windows().await?.last() {
            self.switch_to_window(last.clone()).await?;
        }
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── uc_open_with_disconnect ───────────────────────────────────────────────
    async fn uc_open_with_disconnect(
        &self,
        url: &str,
        timeout_secs: f64,
        port: usize,
        caps: thirtyfour::ChromeCapabilities,
    ) -> Result<WebDriver, Box<dyn Error>> {
        uc_open_with_reconnect(self, url, port, caps, timeout_secs).await
    }

    // ── get_current_url ───────────────────────────────────────────────────────
    async fn get_current_url(&self) -> Result<String, Box<dyn Error>> {
        let r = self.execute("return window.location.href;", vec![]).await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    // ── get_title ─────────────────────────────────────────────────────────────
    async fn get_title(&self) -> Result<String, Box<dyn Error>> {
        let r = self.execute("return document.title;", vec![]).await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    // ── get_page_source ───────────────────────────────────────────────────────
    async fn get_page_source(&self) -> Result<String, Box<dyn Error>> {
        let r = self
            .execute("return document.documentElement.outerHTML;", vec![])
            .await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    // ── bypass_cloudflare ─────────────────────────────────────────────────────
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>> {
        self.goto(url).await?;
        self.enter_frame(0).await?;
        let btn = self
            .find(By::XPath("/html/body//div/div[1]/div[1]/div/label/input"))
            .await?;
        btn.wait_until().clickable().await?;
        time::sleep(Duration::from_secs(2)).await;
        btn.click().await?;
        Ok(())
    }

    // ── uc_click ────��─────────────────────────────────────────────────────────
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "setTimeout(()=>document.querySelector('{}').click(),111);",
                css_selector
            ),
            vec![],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    }

    // ── click_if_visible ──────────────────────────────────────────────────────
    async fn click_if_visible(&self, css_selector: &str) {
        if self.is_element_visible(css_selector).await {
            let _ = self.uc_click(css_selector).await;
        }
    }

    // ── send_keys ─────────────────────────────────────────────────────────────
    async fn send_keys(&self, css_selector: &str, text: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "const e=document.querySelector('{}');if(e){{e.focus();e.value='';}}",
                css_selector
            ),
            vec![],
        )
        .await?;
        self.find(By::Css(css_selector))
            .await?
            .send_keys(text)
            .await?;
        Ok(())
    }

    // ── set_value ─────────────���───────────────────────────────────────────────
    async fn set_value(&self, css_selector: &str, value: &str) -> Result<(), Box<dyn Error>> {
        self.execute(&format!(r#"
            const e=document.querySelector('{}');
            if(e){{
                const s=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;
                s.call(e,'{}');
                e.dispatchEvent(new Event('input',{{bubbles:true}}));
                e.dispatchEvent(new Event('change',{{bubbles:true}}));
            }}"#, css_selector, value), vec![]).await?;
        Ok(())
    }

    // ── clear_input ───────────────────────────────────────────────────────────
    async fn clear_input(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "const e=document.querySelector('{}');if(e)e.value='';",
                css_selector
            ),
            vec![],
        )
        .await?;
        Ok(())
    }

    // ── submit ────────────────────────────────────────────────────────────────
    async fn submit(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                r#"
            const e=document.querySelector('{}');
            if(e)e.dispatchEvent(new KeyboardEvent('keydown',
                {{key:'Enter',keyCode:13,code:'Enter',which:13,bubbles:true}}));"#,
                css_selector
            ),
            vec![],
        )
        .await?;
        Ok(())
    }

    // ── scroll ────────────────────────────────────────────────────────────────
    async fn scroll_to_top(&self) -> Result<(), Box<dyn Error>> {
        self.execute("window.scrollTo(0,0);", vec![]).await?;
        Ok(())
    }
    async fn scroll_to_bottom(&self) -> Result<(), Box<dyn Error>> {
        self.execute("window.scrollTo(0,document.body.scrollHeight);", vec![])
            .await?;
        Ok(())
    }
    async fn scroll_to_y(&self, y: i64) -> Result<(), Box<dyn Error>> {
        self.execute(&format!("window.scrollTo(0,{});", y), vec![])
            .await?;
        Ok(())
    }
    async fn scroll_by_y(&self, y: i64) -> Result<(), Box<dyn Error>> {
        self.execute(&format!("window.scrollBy(0,{});", y), vec![])
            .await?;
        Ok(())
    }
    async fn scroll_into_view(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(&format!(
            "const e=document.querySelector('{}');if(e)e.scrollIntoView({{behavior:'smooth',block:'center'}});",
            css_selector), vec![]).await?;
        Ok(())
    }

    // ── DOM mutation ──────────────────────────────────────────────────────────
    async fn remove_element(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "const e=document.querySelector('{}');if(e)e.remove();",
                css_selector
            ),
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn remove_elements(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "document.querySelectorAll('{}').forEach(e=>e.remove());",
                css_selector
            ),
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn set_attribute(
        &self,
        css_selector: &str,
        attr: &str,
        val: &str,
    ) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "const e=document.querySelector('{}');if(e)e.setAttribute('{}','{}');",
                css_selector, attr, val
            ),
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn get_attribute(
        &self,
        css_selector: &str,
        attr: &str,
    ) -> Result<Option<String>, Box<dyn Error>> {
        let r = self
            .execute(
                &format!(
                    "const e=document.querySelector('{}');return e?e.getAttribute('{}'):null;",
                    css_selector, attr
                ),
                vec![],
            )
            .await?;
        Ok(r.json().as_str().map(|s| s.to_string()))
    }
    async fn get_text(&self, css_selector: &str) -> Result<String, Box<dyn Error>> {
        let r = self
            .execute(
                &format!(
                    "const e=document.querySelector('{}');return e?e.innerText.trim():'';",
                    css_selector
                ),
                vec![],
            )
            .await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }
    async fn internalize_links(&self) -> Result<(), Box<dyn Error>> {
        self.execute(
            "document.querySelectorAll('a[target=\"_blank\"]').forEach(a=>a.target='_self');",
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn window_new(&self, url: &str) -> Result<(), Box<dyn Error>> {
        self.execute(&format!(r#"window.open("{}","_blank");"#, url), vec![])
            .await?;
        Ok(())
    }

    // ── visibility ────────────────────────────────────────────────────────────
    async fn is_element_visible(&self, css_selector: &str) -> bool {
        self.execute(
            &format!(
                r#"
            const e=document.querySelector('{}');
            if(!e)return false;
            const s=window.getComputedStyle(e);
            return s.display!=='none'&&s.visibility!=='hidden'&&s.opacity!=='0'&&e.offsetWidth>0;"#,
                css_selector
            ),
            vec![],
        )
        .await
        .ok()
        .and_then(|r| r.json().as_bool())
        .unwrap_or(false)
    }
    async fn is_element_present(&self, css_selector: &str) -> bool {
        self.execute(
            &format!("return document.querySelector('{}')!==null;", css_selector),
            vec![],
        )
        .await
        .ok()
        .and_then(|r| r.json().as_bool())
        .unwrap_or(false)
    }

    // ── checkbox ──────────────────────────────────────────────────────────────
    async fn is_checked(&self, css_selector: &str) -> bool {
        self.execute(
            &format!(
                "const e=document.querySelector('{}');return e?e.checked:false;",
                css_selector
            ),
            vec![],
        )
        .await
        .ok()
        .and_then(|r| r.json().as_bool())
        .unwrap_or(false)
    }
    async fn check_if_unchecked(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        if !self.is_checked(css_selector).await {
            self.uc_click(css_selector).await?;
        }
        Ok(())
    }
    async fn uncheck_if_checked(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        if self.is_checked(css_selector).await {
            self.uc_click(css_selector).await?;
        }
        Ok(())
    }

    // ── hover_element ─────────────────────────────────────────────────────────
    async fn hover_element(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let pos = self
            .execute(
                &format!(
                    "const e=document.querySelector('{}');if(!e)return null;\
             const r=e.getBoundingClientRect();return [r.left+r.width/2,r.top+r.height/2];",
                    css_selector
                ),
                vec![],
            )
            .await?;
        let x = pos.json()[0].as_f64().ok_or("hover: missing x")?;
        let y = pos.json()[1].as_f64().ok_or("hover: missing y")?;
        dt.execute_cdp_with_params(
            "Input.dispatchMouseEvent",
            json!({"type":"mouseMoved","x":x,"y":y,"button":"none"}),
        )
        .await?;
        time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    // ── drag_and_drop ─────────────────────────────────────────────────────────
    async fn drag_and_drop(&self, drag_sel: &str, drop_sel: &str) -> Result<(), Box<dyn Error>> {
        let center = |sel: &str| {
            format!(
                "const e=document.querySelector('{}');if(!e)return null;\
             const r=e.getBoundingClientRect();return [r.left+r.width/2,r.top+r.height/2];",
                sel
            )
        };
        let r1 = self.execute(&center(drag_sel), vec![]).await?;
        let r2 = self.execute(&center(drop_sel), vec![]).await?;
        self.drag_and_drop_points(
            r1.json()[0].as_f64().ok_or("drag x1")?,
            r1.json()[1].as_f64().ok_or("drag y1")?,
            r2.json()[0].as_f64().ok_or("drag x2")?,
            r2.json()[1].as_f64().ok_or("drag y2")?,
        )
        .await
    }

    // ── drag_and_drop_points ──────────────────────────────────────────────────
    async fn drag_and_drop_points(
        &self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Input.dispatchMouseEvent",
            json!({"type":"mousePressed","x":x1,"y":y1,"button":"left","clickCount":1}),
        )
        .await?;
        time::sleep(Duration::from_millis(50)).await;
        for step in 1..=10usize {
            let t = step as f64 / 10.0;
            dt.execute_cdp_with_params(
                "Input.dispatchMouseEvent",
                json!({
                    "type":"mouseMoved",
                    "x": x1 + (x2-x1)*t,
                    "y": y1 + (y2-y1)*t,
                    "button":"left"
                }),
            )
            .await?;
            time::sleep(Duration::from_millis(16)).await;
        }
        dt.execute_cdp_with_params(
            "Input.dispatchMouseEvent",
            json!({"type":"mouseReleased","x":x2,"y":y2,"button":"left","clickCount":1}),
        )
        .await?;
        Ok(())
    }

    // ── window geometry ───────────────────────────────────────────────────────
    async fn get_window_rect(&self) -> Result<Value, Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
            .await?;
        let wid = r["windowId"].clone();
        let b = dt
            .execute_cdp_with_params("Browser.getWindowBounds", json!({"windowId":wid}))
            .await?;
        Ok(b["bounds"].clone())
    }
    async fn set_window_rect(&self, x: i64, y: i64, w: i64, h: i64) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
            .await?;
        let wid = r["windowId"].clone();
        dt.execute_cdp_with_params(
            "Browser.setWindowBounds",
            json!({
                "windowId":wid,
                "bounds":{"left":x,"top":y,"width":w,"height":h,"windowState":"normal"}
            }),
        )
        .await?;
        Ok(())
    }
    async fn maximize(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
            .await?;
        let wid = r["windowId"].clone();
        dt.execute_cdp_with_params(
            "Browser.setWindowBounds",
            json!({"windowId":wid,"bounds":{"windowState":"maximized"}}),
        )
        .await?;
        Ok(())
    }
    async fn minimize(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
            .await?;
        let wid = r["windowId"].clone();
        dt.execute_cdp_with_params(
            "Browser.setWindowBounds",
            json!({"windowId":wid,"bounds":{"windowState":"minimized"}}),
        )
        .await?;
        Ok(())
    }

    // ── localStorage ──────────────────────────────────────────────────────────
    async fn get_local_storage_item(&self, key: &str) -> Result<Option<String>, Box<dyn Error>> {
        let r = self
            .execute(&format!("return localStorage.getItem('{}');", key), vec![])
            .await?;
        Ok(r.json().as_str().map(|s| s.to_string()))
    }
    async fn set_local_storage_item(&self, key: &str, val: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!("localStorage.setItem('{}','{}');", key, val),
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn remove_local_storage_item(&self, key: &str) -> Result<(), Box<dyn Error>> {
        self.execute(&format!("localStorage.removeItem('{}');", key), vec![])
            .await?;
        Ok(())
    }
    async fn clear_local_storage(&self) -> Result<(), Box<dyn Error>> {
        self.execute("localStorage.clear();", vec![]).await?;
        Ok(())
    }

    // ── sessionStorage ────────────────────────────────────────────────────────
    async fn get_session_storage_item(&self, key: &str) -> Result<Option<String>, Box<dyn Error>> {
        let r = self
            .execute(
                &format!("return sessionStorage.getItem('{}');", key),
                vec![],
            )
            .await?;
        Ok(r.json().as_str().map(|s| s.to_string()))
    }
    async fn set_session_storage_item(&self, key: &str, val: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!("sessionStorage.setItem('{}','{}');", key, val),
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn remove_session_storage_item(&self, key: &str) -> Result<(), Box<dyn Error>> {
        self.execute(&format!("sessionStorage.removeItem('{}');", key), vec![])
            .await?;
        Ok(())
    }
    async fn clear_session_storage(&self) -> Result<(), Box<dyn Error>> {
        self.execute("sessionStorage.clear();", vec![]).await?;
        Ok(())
    }

    // ── cookies ───────────────────────────────────────────────────────────────
    async fn get_all_cookies(&self) -> Result<Value, Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Network.getAllCookies", json!({}))
            .await?;
        Ok(r["cookies"].clone())
    }
    async fn set_all_cookies(&self, cookies: &Value) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        if let Some(arr) = cookies.as_array() {
            for c in arr {
                let _ = dt
                    .execute_cdp_with_params("Network.setCookie", c.clone())
                    .await;
            }
        }
        Ok(())
    }
    async fn get_cookie_string(&self) -> Result<String, Box<dyn Error>> {
        let r = self.execute("return document.cookie;", vec![]).await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }
    async fn clear_cookies(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params("Network.clearBrowserCookies", json!({}))
            .await?;
        Ok(())
    }

    // ── page capture ──────────────────────────────────────────────────────────
    async fn cdp_screenshot(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params(
                "Page.captureScreenshot",
                json!({"format":"png","captureBeyondViewport":false}),
            )
            .await?;
        let b64 = r["data"].as_str().ok_or("screenshot: missing data")?;
        Ok(general_purpose::STANDARD.decode(b64)?)
    }
    async fn save_screenshot(&self, path: &str) -> Result<(), Box<dyn Error>> {
        let bytes = self.cdp_screenshot().await?;
        fs::write(path, bytes)?;
        Ok(())
    }
    async fn print_to_pdf(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Page.printToPDF", json!({"printBackground":true}))
            .await?;
        let b64 = r["data"].as_str().ok_or("pdf: missing data")?;
        Ok(general_purpose::STANDARD.decode(b64)?)
    }
    async fn save_pdf(&self, path: &str) -> Result<(), Box<dyn Error>> {
        let bytes = self.print_to_pdf().await?;
        fs::write(path, bytes)?;
        Ok(())
    }

    // ── CDP overrides ─────────────────────────────────────────────────────────
    async fn set_user_agent(&self, ua: &str) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params("Network.setUserAgentOverride", json!({"userAgent": ua}))
            .await?;
        Ok(())
    }
    async fn set_timezone_and_geolocation(
        &self,
        timezone_id: &str,
        latitude: f64,
        longitude: f64,
        accuracy: f64,
    ) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Emulation.setTimezoneOverride",
            json!({"timezoneId": timezone_id}),
        )
        .await?;
        dt.execute_cdp_with_params(
            "Emulation.setGeolocationOverride",
            json!({"latitude":latitude,"longitude":longitude,"accuracy":accuracy}),
        )
        .await?;
        Ok(())
    }
    async fn grant_all_permissions(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Browser.grantPermissions",
            json!({
                "permissions": [
                    "geolocation","notifications","audioCapture","videoCapture",
                    "clipboardReadWrite","clipboardSanitizedWrite","midi","midiSysex",
                    "sensors","backgroundSync","backgroundFetch","nfc","displayCapture",
                    "storageAccess","protectedMediaIdentifier","idleDetection"
                ]
            }),
        )
        .await?;
        Ok(())
    }
    async fn enable_cdp_log_capture(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp("Log.enable").await?;
        Ok(())
    }

    // ── mobile emulation ──────────────────────────────────────────────────────
    async fn set_mobile_emulation(
        &self,
        width: u32,
        height: u32,
        pixel_ratio: f64,
    ) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Emulation.setDeviceMetricsOverride",
            json!({"width":width,"height":height,"deviceScaleFactor":pixel_ratio,"mobile":true}),
        )
        .await?;
        dt.execute_cdp_with_params(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled":true,"maxTouchPoints":5}),
        )
        .await?;
        Ok(())
    }

    // ── network control ───────────────────────────────────────────────────────
    async fn enable_ad_block(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp_with_params("Network.setBlockedURLs", json!({"urls": AD_BLOCK_PATTERNS}))
            .await?;
        Ok(())
    }
    async fn block_urls(&self, patterns: &[&str]) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp_with_params("Network.setBlockedURLs", json!({"urls": patterns}))
            .await?;
        Ok(())
    }
    async fn block_url_patterns(&self, patterns: &[&str]) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let list = patterns
            .iter()
            .map(|p| format!("\"{}\"", p))
            .collect::<Vec<_>>()
            .join(",");
        let src = format!(
            r#"(()=>{{
            const B=[{}];
            const _f=window.fetch;
            window.fetch=function(i,o){{
                const u=typeof i==='string'?i:i.url;
                if(B.some(p=>u.includes(p)))return Promise.reject(new TypeError('Blocked: '+u));
                return _f.apply(this,arguments);
            }};
            const _o=XMLHttpRequest.prototype.open;
            XMLHttpRequest.prototype.open=function(m,u){{
                if(B.some(p=>u.includes(p)))this._uc_blocked=true;
                return _o.apply(this,arguments);
            }};
            const _s=XMLHttpRequest.prototype.send;
            XMLHttpRequest.prototype.send=function(){{
                if(this._uc_blocked){{this.abort();return;}}
                return _s.apply(this,arguments);
            }};
        }})();"#,
            list
        );
        dt.execute_cdp_with_params(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": src}),
        )
        .await?;
        Ok(())
    }
    async fn disable_csp(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params("Page.setBypassCSP", json!({"enabled":true}))
            .await?;
        Ok(())
    }
    async fn set_proxy(&self, proxy: &str) -> Result<(), Box<dyn Error>> {
        if proxy.contains('@') {
            let creds = proxy.split('@').next().unwrap_or("");
            let encoded = general_purpose::STANDARD.encode(creds);
            let dt = ChromeDevTools::new(self.handle.clone());
            dt.execute_cdp("Network.enable").await?;
            dt.execute_cdp_with_params(
                "Network.setExtraHTTPHeaders",
                json!({"headers":{"Proxy-Authorization": format!("Basic {}", encoded)}}),
            )
            .await?;
        }
        Ok(())
    }

    // ── wait / poll ───────────────────────────────────────────────────────────
    async fn wait_for_element(
        &self,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), Box<dyn Error>> {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
        loop {
            if self.find(By::Css(css_selector)).await.is_ok() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "wait_for_element: '{}' not found within {}s",
                    css_selector, timeout_secs
                )
                .into());
            }
            time::sleep(Duration::from_millis(200)).await;
        }
    }
    async fn wait_for_text(
        &self,
        text: &str,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), Box<dyn Error>> {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
        let script = format!(
            "const e=document.querySelector('{}');return e?e.innerText.includes('{}'):false;",
            css_selector, text
        );
        loop {
            let found = self
                .execute(&script, vec![])
                .await
                .ok()
                .and_then(|r| r.json().as_bool())
                .unwrap_or(false);
            if found {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "wait_for_text: '{}' not in '{}' within {}s",
                    text, css_selector, timeout_secs
                )
                .into());
            }
            time::sleep(Duration::from_millis(200)).await;
        }
    }

    // ── text assertions ───────────────────────────────────────────────────────
    async fn assert_text(&self, text: &str, css_selector: &str) -> Result<(), Box<dyn Error>> {
        let actual = self.get_text(css_selector).await?;
        if actual.contains(text) {
            Ok(())
        } else {
            Err(format!(
                "assert_text: '{}' not in '{}'. Got: '{}'",
                text, css_selector, actual
            )
            .into())
        }
    }
    async fn assert_exact_text(
        &self,
        text: &str,
        css_selector: &str,
    ) -> Result<(), Box<dyn Error>> {
        let actual = self.get_text(css_selector).await?;
        if actual.trim() == text.trim() {
            Ok(())
        } else {
            Err(format!(
                "assert_exact_text '{}': expected '{}', got '{}'",
                css_selector, text, actual
            )
            .into())
        }
    }
    async fn assert_text_not_visible(
        &self,
        text: &str,
        css_selector: &str,
    ) -> Result<(), Box<dyn Error>> {
        let actual = self.get_text(css_selector).await.unwrap_or_default();
        if !actual.contains(text) {
            Ok(())
        } else {
            Err(format!(
                "assert_text_not_visible: '{}' IS visible in '{}'",
                text, css_selector
            )
            .into())
        }
    }

    // ── title / url assertions ────────────────────────────────────────────────
    async fn assert_title(&self, title: &str) -> Result<(), Box<dyn Error>> {
        let actual = self.get_title().await?;
        if actual == title {
            Ok(())
        } else {
            Err(format!("assert_title: expected '{}', got '{}'", title, actual).into())
        }
    }
    async fn assert_title_contains(&self, s: &str) -> Result<(), Box<dyn Error>> {
        let actual = self.get_title().await?;
        if actual.contains(s) {
            Ok(())
        } else {
            Err(format!("assert_title_contains: '{}' not in title '{}'", s, actual).into())
        }
    }
    async fn assert_url(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let actual = self.get_current_url().await?;
        if actual == url {
            Ok(())
        } else {
            Err(format!("assert_url: expected '{}', got '{}'", url, actual).into())
        }
    }
    async fn assert_url_contains(&self, s: &str) -> Result<(), Box<dyn Error>> {
        let actual = self.get_current_url().await?;
        if actual.contains(s) {
            Ok(())
        } else {
            Err(format!("assert_url_contains: '{}' not in url '{}'", s, actual).into())
        }
    }

    // ── find_elements_by_text ─────────────────────────────────────────────────
    async fn find_elements_by_text(
        &self,
        text: &str,
        tag: Option<&str>,
    ) -> Result<Vec<String>, Box<dyn Error>> {
        let tag_filter = tag
            .map(|t| format!("'{}'", t.to_uppercase()))
            .unwrap_or_else(|| "null".into());
        let script = format!(
            r#"
            const search='{}', tag={};
            const walker=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT,null,false);
            const seen=new Set(), results=[];
            let node;
            while((node=walker.nextNode())){{
                if(node.nodeValue&&node.nodeValue.includes(search)){{
                    let el=node.parentElement;
                    if(tag){{while(el&&el.tagName!==tag)el=el.parentElement;}}
                    if(el&&!seen.has(el)){{seen.add(el);results.push(el.outerHTML.substring(0,200));}}
                }}
            }}
            return results;
        "#,
            text, tag_filter
        );
        let r = self.execute(&script, vec![]).await?;
        Ok(r.json()
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default())
    }

    // ── alert handling ────────────────────────────────────────────────────────
    async fn wait_for_and_accept_alert(&self, timeout_secs: f64) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
        loop {
            if dt
                .execute_cdp_with_params("Page.handleJavaScriptDialog", json!({"accept":true}))
                .await
                .is_ok()
            {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("wait_for_and_accept_alert: timed out".into());
            }
            time::sleep(Duration::from_millis(200)).await;
        }
    }
    async fn wait_for_and_dismiss_alert(&self, timeout_secs: f64) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
        loop {
            if dt
                .execute_cdp_with_params("Page.handleJavaScriptDialog", json!({"accept":false}))
                .await
                .is_ok()
            {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("wait_for_and_dismiss_alert: timed out".into());
            }
            time::sleep(Duration::from_millis(200)).await;
        }
    }

    // ── sleep ─────────────────────────────────────────────────────────────────
    async fn sleep(&self, secs: f64) {
        time::sleep(Duration::from_secs_f64(secs)).await;
    }
}
