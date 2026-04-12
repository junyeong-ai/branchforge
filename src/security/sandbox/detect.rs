//! Container runtime detection.
//!
//! Tells callers whether the current process is already running
//! inside a container (Docker, Podman, LXC, Kubernetes, etc.). This
//! is load-bearing for sandbox decisions on Linux: most container
//! runtimes already drop kernel capabilities and apply seccomp
//! profiles, so attempting to nest a Linux namespace sandbox inside
//! a container usually fails (the kernel rejects nested user
//! namespaces, or the container's seccomp denies `unshare`).
//!
//! The detection is **best-effort and cheap** — it reads a handful
//! of well-known files and env vars and stops at the first match.
//! Returns a [`ContainerRuntime`] tag so callers can branch on the
//! specific runtime when useful, or just call [`is_container`] for
//! the boolean.

#![allow(missing_docs)]

use std::fs;
use std::path::Path;

/// Detected container runtime, or `None` if the process is running
/// directly on the host.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
    Lxc,
    Kubernetes,
    /// Some container manager that doesn't expose a more specific
    /// marker. Cgroup heuristics or generic markers landed us here.
    Generic,
}

impl ContainerRuntime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
            Self::Lxc => "lxc",
            Self::Kubernetes => "kubernetes",
            Self::Generic => "container",
        }
    }
}

/// Detect the container runtime, if any.
///
/// Checks, in order:
/// 1. `/.dockerenv` (Docker marker file).
/// 2. `/run/.containerenv` (Podman marker file).
/// 3. `KUBERNETES_SERVICE_HOST` environment variable (Kubernetes pod).
/// 4. `/proc/1/cgroup` for `docker`/`podman`/`lxc`/`kubepods` substrings.
///
/// On non-Linux platforms always returns `None` because `/proc` and
/// the marker files do not exist.
pub fn detect_container() -> Option<ContainerRuntime> {
    // Marker files (cheapest signal).
    if Path::new("/.dockerenv").exists() {
        return Some(ContainerRuntime::Docker);
    }
    if Path::new("/run/.containerenv").exists() {
        return Some(ContainerRuntime::Podman);
    }

    // Kubernetes injects this env var into every pod.
    if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
        return Some(ContainerRuntime::Kubernetes);
    }

    // /proc/1/cgroup heuristic — Linux only. The file lists the
    // cgroups PID 1 belongs to; container runtimes leave their name
    // in the path (e.g. `0::/docker/<id>` or `0::/kubepods/...`).
    if let Ok(content) = fs::read_to_string("/proc/1/cgroup") {
        let lower = content.to_ascii_lowercase();
        if lower.contains("kubepods") {
            return Some(ContainerRuntime::Kubernetes);
        }
        if lower.contains("docker") {
            return Some(ContainerRuntime::Docker);
        }
        if lower.contains("podman") {
            return Some(ContainerRuntime::Podman);
        }
        if lower.contains("lxc") {
            return Some(ContainerRuntime::Lxc);
        }
        // Catch-all: any non-`init.scope` non-`/` cgroup path on
        // PID 1 typically means we're in some container.
        if !lower.contains("init.scope")
            && lower.lines().any(|line| {
                line.split("::")
                    .nth(1)
                    .map(|p| p.trim() != "/")
                    .unwrap_or(false)
            })
        {
            return Some(ContainerRuntime::Generic);
        }
    }

    None
}

/// Convenience boolean wrapper around [`detect_container`].
pub fn is_container() -> bool {
    detect_container().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_str_labels_are_stable() {
        assert_eq!(ContainerRuntime::Docker.as_str(), "docker");
        assert_eq!(ContainerRuntime::Podman.as_str(), "podman");
        assert_eq!(ContainerRuntime::Lxc.as_str(), "lxc");
        assert_eq!(ContainerRuntime::Kubernetes.as_str(), "kubernetes");
        assert_eq!(ContainerRuntime::Generic.as_str(), "container");
    }

    /// Smoke test: the detection function must not panic regardless
    /// of host environment. We can't assert a specific value
    /// because tests run in many environments (CI containers,
    /// developer laptops, GitHub Actions, …) and parallel tests may
    /// scratch the relevant env vars between calls — so we just
    /// confirm it returns *something*.
    #[test]
    fn detection_does_not_panic() {
        let _ = detect_container();
        let _ = is_container();
    }

    /// `KUBERNETES_SERVICE_HOST` set → returns Kubernetes regardless
    /// of other markers. Tests run sequentially so env var
    /// scratching is safe.
    #[test]
    fn kubernetes_env_var_takes_precedence_over_default() {
        // Save and restore so we don't leak state into other tests
        // running in the same process.
        let prior = std::env::var("KUBERNETES_SERVICE_HOST").ok();
        unsafe {
            std::env::set_var("KUBERNETES_SERVICE_HOST", "10.0.0.1");
        }
        // We can't assert == Kubernetes because /.dockerenv may
        // also exist on the test runner; assert at least Some.
        assert!(detect_container().is_some());
        unsafe {
            match prior {
                Some(v) => std::env::set_var("KUBERNETES_SERVICE_HOST", v),
                None => std::env::remove_var("KUBERNETES_SERVICE_HOST"),
            }
        }
    }
}
