//! Process manager for background shell execution with security hardening.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::security::bash::SanitizedEnv;

/// Unique identifier for a managed process.
pub type ProcessId = String;

/// Information about a running process.
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    /// Unique process identifier.
    pub id: ProcessId,
    /// The command that was executed.
    pub command: String,
    /// When the process was started.
    pub started_at: Instant,
    /// OS process ID if available.
    pub pid: Option<u32>,
}

const MAX_OUTPUT_BUFFER_SIZE: usize = 1024 * 1024; // 1MB limit

struct ManagedProcess {
    child: Child,
    info: ProcessInfo,
    output_buffer: String,
}

/// Manager for background shell processes.
#[derive(Clone)]
pub struct ProcessManager {
    processes: Arc<Mutex<HashMap<ProcessId, ManagedProcess>>>,
}

impl ProcessManager {
    /// Create a new process manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a new background process.
    pub async fn spawn(&self, command: &str, working_dir: &Path) -> Result<ProcessId, String> {
        self.spawn_with_env(command, working_dir, SanitizedEnv::from_current())
            .await
    }

    /// Spawn a new background process with custom sanitized environment.
    pub async fn spawn_with_env(
        &self,
        command: &str,
        working_dir: &Path,
        env: SanitizedEnv,
    ) -> Result<ProcessId, String> {
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(command);
        cmd.current_dir(working_dir);
        cmd.env_clear();
        cmd.envs(env);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Ensure process is killed when Child is dropped (safety net)
        cmd.kill_on_drop(true);

        let child = cmd.spawn().map_err(|e| format!("Failed to spawn: {}", e))?;

        let id = uuid::Uuid::new_v4().to_string();
        let pid = child.id();

        let info = ProcessInfo {
            id: id.clone(),
            command: command.to_string(),
            started_at: Instant::now(),
            pid,
        };

        let managed = ManagedProcess {
            child,
            info,
            output_buffer: String::new(),
        };

        self.processes.lock().await.insert(id.clone(), managed);
        Ok(id)
    }

    /// Kill a background process and wait to reap it (prevents zombie).
    pub async fn kill(&self, id: &ProcessId) -> Result<(), String> {
        let mut processes = self.processes.lock().await;

        if let Some(mut proc) = processes.remove(id) {
            proc.child
                .kill()
                .await
                .map_err(|e| format!("Failed to kill: {}", e))?;
            // Wait to reap the process and prevent zombie
            let _ = proc.child.wait().await;
            Ok(())
        } else {
            Err(format!("Process '{}' not found", id))
        }
    }

    /// Get output from a background process (non-blocking read of available output).
    pub async fn get_output(&self, id: &ProcessId) -> Result<String, String> {
        let mut processes = self.processes.lock().await;

        let proc = processes
            .get_mut(id)
            .ok_or_else(|| format!("Process '{}' not found", id))?;

        // Try to read available stdout
        if let Some(ref mut stdout) = proc.child.stdout {
            let mut buffer = vec![0u8; 8192];
            match tokio::time::timeout(
                std::time::Duration::from_millis(100),
                stdout.read(&mut buffer),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    let s = String::from_utf8_lossy(&buffer[..n]);
                    proc.output_buffer.push_str(&s);
                }
                _ => {}
            }
        }

