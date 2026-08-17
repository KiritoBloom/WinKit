//! MCP protocol-level tests: JSON-RPC framing, the initialize handshake,
//! tools/list + tools/call against the mock backend, permission/approval
//! behavior, and response-shape guarantees. Run with
//! `cargo test --features mocks`. Nothing touches the real machine.

use serde_json::{json, Value};
use std::sync::Arc;
use winkit::config::Config;
use winkit::diagnostics::DiagnosticsEngine;
use winkit::permissions::PermissionManager;
use winkit::providers::applications::chrome::managed::ManagedChromeManager;
use winkit::providers::applications::ApplicationRegistry;
use winkit::providers::mock::MockWindowsBackend;
use winkit::providers::windows::WindowsBackend;
use winkit::providers::ProviderRegistry;
use winkit::server::protocol::{McpServer, PROTOCOL_VERSION};
use winkit::server::AppState;
use winkit::tools::{wrap, ToolDefinition, ToolRegistry};

fn state(config: Config) -> Arc<AppState> {
    let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
    AppState::with_backend(config, backend).unwrap()
}

/// The default protocol-test state: read_only, windows provider only.
fn mock_state() -> Arc<AppState> {
    let mut config = Config::default();
    config.permissions.mode = "read_only".to_string();
    config.providers.enabled = vec!["windows".to_string()];
    state(config)
}

fn mock_state_with_profile(profile: &str) -> Arc<AppState> {
    let mut config = Config::default();
    config.tools.profile = profile.to_string();
    config.providers.enabled = vec!["windows".to_string()];
    state(config)
}

fn mock_state_with_disabled(disabled: &[&str]) -> Arc<AppState> {
    let mut config = Config::default();
    config.tools.disabled = disabled.iter().map(|s| s.to_string()).collect();
    config.providers.enabled = vec!["windows".to_string()];
    state(config)
}

fn mock_state_in_mode(mode: &str) -> Arc<AppState> {
    let mut config = Config::default();
    config.permissions.mode = mode.to_string();
    config.providers.enabled = vec!["windows".to_string()];
    state(config)
}

fn mock_state_with_payload_limit(bytes: usize) -> Arc<AppState> {
    let mut config = Config::default();
    config.limits.max_payload_bytes = bytes;
    config.providers.enabled = vec!["windows".to_string()];
    state(config)
}

/// A knowledge-free standalone stub for a managed-browser action tool. Its
/// handler answer proves the approval/denial path let a real invocation
/// through; the capability comes from `tool_action_capability`, so no read
/// capability is declared.
fn action_stub(name: &'static str) -> ToolDefinition {
    ToolDefinition {
        name,
        description: "test stub for managed-browser action dispatch",
        input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        capability: None,
        timeout_ms: Some(1000),
        handler: wrap(|_state, _args| async move { Ok(json!({ "stub_call": true })) }),
    }
}

/// State that overrides the managed-browser action tools with knowledge-free
/// stubs so the protocol permission/approval paths can be exercised
/// end-to-end without a real Chrome. The real lifecycle definitions are now
/// registered; the stubs simply decouple dispatch tests from the browser.
fn stub_action_state(mode: &str) -> Arc<AppState> {
    let mut config = Config::default();
    config.permissions.mode = mode.to_string();
    config.providers.enabled = vec!["windows".to_string()];
    config.tools.profile = "full".to_string();
    config.chrome.managed.enabled = true;
    let backend: Arc<dyn WindowsBackend> = Arc::new(MockWindowsBackend::with_fixtures());
    let permissions = PermissionManager::new(config.permission_mode().unwrap());
    let providers = ProviderRegistry::new();
    let applications = ApplicationRegistry::new();
    let engine = DiagnosticsEngine::with_config(config.diagnostics.clone());
    let mut tools = ToolRegistry::build(&config);
    tools.register(action_stub("chrome_start_managed_session"));
    let managed = Arc::new(ManagedChromeManager::new(config.chrome.clone(), None));
    Arc::new(AppState {
        config,
        permissions,
        providers,
        applications,
        windows: backend,
        engine,
        tools,
        managed,
    })
}

async fn request(server: &McpServer, frame: &str) -> Value {
    let reply = server
        .handle_message(frame)
        .await
        .expect("request must produce a reply");
    serde_json::from_str(&reply).unwrap()
}

/// Drive the initialize handshake and return a ready server.
async fn initialized_server(app_state: Arc<AppState>) -> McpServer {
    let server = McpServer::new(app_state);
    let out = request(&server, &initialize_frame(1)).await;
    assert!(
        out.get("result").is_some(),
        "initialize must succeed: {out}"
    );
    server
}

