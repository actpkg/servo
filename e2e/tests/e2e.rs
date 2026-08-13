//! Drive the packed component through `act run --mcp` with a real MCP client.
//!
//! This replaces the python fastmcp/pytest suite that used to live in this
//! directory: the tests observe exactly what an agent observes, over the same
//! client stack (`rmcp`) the host bridge itself is built on. The translation
//! stays session-less on purpose — the python suite never opened one, it only
//! exercised one-shot `render` plus tool listing.
//!
//! Env: WASM — path to the packed component (default: the component's
//!      release build output);
//!      ACT  — the act invocation (default `act`; `npx @actcore/act`, the
//!             component justfile's default, also works — whitespace-split,
//!             like the shlex.split the python conftest did).

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::{ConfigureCommandExt, TokioChildProcess},
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex as AsyncMutex;

/// `().serve(transport)` hands back the client-role service running over the
/// child process: role first, the unit client handler second.
type Client = rmcp::service::RunningService<rmcp::service::RoleClient, ()>;

/// Deliberately loose. `act run --mcp` instantiates the component before it
/// answers `initialize`, so "connect" includes that cost — for this component
/// it is ~8s healthy, and the python conftest tripped at 30s in CI before
/// settling on 120. The bound exists so a stalled handshake fails with a
/// diagnostic instead of hanging: it is not a performance assertion.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

/// The engine keeps client storage (localStorage and friends) under /tmp/servo
/// and refuses to start without a writable directory there — the path is the
/// engine's own, not this component's, so the grant below names it verbatim.
const CLIENT_STORAGE: &str = "/tmp/servo";

fn wasm_path() -> PathBuf {
    PathBuf::from(std::env::var("WASM").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../target/wasm32-wasip2/release/component_servo.wasm"
        )
        .into()
    }))
}

/// The ACT invocation, honouring the same override the component justfile
/// uses. Its default there is `npx @actcore/act` — two words — which cannot
/// be `argv[0]` for a non-shell spawn, so the value is whitespace-split into
/// program + leading args. Quoted paths with spaces are not a form this
/// fleet passes through `ACT`; a full shlex is deliberately not pulled in.
fn act_argv() -> Vec<String> {
    std::env::var("ACT")
        .unwrap_or_else(|_| "act".into())
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Spawn `act run <wasm> --mcp` with the grant this component needs.
///
/// Grants are NOT optional: the default policy mode is `ask` and a headless
/// run degrades it to deny. The `wasi:filesystem` grant is verbatim from the
/// python conftest, which itself lifted it from the old justfile recipe —
/// and nothing in `wasi:sockets` (the other declared capability) is granted:
/// every python test rendered inline `html`, never a `url`, so no test here
/// needs outbound network access either.
fn act_command() -> tokio::process::Command {
    // The engine's storage directory must exist before the component starts;
    // the python fixture created it for every client and so does this.
    std::fs::create_dir_all(CLIENT_STORAGE).expect("create the engine's client-storage dir");

    let grant = json!({
        "wasi:filesystem": {
            "mode": "allowlist",
            "allow": [{"path": format!("{CLIENT_STORAGE}/**"), "mode": "rw"}],
        }
    })
    .to_string();

    let argv = act_argv();
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.arg("run").arg(wasm_path()).arg("--mcp");
    cmd.args(["--grant", &grant]);
    cmd
}

/// Spawn with stderr captured: the audit trail (refusals, per-call rollup)
/// writes there unconditionally — RUST_LOG never silences it. Every test
/// captures, mirroring the python conftest that routed each client's stderr
/// to a log file and dumped it when the run did not pass.
fn spawn_with_captured_stderr() -> (TokioChildProcess, Arc<AsyncMutex<String>>) {
    let (transport, stderr) = TokioChildProcess::builder(act_command())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn act run --mcp with piped stderr");

    let captured = Arc::new(AsyncMutex::new(String::new()));
    let sink = captured.clone();
    let mut lines = BufReader::new(stderr.expect("stderr was piped")).lines();
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            sink.lock().await.push_str(&line);
            sink.lock().await.push('\n');
        }
    });

    (transport, captured)
}

/// A connected MCP client plus everything its child wrote to stderr.
///
/// The timeout bounds the connect, not the test body: a stalled handshake
/// otherwise consumes the whole runner budget with no diagnostic at all.
async fn connect() -> (Client, Arc<AsyncMutex<String>>) {
    let (transport, captured) = spawn_with_captured_stderr();
    match tokio::time::timeout(CONNECT_TIMEOUT, async { ().serve(transport).await }).await {
        Ok(Ok(client)) => (client, captured),
        Ok(Err(error)) => panic!(
            "rmcp handshake with act run --mcp failed: {error}\n--- act stderr ---\n{}",
            captured.lock().await
        ),
        Err(_) => panic!(
            "MCP client did not connect within {}s; act's stderr so far:\n{}",
            CONNECT_TIMEOUT.as_secs(),
            captured.lock().await
        ),
    }
}

