//! In-crate server tests: auth, provider CRUD, OpenAPI document.

use super::*;
use crate::app_config::AppType;

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

async fn spawn_test_server() -> (std::net::SocketAddr, ServerState) {
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
    let serve_state = state.clone();
    tokio::spawn(async move {
        let _ = serve(listener, serve_state).await;
    });
    (addr, state)
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_is_public_and_reports_apps() {
    let (addr, _state) = spawn_test_server().await;
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
    let (addr, _state) = spawn_test_server().await;
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
    let (addr, _state) = spawn_test_server().await;
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
async fn common_prompt_preset_lifecycle_across_apps() {
    init_test_env();
    let home = crate::config::home_dir().expect("home");
    let zcode_agents = home.join(".zcode").join("AGENTS.md");
    let codex_agents = home.join(".codex").join("AGENTS.md");
    for (dir, file) in [
        (zcode_agents.parent().unwrap(), &zcode_agents),
        (codex_agents.parent().unwrap(), &codex_agents),
    ] {
        std::fs::create_dir_all(dir).expect("app dir");
        std::fs::write(file, "# private notes\n").expect("seed live file");
    }

    let (addr, _state) = spawn_test_server().await;
    let auth = "Bearer test-token-0123456789abcdef";
    let base = format!("http://{addr}/api/prompts/common");
    let marker = |content: &str| {
        format!(
            "<!-- cc-switch common start -->\n{content}\n<!-- cc-switch common end -->\n\n# private notes\n"
        )
    };

    // Create two presets.
    for (id, content) in [("strict", "# strict rules"), ("relaxed", "# relaxed rules")] {
        let created = client()
            .post(&base)
            .header("Authorization", auth)
            .json(&serde_json::json!({ "id": id, "content": content }))
            .send()
            .await
            .expect("create common");
        assert_eq!(created.status(), 200);
    }

    // Use "strict" → both live files get the preamble, private part intact.
    let used = client()
        .post(format!("{base}/strict/use"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("use strict");
    assert_eq!(used.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&zcode_agents).expect("zcode live"),
        marker("# strict rules")
    );
    assert_eq!(
        std::fs::read_to_string(&codex_agents).expect("codex live"),
        marker("# strict rules")
    );

    // Editing the ACTIVE preset repaints live files.
    let patched = client()
        .patch(format!("{base}/strict"))
        .header("Authorization", auth)
        .json(&serde_json::json!({ "content": "# strict v2" }))
        .send()
        .await
        .expect("patch active preset");
    assert_eq!(patched.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&zcode_agents).expect("zcode live after patch"),
        marker("# strict v2")
    );

    // Switch to "relaxed" — single active, preamble swapped.
    let switched = client()
        .post(format!("{base}/relaxed/use"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("use relaxed");
    assert_eq!(switched.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&codex_agents).expect("codex live after switch"),
        marker("# relaxed rules")
    );

    // Per-app preset activation must keep the preamble and only replace the
    // private part.
    let preset: serde_json::Value = client()
        .post(format!("http://{addr}/api/apps/zcode/prompts"))
        .header("Authorization", auth)
        .json(&serde_json::json!({ "name": "Guide", "content": "# guide private" }))
        .send()
        .await
        .expect("create app preset")
        .json()
        .await
        .expect("preset body");
    let preset_id = preset["id"].as_str().expect("preset id").to_string();
    let activated = client()
        .post(format!(
            "http://{addr}/api/apps/zcode/prompts/{preset_id}/activate"
        ))
        .header("Authorization", auth)
        .send()
        .await
        .expect("activate app preset");
    assert_eq!(activated.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&zcode_agents).expect("zcode live after activate"),
        marker("# relaxed rules").replace("# private notes\n", "# guide private")
    );

    // Off removes the marker section entirely; private content survives.
    let off = client()
        .post(format!("{base}/off"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("off");
    assert_eq!(off.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&codex_agents).expect("codex live after off"),
        "# private notes\n"
    );
    assert_eq!(
        std::fs::read_to_string(&zcode_agents).expect("zcode live after off"),
        "# guide private"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zcode_provider_list_works_on_empty_database_and_survives_restart() {
    // Regression guard: the seed tables (MultiAppConfig::default, the db
    // export and the db persist loops) used to stop at grokbuild, so a fresh
    // deployment listed zcode in /api/health but every /api/apps/zcode
    // provider call answered 409 "App type not found", and zcode rows added
    // through the snapshot path never persisted to the database.
    init_test_env();
    let (addr, _state) = spawn_test_server().await;
    let auth = "Bearer test-token-0123456789abcdef";
    let base = format!("http://{addr}/api/apps/zcode/providers");

    // Empty database: list must be 200 with an empty list, not 409.
    let empty = client()
        .get(&base)
        .header("Authorization", auth)
        .send()
        .await
        .expect("list on empty db");
    let status = empty.status();
    let body: serde_json::Value = empty.json().await.expect("empty list body");
    assert_eq!(status, 200, "empty list must not 409: {body}");
    assert!(body["providers"].as_array().is_some_and(Vec::is_empty));

    // Create through the API, then rebuild AppState from disk (simulating a
    // process restart) — the row must have been persisted to the database.
    let home = crate::config::home_dir().expect("home");
    std::fs::create_dir_all(home.join(".zcode").join("v2")).expect("zcode dirs");
    let created = client()
        .post(&base)
        .header("Authorization", auth)
        .json(&serde_json::json!({
            "id": "persist-relay",
            "name": "Persist",
            "settingsConfig": {
                "access": { "type": "api-key", "apiKey": "sk-p" },
                "api": { "type": "openai-chat-completions", "baseUrl": "https://p.example/v1" },
                "personalModelIds": ["m1"],
                "modelOrder": ["m1"]
            }
        }))
        .send()
        .await
        .expect("create");
    let create_status = created.status();
    let create_body = created.text().await.expect("create body");
    assert_eq!(create_status, 200, "create: {create_body}");

    {
        let state = crate::AppState::try_new().expect("fresh state from disk");
        let providers = crate::services::provider::ProviderService::list(
            &state,
            crate::app_config::AppType::Zcode,
        )
        .expect("list after restart");
        assert!(
            providers.contains_key("persist-relay"),
            "zcode provider must survive a restart (persisted to db): {providers:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updating_active_prompt_replays_live_file() {
    init_test_env();
    let home = crate::config::home_dir().expect("home");
    let zcode_dir = home.join(".zcode");
    std::fs::create_dir_all(zcode_dir.join("v2")).expect("create zcode dirs");

    let (addr, _state) = spawn_test_server().await;
    let auth = "Bearer test-token-0123456789abcdef";
    let base = format!("http://{addr}/api/apps/zcode/prompts");

    let created: serde_json::Value = client()
        .post(&base)
        .header("Authorization", auth)
        .json(&serde_json::json!({ "name": "Guide", "content": "# guide v1" }))
        .send()
        .await
        .expect("create prompt")
        .json()
        .await
        .expect("create body");
    let id = created["id"].as_str().expect("id").to_string();

    // Activate writes the live file.
    let activated = client()
        .post(format!("{base}/{id}/activate"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("activate");
    assert_eq!(activated.status(), 200);
    let live_path = zcode_dir.join("AGENTS.md");
    assert_eq!(
        std::fs::read_to_string(&live_path)
            .expect("live after activate")
            .trim(),
        "# guide v1"
    );

    // Editing the ACTIVE preset must replay the new content into the live file.
    let patched = client()
        .patch(format!("{base}/{id}"))
        .header("Authorization", auth)
        .json(&serde_json::json!({ "content": "# guide v2" }))
        .send()
        .await
        .expect("patch prompt");
    assert_eq!(patched.status(), 200);
    assert_eq!(
        std::fs::read_to_string(&live_path)
            .expect("live after patch")
            .trim(),
        "# guide v2"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn import_live_add_only_and_flag_rejection() {
    init_test_env();
    let home = crate::config::home_dir().expect("home");
    let zcode_v2 = home.join(".zcode").join("v2");
    std::fs::create_dir_all(&zcode_v2).expect("create zcode v2 dir");
    // Live has one third-party provider the database has never seen.
    std::fs::write(
        zcode_v2.join("provider_config.json"),
        serde_json::json!({
            "schemaVersion": 1,
            "config": {
                "providerOrder": ["manual-relay"],
                "providerConfigRules": { "providerRules": [
                    {
                        "providerId": "manual-relay",
                        "providerName": "Manual",
                        "config": {
                            "access": { "type": "api-key", "apiKey": "sk-manual" },
                            "api": { "type": "openai-chat-completions", "baseUrl": "https://manual.example/v1" },
                            "personalModelIds": ["m1"],
                            "modelOrder": ["m1"]
                        }
                    }
                ]},
                "modelConfigRules": {
                    "providerModelRules": [],
                    "manualProviderModelRules": []
                }
            }
        })
        .to_string(),
    )
    .expect("seed zcode live");

    let (addr, _state) = spawn_test_server().await;
    let auth = "Bearer test-token-0123456789abcdef";

    // Add-only import (empty body) picks the live provider up.
    let imported = client()
        .post(format!(
            "http://{addr}/api/apps/zcode/providers/import-live"
        ))
        .header("Authorization", auth)
        .send()
        .await
        .expect("import request");
    let status = imported.status();
    let body: serde_json::Value = imported.json().await.expect("import body");
    assert_eq!(status, 200, "import: {body}");
    assert_eq!(body["added"], 1, "one provider imported: {body}");

    let listed = client()
        .get(format!("http://{addr}/api/apps/zcode/providers"))
        .header("Authorization", auth)
        .send()
        .await
        .expect("list request")
        .json::<serde_json::Value>()
        .await
        .expect("list json");
    assert!(listed["providers"]
        .as_array()
        .is_some_and(|items| items.iter().any(|p| p["id"] == "manual-relay")));

    // --update is Pi-only; other apps must reject it instead of ignoring it.
    let rejected = client()
        .post(format!(
            "http://{addr}/api/apps/zcode/providers/import-live"
        ))
        .header("Authorization", auth)
        .json(&serde_json::json!({ "update": true }))
        .send()
        .await
        .expect("flag rejection request");
    assert_eq!(rejected.status(), 409);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webdav_upload_returns_error_instead_of_panicking() {
    // Regression guard: the sync service builds its own tokio runtime, which
    // used to panic the whole server ("cannot start a runtime from within a
    // runtime") before the spawn_blocking fix. Pointing at a dead local port
    // must yield a JSON error, never a crash.
    init_test_env();
    crate::settings::set_webdav_sync_settings(Some(crate::settings::WebDavSyncSettings {
        enabled: true,
        base_url: "http://127.0.0.1:9/dav".to_string(),
        remote_root: "cc-switch-test".to_string(),
        profile: "default".to_string(),
        username: "u".to_string(),
        password: "p".to_string(),
        auto_sync: false,
        status: Default::default(),
    }))
    .expect("configure webdav");

    let (addr, _state) = spawn_test_server().await;
    let response = client()
        .post(format!("http://{addr}/api/sync/webdav/upload"))
        .header("Authorization", "Bearer test-token-0123456789abcdef")
        .send()
        .await
        .expect("upload request must not panic the server");
    assert_eq!(response.status(), 409);
    let body: serde_json::Value = response.json().await.expect("error body");
    assert!(body["error"]
        .as_str()
        .is_some_and(|message| !message.is_empty()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_models_returns_error_instead_of_panicking() {
    // Same runtime-nesting guard for the model discovery endpoint.
    init_test_env();
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
    .expect("seed zcode config");

    let (addr, _state) = spawn_test_server().await;
    let auth = "Bearer test-token-0123456789abcdef";
    let create = client()
        .post(format!("http://{addr}/api/apps/zcode/providers"))
        .header("Authorization", auth)
        .json(&serde_json::json!({
            "id": "dead-endpoint",
            "name": "Dead",
            "settingsConfig": {
                "access": { "type": "api-key", "apiKey": "sk-dead" },
                "api": { "type": "openai-chat-completions", "baseUrl": "http://127.0.0.1:9/v1" },
                "personalModelIds": ["m1"],
                "modelOrder": ["m1"]
            }
        }))
        .send()
        .await
        .expect("create request");
    assert_eq!(create.status(), 200);

    let response = client()
        .get(format!(
            "http://{addr}/api/apps/zcode/providers/dead-endpoint/fetch-models"
        ))
        .header("Authorization", auth)
        .send()
        .await
        .expect("fetch-models must not panic the server");
    assert_eq!(response.status(), 409);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cors_preflight_succeeds_without_token() {
    let (addr, _state) = spawn_test_server().await;
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
    // Browsers cache the preflight for 10 minutes.
    assert_eq!(
        preflight
            .headers()
            .get("access-control-max-age")
            .and_then(|v| v.to_str().ok()),
        Some("600")
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
    let (addr, _state) = spawn_test_server().await;
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

    // Masked read-modify-write: PATCH the *** mask back with another field
    // changed; the stored apiKey must survive.
    let patched = client()
        .patch(format!("{base}/api/apps/zcode/providers/relay-backup"))
        .header("Authorization", auth)
        .json(&serde_json::json!({
            "settingsConfig": {
                "access": { "type": "api-key", "apiKey": "***" },
                "api": { "type": "openai-chat-completions", "baseUrl": "https://backup2.example/v1" },
                "personalModelIds": ["backup-model"],
                "modelOrder": ["backup-model"]
            }
        }))
        .send()
        .await
        .expect("patch request");
    assert_eq!(patched.status(), 200, "patch: {:?}", patched.text().await);

    {
        let providers =
            crate::services::provider::ProviderService::list(&_state.app, AppType::Zcode)
                .expect("list stored providers");
        let stored = providers
            .get("relay-backup")
            .expect("relay-backup stored")
            .settings_config
            .clone();
        assert_eq!(
            stored["access"]["apiKey"], "sk-backup",
            "mask must keep stored key"
        );
        assert_eq!(
            stored["api"]["baseUrl"], "https://backup2.example/v1",
            "non-masked fields update normally"
        );
    }

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
