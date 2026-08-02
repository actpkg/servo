/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Servo as an ACT component.
//!
//! Two ways to use it. `render` is one-shot: load a page, hand back a screenshot
//! and the DOM. A **session** keeps the page alive, so a caller can click,
//! scroll, run scripts and screenshot the result of each step — the loop a
//! browser automation tool actually needs.
//!
//! # One document per instance
//!
//! Whichever way it is used, an instance renders exactly one document. Servo
//! gives every document a script thread and every script thread creates a
//! SpiderMonkey `JSContext`; SpiderMonkey allows one per thread, and a wasip2
//! guest has one thread. So a second session, or a navigation away from the
//! first page, cannot work — both build a new pipeline. Within the one document
//! there is no such limit: scripts, clicks, scrolls and screenshots are
//! unrestricted, and same-document navigation through `history.pushState` or a
//! fragment works normally.
//!
//! # Why the runtime is shaped this way
//!
//! `std::thread::spawn` does not fail to compile on wasip2, it fails at *run
//! time* with `os error 58`. Every Servo actor therefore lives as a task on one
//! runtime rather than on its own OS thread. It has to be a `LocalSet`, not
//! plain `tokio::spawn`: the script thread owns SpiderMonkey's `JSContext` and
//! the DOM, neither of which is `Send`.
//!
//! That runtime is created once and re-entered by each tool call. Servo lives
//! between calls; its actors only run while a call is in progress, which is
//! exactly when something is waiting on them.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use act_sdk::prelude::*;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use servo::RenderingContext as _;

#[act_component]
mod component {
    use super::*;

    thread_local! {
        static SESSIONS: SessionRegistry<crate::BrowserHandle> = SessionRegistry::new("wv");
    }

    /// Args accepted by `open-session`.
    #[derive(Deserialize, JsonSchema)]
    #[schemars(crate = "act_sdk::__private::schemars")]
    #[serde(crate = "act_sdk::__private::serde")]
    pub struct OpenArgs {
        /// URL to open. `http:`, `https:` and `data:` all resolve; network
        /// access needs the `wasi:sockets` capability to be granted.
        #[serde(default)]
        url: Option<String>,
        /// HTML to open instead of a URL. Takes precedence over `url`.
        #[serde(default)]
        html: Option<String>,
        /// Viewport width in CSS pixels. Defaults to 1024.
        #[serde(default)]
        width: Option<u32>,
        /// Viewport height in CSS pixels. Defaults to 768.
        #[serde(default)]
        height: Option<u32>,
    }

    /// Tool metadata: every session tool needs to know which page it addresses.
    #[derive(Deserialize)]
    #[serde(crate = "act_sdk::__private::serde")]
    pub struct ToolMeta {
        #[serde(rename = "std:session-id")]
        session_id: String,
    }

    #[session_open]
    fn open(args: OpenArgs) -> ActResult<String> {
        crate::claim_document()?;
        let width = args.width.unwrap_or(crate::DEFAULT_WIDTH);
        let height = args.height.unwrap_or(crate::DEFAULT_HEIGHT);
        let target = crate::target_url(args.html, args.url)?;

        let browser = crate::block_on(crate::Browser::open(&target, width, height))
            .map_err(|error| ActError::internal(format!("could not open page: {error}")))?;
        Ok(SESSIONS.with(|registry| registry.insert(Rc::new(RefCell::new(browser)))))
    }

    #[session_close]
    fn close(session_id: String) {
        // Let the page go without running Servo's teardown: that waits on the
        // render backend, a task on this same runtime, which cannot answer while
        // we block on it. The instance is finished after this — see the note in
        // `open` about one page at a time.
        if let Some(handle) = SESSIONS.with(|registry| registry.remove(&session_id)) {
            if let Ok(browser) = Rc::try_unwrap(handle) {
                browser.into_inner().forget();
            }
        }
    }

    /// Look the session up and run `body` against it on the component runtime.
    fn with_session<T, F>(
        ctx: &ActContext<ToolMeta>,
        body: impl FnOnce(crate::BrowserHandle) -> F,
    ) -> ActResult<T>
    where
        F: std::future::Future<Output = ActResult<T>>,
    {
        let id = ctx.metadata().session_id.clone();
        let handle = SESSIONS
            .with(|registry| registry.with(&id, |handle| handle.clone()))
            .ok_or_else(|| ActError::session_not_found(format!("Unknown session-id: {id}")))?;
        crate::block_on(body(handle))
    }

