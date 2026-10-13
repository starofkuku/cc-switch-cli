//! In-crate server tests: auth, provider CRUD, OpenAPI document.

use super::*;

thread_local! {
    static TEST_ENV: std::cell::RefCell<Option<(tempfile::TempDir, crate::test_support::TestEnvGuard)>> =
        const { std::cell::RefCell::new(None) };
}

/// Isolate HOME/config dirs once per test (async tasks cannot borrow the
/// non-Send guard, so it lives in a thread-local instead).
fn init_test_env() {
    // TestEnvGuard::isolated already acquires the home/settings lock itself;
    // taking it here too would deadlock on the non-reentrant mutex.
    TEST_ENV.with(|cell| {
        if cell.borrow().is_some() {
            return;
        }
        let temp = tempfile::tempdir().expect("tempdir");
        let guard = crate::test_support::TestEnvGuard::isolated(temp.path());
        *cell.borrow_mut() = Some((temp, guard));
    });
}

async fn spawn_test_server() -> (std::net::SocketAddr, ()) {
    init_test_env();
    let state = build_state(&ServerConfig {
        token: Some("test-token-0123456789abcdef".to_string()),
        ..Default::default()
    })
    .expect("build server state");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = serve(listener, state).await;
    });
    (addr, ())
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_is_public_and_reports_apps() {
    let (addr, ()) = spawn_test_server().await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/api/health"))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    assert_eq!(body["name"], "cc-switch-server");
    assert!(body["apps"].as_array().is_some_and(|apps| !apps.is_empty()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protected_routes_reject_missing_or_wrong_token() {
    let (addr, ()) = spawn_test_server().await;
    let base = format!("http://{addr}");

    let missing = client()
        .get(format!("{base}/api/apps"))
        .send()
        .await
        .expect("request");
    assert_eq!(missing.status(), 401);

    let wrong = client()
        .get(format!("{base}/api/apps"))
        .header("Authorization", "Bearer wrong")
        .send()
        .await
        .expect("request");
    assert_eq!(wrong.status(), 401);

    let ok = client()
        .get(format!("{base}/api/apps"))
        .header("Authorization", "Bearer test-token-0123456789abcdef")
        .send()
        .await
        .expect("request");
    assert_eq!(ok.status(), 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openapi_document_lists_core_paths() {
    let (addr, ()) = spawn_test_server().await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/api/openapi.json"))
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("json");
    let paths = body["paths"].as_object().expect("paths");
    for expected in [
        "/api/apps/{app}/providers",
        "/api/apps/{app}/providers/{id}/switch",
        "/api/mcp",
        "/api/apps/{app}/prompts/{id}/activate",
        "/api/skills/{directory}/apps",
    ] {
        assert!(paths.contains_key(expected), "missing {expected}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_preflight_succeeds_without_token() {
    let (addr, ()) = spawn_test_server().await;
    // Browser preflight: no Authorization header, must not be 401.
    let preflight = client()
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/api/apps"))
        .header("Origin", "http://localhost:5173")
        .header("Access-Control-Request-Method", "GET")
        .header("Access-Control-Request-Headers", "authorization")
        .send()
        .await
        .expect("preflight request");
    assert_eq!(preflight.status().as_u16(), 200);
    assert_eq!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("*")
    );

    // A normal authenticated request also carries the CORS header.
    let response = client()
        .get(format!("http://{addr}/api/apps"))
        .header("Origin", "http://localhost:5173")
        .header("Authorization", "Bearer test-token-0123456789abcdef")
        .send()
        .await
        .expect("cors request");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("*")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_crud_roundtrip_for_zcode() {
    let (addr, ()) = spawn_test_server().await;
    let base = format!("http://{addr}");
    let auth = "Bearer test-token-0123456789abcdef";

    // Seed an initialized zcode live config so writes are allowed.
    let home = crate::config::home_dir().expect("home");
    let zcode_v2 = home.join(".zcode").join("v2");
    std::fs::create_dir_all(&zcode_v2).expect("create zcode v2 dir");
    std::fs::write(
        zcode_v2.join("provider_config.json"),
        serde_json::json!({
            "schemaVersion": 1,
            "config": {
                "providerOrder": [],
                "providerConfigRules": { "providerRules": [] },
                "modelConfigRules": {
                    "providerModelRules": [],
                    "manualProviderModelRules": []
                }
            }
        })
        .to_string(),
    )
    .expect("seed zcode provider config");

    let create = client()
        .post(format!("{base}/api/apps/zcode/providers"))
        .header("Authorization", auth)
        .json(&serde_json::json!({
            "id": "relay-demo",
            "name": "Relay Demo",
            "settingsConfig": {
                "access": { "type": "api-key", "apiKey": "sk-demo" },
                "api": { "type": "openai-chat-completions", "baseUrl": "https://relay.example/v1" },
                "personalModelIds": ["demo-model"],
                "modelOrder": ["demo-model"]
            }
        }))
        .send()
        .await
        .expect("create request");
    let create_status = create.status();
    let create_body = create.text().await.expect("create body");
    assert_eq!(create_status, 200, "create: {create_body}");

    let list: serde_json::Value = client()
        .get(format!("{base}/api/apps/zcode/providers"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("list request")
        .json()
        .await
        .expect("list json");
    assert!(list["providers"]
        .as_array()
        .is_some_and(|items| items.iter().any(|p| p["id"] == "relay-demo")));

    let detail: serde_json::Value = client()
        .get(format!("{base}/api/apps/zcode/providers/relay-demo"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("detail request")
        .json()
        .await
        .expect("detail json");
    // The stored settings_config keeps the submitted bare-config shape for a
    // created zcode provider (the rule wrapper is added when writing live).
    assert_eq!(detail["settingsConfig"]["access"]["apiKey"], "***");

    // zcode add points the selection at the new provider (same as Grok), so
    // deleting the in-use provider is guarded — add a second one first.
    let second = client()
        .post(format!("{base}/api/apps/zcode/providers"))
        .header("Authorization", auth)
        .json(&serde_json::json!({
            "id": "relay-backup",
            "name": "Relay Backup",
            "settingsConfig": {
                "access": { "type": "api-key", "apiKey": "sk-backup" },
                "api": { "type": "openai-chat-completions", "baseUrl": "https://backup.example/v1" },
                "personalModelIds": ["backup-model"],
                "modelOrder": ["backup-model"]
            }
        }))
        .send()
        .await
        .expect("second create request");
    assert_eq!(second.status(), 200);

    let deleted = client()
        .delete(format!("{base}/api/apps/zcode/providers/relay-demo"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("delete request");
    let deleted_status = deleted.status();
    let deleted_body = deleted.text().await.expect("delete body");
    assert_eq!(deleted_status, 200, "delete: {deleted_body}");
}
