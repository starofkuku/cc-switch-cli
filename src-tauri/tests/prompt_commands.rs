use serde_json::json;
use serial_test::serial;

use cc_switch_lib::{
    cli::commands::prompts::{execute, PromptsCommand},
    AppType, MultiAppConfig, PromptService,
};

#[path = "support.rs"]
mod support;
use support::{ensure_test_home, lock_test_mutex, reset_test_fs, state_from_config};

#[test]
#[serial]
fn prompt_service_rename_updates_name_and_timestamp() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let mut config = MultiAppConfig::default();
    config.prompts.claude.prompts = serde_json::from_value(json!({
        "pr1": {
            "id": "pr1",
            "name": "Old Name",
            "content": "hello",
            "enabled": false,
            "createdAt": 1,
            "updatedAt": 1
        }
    }))
    .expect("deserialize prompts");
    let state = state_from_config(config);

    PromptService::rename_prompt(&state, AppType::Claude, "pr1", "New Name")
        .expect("rename prompt succeeds");

    let prompts = PromptService::get_prompts(&state, AppType::Claude).expect("load prompts");
    let prompt = prompts.get("pr1").expect("renamed prompt should exist");
    assert_eq!(prompt.name, "New Name");
    assert!(prompt.updated_at.unwrap_or_default() >= 1);
}

#[test]
#[serial]
fn prompt_service_rename_rejects_empty_name() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let mut config = MultiAppConfig::default();
    config.prompts.claude.prompts = serde_json::from_value(json!({
        "pr1": {
            "id": "pr1",
            "name": "Old Name",
            "content": "hello",
            "enabled": false,
            "createdAt": 1,
            "updatedAt": 1
        }
    }))
    .expect("deserialize prompts");
    let state = state_from_config(config);

    let err = PromptService::rename_prompt(&state, AppType::Claude, "pr1", "   ")
        .expect_err("empty name should fail");
    assert!(
        err.to_string().contains("不能为空"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial]
fn prompt_rename_command_updates_prompt_name() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let mut config = MultiAppConfig::default();
    config.prompts.claude.prompts = serde_json::from_value(json!({
        "pr1": {
            "id": "pr1",
            "name": "Old Name",
            "content": "hello",
            "enabled": false,
            "createdAt": 1,
            "updatedAt": 1
        }
    }))
    .expect("deserialize prompts");
    let state = state_from_config(config);
    state.save().expect("persist config");

    execute(
        PromptsCommand::Rename {
            id: "pr1".to_string(),
            new_id: None,
            named: None,
            description: None,
            name: Some("New Name".to_string()),
        },
        Some(AppType::Claude),
    )
    .expect("rename command succeeds");

    let persisted = cc_switch_lib::AppState::try_new().expect("reload state");
    let prompts = PromptService::get_prompts(&persisted, AppType::Claude).expect("load prompts");
    assert_eq!(
        prompts.get("pr1").map(|prompt| prompt.name.as_str()),
        Some("New Name")
    );
}

#[test]
#[serial]
fn prompt_create_command_uses_explicit_name() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let state = state_from_config(MultiAppConfig::default());
    state.save().expect("persist config");

    let editor_script = ensure_test_home().join("fake-editor.sh");
    std::fs::write(
        &editor_script,
        "#!/bin/sh\nprintf 'system prompt body\\n' > \"$1\"\n",
    )
    .expect("write fake editor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&editor_script)
            .expect("read fake editor metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&editor_script, perms).expect("chmod fake editor");
    }

    std::env::set_var("EDITOR", &editor_script);
    std::env::set_var("VISUAL", &editor_script);

    execute(
        PromptsCommand::Create {
            id: None,
            named: None,
            name: Some("Prompt One".to_string()),
            description: None,
        },
        Some(AppType::Claude),
    )
    .expect("create command succeeds");

    std::env::remove_var("EDITOR");
    std::env::remove_var("VISUAL");

    let persisted = cc_switch_lib::AppState::try_new().expect("reload state");
    let prompts = PromptService::get_prompts(&persisted, AppType::Claude).expect("load prompts");
    let prompt = prompts
        .get("prompt-one")
        .expect("created prompt should exist");
    assert_eq!(prompt.name, "Prompt One");
    assert_eq!(prompt.content, "system prompt body");
}

