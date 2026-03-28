//! Environment variable sanitization.

use std::collections::HashMap;

const SAFE_ENV_VARS: &[&str] = &[
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "PATH",
    "PWD",
    "TMPDIR",
    "TMP",
    "TEMP",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "NODE_PATH",
    "NPM_CONFIG_PREFIX",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "EDITOR",
    "VISUAL",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "SSH_AUTH_SOCK",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
];

const BLOCKED_ENV_PATTERNS: &[&str] = &[
    // Dynamic linker injection
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "LD_DEBUG",
    "LD_PROFILE",
    "LD_DEBUG_OUTPUT",
    "LD_HWCAP_MASK",
    "LD_BIND_",
    "LD_TRACE_",
    // macOS dynamic linker
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_",
    "DYLD_IMAGE_",
    "DYLD_PRINT_",
    // Compiler/build tool injection
    "CC",
    "CXX",
    "LD",
    "AR",
    "AS",
    "CFLAGS",
    "CXXFLAGS",
    "LDFLAGS",
    "CPPFLAGS",
    "MAKEFLAGS",
    "CMAKE_",
    // Python injection
    "PYTHONSTARTUP",
    "PYTHONHOME",
    "PYTHONUSERBASE",
    "PYTHONWARNINGS",
    "PYTHONEXECUTABLE",
    "PYTHONDONTWRITEBYTECODE",
    // Shell startup injection
    "BASH_ENV",
    "ENV",
    "BASH_FUNC_",
    "ZDOTDIR",
    "FPATH",
    "CDPATH",
    // Password/auth prompts
    "SSH_ASKPASS",
    "SUDO_ASKPASS",
    "GIT_ASKPASS",
    // SSH/Git command override
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "SVN_SSH",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    // Shell prompt commands
    "PROMPT_COMMAND",
    "PS1",
    "PS2",
    "PS4",
    // Language runtime injection
    "PERL5OPT",
    "PERL5LIB",
    "PERL_HASH_SEED_DEBUG",
    "PERL_MB_OPT",
    "PERL_MM_OPT",
    "RUBYOPT",
    "RUBYLIB",
    "NODE_OPTIONS",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JAVA_HOME",
    // Rust injection
    "RUSTFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_LOG",
    "CARGO_BUILD_",
    // Debugger/tracing injection
    "STRACE_OPTS",
    "VALGRIND_OPTS",
    "GDB_STARTUP_COMMANDS",
    "LLDB_",
    // glibc exploits
    "GLIBC_TUNABLES",
    "MALLOC_CHECK_",
    "MALLOC_PERTURB_",
    // IFS (field separator attacks)
    "IFS",
    // Pager exploits (less/more can execute commands)
    "LESS",
    "LESSOPEN",
    "LESSCLOSE",
    "MORE",
    "MOST",
    // .NET/PowerShell injection
    "DOTNET_",
    "POWERSHELL_",
    "PSModulePath",
    // Go injection
    "GOPROXY",
    "GOFLAGS",
    // Package manager injection
    "npm_config_",
    "NPM_CONFIG_REGISTRY",
    "NPM_CONFIG_CAFILE",
    "NODE_EXTRA_CA_CERTS",
    "YARN_",
    "PIP_INDEX_URL",
    "PIP_EXTRA_INDEX_URL",
    "PIP_TRUSTED_HOST",
    "PIPENV_",
    "UV_INDEX_URL",
    "UV_EXTRA_INDEX_URL",
    "CARGO_REGISTRIES_",
    "CARGO_NET_",
    // Misc dangerous
    "BROWSER",
    "TEXINPUTS",
    "TERMCAP",
    "TERMINFO",
];

const SAFE_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

#[derive(Debug, Clone)]
pub struct SanitizedEnv {
    vars: HashMap<String, String>,
}

impl SanitizedEnv {
    pub fn from_current() -> Self {
        Self::from_env(std::env::vars())
    }

    pub fn from_env(env: impl Iterator<Item = (String, String)>) -> Self {
        let mut vars = HashMap::new();

        for (key, value) in env {
            if Self::is_blocked(&key) {
                continue;
            }
            if Self::is_safe(&key) {
                vars.insert(key, value);
            }
        }

        vars.insert("PATH".to_string(), SAFE_PATH.to_string());

        Self { vars }
    }