    #[act_tool(description = "Capture the current page as a PNG", read_only)]
    fn screenshot(
        /// Milliseconds to keep driving the page before capturing, for work that
        /// continues after load. Defaults to 300.
        settle_ms: Option<u32>,
        ctx: &mut ActContext<ToolMeta>,
    ) -> ActResult<Content> {
        with_session(ctx, |handle| async move {
            let browser = handle.borrow();
            let png = browser
                .screenshot(settle_ms.unwrap_or(crate::DEFAULT_SETTLE_MS))
                .await
                .map_err(|error| ActError::internal(format!("screenshot failed: {error}")))?;
            Ok(Content("image/png", png))
        })
    }

    #[act_tool(
        description = "Return the page's DOM as HTML, as it stands after scripts have run",
        read_only
    )]
    fn dom(ctx: &mut ActContext<ToolMeta>) -> ActResult<String> {
        with_session(ctx, |handle| async move {
            let browser = handle.borrow();
            browser
                .evaluate("document.documentElement.outerHTML")
                .await
                .map_err(|error| ActError::internal(format!("DOM read failed: {error}")))
        })
    }

    #[act_tool(
        description = "Evaluate JavaScript in the page and return the result as text. This is how to reach anything the other tools do not cover: querying elements, filling fields, submitting forms. Note that loading a different document is not possible in this instance — open a new one instead"
    )]
    fn eval(
        /// JavaScript to evaluate. The value of the last expression is returned.
        script: String,
        ctx: &mut ActContext<ToolMeta>,
    ) -> ActResult<String> {
        with_session(ctx, |handle| async move {
            let browser = handle.borrow();
            browser
                .evaluate(&script)
                .await
                .map_err(|error| ActError::internal(format!("evaluation failed: {error}")))
        })
    }

    #[act_tool(description = "Click at a point in the viewport, in CSS pixels from its top-left")]
    fn click(
        /// Horizontal offset in CSS pixels. Whole pixels — round if you computed
        /// this from `getBoundingClientRect`.
        x: i32,
        /// Vertical offset in CSS pixels. Whole pixels.
        y: i32,
        ctx: &mut ActContext<ToolMeta>,
    ) -> ActResult<String> {
        with_session(ctx, |handle| async move {
            let browser = handle.borrow();
            browser.click(x as f32, y as f32).await;
            Ok(format!("clicked ({x}, {y})"))
        })
    }

    #[act_tool(description = "Scroll the page by a delta in CSS pixels")]
    fn scroll(
        /// Horizontal delta in whole CSS pixels. Positive scrolls right.
        dx: i32,
        /// Vertical delta in whole CSS pixels. Positive scrolls down.
        dy: i32,
        ctx: &mut ActContext<ToolMeta>,
    ) -> ActResult<String> {
        with_session(ctx, |handle| async move {
            let browser = handle.borrow();
            browser.scroll(dx as f64, dy as f64).await;
            Ok(format!("scrolled ({dx}, {dy})"))
        })
    }

    /// Render a page and return its screenshot and DOM, without a session.
    ///
    /// Streaming so the two results can be sent as what they are — an image and
    /// a document — rather than packed into one blob.
    #[act_tool(
        description = "Render a web page in one shot and return a PNG screenshot and the DOM after scripts have run. Open a session instead to interact with the page",
        streaming,
        read_only
    )]
    fn render(
        /// HTML to render directly. Takes precedence over `url`.
        html: Option<String>,
        /// URL to load. `http:`, `https:` and `data:` all resolve; network
        /// access needs the `wasi:sockets` capability to be granted.
        url: Option<String>,
        /// Viewport width in CSS pixels. Defaults to 1024.
        width: Option<u32>,
        /// Viewport height in CSS pixels. Defaults to 768.
        height: Option<u32>,
        /// How long to keep driving the page after it finishes loading, in
        /// milliseconds. Defaults to 300.
        settle_ms: Option<u32>,
        ctx: &mut ActContext<()>,
    ) -> ActResult<()> {
        crate::claim_document()?;
        let width = width.unwrap_or(crate::DEFAULT_WIDTH);
        let height = height.unwrap_or(crate::DEFAULT_HEIGHT);
        let settle_ms = settle_ms.unwrap_or(crate::DEFAULT_SETTLE_MS);
        let target = crate::target_url(html, url)?;

        let (png, dom) = crate::block_on(async move {
            let browser = crate::Browser::open(&target, width, height).await?;
            let png = browser.screenshot(settle_ms).await?;
            let dom = browser
                .evaluate("document.documentElement.outerHTML")
                .await
                .unwrap_or_default();
            browser.forget();
            Ok::<_, String>((png, dom))
        })
        .map_err(|error| ActError::internal(format!("render failed: {error}")))?;

        ctx.send_content(png, Some("image/png".to_owned()), Vec::new());
        ctx.send_content(dom.into_bytes(), Some("text/html".to_owned()), Vec::new());
        Ok(())
    }
}

