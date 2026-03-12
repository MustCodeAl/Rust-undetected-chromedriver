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

// ─── Ad-block URL patterns (mirrors SeleniumBase's browser.py list) ───────────
// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454
const AD_BLOCK_PATTERNS: &[&str] = &[
    "*.googlesyndication.com*", "*.googletagmanager.com*",
    "*.google-analytics.com*",  "*.amazon-adsystem.com*",
    "*.adsafeprotected.com*",   "*.doubleclick.net*",
    "*.fastclick.net*",         "*.snigelweb.com*",
    "*.2mdn.net*",              "*.casalemedia.com*",
    "*.admanmedia.com*",        "*.quantserve.com*",
    "*.bidswitch.net*",         "*.360yield.com*",
    "*.adthrive.com*",          "*.pubmatic.com*",
    "*.id5-sync.com*",          "*.moatads.com*",
    "*.dotomi.com*",            "*.adsrvr.org*",
    "*.adnxs.com*",             "*.openx.net*",
    "*.tapad.com*",             "*.3lift.com*",
];

// ─── Persistent CDP stealth scripts ──────────────────────────────────────────
// Registered via Page.addScriptToEvaluateOnNewDocument — fires before ANY page
// JS on every navigation, including cross-origin ones.

/// Hides webdriver flag, restores window.chrome, spoofs permissions/plugins/
/// languages, and forces shadow roots open.
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

/// Deletes all chromedriver CDC artefact properties from window.
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L388-L394
const CDC_SCRUB_SCRIPT: &str = r#"
    (() => {
        let o = window, p = [];
        while (o !== null) { p = p.concat(Object.getOwnPropertyNames(o)); o = Object.getPrototypeOf(o); }
        p.filter(x => x.match(/^[a-z]{3}_[a-z]{22}_.*/i)).forEach(x => delete window[x]);
    })();
"#;

/// Canvas noise, AudioContext noise, WebGL vendor/renderer spoof,
/// hardware concurrency, device memory, and screen metrics normalization.
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

// ─── ChromeConfig ─────────────────────────────────────────────────────────────

/// Builder-style config for chrome_with_config().
/// All fields are optional with sane stealth defaults.
/// Mirrors SeleniumBase's SB(uc=True, headless=True, mobile=True, proxy=...) pattern.
/// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/cdp_util.py#L278-L294
#[derive(Debug, Clone, Default)]
pub struct ChromeConfig {
    /// Run in headless mode (--headless=new, Chrome ≥112).
    pub headless: bool,
    /// Enable mobile device emulation. Default metrics: 412×732 @ 3x.
    pub mobile: bool,
    /// Override mobile metrics: (css_width, css_height, pixel_ratio).
    pub mobile_metrics: Option<(u32, u32, f64)>,
    /// Override mobile user-agent string.
    pub mobile_user_agent: Option<String>,
    /// Block ad/tracker URLs via Network.setBlockedURLs on startup.
    pub ad_block: bool,
    /// Bypass Content Security Policy (Page.setBypassCSP).
    pub disable_csp: bool,
    /// Optional proxy: "host:port" or "user:pass@host:port".
    pub proxy: Option<String>,
    /// Language locale code, e.g. "en-US".
    pub lang: Option<String>,
}

// ─── Internal: register all persistent CDP stealth hooks ─────────────────────