    pub fn var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        if !Self::is_blocked(&key) {
            self.vars.insert(key, value.into());
        }
        self
    }

    pub fn working_dir(mut self, dir: impl AsRef<std::path::Path>) -> Self {
        self.vars
            .insert("PWD".to_string(), dir.as_ref().display().to_string());
        self
    }

    pub fn with_vars(mut self, vars: HashMap<String, String>) -> Self {
        for (key, value) in vars {
            if !Self::is_blocked(&key) {
                self.vars.insert(key, value);
            }
        }
        self
    }

    pub fn vars(&self) -> &HashMap<String, String> {
        &self.vars
    }

    pub fn into_vec(self) -> Vec<(String, String)> {
        self.vars.into_iter().collect()
    }

    fn is_blocked(key: &str) -> bool {
        BLOCKED_ENV_PATTERNS
            .iter()
            .any(|pattern| key.starts_with(pattern))
    }

    fn is_safe(key: &str) -> bool {
        SAFE_ENV_VARS.contains(&key)
    }
}

impl Default for SanitizedEnv {
    fn default() -> Self {
        Self::from_current()
    }
}

impl IntoIterator for SanitizedEnv {
    type Item = (String, String);
    type IntoIter = std::collections::hash_map::IntoIter<String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.vars.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ld_preload_blocked() {
        let env = vec![
            ("LD_PRELOAD".to_string(), "/evil.so".to_string()),
            ("HOME".to_string(), "/home/user".to_string()),
        ];
        let sanitized = SanitizedEnv::from_env(env.into_iter());

        assert!(!sanitized.vars.contains_key("LD_PRELOAD"));
        assert!(sanitized.vars.contains_key("HOME"));
    }

    #[test]
    fn test_bash_env_blocked() {
        let env = vec![
            ("BASH_ENV".to_string(), "/evil.sh".to_string()),
            ("USER".to_string(), "test".to_string()),
        ];
        let sanitized = SanitizedEnv::from_env(env.into_iter());

        assert!(!sanitized.vars.contains_key("BASH_ENV"));
        assert!(sanitized.vars.contains_key("USER"));
    }

    #[test]
    fn test_bash_func_blocked() {
        let env = vec![(
            "BASH_FUNC_evil%%".to_string(),
            "() { /bin/rm -rf /; }".to_string(),
        )];
        let sanitized = SanitizedEnv::from_env(env.into_iter());

        assert!(!sanitized.vars.contains_key("BASH_FUNC_evil%%"));
    }

    #[test]
    fn test_safe_path_forced() {
        let env = vec![("PATH".to_string(), "/evil:/bin".to_string())];
        let sanitized = SanitizedEnv::from_env(env.into_iter());

        assert_eq!(sanitized.vars.get("PATH").unwrap(), SAFE_PATH);
    }

    #[test]
    fn test_working_dir() {
        let sanitized = SanitizedEnv::from_env(std::iter::empty()).working_dir("/tmp/sandbox");

        assert_eq!(sanitized.vars.get("PWD").unwrap(), "/tmp/sandbox");
    }

    #[test]
    fn test_dyld_blocked() {
        let env = vec![
            (
                "DYLD_INSERT_LIBRARIES".to_string(),
                "/evil.dylib".to_string(),
            ),
            ("DYLD_LIBRARY_PATH".to_string(), "/evil/libs".to_string()),
        ];
        let sanitized = SanitizedEnv::from_env(env.into_iter());

        assert!(!sanitized.vars.contains_key("DYLD_INSERT_LIBRARIES"));
        assert!(!sanitized.vars.contains_key("DYLD_LIBRARY_PATH"));
    }

    #[test]
    fn test_package_manager_blocked() {
        let env = vec![
            (
                "npm_config_registry".to_string(),
                "https://evil.com".to_string(),
            ),
            ("PIP_INDEX_URL".to_string(), "https://evil.com".to_string()),
            ("YARN_REGISTRY".to_string(), "https://evil.com".to_string()),
            (
                "CARGO_REGISTRIES_EVIL".to_string(),
                "https://evil.com".to_string(),
            ),
        ];
        let sanitized = SanitizedEnv::from_env(env.into_iter());

        assert!(!sanitized.vars.contains_key("npm_config_registry"));
        assert!(!sanitized.vars.contains_key("PIP_INDEX_URL"));
        assert!(!sanitized.vars.contains_key("YARN_REGISTRY"));
        assert!(!sanitized.vars.contains_key("CARGO_REGISTRIES_EVIL"));
    }
}
