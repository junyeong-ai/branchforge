//! Environment block generation for system prompts.
//!
//! The `environment_block` function assembles the `<env>...</env>` section
//! that appears in every agent's system prompt. Most fields (working
//! directory, platform, OS version, date, model) are Layer 1 — any agent
//! can legitimately publish them. The git-repo flag is Layer 2b: it only
//! makes sense for a coding agent running over a checked-out repository.
//! Layer 1 callers pass `is_git_repo = None` and the line is omitted;
//! Layer 2b callers compute the value via `is_git_repository` (which is
//! itself feature-gated behind `coding-tools`) and pass `Some(...)`.
//!
//! A richer extension point — `crate::context::EnvironmentSource` — is
//! available for third-party crates that want to plug in additional facts
//! (tenant ids, compliance tags, cloud metadata, …) without modifying this
//! function's signature.

use std::path::Path;

use crate::agent::DEFAULT_REASONING_MODEL;

/// Generates the environment block with runtime information.
///
/// `is_git_repo` is optional: `Some(true)` / `Some(false)` emit the
/// corresponding line, `None` omits it entirely (the default for
/// non-coding agents and pure-core builds).
pub fn environment_block(
    working_dir: Option<&Path>,
    is_git_repo: Option<bool>,
    platform: &str,
    os_version: &str,
    model_name: &str,
    model_id: &str,
) -> String {
    let cwd = working_dir
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let git_line = match is_git_repo {
        Some(true) => "\nIs directory a git repo: Yes",
        Some(false) => "\nIs directory a git repo: No",
        None => "",
    };

    format!(
        r#"Here is useful information about the environment you are running in:
<env>
Working directory: {cwd}{git_line}
Platform: {platform}
OS Version: {os_version}
Today's date: {date}
</env>
You are powered by the model named {model_name}. The exact model ID is {model_id}.

Assistant knowledge cutoff is May 2025.

<claude_background_info>
The most recent frontier Claude model is Claude Opus 4.6 (model ID: '{frontier}').
</claude_background_info>"#,
        frontier = DEFAULT_REASONING_MODEL
    )
}

/// Checks if a directory is a git repository by looking for a `.git`
/// directory.
///
/// Available only under `coding-tools` — pure-core and `local-fs`-only
/// builds do not care whether the current directory is a repository. A
/// future refinement will move this helper into `src/coding/git_context.rs`
/// alongside richer git integration (status, recent commits, diff) behind
/// the same feature gate.
#[cfg(feature = "coding-tools")]
pub(crate) fn is_git_repository(dir: Option<&Path>) -> bool {
    dir.map(|d| d.join(".git").exists()).unwrap_or(false)
}

/// Gets the current platform identifier.
pub(crate) fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Gets the OS version string.
pub(crate) fn os_version() -> String {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("uname")
            .arg("-r")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("Darwin {}", s.trim()))
            .unwrap_or_else(|| "Darwin".to_string())
    }

    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| {
                        l.trim_start_matches("PRETTY_NAME=")
                            .trim_matches('"')
                            .to_string()
                    })
            })
            .unwrap_or_else(|| "Linux".to_string())
    }

    #[cfg(target_os = "windows")]
    {
        "Windows".to_string()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        "Unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_block_with_git_flag() {
        let block = environment_block(
            Some(Path::new("/test/dir")),
            Some(true),
            "darwin",
            "Darwin 25.1.0",
            "Claude Sonnet 4.5",
            "claude-sonnet-4-5-20250929",
        );

        assert!(block.contains("/test/dir"));
        assert!(block.contains("Is directory a git repo: Yes"));
        assert!(block.contains("darwin"));
        assert!(block.contains("claude-sonnet-4-5-20250929"));
        assert!(block.contains("Claude Opus 4.6"));
    }

    #[test]
    fn test_environment_block_without_git_flag_omits_line() {
        let block = environment_block(
            Some(Path::new("/test/dir")),
            None,
            "linux",
            "Ubuntu 24.04",
            "Claude Sonnet 4.5",
            "claude-sonnet-4-5-20250929",
        );

        // Layer 1 / local-fs callers pass `None` — the line is simply
        // omitted rather than displayed as "Is directory a git repo: No".
        assert!(block.contains("/test/dir"));
        assert!(!block.contains("Is directory a git repo"));
        assert!(block.contains("Platform: linux"));
    }

    #[test]
    fn test_environment_block_with_git_false_renders_line() {
        let block = environment_block(
            Some(Path::new("/test/dir")),
            Some(false),
            "darwin",
            "Darwin 25.1.0",
            "Claude",
            "test-model",
        );
        assert!(block.contains("Is directory a git repo: No"));
    }

    #[cfg(feature = "coding-tools")]
    #[test]
    fn test_is_git_repository() {
        assert!(!is_git_repository(None));
        assert!(!is_git_repository(Some(Path::new("/nonexistent"))));
    }

    #[test]
    fn test_current_platform() {
        let platform = current_platform();
        assert!(!platform.is_empty());
        #[cfg(target_os = "macos")]
        assert_eq!(platform, "darwin");
        #[cfg(target_os = "linux")]
        assert_eq!(platform, "linux");
    }
}
