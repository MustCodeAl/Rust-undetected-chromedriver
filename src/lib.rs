use rand::prelude::*;
            use std::error::Error;
            use std::fs;
            use std::path::Path;
            use std::process::Command;
            use std::time::Duration;
            use thirtyfour::prelude::*;
            use thirtyfour::{BrowserCapabilitiesHelper, ChromiumLikeCapabilities, DesiredCapabilities, WebDriver};

            /// Constants for driver filenames based on OS
            const DRIVER_NAME: &str = if cfg!(windows) { "chromedriver.exe" } else { "chromedriver" };
            const PATCHED_DRIVER_NAME: &str = if cfg!(windows) { "chromedriver_PATCHED.exe" } else { "chromedriver_PATCHED" };

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

            fn patch_chromedriver() -> Result<(), Box<dyn Error>> {
                println!("Starting ChromeDriver executable patch...");
                let file_content = fs::read(DRIVER_NAME)?;
                let mut new_content = file_content.clone();
                let mut patch_count = 0;

                // Search for "cdc_" pattern and replace subsequent bytes
                for i in 0..file_content.len().saturating_sub(3) {
                    if &file_content[i..i+4] == b"cdc_" {
                        let mut rng = rand::rng();
                        for x in i+4..i+22 {
                            new_content[x] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"[rng.random_range(0..52)];
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
                println!("Successfully wrote patched executable to '{}'!", PATCHED_DRIVER_NAME);
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
                        eprintln!("codesign failed: {}", String::from_utf8_lossy(&output.stderr));
                    }
                }
                Ok(())
            }

            fn start_driver_process(port: usize) -> Result<(), Box<dyn Error>> {
                println!("Starting chromedriver...");
                Command::new(format!("./{}", PATCHED_DRIVER_NAME))
                    .arg(format!("--port={}", port))
                    .spawn()?;
                Ok(())
            }

            async fn connect_to_driver(port: usize) -> Result<WebDriver, Box<dyn Error>> {
                let mut caps = DesiredCapabilities::chrome();
                caps.set_no_sandbox()?;
                caps.set_disable_dev_shm_usage()?;
                caps.add_arg("--disable-blink-features=AutomationControlled")?;
                caps.add_arg("window-size=960,540")?;
                caps.add_arg("user-agent=Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/102.0.0.0 Safari/537.36")?;
                caps.add_arg("disable-infobars")?;
                caps.insert_browser_option("excludeSwitches", ["enable-automation"])?;

                for _ in 0..20 {
                    // Standard way to connect using thirtyfour as documented
                    if let Ok(driver) = WebDriver::new(&format!("http://localhost:{}", port), caps.clone()).await {
                        return Ok(driver);
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }

                Err("Failed to create WebDriver".into())
            }

            async fn fetch_chromedriver() -> Result<(), Box<dyn Error>> {
                let client = reqwest::Client::new();
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

            async fn get_new_chrome_url(client: &reqwest::Client, version: &str, os: &str, arch: &str) -> Result<String, Box<dyn Error>> {
                let url = "https://googlechromelabs.github.io/chrome-for-testing/latest-versions-per-milestone.json";
                let json: serde_json::Value = client.get(url).send().await?.json().await?;
                let full_version = json["milestones"][version]["version"].as_str().ok_or("Version not found")?;

                let (platform, zip_name) = match (os, arch) {
                    ("linux", _) => ("linux64", "chromedriver-linux64.zip"),
                    ("windows", _) => ("win64", "chromedriver-win64.zip"),
                    ("macos", "aarch64") => ("mac-arm64", "chromedriver-mac-arm64.zip"),
                    ("macos", _) => ("mac-x64", "chromedriver-mac-x64.zip"),
                    _ => return Err("Unsupported OS".into()),
                };

                Ok(format!("https://storage.googleapis.com/chrome-for-testing-public/{}/{}/{}", full_version, platform, zip_name))
            }

            async fn get_legacy_chrome_url(client: &reqwest::Client, version: &str, os: &str, arch: &str) -> Result<String, Box<dyn Error>> {
                let url = format!("https://chromedriver.storage.googleapis.com/LATEST_RELEASE_{}", version);
                let latest_release = client.get(url).send().await?.text().await?;

                let zip_name = match (os, arch) {
                    ("linux", _) => "chromedriver_linux64.zip",
                    ("windows", _) => "chromedriver_win32.zip",
                    ("macos", "aarch64") => return Err("MacOS on Apple Silicon with < Chrome 114 not supported!".into()),
                    ("macos", _) => "chromedriver_mac64.zip",
                    _ => return Err("Unsupported OS".into()),
                };

                Ok(format!("https://chromedriver.storage.googleapis.com/{}/{}", latest_release, zip_name))
            }

            async fn get_chrome_version(os: &str) -> Result<String, Box<dyn Error>> {
                println!("Getting installed Chrome version...");
                let output = match os {
                    "linux" => Command::new("/usr/bin/google-chrome").arg("--version").output()?,
                    "macos" => Command::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome").arg("--version").output()?,
                    "windows" => Command::new("powershell").args(&["-c", "(Get-Item 'C:/Program Files/Google/Chrome/Application/chrome.exe').VersionInfo"]).output()?,
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
                async fn new() -> Self;
                async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>>;
                async fn borrow(&self) -> &WebDriver;
                async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>>;
            }

            #[async_trait::async_trait]
            impl Chrome for WebDriver {
                async fn new() -> WebDriver {
                    chrome().await.expect("Failed to create Chrome driver")
                }

                async fn bypass_cloudflare(&self, url: &str) -> Result<(), Box<dyn Error>> {
                    let driver = self.borrow().await;
                    // Navigation
                    driver.goto(url).await?;

                    // Frame switching
                    driver.enter_frame(0).await?;

                    let button = driver.find(By::XPath("/html/body//div/div[1]/div[1]/div/label/input")).await?;
                    button.wait_until().clickable().await?;

                    tokio::time::sleep(Duration::from_secs(2)).await;
                    button.click().await?;
                    Ok(())
                }

                async fn borrow(&self) -> &WebDriver {
                    self
                }

                async fn goto(&self, url: &str) -> Result<(), Box<dyn Error>> {
                    let driver = self.borrow().await;

                    // Execute script to open new window
                    driver.execute(&format!(r#"window.open("{}", "_blank");"#, url), vec![]).await?;
                    tokio::time::sleep(Duration::from_secs(3)).await;

                    let windows = driver.windows().await?;
                    if let Some(first_window) = windows.first() {
                        driver.switch_to_window(first_window.clone()).await?;
                        driver.close_window().await?;
                    }

                    let windows = driver.windows().await?;
                    if let Some(last_window) = windows.last() {
                        driver.switch_to_window(last_window.clone()).await?;
                    }
                    Ok(())
                }
            }