/// The faces compiled into this component, as `(family, builtin: path, bytes)`.
///
/// DejaVu, under the Bitstream Vera licence — see `fonts/LICENSE`. They back the
/// CSS generic families; a page asking for anything else falls back to these.
const BUILTIN_FONTS: &[(&str, &str, &[u8])] = &[
    (
        "DejaVu Sans",
        "builtin:DejaVuSans",
        include_bytes!("../fonts/DejaVuSans.ttf"),
    ),
    (
        "DejaVu Serif",
        "builtin:DejaVuSerif",
        include_bytes!("../fonts/DejaVuSerif.ttf"),
    ),
    (
        "DejaVu Sans Mono",
        "builtin:DejaVuSansMono",
        include_bytes!("../fonts/DejaVuSansMono.ttf"),
    ),
];

const DEFAULT_WIDTH: u32 = 1024;
const DEFAULT_HEIGHT: u32 = 768;
/// Milliseconds spent driving the page after a load before capturing.
const DEFAULT_SETTLE_MS: u32 = 300;
/// How long to wait for a load before giving up on it.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Gap between turns of the event loop.
///
/// Sleeping rather than yielding is load-bearing: socket readiness is only
/// delivered when the runtime parks, and a runtime whose queue is never empty
/// does not park — so a busy wait starves the very network IO a page needs.
const POLL_INTERVAL: Duration = Duration::from_millis(1);

type BrowserHandle = Rc<RefCell<Browser>>;

/// The engine behind every page.
///
/// `ServoBuilder::build` initialises process-global options and panics if it
/// runs twice, so there is exactly one engine however many pages are open. That
/// is also Servo's own model: one engine, many `WebView`s — the difference
/// between a browser and a tab.
struct Engine {
    servo: servo::Servo,
    rendering_context: Rc<servo::SoftwareRenderingContext>,
}

thread_local! {
    static ENGINE: RefCell<Option<Rc<Engine>>> = const { RefCell::new(None) };
    /// Whether this instance has already had a document.
    ///
    /// Not "has one now": the limit is for the life of the instance, because a
    /// closed page's script thread is leaked rather than torn down.
    static DOCUMENT_USED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Claim this instance's one document, or explain why there is not another.
///
/// Every document Servo opens gets a script thread, and every script thread
/// creates a SpiderMonkey `JSContext`. SpiderMonkey allows one per thread and a
/// wasip2 guest has exactly one thread, so the second document dies inside
/// `JS_NewContext`. That covers a second session, and equally a navigation away
/// from the first page: both build a new pipeline. Refusing with an explanation
/// beats taking the component down.
fn claim_document() -> Result<(), ActError> {
    if DOCUMENT_USED.with(|used| used.replace(true)) {
        return Err(ActError::internal(
            "this component instance renders one document and has already used it: \
             start another instance for another page. Servo gives each document a \
             script thread, and SpiderMonkey allows one JavaScript context per \
             thread — a wasip2 guest has one thread.",
        ));
    }
    Ok(())
}

/// The engine, started on first use and sized to fit `width` x `height`.
fn engine(width: u32, height: u32) -> Result<Rc<Engine>, String> {
    ENGINE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(engine) = slot.as_ref() {
            // One surface serves every page, so it has to be at least as large
            // as the largest of them.
            let current = engine.rendering_context.size();
            if current.width < width || current.height < height {
                engine.rendering_context.resize(servo::PhysicalSize::new(
                    current.width.max(width),
                    current.height.max(height),
                ));
            }
            return Ok(engine.clone());
        }

        // `inventory` collects nothing under wasip2, so register the resource
        // reader explicitly rather than relying on life-before-main.
        servo::resources::set_resource_reader(&servo_default_resources::DefaultResourceReader);
        // There is no system font database in a guest, so the faces come from
        // here. Which faces to carry is this component's choice, which is why the
        // engine takes them rather than embedding them: see `fonts/`.
        servo::set_builtin_fonts(BUILTIN_FONTS);

        let servo = servo::ServoBuilder::default().build();
        // CPU rasterisation: swgl gives WebRender a real GL implementation
        // writing into ordinary memory. There is no GPU inside a component.
        let rendering_context = Rc::new(
            servo::SoftwareRenderingContext::new(servo::PhysicalSize::new(width, height))
                .map_err(|error| format!("software rendering context: {error:?}"))?,
        );
        let engine = Rc::new(Engine {
            servo,
            rendering_context,
        });
        *slot = Some(engine.clone());
        Ok(engine)
    })
}

