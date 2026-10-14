//! `cc-switch prompts common …` — manage the global common preamble that is
//! rendered into every app's live prompt file (marker section at the top).

use clap::Subcommand;

use crate::error::AppError;
use crate::services::prompt_common::{self, CommonPreset};
use crate::AppState;

#[derive(Subcommand, Debug)]
pub enum CommonPromptCommand {
    /// List common presets and mark the active one
    List,
    /// Show the content of a preset (or the active one by default)
    Show {
        /// Preset id (defaults to the active preset)
        id: Option<String>,
    },
    /// Create or update a common preset
    Set {
        /// Preset id
        id: String,
        /// Display name (defaults to the id)
        #[arg(long)]
        name: Option<String>,
        /// Read the content from a file (`-` for stdin)
        #[arg(long)]
        file: String,
    },
    /// Delete a common preset (deactivates it first if it was active)
    Delete {
        /// Preset id
        id: String,
    },
    /// Activate a common preset and repaint every app's live prompt file
    Use {
        /// Preset id
        id: String,
    },
    /// Deactivate the preamble (marker sections are removed from live files)
    Off,
    /// Re-render the preamble into every existing live prompt file
    Reapply,
}

pub fn execute(cmd: CommonPromptCommand) -> Result<(), AppError> {
    let state = AppState::try_new()?;
    match cmd {
        CommonPromptCommand::List => list(&state),
        CommonPromptCommand::Show { id } => show(&state, id.as_deref()),
        CommonPromptCommand::Set { id, name, file } => set(&state, &id, name, &file),
        CommonPromptCommand::Delete { id } => delete(&state, &id),
        CommonPromptCommand::Use { id } => use_preset(&state, &id),
        CommonPromptCommand::Off => off(&state),
        CommonPromptCommand::Reapply => reapply(&state),
    }
}

fn list(state: &AppState) -> Result<(), AppError> {
    let current = prompt_common::list(state)?;
    if current.presets.is_empty() {
        println!("{}", crate::cli::ui::info("暂无公共提示词预设"));
        return Ok(());
    }
    for preset in &current.presets {
        let marker = if current.active_id.as_deref() == Some(preset.id.as_str()) {
            "*"
        } else {
            " "
        };
        println!("{marker} {}  {}", preset.id, preset.name);
    }
    match &current.active_id {
        Some(id) => println!(
            "{}",
            crate::cli::ui::info(&format!("当前激活: {id}(渲染到所有 live 提示词文件顶部)"))
        ),
        None => println!(
            "{}",
            crate::cli::ui::info("当前未激活(live 文件不含公共前缀)")
        ),
    }
    Ok(())
}

fn show(state: &AppState, id: Option<&str>) -> Result<(), AppError> {
    let current = prompt_common::list(state)?;
    let target_id = match id {
        Some(id) => id.to_string(),
        None => current
            .active_id
            .clone()
            .ok_or_else(|| AppError::InvalidInput("当前没有激活的公共预设,请指定 id".into()))?,
    };
    let preset = current
        .presets
        .iter()
        .find(|p| p.id == target_id)
        .ok_or_else(|| AppError::InvalidInput(format!("公共预设 {target_id} 不存在")))?;
    println!("== {} ({}) ==", preset.name, preset.id);
    println!("{}", preset.content);
    Ok(())
}

fn set(state: &AppState, id: &str, name: Option<String>, file: &str) -> Result<(), AppError> {
    let content = if file == "-" {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|e| AppError::Message(format!("读取 stdin 失败: {e}")))?;
        buffer
    } else {
        std::fs::read_to_string(file).map_err(|e| AppError::io(std::path::Path::new(file), e))?
    };
    let preset = CommonPreset {
        id: id.to_string(),
        name: name.unwrap_or_else(|| id.to_string()),
        content,
    };
    let updated = prompt_common::upsert(state, preset)?;
    println!(
        "{}",
        crate::cli::ui::success(&format!(
            "{}公共预设 {id}{}",
            if updated { "已更新" } else { "已创建" },
            if updated {
                "(当前激活,已重渲染 live 文件)"
            } else {
                ""
            }
        ))
    );
    Ok(())
}

fn delete(state: &AppState, id: &str) -> Result<(), AppError> {
    match prompt_common::delete(state, id)? {
        true => println!(
            "{}",
            crate::cli::ui::success(&format!("已删除公共预设 {id}"))
        ),
        false => {
            return Err(AppError::InvalidInput(format!("公共预设 {id} 不存在")));
        }
    }
    Ok(())
}

fn use_preset(state: &AppState, id: &str) -> Result<(), AppError> {
    let touched = prompt_common::use_preset(state, id)?;
    println!(
        "{}",
        crate::cli::ui::success(&format!(
            "已激活公共预设 {id},重渲染了 {touched} 个 live 提示词文件"
        ))
    );
    Ok(())
}

fn off(state: &AppState) -> Result<(), AppError> {
    let touched = prompt_common::deactivate(state)?;
    println!(
        "{}",
        crate::cli::ui::success(&format!(
            "已停用公共前缀,更新了 {touched} 个 live 提示词文件"
        ))
    );
    Ok(())
}

fn reapply(state: &AppState) -> Result<(), AppError> {
    let touched = prompt_common::reapply_common_to_all_apps(state)?;
    println!(
        "{}",
        crate::cli::ui::success(&format!("已重渲染 {touched} 个 live 提示词文件"))
    );
    Ok(())
}