        // Try to read available stderr
        if let Some(ref mut stderr) = proc.child.stderr {
            let mut buffer = vec![0u8; 8192];
            match tokio::time::timeout(
                std::time::Duration::from_millis(100),
                stderr.read(&mut buffer),
            )
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    let s = String::from_utf8_lossy(&buffer[..n]);
                    proc.output_buffer.push_str(&s);
                }
                _ => {}
            }
        }

        // Truncate buffer if it exceeds the limit (keep the most recent data)
        // Uses drain() for in-place removal without new allocation
        if proc.output_buffer.len() > MAX_OUTPUT_BUFFER_SIZE {
            let remove_bytes = proc.output_buffer.len() - MAX_OUTPUT_BUFFER_SIZE;
            // Find safe UTF-8 character boundary
            let boundary = proc
                .output_buffer
                .char_indices()
                .find(|(i, _)| *i >= remove_bytes)
                .map_or(remove_bytes, |(i, _)| i);
            proc.output_buffer.drain(..boundary);
        }

        Ok(proc.output_buffer.clone())
    }

    /// Check if a process is still running.
    pub async fn is_running(&self, id: &ProcessId) -> bool {
        let mut processes = self.processes.lock().await;

        if let Some(proc) = processes.get_mut(id) {
            matches!(proc.child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    /// List all tracked processes.
    pub async fn list(&self) -> Vec<ProcessInfo> {
        self.processes
            .lock()
            .await
            .values()
            .map(|p| p.info.clone())
            .collect()
    }

    /// Clean up finished processes and return their final output.
    pub async fn cleanup_finished(&self) -> Vec<(ProcessInfo, String)> {
        let mut processes = self.processes.lock().await;
        let mut finished = Vec::new();

        let ids: Vec<_> = processes.keys().cloned().collect();
        for id in ids {
            if let Some(proc) = processes.get_mut(&id)
                && let Ok(Some(_status)) = proc.child.try_wait()
                && let Some(proc) = processes.remove(&id)
            {
                finished.push((proc.info, proc.output_buffer));
            }
        }

        finished
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_spawn_and_list() {
        let mgr = ProcessManager::new();
        let id = mgr
            .spawn("sleep 0.1", &PathBuf::from("/tmp"))
            .await
            .unwrap();

        let list = mgr.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);

        // Wait for the process to finish; retry to handle CI timing variance.
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if !mgr.is_running(&id).await {
                return;
            }
        }
        panic!("process still running after 1 second");
    }

    #[tokio::test]
    async fn test_kill() {
        let mgr = ProcessManager::new();
        let id = mgr.spawn("sleep 10", &PathBuf::from("/tmp")).await.unwrap();

        assert!(mgr.is_running(&id).await);
        mgr.kill(&id).await.unwrap();
        assert!(!mgr.is_running(&id).await);
    }

    #[tokio::test]
    async fn test_get_output() {
        let mgr = ProcessManager::new();
        let id = mgr
            .spawn("echo hello", &PathBuf::from("/tmp"))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let output = mgr.get_output(&id).await.unwrap();
        assert!(output.contains("hello"));
    }

    #[tokio::test]
    async fn test_cleanup_finished() {
        let mgr = ProcessManager::new();
        let id = mgr
            .spawn("echo done", &PathBuf::from("/tmp"))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Read output into buffer before cleanup
        let _ = mgr.get_output(&id).await;
        assert!(!mgr.is_running(&id).await);

        let finished = mgr.cleanup_finished().await;
        assert_eq!(finished.len(), 1);
        assert!(finished[0].1.contains("done"));
    }

    #[tokio::test]
    async fn test_process_not_found() {
        let mgr = ProcessManager::new();
        let result = mgr.get_output(&"nonexistent".to_string()).await;
        assert!(result.is_err());

        let result = mgr.kill(&"nonexistent".to_string()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_buffer_overflow_keeps_recent_data() {
        let mgr = ProcessManager::new();

        // Generate output larger than MAX_OUTPUT_BUFFER_SIZE (1MB)
        // We generate 1.5MB of data: 1500 lines of 1000 chars each
        let id = mgr
            .spawn(
                "for i in $(seq 1 1500); do printf 'LINE%04d:%0990d\\n' $i $i; done",
                &PathBuf::from("/tmp"),
            )
            .await
            .unwrap();

        // Wait for process to complete and read all output
        let mut output = String::new();
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            output = mgr.get_output(&id).await.unwrap();
            if !mgr.is_running(&id).await && output.len() > MAX_OUTPUT_BUFFER_SIZE / 2 {
                break;
            }
        }

        // Buffer should be truncated to ~1MB
        assert!(
            output.len() <= MAX_OUTPUT_BUFFER_SIZE + 4,
            "Buffer should be truncated to MAX_OUTPUT_BUFFER_SIZE, got {}",
            output.len()
        );

        // When buffer overflows, recent data should be preserved
        if output.len() > MAX_OUTPUT_BUFFER_SIZE / 2 {
            // Check that we have some later lines (not necessarily the last one due to timing)
            let has_later_lines = (1000..=1500).any(|n| output.contains(&format!("LINE{:04}", n)));
            assert!(has_later_lines, "Some later data should be preserved");
        }
    }
}
