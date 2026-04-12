//! Source-level audits that enforce naming and FSM conventions.
//!
//! These tests scan `src/**/*.rs` with regex to catch violations that
//! Rust's type system cannot express: FSM state assigned outside
//! `transition_to()`, and `pub enum` missing or carrying `#[non_exhaustive]`
//! against the open/closed list in `.claude/rules/naming.md`.
//!
//! Run: `cargo test audit`

#[cfg(test)]
mod tests {
    use regex::Regex;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    fn src_root() -> PathBuf {
        // When run via `cargo test`, the working directory is the repo root.
        let cwd = std::env::current_dir().expect("cwd");
        let src = cwd.join("src");
        assert!(src.is_dir(), "run from repo root (src/ not found)");
        src
    }

    // ===================================================================
    // FSM bypass audit
    // ===================================================================

    /// Detects direct assignment to FSM state fields outside validated
    /// `transition_to()` / `try_reset()` methods. Legitimate sites must
    /// carry a `// fsm-init:` or `// fsm-rebuild:` marker.
    #[test]
    fn audit_fsm_bypass() {
        const FSMS: &[&str] = &[
            "AgentState",
            "SessionState",
            "PlanState",
            "QueueItemState",
            "McpClientState",
        ];
        let rehydration_files: HashSet<&str> = [
            "persistence.rs",
            "persistence_jsonl.rs",
            "persistence_redis.rs",
            "persistence_postgres.rs",
            "archive.rs",
        ]
        .into_iter()
        .collect();

        let pattern_str = format!(r"\.\w+\s*=\s*({})::", FSMS.join("|"));
        let assign_re = Regex::new(&pattern_str).unwrap();
        let transition_re = Regex::new(r"fn (transition_to|try_reset)\b").unwrap();

        let mut violations = Vec::new();

        for path in walk_rs_files(&src_root()) {
            let fname = path.file_name().unwrap().to_str().unwrap();
            if rehydration_files.contains(fname) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = content.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                if line.contains("==") {
                    continue;
                }
                if !assign_re.is_match(line) {
                    continue;
                }
                let stripped = line.trim();
                if stripped.starts_with("//") || stripped.starts_with('*') {
                    continue;
                }
                if line.contains("=>") {
                    continue;
                }
                // Check if inside transition_to / try_reset
                if is_in_transition_method(&lines, i, &transition_re) {
                    continue;
                }
                // Check carve-out markers
                if has_carveout(&lines, i) {
                    continue;
                }
                violations.push(format!(
                    "  {}:{}  {}",
                    path.display(),
                    i + 1,
                    &stripped[..stripped.len().min(120)]
                ));
            }
        }

        assert!(
            violations.is_empty(),
            "FSM bypass audit: {} violation(s)\n{}\n\n\
             Fix: use transition_to(), or add `// fsm-init:` / `// fsm-rebuild:` marker.",
            violations.len(),
            violations.join("\n")
        );
    }

    fn is_in_transition_method(lines: &[&str], idx: usize, re: &Regex) -> bool {
        let mut depth: i32 = 0;
        for j in (0..=idx).rev() {
            let line = lines[j];
            depth += line.matches('}').count() as i32;
            depth -= line.matches('{').count() as i32;
            if depth < 0 {
                return false;
            }
            if re.is_match(line) {
                return true;
            }
        }
        false
    }

    fn has_carveout(lines: &[&str], idx: usize) -> bool {
        for j in (0..idx).rev() {
            let stripped = lines[j].trim();
            if stripped.is_empty() {
                continue;
            }
            if !stripped.starts_with("//") {
                return false;
            }
            if stripped.starts_with("// fsm-init:") || stripped.starts_with("// fsm-rebuild:") {
                return true;
            }
        }
        false
    }

    // ===================================================================
    // #[non_exhaustive] hygiene audit
    // ===================================================================

    /// Verifies that closed-list enums do NOT carry `#[non_exhaustive]`
    /// and all other `pub enum`s DO carry it.
    #[test]
    fn audit_non_exhaustive() {
        let closed_list: HashSet<&str> = [
            "SessionState",
            "PlanState",
            "QueueItemState",
            "McpClientState",
            "AgentState",
            "SchemaVersionMismatchDirection",
            "Role",
        ]
        .into_iter()
        .collect();

        let enum_re = Regex::new(r"^\s*pub enum (\w+)").unwrap();
        let mut violations = Vec::new();
        let mut total = 0u32;

        for path in walk_rs_files(&src_root()) {
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let lines: Vec<&str> = content.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                let Some(cap) = enum_re.captures(line) else {
                    continue;
                };
                let name = &cap[1];
                total += 1;

                let has_attr =
                    (i.saturating_sub(10)..i).any(|j| lines[j].contains("#[non_exhaustive]"));

                if closed_list.contains(name) {
                    if has_attr {
                        violations.push(format!(
                            "  {}:{}  {}: closed-list enum has #[non_exhaustive] — remove it",
                            path.display(),
                            i + 1,
                            name
                        ));
                    }
                } else if !has_attr {
                    violations.push(format!(
                        "  {}:{}  {}: open-list enum missing #[non_exhaustive]",
                        path.display(),
                        i + 1,
                        name
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "non_exhaustive audit: {} pub enums, {} violation(s)\n{}\n\n\
             Fix: add/remove #[non_exhaustive] per .claude/rules/naming.md open/closed list.",
            total,
            violations.len(),
            violations.join("\n")
        );
    }
}