#[test]
#[serial]
fn prompt_rename_command_can_update_id_without_prompting_for_name() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let mut config = MultiAppConfig::default();
    config.prompts.claude.prompts = serde_json::from_value(json!({
        "pr1": {
            "id": "pr1",
            "name": "Old Name",
            "content": "hello",
            "enabled": false,
            "createdAt": 1,
            "updatedAt": 1
        }
    }))
    .expect("deserialize prompts");
    let state = state_from_config(config);
    state.save().expect("persist config");

    execute(
        PromptsCommand::Rename {
            id: "pr1".to_string(),
            new_id: Some("renamed".to_string()),
            named: None,
            description: None,
            name: None,
        },
        Some(AppType::Claude),
    )
    .expect("rename command succeeds");

    let persisted = cc_switch_lib::AppState::try_new().expect("reload state");
    let prompts = PromptService::get_prompts(&persisted, AppType::Claude).expect("load prompts");
    assert!(!prompts.contains_key("pr1"));
    assert_eq!(
        prompts.get("renamed").map(|prompt| prompt.name.as_str()),
        Some("Old Name")
    );
}

#[test]
#[serial]
fn prompt_create_command_accepts_custom_id_and_description() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let state = state_from_config(MultiAppConfig::default());
    state.save().expect("persist config");

    let editor_script = ensure_test_home().join("fake-editor.sh");
    std::fs::write(
        &editor_script,
        "#!/bin/sh\nprintf 'custom body\\n' > \"$1\"\n",
    )
    .expect("write fake editor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&editor_script)
            .expect("read fake editor metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&editor_script, perms).expect("chmod fake editor");
    }

    std::env::set_var("EDITOR", &editor_script);
    std::env::set_var("VISUAL", &editor_script);

    execute(
        PromptsCommand::Create {
            id: Some("custom-id".to_string()),
            named: Some("Custom Prompt".to_string()),
            name: None,
            description: Some("Custom description".to_string()),
        },
        Some(AppType::Claude),
    )
    .expect("create command succeeds");

    std::env::remove_var("EDITOR");
    std::env::remove_var("VISUAL");

    let persisted = cc_switch_lib::AppState::try_new().expect("reload state");
    let prompts = PromptService::get_prompts(&persisted, AppType::Claude).expect("load prompts");
    let prompt = prompts
        .get("custom-id")
        .expect("created prompt should exist");
    assert_eq!(prompt.name, "Custom Prompt");
    assert_eq!(prompt.description.as_deref(), Some("Custom description"));
    assert_eq!(prompt.content, "custom body");
}

#[test]
#[serial]
fn prompt_import_command_imports_live_file_as_inactive_prompt() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let state = state_from_config(MultiAppConfig::default());
    state.save().expect("persist config");

    let live_path = cc_switch_lib::get_claude_settings_path()
        .parent()
        .expect("claude settings parent")
        .join("CLAUDE.md");
    std::fs::create_dir_all(live_path.parent().expect("live prompt parent"))
        .expect("create live prompt parent");
    std::fs::write(&live_path, "existing live prompt\nwith two lines\n")
        .expect("write live prompt");

    execute(PromptsCommand::Import, Some(AppType::Claude)).expect("import command succeeds");

    let persisted = cc_switch_lib::AppState::try_new().expect("reload state");
    let prompts = PromptService::get_prompts(&persisted, AppType::Claude).expect("load prompts");
    assert_eq!(prompts.len(), 1);

    let (id, prompt) = prompts.iter().next().expect("imported prompt");
    assert!(id.starts_with("imported-"), "unexpected id: {id}");
    assert_eq!(prompt.id, *id);
    assert_eq!(prompt.content, "existing live prompt\nwith two lines\n");
    assert_eq!(prompt.description.as_deref(), Some("从现有配置文件导入"));
    assert!(!prompt.enabled);
}

#[test]
#[serial]
fn prompt_import_command_reports_missing_live_file() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let state = state_from_config(MultiAppConfig::default());
    state.save().expect("persist config");

    let err = execute(PromptsCommand::Import, Some(AppType::Claude))
        .expect_err("missing live prompt file should fail");

    assert!(
        err.to_string().contains("提示词文件不存在"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial]
fn prompt_copy_command_copies_live_file_between_apps() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let claude_path = cc_switch_lib::get_claude_settings_path()
        .parent()
        .expect("claude settings parent")
        .join("CLAUDE.md");
    std::fs::create_dir_all(claude_path.parent().expect("claude parent"))
        .expect("create claude dir");
    std::fs::write(&claude_path, "# Shared instructions\nbody\n").expect("write claude prompt");

    // Destination app must look initialized for the copy to be allowed.
    let codex_dir = ensure_test_home().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("create codex dir");

    execute(
        PromptsCommand::Copy {
            from: AppType::Claude,
            to: AppType::Codex,
            force: false,
        },
        None,
    )
    .expect("copy command succeeds");

    let codex_prompt = std::fs::read_to_string(codex_dir.join("AGENTS.md")).expect("read copy");
    assert_eq!(codex_prompt, "# Shared instructions\nbody\n");
}