async fn inject_all_persistent_stealth(driver: &WebDriver) -> Result<(), Box<dyn Error>> {
    let dt = ChromeDevTools::new(driver.handle.clone());
    for src in [STEALTH_SCRIPT, CDC_SCRUB_SCRIPT, ADVANCED_FINGERPRINT_SCRIPT] {
        dt.execute_cdp_with_params(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": src }),
        )
            .await?;
    }
    Ok(())
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

    let driver = connect_to_driver_with_config(port, &config).await?;
    let dt = ChromeDevTools::new(driver.handle.clone());

    // Persistent stealth hooks
    inject_all_persistent_stealth(&driver).await?;

    // Grant all permissions upfront — prompts are a bot-detection signal.
    // Ref: https://chromedevtools.github.io/devtools-protocol/tot/Browser/#method-grantPermissions
    let _ = dt.execute_cdp_with_params(
        "Browser.grantPermissions",
        json!({
            "permissions": [
                "geolocation","notifications","audioCapture","videoCapture",
                "clipboardReadWrite","clipboardSanitizedWrite","midi","midiSysex",
                "sensors","backgroundSync","backgroundFetch","nfc","displayCapture",
                "storageAccess","protectedMediaIdentifier","idleDetection"
            ]
        }),
    ).await;

    // Mobile device metrics override
    // Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L6058-L6074
    if config.mobile {
        let (w, h, dpr) = config.mobile_metrics.unwrap_or((412, 732, 3.0));
        let _ = dt.execute_cdp_with_params(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": w, "height": h, "deviceScaleFactor": dpr, "mobile": true }),
        ).await;
        let _ = dt.execute_cdp_with_params(
            "Emulation.setTouchEmulationEnabled",
            json!({ "enabled": true, "maxTouchPoints": 5 }),
        ).await;
    }

    // Ad/tracker URL blocking
    // Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454
    if config.ad_block {
        let _ = dt.execute_cdp("Network.enable").await;
        let _ = dt.execute_cdp_with_params(
            "Network.setBlockedURLs",
            json!({ "urls": AD_BLOCK_PATTERNS }),
        ).await;
    }

    // Bypass CSP — required for JS injection on strict sites
    // Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setBypassCSP
    if config.disable_csp {
        let _ = dt.execute_cdp_with_params(
            "Page.setBypassCSP",
            json!({ "enabled": true }),
        ).await;
    }

    // Authenticated proxy header injection
    if let Some(ref proxy) = config.proxy {
        if proxy.contains('@') {
            let creds = proxy.split('@').next().unwrap_or("");
            let encoded = general_purpose::STANDARD.encode(creds);
            let _ = dt.execute_cdp("Network.enable").await;
            let _ = dt.execute_cdp_with_params(
                "Network.setExtraHTTPHeaders",
                json!({ "headers": { "Proxy-Authorization": format!("Basic {}", encoded) } }),
            ).await;
        }
    }

    Ok(driver)
}

/// Zero-config entry point — backward-compatible with existing callers.
pub async fn chrome() -> Result<WebDriver, Box<dyn Error>> {
    chrome_with_config(ChromeConfig::default()).await
}

// ─── Public reconnect helper ──────────────────────────────────────────────────

/// Canonical CF/bot bypass: window.open in new tab → full quit → sleep → reconnect.
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

