//! Environment block generation for system prompts.

use std::path::Path;

use crate::client::DEFAULT_REASONING_MODEL;

/// Generates the environment block with runtime information.
pub fn environment_block(
    working_dir: Option<&Path>,
    is_git_repo: bool,
    platform: &str,
    os_version: &str,
    model_name: &str,
    model_id: &str,
) -> String {
    let cwd = working_dir
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let git_status = if is_git_repo { "Yes" } else { "No" };

    format!(
        r#"Here is useful information about the environment you are running in:
<env>
Working directory: {cwd}
Is directory a git repo: {git_status}
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

/// Checks if a directory is a git repository.
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
    fn test_environment_block() {
        let block = environment_block(
            Some(Path::new("/test/dir")),
            true,
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
