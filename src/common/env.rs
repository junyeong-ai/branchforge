//! Environment-variable lookup seam shared across the SDK.
//!
//! # Why this exists
//!
//! Pre-Phase-G the SDK called `std::env::var` directly from multiple
//! library modules, making tests depend on process-wide state and
//! breaking parallel test execution. Phase G-3 introduced an
//! `EnvLookup` trait inside `src/client/preset.rs` to close the
//! credential-resolution hole; Phase H-1 promoted it here so every
//! module can depend on the same seam instead of reinventing one.
//!
//! # Contract
//!
//! Library code that needs to read an environment variable MUST
//! depend on [`EnvLookup`], not on `std::env::var` directly. The
//! production entry point uses [`SystemEnv`], which delegates to
//! `std::env::var`; test harnesses inject an in-memory fake.
//!
//! This lets the entire SDK stay hermetic under `cargo test --jobs N`:
//! no two tests race on the process environment, and embedding
//! scenarios (serverless, sandboxed, keychain-backed) can plug
//! their own lookup without touching library internals.

use std::fmt::Debug;

/// Abstract environment-variable lookup. Production code resolves via
/// [`SystemEnv`]; test harnesses inject in-memory fakes.
///
/// New modules that need env access MUST depend on this trait rather
/// than calling `std::env::var` directly. Phase H-1 promoted the
/// trait from `client::preset` to `common::env` so all SDK modules
/// can share a single seam.
pub trait EnvLookup: Debug + Send + Sync {
    /// Return the value of `name`, or `None` if unset.
    fn get(&self, name: &str) -> Option<String>;

    /// Convenience: lookup with a default fallback. The default
    /// implementation suffices for every backend — backends never
    /// need to override this.
    fn get_or(&self, name: &str, default: &str) -> String {
        self.get(name).unwrap_or_else(|| default.to_string())
    }
}

/// Process-environment implementation of [`EnvLookup`]. Delegates
/// every lookup to `std::env::var`. This is the only place in the
/// SDK that calls `std::env::var`; all other modules go through the
/// [`EnvLookup`] trait.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemEnv;

impl EnvLookup for SystemEnv {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_env_delegates_to_process_env() {
        // SAFETY: the test isolates a unique key not used anywhere
        // else in the test suite to avoid races with parallel tests.
        let key = "BRANCHFORGE_H1_SYSTEMENV_TEST_KEY";
        // We do not mutate the env here — instead we assert the
        // trait returns `None` for a key that is deterministically
        // unset. The inverse case (key set) would require process
        // mutation and is intentionally covered by the hermetic
        // fake-based tests in client::preset.
        assert!(SystemEnv.get(key).is_none());
        assert_eq!(SystemEnv.get_or(key, "fallback"), "fallback");
    }

    #[derive(Debug, Default)]
    struct FakeEnv(std::collections::HashMap<String, String>);

    impl EnvLookup for FakeEnv {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn default_get_or_uses_fallback_when_absent() {
        let env = FakeEnv::default();
        assert_eq!(env.get_or("MISSING", "default"), "default");
    }

    #[test]
    fn default_get_or_returns_value_when_present() {
        let mut env = FakeEnv::default();
        env.0.insert("PRESENT".into(), "actual".into());
        assert_eq!(env.get_or("PRESENT", "default"), "actual");
    }
}