// ─── Chrome trait ─────────────────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait Chrome {
    // ── Stealth ──────────────────────────────────────────────────────────────

    /// Scrub CDC props from the current page context (runtime).
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>>;

    /// Re-inject all persistent CDP stealth hooks (call after manual reconnect).
    async fn inject_persistent_stealth(&self) -> Result<(), Box<dyn Error>>;

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// Create a new fully stealthed driver with default config.
    async fn new() -> Self;

    // ── Navigation ───────────────────────────────────────────────────────────

    /// Navigate with pre/post CDC scrub.
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>>;

    /// Alias for goto.
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>>;

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

    /// Navigate back with pre/post CDC scrub.
    async fn go_back(&self) -> Result<(), Box<dyn Error>>;

    /// Navigate forward with pre/post CDC scrub.
    async fn go_forward(&self) -> Result<(), Box<dyn Error>>;

    // ── Interaction ──────────────────────────────────────────────────────────

    /// Delayed JS click (111 ms). Mirrors Python's js_utils.call_me_later.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L655
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;

    /// Bypass a Cloudflare Turnstile challenge.
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>>;

    // ── DOM helpers ──────────────────────────────────────────────────────────

    /// Full page HTML (outerHTML of <html>).
    async fn get_page_source(&self) -> Result<String, Box<dyn Error>>;

    /// True if the CSS selector matches any element in the DOM.
    async fn is_element_present(&self, css_selector: &str) -> bool;

    /// True if the CSS selector matches a visible (non-zero-size) element.
    async fn is_element_visible(&self, css_selector: &str) -> bool;

    /// Rewrite all target="_blank" links to target="_self".
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/sb_cdp.py#L1736-L1738
    async fn internalize_links(&self) -> Result<(), Box<dyn Error>>;

    /// Open a new browser window at the given URL.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L421-L428
    async fn window_new(&self, url: &str) -> Result<(), Box<dyn Error>>;

    // ── CDP overrides ────────────────────────────────────────────────────────

    /// Override User-Agent at the CDP Network layer.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setUserAgentOverride
    async fn set_user_agent(&self, ua: &str) -> Result<(), Box<dyn Error>>;

    /// Spoof timezone + geolocation via CDP Emulation domain.
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

    /// Lossless PNG screenshot via CDP Page.captureScreenshot.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-captureScreenshot
    async fn cdp_screenshot(&self) -> Result<Vec<u8>, Box<dyn Error>>;

    /// Print current page to PDF bytes via CDP Page.printToPDF.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-printToPDF
    async fn print_to_pdf(&self) -> Result<Vec<u8>, Box<dyn Error>>;

    // ── Mobile emulation ─────────────────────────────────────────────────────

    /// Enable mobile device emulation at runtime (post-startup).
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride
    async fn set_mobile_emulation(
        &self,
        width: u32,
        height: u32,
        pixel_ratio: f64,
    ) -> Result<(), Box<dyn Error>>;

    // ── Network control ──────────────────────────────────────────────────────

    /// Block ad/tracker URLs via CDP Network.setBlockedURLs.
    /// Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454
    async fn enable_ad_block(&self) -> Result<(), Box<dyn Error>>;

    /// Block a custom list of URL glob patterns.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setBlockedURLs
    async fn block_urls(&self, patterns: &[&str]) -> Result<(), Box<dyn Error>>;

    /// Bypass Content Security Policy — needed for JS injection on strict sites.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-setBypassCSP
    async fn disable_csp(&self) -> Result<(), Box<dyn Error>>;

    /// Inject Proxy-Authorization at the CDP Network layer.
    /// Ref: https://chromedevtools.github.io/devtools-protocol/tot/Network/#method-setExtraHTTPHeaders
    async fn set_proxy(&self, proxy: &str) -> Result<(), Box<dyn Error>>;

    // ── Cookies ──────────────────────────────────────────────────────────────

    /// Get all cookies as a JSON array via CDP Network.getAllCookies.
    async fn get_all_cookies(&self) -> Result<Value, Box<dyn Error>>;

    /// Set cookies from a JSON array previously returned by get_all_cookies().
    async fn set_all_cookies(&self, cookies: &Value) -> Result<(), Box<dyn Error>>;

    /// Clear all cookies via CDP Network.clearBrowserCookies.
    async fn clear_cookies(&self) -> Result<(), Box<dyn Error>>;

    // ── Internal ─────────────────────────────────────────────────────────────

    async fn borrow(&self) -> &WebDriver;
}