/// The shape check python got for free from `"id" in capabilities`: the
/// manifest emits an object keyed by capability id (the `in` also tolerated
/// a bare list, so both are accepted here rather than assumed).
fn declares_capability(capabilities: &Value, id: &str) -> bool {
    match capabilities {
        Value::Object(map) => map.contains_key(id),
        Value::Array(items) => items.iter().any(|v| v.as_str() == Some(id)),
        _ => false,
    }
}

/// The manifest probe from the python test_info.py — and the fast-fail the
/// python `wasm_path` fixture provided. An unpacked wasm (raw `cargo build`
/// output, no `act:component` section) declares no ceiling, so every grant
/// is refused as "outside ceiling" and the failures point anywhere but at
/// the missing metadata. Unlike every other component in this sweep, a
/// missing/stale wasm here is not a quick rebuild away: this engine compiles
/// SpiderMonkey, FreeType, aws-lc-rs and swgl, a cold build measured in tens
/// of minutes. The justfile's `test: build` ordering exists so this test
/// finds a packed artifact.
#[test]
fn manifest_reports_name_version_and_capabilities() {
    let output = {
        let argv = act_argv();
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.args(["inspect", "component-manifest"])
            .arg(wasm_path())
            .output()
            .expect("run act inspect component-manifest")
    };
    assert!(
        output.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: Value = serde_json::from_slice(&output.stdout).expect("manifest is JSON");
    assert_eq!(
        manifest["std"]["name"], "servo",
        "packed manifest must carry the component name"
    );
    assert!(
        manifest["std"]["version"].is_string(),
        "packed manifest must carry a version, got: {}",
        manifest["std"]["version"]
    );
    let capabilities = &manifest["std"]["capabilities"];
    assert!(
        declares_capability(capabilities, "wasi:sockets"),
        "servo fetches pages itself over TCP — wasi:sockets must be declared, got: {capabilities}"
    );
    assert!(
        declares_capability(capabilities, "wasi:filesystem"),
        "the engine refuses to start without writable client storage — \
         wasi:filesystem must be declared, got: {capabilities}"
    );
}

#[tokio::test]
async fn lists_the_page_interaction_tools() {
    let (client, _captured) = connect().await;
    let tools = client.list_all_tools().await.expect("list_all_tools");
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    for expected in ["render", "dom", "eval", "click", "scroll", "screenshot"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} must be among the tools, got: {names:?}"
        );
    }
    // Deliberately absent: an instance renders one document, so there is
    // nothing to navigate to. See skill/SKILL.md.
    assert!(
        !names.iter().any(|n| n == "navigate"),
        "navigate must not be exposed (one document per instance), got: {names:?}"
    );
    client.cancel().await.ok();
}

#[tokio::test]
async fn renders_a_page_built_by_script() {
    let (client, _captured) = connect().await;

    // A page whose content is built by script, rendered end to end: the DOM
    // that comes back proves the engine ran the script, not just parsed the
    // markup.
    let args = json!({
        "html": (
            "<html><body><div id=out></div><script>document.getElementById('out').textContent='rendered by script'</script></body></html>"
        ),
        "width": 400,
        "height": 200,
    })
    .as_object()
    .unwrap()
    .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("render").with_arguments(args))
        .await
        .expect("call_tool render");
    assert_ne!(result.is_error, Some(true), "render failed: {result:?}");
    assert_eq!(
        result.content.len(),
        2,
        "render returns exactly a screenshot and the DOM, got: {:?}",
        result.content
    );

    // A screenshot is native MCP ImageContent: its mime type lives on the
    // content block itself — rmcp models the wire `type: "image"` as this
    // variant, so matching it *is* the type assertion. The respelled
    // `dev.actcore/mime-type` meta key is only injected for text blocks,
    // which otherwise have no type of their own to carry it on. Measured
    // directly against this component, not assumed from the earlier
    // hurl-derived mapping.
    let screenshot = match &result.content[0] {
        rmcp::model::ContentBlock::Image(image) => image,
        other => panic!("expected the first block to be an image, got: {other:?}"),
    };
    assert_eq!(
        screenshot.mime_type, "image/png",
        "the screenshot must be a PNG"
    );

    let dom = match &result.content[1] {
        rmcp::model::ContentBlock::Text(text) => text,
        other => panic!("expected the second block to be text, got: {other:?}"),
    };
    let meta = dom
        .meta
        .as_ref()
        .expect("the DOM block must carry _meta");
    assert_eq!(
        meta.0.get("dev.actcore/mime-type").and_then(|v| v.as_str()),
        Some("text/html"),
        "the DOM is HTML, and text blocks surface their type only through meta"
    );
    // The text the script wrote, not the markup that was sent: a page that
    // failed to load at all would still come back as two parts with these
    // same mime types, so without this the test would pass on the engine's
    // own error document.
    assert!(
        dom.text.contains("rendered by script"),
        "expected the script's text in the DOM, got: {}",
        dom.text
    );

    client.cancel().await.ok();
}
