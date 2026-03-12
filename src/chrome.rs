//! The `Chrome` trait — complete public API for stealth browser automation.
//!
//! Implemented for [`thirtyfour::WebDriver`] and provides navigation, DOM
//! manipulation, CDP overrides, assertions, and bot-detection bypass helpers.

use crate::config::{AD_BLOCK_PATTERNS, BROWSER_PERMISSIONS};
use crate::driver;
use crate::error::ChromeError;
use crate::stealth::{inject_all_persistent_stealth, CDC_SCRUB_SCRIPT};

use base64::{engine::general_purpose, Engine};
use serde_json::{json, Value};
use std::time::Duration;
use thirtyfour::extensions::cdp::ChromeDevTools;
use thirtyfour::prelude::*;
use thirtyfour::WebDriver;
use tokio::time;

// ─── Public standalone function ───────────────────────────────────────────────

/// Canonical CF/bot bypass: open URL in new tab → quit → sleep → reconnect.
///
/// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/core/browser_launcher.py#L587-L633>
pub async fn uc_open_with_reconnect(
    driver: &WebDriver,
    url: &str,
    port: usize,
    caps: thirtyfour::ChromeCapabilities,
    reconnect_secs: f64,
) -> Result<WebDriver, ChromeError> {
    let _ = driver
        .execute(&format!(r#"window.open("{url}","_blank");"#), vec![])
        .await;
    let _ = driver.clone().quit().await;
    time::sleep(Duration::from_secs_f64(reconnect_secs)).await;

    for _ in 0..20 {
        if let Ok(d) = WebDriver::new(&format!("http://localhost:{port}"), caps.clone()).await {
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
    // ── Stealth ───────────────────────────────────────────────────────────
    /// Scrub CDC props from the current page context (runtime call).
    async fn remove_cdc_props(&self) -> Result<(), ChromeError>;
    /// Re-inject all persistent CDP stealth hooks (call after manual reconnect).
    async fn inject_persistent_stealth(&self) -> Result<(), ChromeError>;

    // ── Lifecycle ─────────────────────────────────────────────────────────
    async fn new() -> Self;
    async fn borrow(&self) -> &WebDriver;

    // ── Navigation ────────────────────────────────────────────────────────
    /// Navigate with pre/post CDC scrub.
    async fn goto(&self, url: &str) -> Result<(), ChromeError>;
    /// Double-scrub alias for goto.
    async fn uc_get(&self, url: &str) -> Result<(), ChromeError>;
    /// Reload with pre/post CDC scrub.
    async fn refresh(&self) -> Result<(), ChromeError>;
    /// Navigate back with CDC scrub.
    async fn go_back(&self) -> Result<(), ChromeError>;
    /// Navigate forward with CDC scrub.
    async fn go_forward(&self) -> Result<(), ChromeError>;
    /// Open URL in a new tab, close the original.
    async fn uc_open_with_tab(&self, url: &str) -> Result<(), ChromeError>;
    /// Open URL in new tab → quit session → sleep → reconnect.
    async fn uc_open_with_disconnect(
        &self,
        url: &str,
        timeout_secs: f64,
        port: usize,
        caps: thirtyfour::ChromeCapabilities,
    ) -> Result<WebDriver, ChromeError>;
    async fn get_current_url(&self) -> Result<String, ChromeError>;
    async fn get_title(&self) -> Result<String, ChromeError>;
    async fn get_page_source(&self) -> Result<String, ChromeError>;

    // ── Cloudflare bypass ─────────────────────────────────────────────────
    async fn bypass_cloudflare(&self, url: &str) -> Result<(), ChromeError>;

    // ── Clicking ──────────────────────────────────────────────────────────
    /// 111 ms delayed JS click. Mirrors Python's `js_utils.call_me_later`.
    async fn uc_click(&self, css_selector: &str) -> Result<(), ChromeError>;
    /// Click without failing if element is not visible.
    async fn click_if_visible(&self, css_selector: &str);

    // ── Form / typing ─────────────────────────────────────────────────────
    /// Focus + clear + native `send_keys`.
    async fn send_keys(&self, css_selector: &str, text: &str) -> Result<(), ChromeError>;
    /// Set `.value` via React-compatible native setter + input/change events.
    async fn set_value(&self, css_selector: &str, value: &str) -> Result<(), ChromeError>;
    async fn clear_input(&self, css_selector: &str) -> Result<(), ChromeError>;
    /// Dispatch Enter keydown — submits forms without clicking a button.
    async fn submit(&self, css_selector: &str) -> Result<(), ChromeError>;

    // ── Scroll ────────────────────────────────────────────────────────────
    async fn scroll_to_top(&self) -> Result<(), ChromeError>;
    async fn scroll_to_bottom(&self) -> Result<(), ChromeError>;
    async fn scroll_to_y(&self, y: i64) -> Result<(), ChromeError>;
    async fn scroll_by_y(&self, y: i64) -> Result<(), ChromeError>;
    async fn scroll_into_view(&self, css_selector: &str) -> Result<(), ChromeError>;

    // ── DOM mutation ──────────────────────────────────────────────────────
    async fn remove_element(&self, css_selector: &str) -> Result<(), ChromeError>;
    async fn remove_elements(&self, css_selector: &str) -> Result<(), ChromeError>;
    async fn set_attribute(
        &self,
        css_selector: &str,
        attr: &str,
        val: &str,
    ) -> Result<(), ChromeError>;
    async fn get_attribute(
        &self,
        css_selector: &str,
        attr: &str,
    ) -> Result<Option<String>, ChromeError>;
    async fn get_text(&self, css_selector: &str) -> Result<String, ChromeError>;
    /// Rewrite all `target="_blank"` links to `target="_self"`.
    async fn internalize_links(&self) -> Result<(), ChromeError>;
    /// Open a new browser window at URL.
    async fn window_new(&self, url: &str) -> Result<(), ChromeError>;

    // ── Visibility helpers ────────────────────────────────────────────────
    async fn is_element_visible(&self, css_selector: &str) -> bool;
    async fn is_element_present(&self, css_selector: &str) -> bool;

    // ── Checkbox helpers ──────────────────────────────────────────────────
    async fn is_checked(&self, css_selector: &str) -> bool;
    async fn check_if_unchecked(&self, css_selector: &str) -> Result<(), ChromeError>;
    async fn uncheck_if_checked(&self, css_selector: &str) -> Result<(), ChromeError>;

    // ── Mouse / gesture ───────────────────────────────────────────────────
    /// CDP `Input.dispatchMouseEvent` hover — works headless.
    async fn hover_element(&self, css_selector: &str) -> Result<(), ChromeError>;
    /// CDP drag from one selector to another.
    async fn drag_and_drop(&self, drag_sel: &str, drop_sel: &str) -> Result<(), ChromeError>;
    /// CDP drag between raw viewport coordinates.
    async fn drag_and_drop_points(
        &self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> Result<(), ChromeError>;

    // ── Window geometry ───────────────────────────────────────────────────
    async fn get_window_rect(&self) -> Result<Value, ChromeError>;
    async fn set_window_rect(&self, x: i64, y: i64, w: i64, h: i64) -> Result<(), ChromeError>;
    async fn maximize(&self) -> Result<(), ChromeError>;
    async fn minimize(&self) -> Result<(), ChromeError>;

    // ── localStorage ──────────────────────────────────────────────────────
    async fn get_local_storage_item(&self, key: &str) -> Result<Option<String>, ChromeError>;
    async fn set_local_storage_item(&self, key: &str, val: &str) -> Result<(), ChromeError>;
    async fn remove_local_storage_item(&self, key: &str) -> Result<(), ChromeError>;
    async fn clear_local_storage(&self) -> Result<(), ChromeError>;

    // ── sessionStorage ────────────────────────────────────────────────────
    async fn get_session_storage_item(&self, key: &str) -> Result<Option<String>, ChromeError>;
    async fn set_session_storage_item(&self, key: &str, val: &str) -> Result<(), ChromeError>;
    async fn remove_session_storage_item(&self, key: &str) -> Result<(), ChromeError>;
    async fn clear_session_storage(&self) -> Result<(), ChromeError>;

    // ── Cookies ───────────────────────────────────────────────────────────
    async fn get_all_cookies(&self) -> Result<Value, ChromeError>;
    /// Restore cookies from a `Value` returned by `get_all_cookies`.
    async fn set_all_cookies(&self, cookies: &Value) -> Result<(), ChromeError>;
    async fn get_cookie_string(&self) -> Result<String, ChromeError>;
    async fn clear_cookies(&self) -> Result<(), ChromeError>;

    // ── Page capture ──────────────────────────────────────────────────────
    /// Lossless PNG screenshot via CDP. Returns raw bytes.
    async fn cdp_screenshot(&self) -> Result<Vec<u8>, ChromeError>;
    /// Save screenshot PNG to `path`.
    async fn save_screenshot(&self, path: &str) -> Result<(), ChromeError>;
    /// Print page to PDF bytes via CDP.
    async fn print_to_pdf(&self) -> Result<Vec<u8>, ChromeError>;
    /// Save PDF to `path`.
    async fn save_pdf(&self, path: &str) -> Result<(), ChromeError>;

    // ── CDP overrides ─────────────────────────────────────────────────────
    /// Override User-Agent at the CDP Network layer.
    async fn set_user_agent(&self, ua: &str) -> Result<(), ChromeError>;
    /// Spoof timezone + geolocation.
    async fn set_timezone_and_geolocation(
        &self,
        timezone_id: &str,
        latitude: f64,
        longitude: f64,
        accuracy: f64,
    ) -> Result<(), ChromeError>;
    /// Grant all common browser permissions so prompts never appear.
    async fn grant_all_permissions(&self) -> Result<(), ChromeError>;
    /// Enable CDP Network + Log domains for request/event capture.
    async fn enable_cdp_log_capture(&self) -> Result<(), ChromeError>;

    // ── Mobile emulation ──────────────────────────────────────────────────
    /// Enable mobile device emulation at runtime (post-startup).
    async fn set_mobile_emulation(
        &self,
        width: u32,
        height: u32,
        pixel_ratio: f64,
    ) -> Result<(), ChromeError>;

    // ── Network control ───────────────────────────────────────────────────
    /// Block ad/tracker URLs via CDP `Network.setBlockedURLs`.
    async fn enable_ad_block(&self) -> Result<(), ChromeError>;
    /// Block a custom list of URL glob patterns via CDP.
    async fn block_urls(&self, patterns: &[&str]) -> Result<(), ChromeError>;
    /// JS-layer fetch/XHR override injected via `Page.addScriptToEvaluateOnNewDocument`.
    async fn block_url_patterns(&self, patterns: &[&str]) -> Result<(), ChromeError>;
    /// Bypass CSP — needed for JS injection on strict sites.
    async fn disable_csp(&self) -> Result<(), ChromeError>;
    /// Inject Proxy-Authorization at the CDP Network layer.
    async fn set_proxy(&self, proxy: &str) -> Result<(), ChromeError>;

    // ── Wait / poll ───────────────────────────────────────────────────────
    async fn wait_for_element(
        &self,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), ChromeError>;
    async fn wait_for_text(
        &self,
        text: &str,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), ChromeError>;

    // ── Text assertions ───────────────────────────────────────────────────
    async fn assert_text(&self, text: &str, css_selector: &str) -> Result<(), ChromeError>;
    async fn assert_exact_text(&self, text: &str, css_selector: &str) -> Result<(), ChromeError>;
    async fn assert_text_not_visible(
        &self,
        text: &str,
        css_selector: &str,
    ) -> Result<(), ChromeError>;

    // ── Title / URL assertions ────────────────────────────────────────────
    async fn assert_title(&self, title: &str) -> Result<(), ChromeError>;
    async fn assert_title_contains(&self, s: &str) -> Result<(), ChromeError>;
    async fn assert_url(&self, url: &str) -> Result<(), ChromeError>;
    async fn assert_url_contains(&self, s: &str) -> Result<(), ChromeError>;

    // ── Text search ───────────────────────────────────────────────────────
    async fn find_elements_by_text(
        &self,
        text: &str,
        tag: Option<&str>,
    ) -> Result<Vec<String>, ChromeError>;

    // ── Alert handling ────────────────────────────────────────────────────
    async fn wait_for_and_accept_alert(&self, timeout_secs: f64) -> Result<(), ChromeError>;
    async fn wait_for_and_dismiss_alert(&self, timeout_secs: f64) -> Result<(), ChromeError>;

    // ── Ergonomics ────────────────────────────────────────────────────────
    async fn sleep(&self, secs: f64);
}

// ─── impl Chrome for WebDriver ───────────────────────────────────────────────

#[async_trait::async_trait]
impl Chrome for WebDriver {
    async fn remove_cdc_props(&self) -> Result<(), ChromeError> {
        let _ = self.execute(CDC_SCRUB_SCRIPT, vec![]).await;
        Ok(())
    }

    async fn inject_persistent_stealth(&self) -> Result<(), ChromeError> {
        inject_all_persistent_stealth(self).await
    }

    async fn new() -> WebDriver {
        driver::chrome()
            .await
            .expect("Failed to create Chrome driver")
    }

    async fn borrow(&self) -> &WebDriver {
        self
    }

    // ── Navigation ────────────────────────────────────────────────────────

    async fn goto(&self, url: &str) -> Result<(), ChromeError> {
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

    async fn uc_get(&self, url: &str) -> Result<(), ChromeError> {
        let _ = self.remove_cdc_props().await;
        self.goto(url).await?;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    async fn refresh(&self) -> Result<(), ChromeError> {
        let _ = self.remove_cdc_props().await;
        self.execute("window.location.reload();", vec![]).await?;
        time::sleep(Duration::from_millis(300)).await;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    async fn go_back(&self) -> Result<(), ChromeError> {
        let _ = self.remove_cdc_props().await;
        self.execute("window.history.back();", vec![]).await?;
        time::sleep(Duration::from_millis(300)).await;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    async fn go_forward(&self) -> Result<(), ChromeError> {
        let _ = self.remove_cdc_props().await;
        self.execute("window.history.forward();", vec![]).await?;
        time::sleep(Duration::from_millis(300)).await;
        let _ = self.remove_cdc_props().await;
        Ok(())
    }

    async fn uc_open_with_tab(&self, url: &str) -> Result<(), ChromeError> {
        self.execute(&format!(r#"window.open("{url}","_blank");"#), vec![])
            .await?;
        time::sleep(Duration::from_secs(1)).await;
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

    async fn uc_open_with_disconnect(
        &self,
        url: &str,
        timeout_secs: f64,
        port: usize,
        caps: thirtyfour::ChromeCapabilities,
    ) -> Result<WebDriver, ChromeError> {
        uc_open_with_reconnect(self, url, port, caps, timeout_secs).await
    }

    async fn get_current_url(&self) -> Result<String, ChromeError> {
        let r = self.execute("return window.location.href;", vec![]).await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    async fn get_title(&self) -> Result<String, ChromeError> {
        let r = self.execute("return document.title;", vec![]).await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    async fn get_page_source(&self) -> Result<String, ChromeError> {
        let r = self
            .execute("return document.documentElement.outerHTML;", vec![])
            .await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    // ── Cloudflare bypass ─────────────────────────────────────────────────

    async fn bypass_cloudflare(&self, url: &str) -> Result<(), ChromeError> {
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

    // ── Clicking ──────────────────────────────────────────────────────────

    async fn uc_click(&self, css_selector: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!("setTimeout(()=>document.querySelector('{css_selector}').click(),111);"),
            vec![],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    }

    async fn click_if_visible(&self, css_selector: &str) {
        if self.is_element_visible(css_selector).await {
            let _ = self.uc_click(css_selector).await;
        }
    }

    // ── Form / typing ─────────────────────────────────────────────────────

    async fn send_keys(&self, css_selector: &str, text: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!(
                "const e=document.querySelector('{css_selector}');if(e){{e.focus();e.value='';}}"
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

    async fn set_value(&self, css_selector: &str, value: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!(
                r#"
            const e=document.querySelector('{css_selector}');
            if(e){{
                const s=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;
                s.call(e,'{value}');
                e.dispatchEvent(new Event('input',{{bubbles:true}}));
                e.dispatchEvent(new Event('change',{{bubbles:true}}));
            }}"#
            ),
            vec![],
        )
        .await?;
        Ok(())
    }

    async fn clear_input(&self, css_selector: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!("const e=document.querySelector('{css_selector}');if(e)e.value='';"),
            vec![],
        )
        .await?;
        Ok(())
    }

    async fn submit(&self, css_selector: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!(
                r#"
            const e=document.querySelector('{css_selector}');
            if(e)e.dispatchEvent(new KeyboardEvent('keydown',
                {{key:'Enter',keyCode:13,code:'Enter',which:13,bubbles:true}}));"#
            ),
            vec![],
        )
        .await?;
        Ok(())
    }

    // ── Scroll ────────────────────────────────────────────────────────────

    async fn scroll_to_top(&self) -> Result<(), ChromeError> {
        self.execute("window.scrollTo(0,0);", vec![]).await?;
        Ok(())
    }

    async fn scroll_to_bottom(&self) -> Result<(), ChromeError> {
        self.execute("window.scrollTo(0,document.body.scrollHeight);", vec![])
            .await?;
        Ok(())
    }

    async fn scroll_to_y(&self, y: i64) -> Result<(), ChromeError> {
        self.execute(&format!("window.scrollTo(0,{y});"), vec![])
            .await?;
        Ok(())
    }

    async fn scroll_by_y(&self, y: i64) -> Result<(), ChromeError> {
        self.execute(&format!("window.scrollBy(0,{y});"), vec![])
            .await?;
        Ok(())
    }

    async fn scroll_into_view(&self, css_selector: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!(
                "const e=document.querySelector('{css_selector}');\
                 if(e)e.scrollIntoView({{behavior:'smooth',block:'center'}});"
            ),
            vec![],
        )
        .await?;
        Ok(())
    }

    // ── DOM mutation ──────────────────────────────────────────────────────

    async fn remove_element(&self, css_selector: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!("const e=document.querySelector('{css_selector}');if(e)e.remove();"),
            vec![],
        )
        .await?;
        Ok(())
    }

    async fn remove_elements(&self, css_selector: &str) -> Result<(), ChromeError> {
        self.execute(
            &format!("document.querySelectorAll('{css_selector}').forEach(e=>e.remove());"),
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
    ) -> Result<(), ChromeError> {
        self.execute(
            &format!(
                "const e=document.querySelector('{css_selector}');if(e)e.setAttribute('{attr}','{val}');"
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
    ) -> Result<Option<String>, ChromeError> {
        let r = self
            .execute(
                &format!(
                    "const e=document.querySelector('{css_selector}');return e?e.getAttribute('{attr}'):null;"
                ),
                vec![],
            )
            .await?;
        Ok(r.json().as_str().map(|s| s.to_string()))
    }

    async fn get_text(&self, css_selector: &str) -> Result<String, ChromeError> {
        let r = self
            .execute(
                &format!(
                    "const e=document.querySelector('{css_selector}');return e?e.innerText.trim():'';"
                ),
                vec![],
            )
            .await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    async fn internalize_links(&self) -> Result<(), ChromeError> {
        self.execute(
            "document.querySelectorAll('a[target=\"_blank\"]').forEach(a=>a.target='_self');",
            vec![],
        )
        .await?;
        Ok(())
    }

    async fn window_new(&self, url: &str) -> Result<(), ChromeError> {
        self.execute(&format!(r#"window.open("{url}","_blank");"#), vec![])
            .await?;
        Ok(())
    }

    // ── Visibility ────────────────────────────────────────────────────────

    async fn is_element_visible(&self, css_selector: &str) -> bool {
        self.execute(
            &format!(
                r#"
            const e=document.querySelector('{css_selector}');
            if(!e)return false;
            const s=window.getComputedStyle(e);
            return s.display!=='none'&&s.visibility!=='hidden'&&s.opacity!=='0'&&e.offsetWidth>0;"#
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
            &format!("return document.querySelector('{css_selector}')!==null;"),
            vec![],
        )
        .await
        .ok()
        .and_then(|r| r.json().as_bool())
        .unwrap_or(false)
    }

    // ── Checkbox ──────────────────────────────────────────────────────────

    async fn is_checked(&self, css_selector: &str) -> bool {
        self.execute(
            &format!("const e=document.querySelector('{css_selector}');return e?e.checked:false;"),
            vec![],
        )
        .await
        .ok()
        .and_then(|r| r.json().as_bool())
        .unwrap_or(false)
    }

    async fn check_if_unchecked(&self, css_selector: &str) -> Result<(), ChromeError> {
        if !self.is_checked(css_selector).await {
            self.uc_click(css_selector).await?;
        }
        Ok(())
    }

    async fn uncheck_if_checked(&self, css_selector: &str) -> Result<(), ChromeError> {
        if self.is_checked(css_selector).await {
            self.uc_click(css_selector).await?;
        }
        Ok(())
    }

    // ── Mouse / gesture ───────────────────────────────────────────────────

    async fn hover_element(&self, css_selector: &str) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let pos = self
            .execute(
                &format!(
                    "const e=document.querySelector('{css_selector}');if(!e)return null;\
                     const r=e.getBoundingClientRect();return [r.left+r.width/2,r.top+r.height/2];"
                ),
                vec![],
            )
            .await?;
        let x = pos.json()[0]
            .as_f64()
            .ok_or("hover: missing x coordinate")?;
        let y = pos.json()[1]
            .as_f64()
            .ok_or("hover: missing y coordinate")?;
        dt.execute_cdp_with_params(
            "Input.dispatchMouseEvent",
            json!({"type":"mouseMoved","x":x,"y":y,"button":"none"}),
        )
        .await?;
        time::sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    async fn drag_and_drop(&self, drag_sel: &str, drop_sel: &str) -> Result<(), ChromeError> {
        let center_js = |sel: &str| {
            format!(
                "const e=document.querySelector('{sel}');if(!e)return null;\
                 const r=e.getBoundingClientRect();return [r.left+r.width/2,r.top+r.height/2];"
            )
        };
        let r1 = self.execute(&center_js(drag_sel), vec![]).await?;
        let r2 = self.execute(&center_js(drop_sel), vec![]).await?;
        self.drag_and_drop_points(
            r1.json()[0].as_f64().ok_or("drag: missing x1")?,
            r1.json()[1].as_f64().ok_or("drag: missing y1")?,
            r2.json()[0].as_f64().ok_or("drag: missing x2")?,
            r2.json()[1].as_f64().ok_or("drag: missing y2")?,
        )
        .await
    }

    async fn drag_and_drop_points(
        &self,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Input.dispatchMouseEvent",
            json!({"type":"mousePressed","x":x1,"y":y1,"button":"left","clickCount":1}),
        )
        .await?;
        time::sleep(Duration::from_millis(50)).await;

        const DRAG_STEPS: usize = 10;
        for step in 1..=DRAG_STEPS {
            let t = step as f64 / DRAG_STEPS as f64;
            dt.execute_cdp_with_params(
                "Input.dispatchMouseEvent",
                json!({
                    "type":"mouseMoved",
                    "x": x1 + (x2 - x1) * t,
                    "y": y1 + (y2 - y1) * t,
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

    // ── Window geometry ───────────────────────────────────────────────────

    async fn get_window_rect(&self) -> Result<Value, ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
            .await?;
        let wid = r["windowId"].clone();
        let b = dt
            .execute_cdp_with_params("Browser.getWindowBounds", json!({"windowId": wid}))
            .await?;
        Ok(b["bounds"].clone())
    }

    async fn set_window_rect(&self, x: i64, y: i64, w: i64, h: i64) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
            .await?;
        let wid = r["windowId"].clone();
        dt.execute_cdp_with_params(
            "Browser.setWindowBounds",
            json!({
                "windowId": wid,
                "bounds": {"left":x,"top":y,"width":w,"height":h,"windowState":"normal"}
            }),
        )
        .await?;
        Ok(())
    }

    async fn maximize(&self) -> Result<(), ChromeError> {
        set_window_state(self, "maximized").await
    }

    async fn minimize(&self) -> Result<(), ChromeError> {
        set_window_state(self, "minimized").await
    }

    // ── localStorage ──────────────────────────────────────────────────────

    async fn get_local_storage_item(&self, key: &str) -> Result<Option<String>, ChromeError> {
        let r = self
            .execute(&format!("return localStorage.getItem('{key}');"), vec![])
            .await?;
        Ok(r.json().as_str().map(|s| s.to_string()))
    }

    async fn set_local_storage_item(&self, key: &str, val: &str) -> Result<(), ChromeError> {
        self.execute(&format!("localStorage.setItem('{key}','{val}');"), vec![])
            .await?;
        Ok(())
    }

    async fn remove_local_storage_item(&self, key: &str) -> Result<(), ChromeError> {
        self.execute(&format!("localStorage.removeItem('{key}');"), vec![])
            .await?;
        Ok(())
    }

    async fn clear_local_storage(&self) -> Result<(), ChromeError> {
        self.execute("localStorage.clear();", vec![]).await?;
        Ok(())
    }

    // ── sessionStorage ────────────────────────────────────────────────────

    async fn get_session_storage_item(&self, key: &str) -> Result<Option<String>, ChromeError> {
        let r = self
            .execute(&format!("return sessionStorage.getItem('{key}');"), vec![])
            .await?;
        Ok(r.json().as_str().map(|s| s.to_string()))
    }

    async fn set_session_storage_item(&self, key: &str, val: &str) -> Result<(), ChromeError> {
        self.execute(&format!("sessionStorage.setItem('{key}','{val}');"), vec![])
            .await?;
        Ok(())
    }

    async fn remove_session_storage_item(&self, key: &str) -> Result<(), ChromeError> {
        self.execute(&format!("sessionStorage.removeItem('{key}');"), vec![])
            .await?;
        Ok(())
    }

    async fn clear_session_storage(&self) -> Result<(), ChromeError> {
        self.execute("sessionStorage.clear();", vec![]).await?;
        Ok(())
    }

    // ── Cookies ───────────────────────────────────────────────────────────

    async fn get_all_cookies(&self) -> Result<Value, ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Network.getAllCookies", json!({}))
            .await?;
        Ok(r["cookies"].clone())
    }

    async fn set_all_cookies(&self, cookies: &Value) -> Result<(), ChromeError> {
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

    async fn get_cookie_string(&self) -> Result<String, ChromeError> {
        let r = self.execute("return document.cookie;", vec![]).await?;
        Ok(r.json().as_str().unwrap_or("").to_string())
    }

    async fn clear_cookies(&self) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params("Network.clearBrowserCookies", json!({}))
            .await?;
        Ok(())
    }

    // ── Page capture ──────────────────────────────────────────────────────

    async fn cdp_screenshot(&self) -> Result<Vec<u8>, ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params(
                "Page.captureScreenshot",
                json!({"format":"png","captureBeyondViewport":false}),
            )
            .await?;
        let b64 = r["data"].as_str().ok_or("screenshot: missing data field")?;
        Ok(general_purpose::STANDARD.decode(b64)?)
    }

    async fn save_screenshot(&self, path: &str) -> Result<(), ChromeError> {
        let bytes = self.cdp_screenshot().await?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    async fn print_to_pdf(&self) -> Result<Vec<u8>, ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let r = dt
            .execute_cdp_with_params("Page.printToPDF", json!({"printBackground":true}))
            .await?;
        let b64 = r["data"].as_str().ok_or("pdf: missing data field")?;
        Ok(general_purpose::STANDARD.decode(b64)?)
    }

    async fn save_pdf(&self, path: &str) -> Result<(), ChromeError> {
        let bytes = self.print_to_pdf().await?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    // ── CDP overrides ─────────────────────────────────────────────────────

    async fn set_user_agent(&self, ua: &str) -> Result<(), ChromeError> {
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
    ) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Emulation.setTimezoneOverride",
            json!({"timezoneId": timezone_id}),
        )
        .await?;
        dt.execute_cdp_with_params(
            "Emulation.setGeolocationOverride",
            json!({"latitude": latitude, "longitude": longitude, "accuracy": accuracy}),
        )
        .await?;
        Ok(())
    }

    async fn grant_all_permissions(&self) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params(
            "Browser.grantPermissions",
            json!({ "permissions": BROWSER_PERMISSIONS }),
        )
        .await?;
        Ok(())
    }

    async fn enable_cdp_log_capture(&self) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp("Log.enable").await?;
        Ok(())
    }

    // ── Mobile emulation ──────────────────────────────────────────────────

    async fn set_mobile_emulation(
        &self,
        width: u32,
        height: u32,
        pixel_ratio: f64,
    ) -> Result<(), ChromeError> {
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

    // ── Network control ───────────────────────────────────────────────────

    async fn enable_ad_block(&self) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp_with_params("Network.setBlockedURLs", json!({"urls": AD_BLOCK_PATTERNS}))
            .await?;
        Ok(())
    }

    async fn block_urls(&self, patterns: &[&str]) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp("Network.enable").await?;
        dt.execute_cdp_with_params("Network.setBlockedURLs", json!({"urls": patterns}))
            .await?;
        Ok(())
    }

    async fn block_url_patterns(&self, patterns: &[&str]) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        let list = patterns
            .iter()
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(",");
        let src = format!(
            r#"(()=>{{
            const B=[{list}];
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
        }})();"#
        );
        dt.execute_cdp_with_params(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": src}),
        )
        .await?;
        Ok(())
    }

    async fn disable_csp(&self) -> Result<(), ChromeError> {
        let dt = ChromeDevTools::new(self.handle.clone());
        dt.execute_cdp_with_params("Page.setBypassCSP", json!({"enabled": true}))
            .await?;
        Ok(())
    }

    async fn set_proxy(&self, proxy: &str) -> Result<(), ChromeError> {
        if let Some(creds) = proxy.split('@').next().filter(|_| proxy.contains('@')) {
            let encoded = general_purpose::STANDARD.encode(creds);
            let dt = ChromeDevTools::new(self.handle.clone());
            dt.execute_cdp("Network.enable").await?;
            dt.execute_cdp_with_params(
                "Network.setExtraHTTPHeaders",
                json!({"headers": {"Proxy-Authorization": format!("Basic {encoded}")}}),
            )
            .await?;
        }
        Ok(())
    }

    // ── Wait / poll ───────────────────────────────────────────────────────

    async fn wait_for_element(
        &self,
        css_selector: &str,
        timeout_secs: f64,
    ) -> Result<(), ChromeError> {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
        loop {
            if self.find(By::Css(css_selector)).await.is_ok() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(format!(
                    "wait_for_element: '{css_selector}' not found within {timeout_secs}s"
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
    ) -> Result<(), ChromeError> {
        let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
        let script = format!(
            "const e=document.querySelector('{css_selector}');\
             return e?e.innerText.includes('{text}'):false;"
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
                    "wait_for_text: '{text}' not in '{css_selector}' within {timeout_secs}s"
                )
                .into());
            }
            time::sleep(Duration::from_millis(200)).await;
        }
    }

    // ── Text assertions ───────────────────────────────────────────────────

    async fn assert_text(&self, text: &str, css_selector: &str) -> Result<(), ChromeError> {
        let actual = self.get_text(css_selector).await?;
        if actual.contains(text) {
            Ok(())
        } else {
            Err(format!("assert_text: '{text}' not in '{css_selector}'. Got: '{actual}'").into())
        }
    }

    async fn assert_exact_text(&self, text: &str, css_selector: &str) -> Result<(), ChromeError> {
        let actual = self.get_text(css_selector).await?;
        if actual.trim() == text.trim() {
            Ok(())
        } else {
            Err(
                format!("assert_exact_text '{css_selector}': expected '{text}', got '{actual}'")
                    .into(),
            )
        }
    }

    async fn assert_text_not_visible(
        &self,
        text: &str,
        css_selector: &str,
    ) -> Result<(), ChromeError> {
        let actual = self.get_text(css_selector).await.unwrap_or_default();
        if !actual.contains(text) {
            Ok(())
        } else {
            Err(format!("assert_text_not_visible: '{text}' IS visible in '{css_selector}'").into())
        }
    }

    // ── Title / URL assertions ────────────────────────────────────────────

    async fn assert_title(&self, title: &str) -> Result<(), ChromeError> {
        let actual = self.get_title().await?;
        if actual == title {
            Ok(())
        } else {
            Err(format!("assert_title: expected '{title}', got '{actual}'").into())
        }
    }

    async fn assert_title_contains(&self, s: &str) -> Result<(), ChromeError> {
        let actual = self.get_title().await?;
        if actual.contains(s) {
            Ok(())
        } else {
            Err(format!("assert_title_contains: '{s}' not in title '{actual}'").into())
        }
    }

    async fn assert_url(&self, url: &str) -> Result<(), ChromeError> {
        let actual = self.get_current_url().await?;
        if actual == url {
            Ok(())
        } else {
            Err(format!("assert_url: expected '{url}', got '{actual}'").into())
        }
    }

    async fn assert_url_contains(&self, s: &str) -> Result<(), ChromeError> {
        let actual = self.get_current_url().await?;
        if actual.contains(s) {
            Ok(())
        } else {
            Err(format!("assert_url_contains: '{s}' not in url '{actual}'").into())
        }
    }

    // ── Text search ───────────────────────────────────────────────────────

    async fn find_elements_by_text(
        &self,
        text: &str,
        tag: Option<&str>,
    ) -> Result<Vec<String>, ChromeError> {
        let tag_filter = tag
            .map(|t| format!("'{}'", t.to_uppercase()))
            .unwrap_or_else(|| "null".into());
        let script = format!(
            r#"
            const search='{text}', tag={tag_filter};
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
        "#
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

    // ── Alert handling ────────────────────────────────────────────────────

    async fn wait_for_and_accept_alert(&self, timeout_secs: f64) -> Result<(), ChromeError> {
        wait_for_alert(self, true, timeout_secs).await
    }

    async fn wait_for_and_dismiss_alert(&self, timeout_secs: f64) -> Result<(), ChromeError> {
        wait_for_alert(self, false, timeout_secs).await
    }

    // ── Ergonomics ────────────────────────────────────────────────────────

    async fn sleep(&self, secs: f64) {
        time::sleep(Duration::from_secs_f64(secs)).await;
    }
}

// ─── Shared private helpers ───────────────────────────────────────────────────

/// Set the browser window to a specific state (maximized/minimized).
async fn set_window_state(driver: &WebDriver, state: &str) -> Result<(), ChromeError> {
    let dt = ChromeDevTools::new(driver.handle.clone());
    let r = dt
        .execute_cdp_with_params("Browser.getWindowForTarget", json!({}))
        .await?;
    let wid = r["windowId"].clone();
    dt.execute_cdp_with_params(
        "Browser.setWindowBounds",
        json!({"windowId": wid, "bounds": {"windowState": state}}),
    )
    .await?;
    Ok(())
}

/// Poll for a JS dialog and accept or dismiss it.
async fn wait_for_alert(
    driver: &WebDriver,
    accept: bool,
    timeout_secs: f64,
) -> Result<(), ChromeError> {
    let dt = ChromeDevTools::new(driver.handle.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs_f64(timeout_secs);
    let action = if accept { "accept" } else { "dismiss" };
    loop {
        if dt
            .execute_cdp_with_params("Page.handleJavaScriptDialog", json!({"accept": accept}))
            .await
            .is_ok()
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(
                format!("wait_for_and_{action}_alert: timed out after {timeout_secs}s").into(),
            );
        }
        time::sleep(Duration::from_millis(200)).await;
    }
}
