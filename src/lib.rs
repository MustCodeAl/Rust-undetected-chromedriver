use rand::prelude::*;
use reqwest::Client;
use serde_json::{Map, Value};
use std::error::Error;
use std::fs;
#[cfg(target_family = "unix")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use thirtyfour::prelude::*;
use thirtyfour::{ChromiumLikeCapabilities, DesiredCapabilities, WebDriver};
use tokio::time;

/// Constants for driver filenames based on OS
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

/// Fetches a new ChromeDriver executable and patches it to prevent detection.
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

    connect_to_driver(port).await
}

async fn inject_stealth_cdp(driver: &WebDriver) -> Result<(), Box<dyn Error>> {
    let stealth_script = r#"
        // 1. Pass the Webdriver Test
        Object.defineProperty(navigator, 'webdriver', { get: () => undefined });

        // 2. Pass the Chrome Test
        window.chrome = {
            runtime: {},
            app: {
                InstallState: {
                    DISABLED: 'disabled',
                    INSTALLED: 'installed',
                    NOT_INSTALLED: 'not_installed'
                },
                RunningState: {
                    CANNOT_RUN: 'cannot_run',
                    READY_TO_RUN: 'ready_to_run',
                    RUNNING: 'running'
                }
            }
        };

        // 3. Pass the Permissions Test
        const originalQuery = window.navigator.permissions.query;
        window.navigator.permissions.query = parameters => (
            parameters.name === 'notifications' ?
                Promise.resolve({ state: Notification.permission }) :
                originalQuery(parameters)
        );

        // 4. Pass the Plugins Length Test
        Object.defineProperty(navigator, 'plugins', { get: () => [1, 2, 3, 4, 5] });
        Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'] });
    "#;

    // Send the CDP command via thirtyfour's custom command capability
    let mut params = Map::new();
    params.insert(
        "source".to_string(),
        Value::String(stealth_script.to_string()),
    );

    // NOTE: thirtyfour might not have a direct `execute_cdp` method built-in yet depending on version,
    // but you can often route it through a custom HTTP command, or simply execute it immediately on load:
    // If native CDP is unavailable, execute it once globally right after driver creation:
    driver.execute(stealth_script, vec![]).await?;

    Ok(())
}
fn patch_chromedriver() -> Result<(), Box<dyn Error>> {
    println!("Starting ChromeDriver executable patch...");
    let file_content = fs::read(DRIVER_NAME)?;
    let mut new_content = file_content.clone();
    let mut patch_count = 0;

    // Search for "cdc_" pattern and replace subsequent bytes
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

    println!("Writing to binary file...");
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
        let output = Command::new("codesign")
            .args(&["--force", "--sign", "-", PATCHED_DRIVER_NAME])
            .output()?;
        if !output.status.success() {
            eprintln!(
                "codesign failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    Ok(())
}

fn start_driver_process(port: usize) -> Result<(), Box<dyn Error>> {
    println!("Starting detached chromedriver...");
    let mut cmd = Command::new(format!("./{}", PATCHED_DRIVER_NAME));
    cmd.arg(format!("--port={}", port));

    #[cfg(target_family = "unix")]
    cmd.process_group(0);

    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x00000008); // DETACHED_PROCESS

    cmd.spawn()?;
    Ok(())
}

/// Recursively expands dot-notation keys (e.g., "a.b.c") into nested JSON objects
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

    // 1. Core Stealth and System Arguments
    caps.set_no_sandbox()?;
    caps.set_disable_dev_shm_usage()?;
    caps.add_arg("--disable-blink-features=AutomationControlled")?;
    caps.add_arg("window-size=960,540")?;
    caps.add_arg("--disable-infobars")?; // Fixed missing '--'
    caps.add_arg("--no-default-browser-check")?;
    caps.add_arg("--no-first-run")?;
    caps.add_arg("--no-service-autorun")?;
    caps.add_arg("--password-store=basic")?;
    caps.add_arg("--profile-directory=Default")?;

    // 1.5 Stop Chrome from Phoning Home to Google (From config.py)
    caps.add_arg("--no-pings")?;
    caps.add_arg("--homepage=about:blank")?;
    caps.add_arg("--safebrowsing-disable-download-protection")?;
    caps.add_arg("--disable-client-side-phishing-detection")?;

    // Spoof the auto-updater to prevent background update checks from leaking your IP
    caps.add_arg("--simulate-outdated-no-au=\"Tue, 31 Dec 2099 23:59:59 GMT\"")?;

    // Disable intrusive autocomplete and save-data prompts
    caps.add_arg("--disable-single-click-autofill")?;
    caps.add_arg("--disable-password-generation")?;
    caps.add_arg("--disable-save-password-bubble")?;

    // 2. Dynamic User-Agent Override based on OS
    let os = std::env::consts::OS;
    let user_agent = match os {
        "windows" => "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        "macos" => "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
        _ => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36",
    };
    caps.add_arg(&format!("--user-agent={}", user_agent))?;

    // 3. Suppress UI prompts, translation bubbles, and extension warnings
    caps.add_arg("--disable-popup-blocking")?;
    caps.add_arg("--disable-translate")?;
    caps.add_arg("--disable-search-engine-choice-screen")?;
    caps.add_arg("--enable-unsafe-extension-debugging")?; // NEW: Suppress dev mode extension popups

    // 4. Language Normalization
    caps.add_arg("--lang=en-US,en;q=0.9")?;

    // 5. Mega Disable-Features List (from config.py)
    caps.add_arg("--disable-features=IsolateOrigins,site-per-process,Translate,InsecureDownloadWarnings,DownloadBubble,DownloadBubbleV2,OptimizationTargetPrediction,OptimizationGuideModelDownloading,SidePanelPinning,UserAgentClientHint,PrivacySandboxSettings4,OptimizationHintsFetching,InterestFeedContentSuggestions,Bluetooth,WebBluetooth,UnifiedWebBluetooth,ComponentUpdater,DisableLoadExtensionCommandLineSwitch,WebAuthentication,PasskeyAuth")?;

    // 6. Anti-Throttling Evasions
    caps.add_arg("--disable-background-timer-throttling")?;
    caps.add_arg("--disable-backgrounding-occluded-windows")?;
    caps.add_arg("--disable-renderer-backgrounding")?;

    // 7. Setup Profile Path (Creates a temp profile to avoid wire-fingerprinting)
    let temp_dir = tempfile::Builder::new().prefix("uc_").tempdir()?;
    let profile_path = temp_dir.path().to_path_buf();
    caps.add_arg(&format!("--user-data-dir={}", profile_path.display()))?;

    // 8. Build the Default Profile path and write Preferences directly to disk
    let default_path = profile_path.join("Default");
    fs::create_dir_all(&default_path)?;
    let prefs_file = default_path.join("Preferences");

    // Start with our stealth preferences
    let mut stealth_prefs = Value::Object(Map::new());
    if let Value::Object(ref mut map) = stealth_prefs {
        // Disables password/save prompts
        insert_nested_pref(map, "credentials_enable_service", Value::Bool(false));
        insert_nested_pref(map, "profile.password_manager_enabled", Value::Bool(false));
        insert_nested_pref(
            map,
            "profile.password_manager_leak_detection",
            Value::Bool(false),
        );
        insert_nested_pref(map, "profile.exit_type", Value::Null);

        // WebRTC Obfuscation (IP Leak Prevention)
        insert_nested_pref(
            map,
            "webrtc.ip_handling_policy",
            Value::String("disable_non_proxied_udp".to_string()),
        );
        insert_nested_pref(map, "webrtc.multiple_routes_enabled", Value::Bool(false));
        insert_nested_pref(map, "webrtc.nonproxied_udp_enabled", Value::Bool(false));

        // Uncomment the line below to block images for speed and stealth:
        // insert_nested_pref(map, "profile.managed_default_content_settings.images", Value::Number(2.into()));
    }

    // If a Preferences file already exists, read it and merge!
    if prefs_file.exists() {
        if let Ok(content) = fs::read_to_string(&prefs_file) {
            if let Ok(mut existing_prefs) = serde_json::from_str::<Value>(&content) {
                merge_json(&mut existing_prefs, stealth_prefs);
                stealth_prefs = existing_prefs;
            }
        }
    }

    // Write the securely merged preferences back to disk
    fs::write(prefs_file, serde_json::to_string(&stealth_prefs)?)?;

    // 9. Connection Retry Loop
    for _ in 0..20 {
        if let Ok(driver) =
            WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await
        {
            // Leak the tempdir so it doesn't get automatically deleted when out of scope
            let _ = temp_dir.into_path();
            return Ok(driver);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    Err("Failed to create WebDriver".into())
}
async fn fetch_chromedriver() -> Result<(), Box<dyn Error>> {
    let client = Client::new();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let installed_version = get_chrome_version(os).await?;

    let download_url = if installed_version.as_str() >= "114" {
        get_new_chrome_url(&client, &installed_version, os, arch).await?
    } else {
        get_legacy_chrome_url(&client, &installed_version, os, arch).await?
    };

    let resp = client.get(&download_url).send().await?.bytes().await?;
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
            let file_name = outpath.file_name().ok_or("Invalid file name")?;
            let mut outfile = fs::File::create(file_name)?;
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
    let url =
        "https://googlechromelabs.github.io/chrome-for-testing/latest-versions-per-milestone.json";
    let json: Value = client.get(url).send().await?.json().await?;
    let full_version = json["milestones"][version]["version"]
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
        full_version, platform, zip_name
    ))
}

