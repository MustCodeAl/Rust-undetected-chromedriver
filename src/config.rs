//! Configuration types and constants for the undetected chromedriver.

/// Platform-specific chromedriver binary name.
pub(crate) const DRIVER_NAME: &str = if cfg!(windows) {
    "chromedriver.exe"
} else {
    "chromedriver"
};

/// Platform-specific patched binary name.
pub(crate) const PATCHED_DRIVER_NAME: &str = if cfg!(windows) {
    "chromedriver_PATCHED.exe"
} else {
    "chromedriver_PATCHED"
};

/// Ad/tracker URL patterns blocked via `Network.setBlockedURLs`.
/// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/browser.py#L433-L454>
pub(crate) const AD_BLOCK_PATTERNS: &[&str] = &[
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

/// Browser permission names granted by default so prompts never appear.
/// Ref: <https://chromedevtools.github.io/devtools-protocol/tot/Browser/#method-grantPermissions>
pub(crate) const BROWSER_PERMISSIONS: &[&str] = &[
    "geolocation",
    "notifications",
    "audioCapture",
    "videoCapture",
    "clipboardReadWrite",
    "clipboardSanitizedWrite",
    "midi",
    "midiSysex",
    "sensors",
    "backgroundSync",
    "backgroundFetch",
    "nfc",
    "displayCapture",
    "storageAccess",
    "protectedMediaIdentifier",
    "idleDetection",
];

/// Builder-style configuration for [`crate::chrome_with_config`].
///
/// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/cdp_driver/cdp_util.py#L278-L294>
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
    /// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L2240>
    pub extensions: Vec<String>,
}