#[test]
#[serial]
fn prompt_copy_refuses_to_overwrite_without_force() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let claude_path = cc_switch_lib::get_claude_settings_path()
        .parent()
        .expect("claude settings parent")
        .join("CLAUDE.md");
    std::fs::create_dir_all(claude_path.parent().expect("claude parent"))
        .expect("create claude dir");
    std::fs::write(&claude_path, "source\n").expect("write claude prompt");

    let codex_dir = ensure_test_home().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("create codex dir");
    let codex_prompt = codex_dir.join("AGENTS.md");
    std::fs::write(&codex_prompt, "existing\n").expect("write existing codex prompt");

    let err = execute(
        PromptsCommand::Copy {
            from: AppType::Claude,
            to: AppType::Codex,
            force: false,
        },
        None,
    )
    .expect_err("copy without force should fail when destination exists");
    assert!(
        err.to_string().contains("已存在"),
        "unexpected error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&codex_prompt).expect("read existing"),
        "existing\n",
        "destination must stay untouched without --force"
    );
}

#[test]
#[serial]
fn prompt_copy_force_overwrites_and_leaves_no_preset() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let claude_path = cc_switch_lib::get_claude_settings_path()
        .parent()
        .expect("claude settings parent")
        .join("CLAUDE.md");
    std::fs::create_dir_all(claude_path.parent().expect("claude parent"))
        .expect("create claude dir");
    std::fs::write(&claude_path, "fresh source\n").expect("write claude prompt");

    let codex_dir = ensure_test_home().join(".codex");
    std::fs::create_dir_all(&codex_dir).expect("create codex dir");
    let codex_prompt = codex_dir.join("AGENTS.md");
    std::fs::write(&codex_prompt, "stale\n").expect("write existing codex prompt");

    execute(
        PromptsCommand::Copy {
            from: AppType::Claude,
            to: AppType::Codex,
            force: true,
        },
        None,
    )
    .expect("copy --force succeeds");

    assert_eq!(
        std::fs::read_to_string(&codex_prompt).expect("read copy"),
        "fresh source\n"
    );

    // A raw live-file copy must not register a Codex prompt preset.
    let state = cc_switch_lib::AppState::try_new().expect("reload state");
    let prompts = PromptService::get_prompts(&state, AppType::Codex).expect("load codex prompts");
    assert!(
        prompts.is_empty(),
        "copy must not create prompt presets, got {prompts:?}"
    );
}

#[test]
#[serial]
fn prompt_copy_rejects_uninitialized_destination_without_creating_dir() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let claude_path = cc_switch_lib::get_claude_settings_path()
        .parent()
        .expect("claude settings parent")
        .join("CLAUDE.md");
    std::fs::create_dir_all(claude_path.parent().expect("claude parent"))
        .expect("create claude dir");
    std::fs::write(&claude_path, "source\n").expect("write claude prompt");

    let err = execute(
        PromptsCommand::Copy {
            from: AppType::Claude,
            to: AppType::Gemini,
            force: true,
        },
        None,
    )
    .expect_err("uninitialized destination should fail");
    assert!(
        err.to_string().contains("尚未初始化"),
        "unexpected error: {err}"
    );
    assert!(
        !ensure_test_home().join(".gemini").exists(),
        "must not create the destination app directory"
    );
}

#[test]
#[serial]
fn prompt_copy_rejects_same_app() {
    let _guard = lock_test_mutex();
    reset_test_fs();
    ensure_test_home();

    let err = execute(
        PromptsCommand::Copy {
            from: AppType::Claude,
            to: AppType::Claude,
            force: true,
        },
        None,
    )
    .expect_err("same app should fail");
    assert!(
        err.to_string().contains("不能相同"),
        "unexpected error: {err}"
    );
}

#[test]
#[serial]
fn generate_prompt_id_falls_back_when_name_has_no_valid_slug_chars() {
    let ids = vec!["prompt".to_string(), "prompt-1".to_string()];
    let generated = PromptService::generate_prompt_id("!!!", &ids);
    assert_eq!(generated, "prompt-2");
}