fn initialize_frame(id: u64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "winkit-test", "version": "0.0.0" },
        },
    })
    .to_string()
}

async fn tools_list(server: &McpServer) -> Value {
    request(server, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await
}

async fn listed_names(server: &McpServer) -> Vec<String> {
    tools_list(server).await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .map(|s| s.to_string())
        .collect()
}

#[tokio::test]
async fn initialize_negotiates_protocol_and_server_info() {
    let server = McpServer::new(mock_state());
    let out = request(&server, &initialize_frame(1)).await;
    assert_eq!(out["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(out["result"]["serverInfo"]["name"], "winkit");
    let version = out["result"]["serverInfo"]["version"].as_str().unwrap();
    assert!(!version.is_empty());
    assert_eq!(out["result"]["capabilities"]["tools"]["listChanged"], false);
    assert_eq!(out["id"], 1);
}

#[tokio::test]
async fn tools_list_before_initialize_is_rejected() {
    let server = McpServer::new(mock_state());
    let out = request(&server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).await;
    assert_eq!(out["error"]["code"], -32002);
}

#[tokio::test]
async fn tools_list_after_initialize_contains_system_info() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(&server, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).await;
    let names: Vec<&str> = out["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"system_info"));
    assert!(names.contains(&"list_processes"));
    assert!(names.contains(&"find_process_on_port"));
    assert!(names.contains(&"hardware_snapshot"));
    assert!(out["result"]["tools"].as_array().unwrap().len() >= 30);
}

#[tokio::test]
async fn tools_list_reflects_effective_profile_and_omits_disabled() {
    // core: exactly the safe, low-latency essentials.
    let server = initialized_server(mock_state_with_profile("core")).await;
    let mut core = listed_names(&server).await;
    core.sort();
    assert_eq!(
        core,
        [
            "list_listening_ports",
            "list_processes",
            "privacy_info",
            "system_health",
            "workspace_snapshot",
        ]
    );

    // developer (default): the recommended surface.
    let server = initialized_server(mock_state_with_profile("developer")).await;
    let dev = listed_names(&server).await;
    assert!(dev.iter().any(|n| *n == "system_info"));
    assert!(dev.iter().any(|n| *n == "hardware_snapshot"));
    assert!(dev.iter().any(|n| *n == "list_dev_servers"));
    assert!(dev.iter().any(|n| *n == "workspace_snapshot"));
    assert!(dev.iter().any(|n| *n == "list_processes"));
    assert!(!dev.iter().any(|n| *n == "chrome_diagnose_tab"));

    // browser: deep Chrome inspection but no dev-server workflow.
    let server = initialized_server(mock_state_with_profile("browser")).await;
    let browser = listed_names(&server).await;
    assert!(browser.iter().any(|n| *n == "chrome_diagnose_tab"));
    assert!(browser.iter().any(|n| *n == "chrome_list_tabs"));
    assert!(browser.iter().any(|n| *n == "get_application"));
    assert!(!browser.iter().any(|n| *n == "list_dev_servers"));
    assert!(!browser.iter().any(|n| *n == "workspace_snapshot"));

    // full: everything registered.
    let server = initialized_server(mock_state_with_profile("full")).await;
    let full = listed_names(&server).await;
    assert!(full.iter().any(|n| *n == "workspace_snapshot"));
    assert!(full.iter().any(|n| *n == "chrome_diagnose_tab"));
    assert!(full.iter().any(|n| *n == "list_dev_servers"));
    assert!(full.iter().any(|n| *n == "system_diagnose"));

    // Disabled tools are hidden from the listing.
    let server = initialized_server(mock_state_with_disabled(&["list_windows"])).await;
    let disabled = listed_names(&server).await;
    assert!(!disabled.iter().any(|n| *n == "list_windows"));
    assert!(disabled.iter().any(|n| *n == "system_info"));
}

#[tokio::test]
async fn tool_schemas_are_valid_json_objects_with_required_fields() {
    let server = initialized_server(mock_state()).await;
    let out = tools_list(&server).await;
    let tools = out["result"]["tools"].as_array().unwrap();
    let by_name: std::collections::HashMap<&str, &Value> = tools
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), t))
        .collect();

    for tool in tools {
        let schema = &tool["inputSchema"];
        assert!(schema.is_object(), "tool schema must be an object");
        assert_eq!(
            schema["type"], "object",
            "tool schema must declare type object"
        );
        assert!(
            schema.get("properties").is_some(),
            "tool schema must declare properties"
        );
    }

    // Spot-check required fields on argument-taking tools.
    assert_eq!(
        by_name["get_process"]["inputSchema"]["required"],
        json!(["pid"])
    );
    assert_eq!(
        by_name["get_service"]["inputSchema"]["required"],
        json!(["name"])
    );
    assert_eq!(
        by_name["workspace_snapshot"]["inputSchema"]["required"],
        json!(["workspace_path"])
    );
    // The zero-argument tools declare no required fields.
    assert!(by_name["system_info"]["inputSchema"]
        .get("required")
        .is_none());
}