thread_local! {
    /// The one runtime every Servo actor lives on, created on first use and
    /// re-entered by each tool call.
    static RUNTIME: (tokio::runtime::Runtime, tokio::task::LocalSet) = (
        tokio::runtime::Builder::new_current_thread()
            // IO as well as time: the network reaches the guest through
            // `tokio::net`, which needs the IO driver.
            .enable_all()
            .build()
            .expect("could not build the component runtime"),
        tokio::task::LocalSet::new(),
    );
}

/// Run `future` on the component runtime.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    RUNTIME.with(|(runtime, local)| local.block_on(runtime, future))
}

/// Turn the caller's `html` / `url` pair into something Servo can load.
fn target_url(html: Option<String>, url: Option<String>) -> Result<String, ActError> {
    match (html, url) {
        (Some(html), _) => Ok(format!(
            "data:text/html;charset=utf-8,{}",
            utf8_percent_encode(&html, NON_ALPHANUMERIC)
        )),
        (None, Some(url)) => Ok(url),
        (None, None) => Err(ActError::invalid_args("provide either `html` or `url`")),
    }
}

/// A live page: one `WebView` on the shared engine.
struct Browser {
    engine: Rc<Engine>,
    webview: servo::WebView,
    width: u32,
    height: u32,
}

impl Browser {
    async fn open(url: &str, width: u32, height: u32) -> Result<Browser, String> {
        let engine = engine(width, height)?;
        let parsed = servo::ServoUrl::parse(url).map_err(|error| format!("bad url: {error}"))?;
        let webview = servo::WebViewBuilder::new(&engine.servo, engine.rendering_context.clone())
            .url(parsed.into_url())
            .build();

        let browser = Browser {
            engine,
            webview,
            width,
            height,
        };
        browser.activate();
        browser.wait_for_load().await;
        Ok(browser)
    }

    /// Make this the page that paints.
    ///
    /// Pages share one surface, so whichever one is being worked on has to be
    /// raised first. Calls are strictly sequential, so this is enough.
    fn activate(&self) {
        self.webview.show();
    }

