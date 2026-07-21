//! Integration tests for Grok provider live config management.

mod support;

use cc_switch_lib::{AppState, AppType, Provider, ProviderService};
use serde_json::json;
use std::fs;
use support::{ensure_test_home, lock_test_mutex, reset_test_fs};

fn seed_grok_config(home: &std::path::Path, content: &str) {
    let grok_home = home.join(".grok");
    fs::create_dir_all(&grok_home).expect("create grok home");
    std::env::set_var("GROK_HOME", &grok_home);
    fs::write(grok_home.join("config.toml"), content).expect("write config");
}

fn grok_provider(id: &str, model: &str, base_url: &str, api_key: &str) -> Provider {
    Provider::with_id(
        id.to_string(),
        id.to_string(),
        json!({
            "model": model,
            "base_url": base_url,
            "api_key": api_key,
            "api_backend": "responses",
            "name": id,
        }),
        None,
    )
}

#[test]
fn grok_switch_writes_model_section_and_default() {
    let _lock = lock_test_mutex();
    let home = ensure_test_home();
    reset_test_fs();
    seed_grok_config(
        &home,
        r#"
# keep comment
[ui]
yolo = false

[models]
default = "old"

[model.old]
model = "old-model"
base_url = "https://old.example/v1"
api_key = "old-key"
"#,
    );

    let state = AppState::try_new().expect("state");
    ProviderService::add(
        &state,
        AppType::Grok,
        grok_provider("relay", "grok-4.5", "https://relay.example/v1", "sk-relay"),
    )
    .expect("add");
    ProviderService::switch(&state, AppType::Grok, "relay").expect("switch");

    let text = fs::read_to_string(home.join(".grok").join("config.toml")).expect("read live");
    assert!(text.contains("# keep comment"), "{text}");
    assert!(text.contains("yolo = false"), "{text}");
    assert!(text.contains("[model.relay]"), "{text}");
    assert!(text.contains("default = \"relay\""), "{text}");
    assert!(
        text.contains("base_url = \"https://relay.example/v1\""),
        "{text}"
    );
    assert_eq!(
        ProviderService::current(&state, AppType::Grok).expect("current"),
        "relay"
    );
}

#[test]
fn grok_delete_rejects_current_provider() {
    let _lock = lock_test_mutex();
    let home = ensure_test_home();
    reset_test_fs();
    seed_grok_config(
        &home,
        r#"
[models]
default = "relay"

[model.relay]
model = "grok-4.5"
base_url = "https://relay.example/v1"
api_key = "sk-relay"
"#,
    );

    let state = AppState::try_new().expect("state");
    ProviderService::add(
        &state,
        AppType::Grok,
        grok_provider("relay", "grok-4.5", "https://relay.example/v1", "sk-relay"),
    )
    .expect("add");
    ProviderService::switch(&state, AppType::Grok, "relay").expect("switch");

    let err = ProviderService::delete(&state, AppType::Grok, "relay").expect_err("reject current");
    let msg = err.to_string();
    assert!(
        msg.contains("当前") || msg.to_lowercase().contains("current"),
        "unexpected error: {msg}"
    );
}

#[test]
fn grok_delete_non_current_removes_live_section() {
    let _lock = lock_test_mutex();
    let home = ensure_test_home();
    reset_test_fs();
    seed_grok_config(
        &home,
        r#"
[models]
default = "keep"

[model.keep]
model = "a"
base_url = "https://a.example/v1"
api_key = "a"

[model.drop]
model = "b"
base_url = "https://b.example/v1"
api_key = "b"
"#,
    );

    let state = AppState::try_new().expect("state");
    ProviderService::add(
        &state,
        AppType::Grok,
        grok_provider("keep", "a", "https://a.example/v1", "a"),
    )
    .expect("add keep");
    ProviderService::add(
        &state,
        AppType::Grok,
        grok_provider("drop", "b", "https://b.example/v1", "b"),
    )
    .expect("add drop");
    ProviderService::switch(&state, AppType::Grok, "keep").expect("switch keep");

    ProviderService::delete(&state, AppType::Grok, "drop").expect("delete drop");

    let text = fs::read_to_string(home.join(".grok").join("config.toml")).expect("read live");
    assert!(text.contains("[model.keep]"), "{text}");
    assert!(!text.contains("[model.drop]"), "{text}");
    assert!(text.contains("default = \"keep\""), "{text}");
}

#[test]
fn grok_import_live_providers_on_startup() {
    let _lock = lock_test_mutex();
    let home = ensure_test_home();
    reset_test_fs();
    seed_grok_config(
        &home,
        r#"
[models]
default = "imported"

[model.imported]
model = "grok-4.5"
base_url = "https://import.example/v1"
api_key = "sk-import"
name = "Imported Relay"
"#,
    );

    let state = AppState::try_new_with_startup_recovery().expect("startup state");
    let providers = ProviderService::list(&state, AppType::Grok).expect("list");
    assert!(
        providers.contains_key("imported"),
        "expected imported provider, got {:?}",
        providers.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        providers["imported"].name, "Imported Relay",
        "display name from live name field"
    );
}