#[tokio::test]
async fn tools_call_system_info_returns_mock_data() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_info","arguments":{}}}"#,
    )
    .await;
    let text = out["result"]["content"][0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(text).unwrap();
    assert_eq!(payload["system"]["hostname"], "mock-host");
    assert_eq!(out["result"]["isError"], false);
}

#[tokio::test]
async fn tools_call_unknown_tool_is_invalid_params_with_winkit_code() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32602);
    assert_eq!(out["error"]["data"]["winkit_code"], 1);
}

#[tokio::test]
async fn tools_call_bad_arguments_is_invalid_params_with_winkit_code() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_process","arguments":{"pid":-5}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32602);
    assert_eq!(out["error"]["data"]["winkit_code"], 1);
    assert!(out["error"]["message"].as_str().unwrap().contains("pid"));
}

#[tokio::test]
async fn tools_call_missing_arguments_is_invalid_params() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"get_process","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32602);
    assert_eq!(out["error"]["data"]["winkit_code"], 1);
    assert!(out["error"]["message"].as_str().unwrap().contains("pid"));
}

#[tokio::test]
async fn unknown_method_returns_method_not_found() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"bogus/method"}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32601);
    assert!(out.get("result").is_none());
}

#[tokio::test]
async fn missing_method_returns_invalid_request() {
    let server = McpServer::new(mock_state());
    let out = request(&server, r#"{"jsonrpc":"2.0","id":2}"#).await;
    assert_eq!(out["error"]["code"], -32600);
    assert!(out["error"]["message"].as_str().unwrap().contains("method"));
}

#[tokio::test]
async fn parse_error_returns_null_id() {
    let server = McpServer::new(mock_state());
    let out = request(&server, "this is not json").await;
    assert_eq!(out["error"]["code"], -32700);
    assert!(out["id"].is_null());
}

#[tokio::test]
async fn disabled_tool_call_is_rejected() {
    let server = initialized_server(mock_state_with_disabled(&["system_info"])).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_info","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32602);
    assert_eq!(out["error"]["data"]["winkit_code"], 1);
    assert!(out["error"]["message"]
        .as_str()
        .unwrap()
        .contains("disabled"));
}

#[tokio::test]
async fn safe_mode_denial_names_the_capability() {
    let server = initialized_server(mock_state_in_mode("safe")).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_list_tabs","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32603);
    assert_eq!(out["error"]["data"]["winkit_code"], 2);
    let message = out["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("application.tabs.read"),
        "message names the capability: {message}"
    );
    assert!(message.contains("chrome_list_tabs"));
}

#[tokio::test]
async fn read_only_action_denial_names_the_capability() {
    // chrome_start_managed_session carries the BrowserLaunch action
    // capability; in read_only mode it must be denied before dispatch.
    let state = stub_action_state("read_only");
    let server = McpServer::new(state.clone());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_start_managed_session","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32603);
    assert_eq!(out["error"]["data"]["winkit_code"], 2);
    let message = out["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("application.browser.launch"),
        "message names the action capability: {message}"
    );
    assert!(message.contains("chrome_start_managed_session"));
}