// ─── Chrome trait impl ────────────────────────────────────────────────────────

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

    // ── goto ──────────────────────────────────────────────────────────────────
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        // Check + delete any live CDC props before navigating
        // Ref: https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L365-L394
        let check = r#"
            let o=window,r=[];
            while(o!==null){r=r.concat(Object.getOwnPropertyNames(o));o=Object.getPrototypeOf(o);}
            return r.filter(p=>p.match(/^[a-z]{3}_[a-z]{22}_.*/i));
        "#;
        if let Ok(res) = self.execute(check, vec![]).await {
            if let Some(arr) = res.json().as_array() {
                if !arr.is_empty() {
                    if let Ok(j) = serde_json::to_string(arr) {
                        let _ = self
                            .execute(&format!("{}.forEach(p=>delete window[p]);", j), vec![])
                            .await;
                        time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
        self.get(url).await?;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── uc_get ────────────────────────────────────────────────────────────────
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>> {
        self.goto(url).await
    }

    // ── uc_open_with_tab ──────────────────────────────────────────────────────
    async fn uc_open_with_tab(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        self.execute(&format!(r#"window.open("{}","_blank");"#, url), vec![]).await?;
        time::sleep(Duration::from_millis(500)).await;
        let handles = self.windows().await?;
        if let Some(original) = handles.first() {
            self.switch_to_window(original.clone()).await?;
            self.close_window().await?;
        }
        let handles = self.windows().await?;
        if let Some(last) = handles.last() {
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
        let _ = self.remove_cdc_props().await;
        let _ = self
            .execute(&format!(r#"window.open("{}","_blank");"#, url), vec![])
            .await;
        let _ = self.clone().quit().await;
        time::sleep(Duration::from_secs_f64(timeout_secs)).await;
        for _ in 0..20 {
            if let Ok(d) =
                WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await
            {
                let handles = d.windows().await.unwrap_or_default();
                if let Some(last) = handles.last() {
                    let _ = d.switch_to_window(last.clone()).await;
                }
                inject_all_persistent_stealth(&d).await?;
                return Ok(d);
            }
            time::sleep(Duration::from_millis(250)).await;
        }
        Err("Failed to reconnect after disconnect".into())
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

    // ── uc_click ──────────────────────────────────────────────────────────────
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        self.execute(
            &format!(
                "setTimeout(()=>document.querySelector('{}').click(),111);",
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

    // ── get_page_source ───────────────────────────────────────────────────────
    async fn get_page_source(&self) -> Result<String, Box<dyn Error>> {
        let r = self
            .execute("return document.documentElement.outerHTML;", vec![])
            .await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    // ── is_element_present ────────────────────────────────────────────────────
    async fn is_element_present(&self, css_selector: &str) -> bool {
        self.find(By::Css(css_selector)).await.is_ok()
    }

    // ── is_element_visible ────────────────────────────────────────────────────
    async fn is_element_visible(&self, css_selector: &str) -> bool {
        let script = format!(
            "const e=document.querySelector('{}');if(!e)return false;\
             const r=e.getBoundingClientRect();return r.width>0&&r.height>0;",
            css_selector
        );
        self.execute(&script, vec![])
            .await
            .ok()
            .and_then(|r| r.json().as_bool())
            .unwrap_or(false)
    }

    // ── internalize_links ─────────────────────────────────────────────────────
    async fn internalize_links(&self) -> Result<(), Box<dyn Error>> {
        self.execute(
            r#"document.querySelectorAll('[target="_blank"]')
               .forEach(e=>e.setAttribute('target','_self'));"#,
            vec![],
        )
            .await?;
        Ok(())
    }

    // ── window_new ────────────────────────────────────────────────────────────
    async fn window_new(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let _ = self.remove_cdc_props().await;
        self.execute(
            &format!(
                r#"window.open("{}","_blank","location=yes,height=768,width=1024,scrollbars=yes,status=yes");"#,
                url
            ),
            vec![],
        )
            .await?;
        time::sleep(Duration::from_millis(300)).await;
        let handles = self.windows().await?;
        if let Some(last) = handles.last() {
            self.switch_to_window(last.clone()).await?;
        }
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    // ── set_user_agent ────────────────────────────────────────────────────────
    async fn set_user_agent(&self, ua: &str) -> Result<(), Box<dyn Error>> {
        ChromeDevTools::new(self.handle.clone())
            .execute_cdp_with_params(
                "Network.setUserAgentOverride",
                json!({ "userAgent": ua, "acceptLanguage": "en-US,en;q=0.9", "platform": "Win32" }),
            )
            .await?;
        Ok(())
    }

    // ── set_timezone_and_geolocation ──────────────────────────────────────────
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
            json!({ "timezoneId": timezone_id }),
        )
            .await?;
        dt.execute_cdp_with_params(
            "Emulation.setGeolocationOverride",
            json!({ "latitude": latitude, "longitude": longitude, "accuracy": accuracy }),
        )
            .await?;
        Ok(())
    }

    // ── grant_all_permissions ─────────────────────────────────────────────────
    async fn grant_all_permissions(&self) -> Result<(), Box<dyn Error>> {
        ChromeDevTools::new(self.handle.clone())
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
            .await?;
        Ok(())
    }

    // ── enable_cdp_log_capture ────────────────────────────────────────────────
    async fn enable_cdp_log_capture(&self) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp_with_params("Log.enable", json!({})).await?;
        Ok(())
    }

    // ── cdp_screenshot ────────────────────────────────────────────────────────
    async fn cdp_screenshot(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let r = ChromeDevTools::new(self.handle.clone())
            .execute_cdp_with_params(
                "Page.captureScreenshot",
                json!({ "format": "png", "fromSurface": true }),
            )
            .await?;
        let b64 = r["data"].as_str().ok_or("Missing screenshot data")?;
        Ok(general_purpose::STANDARD.decode(b64)?)
    }

    // ── print_to_pdf ──────────────────────────────────────────────────────────
    async fn print_to_pdf(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        let r = ChromeDevTools::new(self.handle.clone())
            .execute_cdp_with_params(
                "Page.printToPDF",
                json!({ "printBackground": true, "preferCSSPageSize": true }),
            )
            .await?;
        let b64 = r["data"].as_str().ok_or("Missing PDF data")?;
        Ok(general_purpose::STANDARD.decode(b64)?)
    }

    // ── set_mobile_emulation ──────────────────────────────────────────────────
    async fn set_mobile_emulation(
        &self,
        width: u32,
        height: u32,
        pixel_ratio: f64,
    ) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": width, "height": height, "deviceScaleFactor": pixel_ratio, "mobile": true }),
        )
            .await?;
        dt.execute_cdp_with_params(
            "Emulation.setTouchEmulationEnabled",
            json!({ "enabled": true, "maxTouchPoints": 5 }),
        )
            .await?;
        Ok(())
    }

    // ── enable_ad_block ───────────────────────────────────────────────────────
    async fn enable_ad_block(&self) -> Result<(), Box<dyn Error>> {
        self.block_urls(AD_BLOCK_PATTERNS).await
    }

    // ── block_urls ────────────────────────────────────────────────────────────
    async fn block_urls(&self, patterns: &[&str]) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp_with_params(
            "Network.setBlockedURLs",
            json!({ "urls": patterns }),
        )
            .await?;
        Ok(())
    }

    // ── disable_csp ───────────────────────────────────────────────────────────
    async fn disable_csp(&self) -> Result<(), Box<dyn Error>> {
        ChromeDevTools::new(self.handle.clone())
            .execute_cdp_with_params("Page.setBypassCSP", json!({ "enabled": true }))
            .await?;
        Ok(())
    }

    // ── set_proxy ─────────────────────────────────────────────────────────────
    async fn set_proxy(&self, proxy: &str) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        if proxy.contains('@') {
            let creds = proxy.split('@').next().unwrap_or("");
            let encoded = general_purpose::STANDARD.encode(creds);
            dt.execute_cdp_with_params(
                "Network.setExtraHTTPHeaders",
                json!({ "headers": { "Proxy-Authorization": format!("Basic {}", encoded) } }),
            )
                .await?;
        }
        Ok(())
    }

    // ── get_all_cookies ───────────────────────────────────────────────────────
    async fn get_all_cookies(&self) -> Result<Value, Box<dyn Error>> {
        let r = ChromeDevTools::new(self.handle.clone())
            .execute_cdp_with_params("Network.getAllCookies", json!({}))
            .await?;
        Ok(r["cookies"].clone())
    }

    // ── set_all_cookies ───────────────────────────────────────────────────────
    async fn set_all_cookies(&self, cookies: &Value) -> Result<(), Box<dyn Error>> {
        let dt = ChromeDevTools::new(self.handle.clone());
        if let Some(arr) = cookies.as_array() {
            for c in arr {
                let _ = dt.execute_cdp_with_params("Network.setCookie", c.clone()).await;
            }
        }
        Ok(())
    }

    // ── clear_cookies ─────────────────────────────────────────────────────────
    async fn clear_cookies(&self) -> Result<(), Box<dyn Error>> {
        ChromeDevTools::new(self.handle.clone())
            .execute_cdp_with_params("Network.clearBrowserCookies", json!({}))
            .await?;
        Ok(())
    }

    // ── borrow ────────────────────────────────────────────────────────────────
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
                new_content[x] =
                    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
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
            eprintln!(
                "codesign failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
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

async fn connect_to_driver_with_config(
    port: usize,
    config: &ChromeConfig,
) -> Result<WebDriver, Box<dyn Error>> {
    let mut caps = DesiredCapabilities::chrome();

    caps.set_no_sandbox()?;
    caps.set_disable_dev_shm_usage()?;
    caps.add_arg("--disable-blink-features=AutomationControlled")?;
    caps.add_arg("--disable-infobars")?;
    caps.add_arg("--no-default-browser-check")?;
    caps.add_arg("--no-first-run")?;
    caps.add_arg("--no-service-autorun")?;
    caps.add_arg("--password-store=basic")?;
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
    caps.add_arg("--enable-unsafe-extension-debugging")?;
    caps.add_arg("--disable-background-timer-throttling")?;
    caps.add_arg("--disable-backgrounding-occluded-windows")?;
    caps.add_arg("--disable-renderer-backgrounding")?;
    caps.add_arg("--disable-features=IsolateOrigins,site-per-process,Translate,InsecureDownloadWarnings,DownloadBubble,DownloadBubbleV2,OptimizationTargetPrediction,OptimizationGuideModelDownloading,SidePanelPinning,UserAgentClientHint,PrivacySandboxSettings4,ComponentUpdater")?;

    if config.headless {
        caps.add_arg("--headless=new")?;
        caps.add_arg("--window-size=1920,1080")?;
    } else {
        caps.add_arg("window-size=960,540")?;
    }

    let lang = config.lang.as_deref().unwrap_or("en-US");
    caps.add_arg(&format!("--lang={},en;q=0.9", lang))?;

    let user_agent = if config.mobile {
        config.mobile_user_agent.clone().unwrap_or_else(|| {
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Mobile Safari/537.36".to_string()
        })
    } else {
        let os = std::env::consts::OS;
        match os {
            "windows" => "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            "macos"   => "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
            _         => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        }.to_string()
    };
    caps.add_arg(&format!("--user-agent={}", user_agent))?;

    if let Some(ref proxy) = config.proxy {
        let host_port = if proxy.contains('@') {
            proxy.split('@').last().unwrap_or(proxy.as_str())
        } else {
            proxy.as_str()
        };
        caps.add_arg(&format!("--proxy-server={}", host_port))?;
    }

    let temp_dir = tempfile::Builder::new().prefix("uc_").tempdir()?;
    let profile_path = temp_dir.path().to_path_buf();
    caps.add_arg(&format!("--user-data-dir={}", profile_path.display()))?;

    let default_path = profile_path.join("Default");
    fs::create_dir_all(&default_path)?;
    let prefs_file = default_path.join("Preferences");

    let mut stealth_prefs = Value::Object(Map::new());
    if let Value::Object(ref mut map) = stealth_prefs {
        insert_nested_pref(map, "credentials_enable_service",              Value::Bool(false));
        insert_nested_pref(map, "profile.password_manager_enabled",        Value::Bool(false));
        insert_nested_pref(map, "profile.password_manager_leak_detection", Value::Bool(false));
        insert_nested_pref(map, "profile.exit_type",                       Value::Null);
        insert_nested_pref(map, "webrtc.ip_handling_policy",    Value::String("disable_non_proxied_udp".into()));
        insert_nested_pref(map, "webrtc.multiple_routes_enabled", Value::Bool(false));
        insert_nested_pref(map, "webrtc.nonproxied_udp_enabled",  Value::Bool(false));
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

    for _ in 0..20 {
        if let Ok(driver) =
            WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await
        {
            let _ = temp_dir.into_path();
            return Ok(driver);
        }
        time::sleep(Duration::from_millis(250)).await;
    }
    Err("Failed to create WebDriver".into())
}

// Kept for internal backward-compat
async fn connect_to_driver(port: usize) -> Result<WebDriver, Box<dyn Error>> {
    connect_to_driver_with_config(port, &ChromeConfig::default()).await
}

// ─── ChromeDriver download ────────────────────────────────────────────────────

async fn fetch_chromedriver() -> Result<(), Box<dyn Error>> {
    let client  = Client::new();
    let os      = std::env::consts::OS;
    let arch    = std::env::consts::ARCH;
    let version = get_chrome_version(os).await?;

    let url = if version.as_str() >= "114" {
        get_new_chrome_url(&client, &version, os, arch).await?
    } else {
        get_legacy_chrome_url(&client, &version, os, arch).await?
    };

    let bytes = client.get(&url).send().await?.bytes().await?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;

    for i in 0..archive.len() {
        let mut file    = archive.by_index(i)?;
        let outpath     = file.mangled_name();
        if file.name().ends_with('/') {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() { fs::create_dir_all(p)?; }
            }
            let mut out = fs::File::create(outpath.file_name().ok_or("Invalid file name")?)?;
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
        ("linux",   _)         => ("linux64",   "chromedriver-linux64.zip"),
        ("windows", _)         => ("win64",     "chromedriver-win64.zip"),
        ("macos",   "aarch64") => ("mac-arm64", "chromedriver-mac-arm64.zip"),
        ("macos",   _)         => ("mac-x64",   "chromedriver-mac-x64.zip"),
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
        .send().await?.text().await?;
    let zip = match (os, arch) {
        ("linux",   _)         => "chromedriver_linux64.zip",
        ("windows", _)         => "chromedriver_win32.zip",
        ("macos",   "aarch64") => return Err("macOS Apple Silicon requires Chrome ≥114".into()),
        ("macos",   _)         => "chromedriver_mac64.zip",
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
        "linux"   => Command::new("/usr/bin/google-chrome").arg("--version").output()?,
        "macos"   => Command::new(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ).arg("--version").output()?,
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