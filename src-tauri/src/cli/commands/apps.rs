//! Supported app listing for scripting and discovery.

use clap::Subcommand;
use clap::ValueEnum;

use crate::app_config::AppType;
use crate::error::AppError;

#[derive(Subcommand, Debug, Clone)]
pub enum AppsCommand {
    /// Print supported apps as a comma-separated list
    List,
}

pub fn execute(cmd: AppsCommand) -> Result<(), AppError> {
    match cmd {
        AppsCommand::List => {
            println!("{}", supported_apps_csv());
            Ok(())
        }
    }
}

/// CLI labels for `--app` (clap ValueEnum names: `open-code`, `open-claw`, …).
pub fn supported_app_labels() -> Vec<String> {
    AppType::value_variants()
        .iter()
        .filter_map(|app| {
            app.to_possible_value()
                .map(|v| v.get_name().to_string())
        })
        .collect()
}

pub fn supported_apps_csv() -> String {
    supported_app_labels().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_apps_csv_matches_cli_app_flags() {
        let csv = supported_apps_csv();
        assert_eq!(
            csv,
            "claude, codex, gemini, open-code, hermes, open-claw, pi, grok"
        );
    }

    #[test]
    fn every_label_is_in_user_expected_set() {
        let expected = [
            "claude",
            "codex",
            "gemini",
            "open-code",
            "hermes",
            "open-claw",
            "pi",
            "grok",
        ];
        let labels = supported_app_labels();
        for label in &labels {
            assert!(
                expected.contains(&label.as_str()),
                "unexpected app label not in agreed list: {label}"
            );
        }
        assert_eq!(labels.len(), expected.len());
    }
}