#[tokio::test]
async fn approval_mode_embeds_request_id_and_grant_consumes_it() {
    let state = stub_action_state("approval");
    let server = McpServer::new(state.clone());
    let _ = request(&server, &initialize_frame(1)).await;
    let frame = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_start_managed_session","arguments":{}}}"#;

    let first = request(&server, frame).await;
    assert_eq!(first["error"]["code"], -32603);
    assert_eq!(first["error"]["data"]["winkit_code"], 12);
    let message = first["error"]["message"].as_str().unwrap();
    let request_id: u64 = message
        .split("request_id = ")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .and_then(|s| s.trim().parse().ok())
        .expect("message embeds the request id");
    assert!(message.contains("application.browser.launch"));

    // The explicit grant is consumed by the retry of the same action, so the
    // approval workflow is usable: grant, then retry succeeds.
    state.permissions.grant_approval(request_id).unwrap();
    let second = request(&server, frame).await;
    assert!(
        second.get("error").is_none(),
        "retry after grant must dispatch: {second}"
    );
    assert_eq!(
        second["result"]["content"][0]["text"],
        "{\"stub_call\":true}"
    );

    // Grants are per-request, never a standing permission: a fresh action
    // still requires a fresh approval.
    let third = request(&server, frame).await;
    assert_eq!(third["error"]["code"], -32603);
    assert_eq!(third["error"]["data"]["winkit_code"], 12);
    let third_message = third["error"]["message"].as_str().unwrap();
    let third_id: u64 = third_message
        .split("request_id = ")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap();
    assert_ne!(third_id, request_id, "each call creates a fresh request");
    assert_eq!(state.permissions.pending_approvals().len(), 1);
}

#[tokio::test]
async fn provider_unavailable_maps_to_structured_server_error_not_internal() {
    // In the full profile the chrome tools are exposed, but with only the
    // windows provider registered the chrome provider is absent. That is a
    // structured "provider unavailable" condition, not an internal error:
    // it must map to a distinct server error code (-32001) with the stable
    // winkit_code (3) so agents can tell "not enabled" from "broke".
    let server = initialized_server(mock_state_with_profile("full")).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"chrome_info","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32001);
    assert_eq!(out["error"]["data"]["winkit_code"], 3);
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap()
            .contains("chrome provider is not enabled"),
        "message explains the condition: {}",
        out["error"]["message"]
    );
}

#[tokio::test]
async fn oversized_payload_is_rejected_with_resource_limit_code() {
    let server = initialized_server(mock_state_with_payload_limit(64)).await;
    let out = request(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_info","arguments":{}}}"#,
    )
    .await;
    assert_eq!(out["error"]["code"], -32603);
    assert_eq!(out["error"]["data"]["winkit_code"], 9);
    assert!(out["error"]["message"]
        .as_str()
        .unwrap()
        .contains("payload"));
}

#[tokio::test]
async fn concurrent_calls_on_independent_tools_both_succeed() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let (a, b) = tokio::join!(
        request(
            &server,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_info","arguments":{}}}"#,
        ),
        request(
            &server,
            r#"{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"list_processes","arguments":{}}}"#,
        ),
    );
    assert!(a.get("error").is_none(), "system_info must succeed: {a}");
    assert!(b.get("error").is_none(), "list_processes must succeed: {b}");
    assert_eq!(a["id"], 2);
    assert_eq!(b["id"], 20);
}

#[tokio::test]
async fn every_reply_is_well_formed_json_without_log_leakage() {
    let server = McpServer::new(mock_state());
    let frames = [
        initialize_frame(1),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_info","arguments":{}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"bogus/method"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#.to_string(),
        "INFO a stray log line would break JSON framing".to_string(),
        r#"{"jsonrpc":"2.0","id":2}"#.to_string(),
    ];
    let mut replies = 0;
    for frame in &frames {
        if let Some(reply) = server.handle_message(frame).await {
            replies += 1;
            // The frame itself must parse as a JSON object — any log output
            // on the same channel would make this fail.
            let parsed: Value = serde_json::from_str(&reply)
                .unwrap_or_else(|e| panic!("reply is not well-formed JSON ({e}): {reply:?}"));
            let obj = parsed.as_object().expect("reply is a JSON object");
            assert!(obj.contains_key("jsonrpc"), "reply carries jsonrpc version");
            assert!(obj.contains_key("id"), "reply carries an id");
            assert!(
                obj.contains_key("result") ^ obj.contains_key("error"),
                "reply carries exactly one of result/error: {reply}"
            );
            assert_eq!(obj.len(), 3, "no unexpected top-level fields: {reply}");
        }
    }
    // Every frame in the list above produces a reply.
    assert_eq!(replies, frames.len());
}

#[tokio::test]
async fn notification_produces_no_reply() {
    let server = McpServer::new(mock_state());
    let reply = server
        .handle_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
        .await;
    assert!(reply.is_none());
}

#[tokio::test]
async fn ping_round_trips() {
    let server = McpServer::new(mock_state());
    let _ = request(&server, &initialize_frame(1)).await;
    let out = request(&server, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).await;
    assert!(out.get("result").is_some());
    assert!(out.get("error").is_none());
}