async fn get_legacy_chrome_url(
    client: &Client,
    version: &str,
    os: &str,
    arch: &str,
) -> Result<String, Box<dyn Error>> {
    let url = format!(
        "https://chromedriver.storage.googleapis.com/LATEST_RELEASE_{}",
        version
    );
    let latest_release = client.get(url).send().await?.text().await?;

    let zip_name = match (os, arch) {
        ("linux", _) => "chromedriver_linux64.zip",
        ("windows", _) => "chromedriver_win32.zip",
        ("macos", "aarch64") => {
            return Err("MacOS on Apple Silicon with < Chrome 146 not supported!".into())
        }
        ("macos", _) => "chromedriver_mac64.zip",
        _ => return Err("Unsupported OS".into()),
    };

    Ok(format!(
        "https://chromedriver.storage.googleapis.com/{}/{}",
        latest_release, zip_name
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

    println!("Currently installed Chrome version: {}", version);
    Ok(version)
}

#[async_trait::async_trait]
pub trait Chrome {
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>>;

    async fn new() -> Self;
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>>;
    async fn borrow(&self) -> &WebDriver;
    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>>;
    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>>;
    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>>;
}

#[async_trait::async_trait]
impl Chrome for WebDriver {
    async fn remove_cdc_props(&self) -> Result<(), Box<dyn Error>> {
        let script = r#"
        let objectToInspect = window, result = [];
        while(objectToInspect !== null) {
            result = result.concat(Object.getOwnPropertyNames(objectToInspect));
            objectToInspect = Object.getPrototypeOf(objectToInspect);
        }
        result.forEach(p => {
            if (p.match(/^[a-z]{3}_[a-z]{22}_.*/i)) { delete window[p]; }
        });
    "#;
        let driver = self.borrow().await;
        driver.execute(script, vec![]).await?;
        Ok(())
    }

    async fn new() -> WebDriver {
        let driver = chrome().await.expect("Failed to create Chrome driver");

        // Immediately inject stealth properties into the root context
        let _ = inject_stealth_cdp(&driver).await;

        driver
    }

    async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let driver = self.borrow().await;
        // Navigation
        driver.goto(url).await?;

        // Frame switching
        driver.enter_frame(0).await?;

        let button = driver
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

    async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let driver = self.borrow().await;

        // 1. Mirror _get_cdc_props from __init__.py
        let check_script = r#"
            let objectToInspect = window, result = [];
            while(objectToInspect !== null) {
                result = result.concat(Object.getOwnPropertyNames(objectToInspect));
                objectToInspect = Object.getPrototypeOf(objectToInspect);
            }
            return result.filter(i => i.match(/^[a-z]{3}_[a-z]{22}_.*/i));
        "#;

        let props = driver.execute(check_script, vec![]).await?;
        let props_array = props.json().as_array().unwrap();

        // 2. Mirror _hook_remove_cdc_props from __init__.py
        if !props_array.is_empty() {
            let props_json = serde_json::to_string(props_array)?;
            let script = format!("{}.forEach(p => delete window[p]);", props_json);

            // Note: Since thirtyfour lacks native CDP addScriptToEvaluateOnNewDocument,
            // we execute it directly to scrub the current context before navigation.
            driver.execute(&script, vec![]).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // 3. Proceed with navigation
        driver.get(url).await?;

        // Scrub immediately after load to ensure total stealth
        self.remove_cdc_props().await?;
        Ok(())
    }

    async fn uc_get(&self, url: &str) -> Result<(), Box<dyn Error>> {
        let driver = self.borrow().await;
        // Scrub the current page before navigation starts
        let _ = self.remove_cdc_props().await;

        driver.goto(url).await?;

        // Scrub the new page immediately after load
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    async fn uc_click(&self, css_selector: &str) -> Result<(), Box<dyn Error>> {
        let driver = self.borrow().await;

        // Mirror webelement.py's js_utils.call_me_later with a 111ms delay
        let script = format!(
            "setTimeout(() => document.querySelector('{}').click(), 111);",
            css_selector
        );

        driver.execute(&script, vec![]).await?;

        // Give the delayed click time to execute
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    }
}