    /// Turn the event loop until the page reports it has finished loading.
    async fn wait_for_load(&self) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            self.engine.servo.spin_event_loop();
            tokio::time::sleep(POLL_INTERVAL).await;
            if self.webview.load_status() == servo::LoadStatus::Complete {
                return;
            }
            if Instant::now() >= deadline {
                log::warn!("load did not finish within {LOAD_TIMEOUT:?}");
                return;
            }
        }
    }

    /// Turn the event loop `turns` times, painting as we go.
    async fn pump(&self, turns: u32) {
        for _ in 0..turns {
            self.webview.paint();
            self.engine.servo.spin_event_loop();
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn screenshot(&self, settle_ms: u32) -> Result<Vec<u8>, String> {
        self.activate();
        // Layout, display-list building, scene building and frame building are
        // four hand-offs between actors, and each needs a turn of the loop
        // before the result reaches the framebuffer.
        self.pump(settle_ms).await;

        let rect = servo::DeviceIntRect::from_origin_and_size(
            servo::DeviceIntPoint::new(0, 0),
            servo::DeviceIntSize::new(self.width as i32, self.height as i32),
        );
        let image = self
            .engine
            .rendering_context
            .read_to_image(rect)
            .ok_or("no frame was produced")?;

        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|error| format!("png encoding failed: {error}"))?;
        Ok(png)
    }

    /// Evaluate `script` in the page, through the same path an embedder uses.
    async fn evaluate(&self, script: &str) -> Result<String, String> {
        self.activate();
        let slot: Rc<RefCell<Option<Result<String, String>>>> = Default::default();
        let sink = slot.clone();
        self.webview.evaluate_javascript(script, move |result| {
            *sink.borrow_mut() = Some(match result {
                Ok(value) => Ok(render_js_value(&value)),
                Err(error) => Err(format!("{error:?}")),
            });
        });

        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            self.engine.servo.spin_event_loop();
            tokio::time::sleep(POLL_INTERVAL).await;
            if let Some(result) = slot.borrow_mut().take() {
                return result;
            }
            if Instant::now() >= deadline {
                return Err("evaluation did not return in time".to_owned());
            }
        }
    }

    async fn click(&self, x: f32, y: f32) {
        self.activate();
        let point = servo::WebViewPoint::Page(euclid::Point2D::new(x, y));
        // A real click is three events: the pointer arrives, presses, releases.
        // Sending only the button events leaves hover state unset, and pages
        // that react to `mouseover` behave differently.
        self.webview
            .notify_input_event(servo::InputEvent::MouseMove(servo::MouseMoveEvent::new(
                point,
            )));
        for action in [servo::MouseButtonAction::Down, servo::MouseButtonAction::Up] {
            self.webview
                .notify_input_event(servo::InputEvent::MouseButton(
                    servo::MouseButtonEvent::new(action, servo::MouseButton::Left, point),
                ));
        }
        self.pump(DEFAULT_SETTLE_MS).await;
    }

    async fn scroll(&self, dx: f64, dy: f64) {
        self.activate();
        // Servo's wheel convention is the inverse of a scroll delta: a positive
        // `y` reveals content *above* the viewport.
        let delta = servo::WheelDelta {
            x: -dx,
            y: -dy,
            z: 0.0,
            mode: servo::WheelMode::DeltaPixel,
        };
        let point = servo::WebViewPoint::Page(euclid::Point2D::new(
            self.width as f32 / 2.0,
            self.height as f32 / 2.0,
        ));
        self.webview
            .notify_input_event(servo::InputEvent::Wheel(servo::WheelEvent::new(
                delta, point,
            )));
        self.pump(DEFAULT_SETTLE_MS).await;
    }

    /// Give up the page without running Servo's teardown.
    ///
    /// Dropping it properly would be better — the script thread's SpiderMonkey
    /// `JSContext` is what limits an instance to one page — but the teardown
    /// path blocks on the render backend, a task on this same runtime, and that
    /// hangs the component outright. Leaking is the lesser failure.
    fn forget(self) {
        std::mem::forget(self);
    }
}

/// Render a JavaScript value as text a caller can read or parse.
fn render_js_value(value: &servo::JSValue) -> String {
    match value {
        servo::JSValue::Undefined => "undefined".to_owned(),
        servo::JSValue::Null => "null".to_owned(),
        servo::JSValue::Boolean(value) => value.to_string(),
        servo::JSValue::Number(value) => value.to_string(),
        servo::JSValue::String(value) => value.clone(),
        servo::JSValue::Element(id)
        | servo::JSValue::ShadowRoot(id)
        | servo::JSValue::Frame(id)
        | servo::JSValue::Window(id) => id.clone(),
        servo::JSValue::Array(values) => {
            let rendered: Vec<_> = values.iter().map(render_js_value).collect();
            format!("[{}]", rendered.join(", "))
        }
        servo::JSValue::Object(fields) => {
            let rendered: Vec<_> = fields
                .iter()
                .map(|(key, value)| format!("{key}: {}", render_js_value(value)))
                .collect();
            format!("{{{}}}", rendered.join(", "))
        }
    }
}
