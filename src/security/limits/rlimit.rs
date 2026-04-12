//! Resource limits using setrlimit.

#![allow(missing_docs)]

use crate::security::SecurityError;

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;

#[derive(Debug, Clone)]
pub struct ResourceLimits {
    pub cpu_time: Option<u64>,
    pub file_size: Option<u64>,
    pub open_files: Option<u64>,
    pub processes: Option<u64>,
    pub virtual_memory: Option<u64>,
    pub data_size: Option<u64>,
    pub stack_size: Option<u64>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_time: Some(300),       // 5 minutes
            file_size: Some(100 * MB), // 100 MB
            open_files: Some(256),
            processes: None, // NPROC limits the entire UID, not just child processes
            virtual_memory: Some(2 * GB), // 2 GB
            data_size: Some(GB), // 1 GB (applied on Linux only)
            stack_size: Some(8 * MB), // 8 MB
        }
    }
}

impl ResourceLimits {
    pub fn none() -> Self {
        Self {
            cpu_time: None,
            file_size: None,
            open_files: None,
            processes: None,
            virtual_memory: None,
            data_size: None,
            stack_size: None,
        }
    }

    pub fn strict() -> Self {
        Self {
            cpu_time: Some(60),       // 1 minute
            file_size: Some(10 * MB), // 10 MB
            open_files: Some(64),
            processes: None, // NPROC limits the entire UID — use sandbox instead
            virtual_memory: Some(512 * MB), // 512 MB
            data_size: Some(256 * MB), // 256 MB
            stack_size: Some(MB), // 1 MB
        }
    }

    pub fn cpu_time(mut self, seconds: u64) -> Self {
        self.cpu_time = Some(seconds);
        self
    }

    pub fn file_size(mut self, bytes: u64) -> Self {
        self.file_size = Some(bytes);
        self
    }

    pub fn open_files(mut self, count: u64) -> Self {
        self.open_files = Some(count);
        self
    }

    pub fn processes(mut self, count: u64) -> Self {
        self.processes = Some(count);
        self
    }

    pub fn virtual_memory(mut self, bytes: u64) -> Self {
        self.virtual_memory = Some(bytes);
        self
    }

    #[cfg(unix)]
    pub fn apply(&self) -> Result<(), SecurityError> {
        use rustix::process::{Resource, Rlimit, setrlimit};

        if let Some(cpu) = self.cpu_time {
            let rlim = Rlimit {
                current: Some(cpu),
                maximum: Some(cpu),
            };
            setrlimit(Resource::Cpu, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("CPU: {}", e)))?;
        }

        if let Some(fsize) = self.file_size {
            let rlim = Rlimit {
                current: Some(fsize),
                maximum: Some(fsize),
            };
            setrlimit(Resource::Fsize, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("FSIZE: {}", e)))?;
        }

        if let Some(nofile) = self.open_files {
            let rlim = Rlimit {
                current: Some(nofile),
                maximum: Some(nofile),
            };
            setrlimit(Resource::Nofile, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("NOFILE: {}", e)))?;
        }

        if let Some(nproc) = self.processes {
            let rlim = Rlimit {
                current: Some(nproc),
                maximum: Some(nproc),
            };
            setrlimit(Resource::Nproc, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("NPROC: {}", e)))?;
        }

        #[cfg(target_os = "linux")]
        if let Some(vmem) = self.virtual_memory {
            let rlim = Rlimit {
                current: Some(vmem),
                maximum: Some(vmem),
            };
            setrlimit(Resource::As, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("AS: {}", e)))?;
        }

        // RLIMIT_DATA is only supported on Linux.
        // macOS rejects it with EINVAL — the kernel manages virtual memory directly.
        #[cfg(target_os = "linux")]
        if let Some(data) = self.data_size {
            let rlim = Rlimit {
                current: Some(data),
                maximum: Some(data),
            };
            setrlimit(Resource::Data, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("DATA: {}", e)))?;
        }

        if let Some(stack) = self.stack_size {
            let rlim = Rlimit {
                current: Some(stack),
                maximum: Some(stack),
            };
            setrlimit(Resource::Stack, rlim)
                .map_err(|e| SecurityError::ResourceLimit(format!("STACK: {}", e)))?;
        }

        Ok(())
    }

    #[cfg(not(unix))]
    pub fn apply(&self) -> Result<(), SecurityError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_limits() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.cpu_time, Some(300));
        assert_eq!(limits.open_files, Some(256));
    }

    #[test]
    fn test_strict_limits() {
        let limits = ResourceLimits::strict();
        assert_eq!(limits.cpu_time, Some(60));
        assert!(limits.processes.is_none());
    }

    #[test]
    fn test_none_limits() {
        let limits = ResourceLimits::none();
        assert!(limits.cpu_time.is_none());
        assert!(limits.file_size.is_none());
    }

    #[test]
    fn test_builder() {
        let limits = ResourceLimits::none()
            .cpu_time(120)
            .file_size(1024 * 1024)
            .open_files(128);

        assert_eq!(limits.cpu_time, Some(120));
        assert_eq!(limits.file_size, Some(1024 * 1024));
        assert_eq!(limits.open_files, Some(128));
    }
}
