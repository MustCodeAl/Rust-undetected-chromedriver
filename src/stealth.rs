//! CDP stealth scripts and injection helpers.
//!
//! All scripts are registered via `Page.addScriptToEvaluateOnNewDocument` so
//! they fire before ANY page JS on every navigation, including cross-origin.

use crate::error::ChromeError;
use serde_json::json;
use thirtyfour::extensions::cdp::ChromeDevTools;
use thirtyfour::WebDriver;

/// Hides webdriver, restores `window.chrome`, spoofs permissions/plugins/languages,
/// and forces shadow roots open.
///
/// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L380-L401>
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

/// Scrubs all chromedriver CDC artefact properties from `window`.
///
/// Ref: <https://github.com/seleniumbase/SeleniumBase/blob/main/seleniumbase/undetected/__init__.py#L388-L394>
pub(crate) const CDC_SCRUB_SCRIPT: &str = r#"
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

/// Register all persistent stealth hooks so they fire before every page's JS.
pub(crate) async fn inject_all_persistent_stealth(driver: &WebDriver) -> Result<(), ChromeError> {
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
