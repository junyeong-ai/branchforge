//! Sandbox Verification Test
//!
//! Verifies that:
//! - Files within working_dir are accessible
//! - Files outside working_dir are blocked
//! - Path traversal attacks (../) are prevented
//!
//! Run: cargo run --example sandbox_verification

use branchforge::security::{SecureFs, SecurityContext, SecurityError};
use branchforge::tools::{ExecutionContext, GlobTool, GrepTool, ReadTool, Tool, WriteTool};
use std::path::PathBuf;

struct TestRunner {
    passed: usize,
    failed: usize,
}

impl TestRunner {
    fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
        }
    }

    fn check(&mut self, name: &str, result: Result<(), String>) {
        match result {
            Ok(()) => {
                println!("  [PASS] {}", name);
                self.passed += 1;
            }
            Err(e) => {
                println!("  [FAIL] {} - {}", name, e);
                self.failed += 1;
            }
        }
    }

    fn summary(&self) -> bool {
        println!("\n------------------------------------------------------------------------");
        println!("  Result: {} passed, {} failed", self.passed, self.failed);
        println!("------------------------------------------------------------------------");
        self.failed == 0
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("========================================================================");
    println!("                    Sandbox Verification Test                           ");
    println!("========================================================================\n");

    let mut runner = TestRunner::new();

    // Create nested temp directories for testing
    // Structure:
    //   parent_dir/
    //     secret.txt (should NOT be accessible)
    //     working_dir/
    //       allowed.txt (should be accessible)
    //       subdir/
    //         nested.txt (should be accessible)
    let parent_dir = tempfile::tempdir()?;
    let parent_path = std::fs::canonicalize(parent_dir.path())?;

    let working_dir = parent_path.join("working_dir");
    std::fs::create_dir_all(&working_dir)?;
    let working_path = std::fs::canonicalize(&working_dir)?;

    let subdir = working_path.join("subdir");
    std::fs::create_dir_all(&subdir)?;

    // Create test files
    std::fs::write(
        parent_path.join("secret.txt"),
        "SECRET DATA - should not be accessible",
    )?;
    std::fs::write(working_path.join("allowed.txt"), "This file is allowed")?;
    std::fs::write(subdir.join("nested.txt"), "Nested file content")?;

    println!("Test Directory Structure:");
    println!("  parent_dir/");
    println!("    secret.txt (OUTSIDE sandbox)");
    println!("    working_dir/  <-- SANDBOX ROOT");
    println!("      allowed.txt");
    println!("      subdir/");
    println!("        nested.txt");
    println!();

    // =========================================================================
    // Section 1: SecureFs Direct Tests
    // =========================================================================
    println!("Section 1: SecureFs Path Resolution");
    println!("------------------------------------------------------------------------");

    let secure_fs = SecureFs::new(&working_path, vec![], &[], 10)?;

    // Test 1: Access file within working_dir (should succeed)
    runner.check("Resolve allowed.txt (within sandbox)", {
        match secure_fs.resolve("allowed.txt") {
            Ok(path) => {
                if path.as_path() == working_path.join("allowed.txt") {
                    Ok(())
                } else {
                    Err(format!("Wrong path: {:?}", path.as_path()))
                }
            }
            Err(e) => Err(format!("Should succeed: {}", e)),
        }
    });

    // Test 2: Access nested file (should succeed)
    runner.check("Resolve subdir/nested.txt (within sandbox)", {
        match secure_fs.resolve("subdir/nested.txt") {
            Ok(path) => {
                if path.as_path() == subdir.join("nested.txt") {
                    Ok(())
                } else {
                    Err(format!("Wrong path: {:?}", path.as_path()))
                }
            }
            Err(e) => Err(format!("Should succeed: {}", e)),
        }
    });

    // Test 3: Path traversal to parent (should FAIL)
    runner.check("Block ../secret.txt (path traversal)", {
        match secure_fs.resolve("../secret.txt") {
            Err(SecurityError::PathEscape(_)) => Ok(()),
            Err(e) => Err(format!("Wrong error type: {}", e)),
            Ok(path) => Err(format!("Should be blocked! Got: {:?}", path.as_path())),
        }
    });

    // Test 4: Absolute path outside sandbox (should FAIL)
    runner.check("Block absolute path outside sandbox", {
        let outside_path = parent_path.join("secret.txt");
        match secure_fs.resolve(outside_path.to_str().unwrap()) {
            Err(SecurityError::PathEscape(_)) => Ok(()),
            Err(e) => Err(format!("Wrong error type: {}", e)),
            Ok(path) => Err(format!("Should be blocked! Got: {:?}", path.as_path())),
        }
    });

    // Test 5: Multiple parent traversal (should FAIL)
    runner.check("Block ../../ traversal", {
        match secure_fs.resolve("subdir/../../secret.txt") {
            Err(SecurityError::PathEscape(_)) => Ok(()),
            Err(e) => Err(format!("Wrong error type: {}", e)),
            Ok(path) => Err(format!("Should be blocked! Got: {:?}", path.as_path())),
        }
    });

    // Test 6: Hidden traversal in middle (should FAIL)
    runner.check("Block hidden traversal (subdir/../../../secret.txt)", {
        match secure_fs.resolve("subdir/../../../secret.txt") {
            Err(SecurityError::PathEscape(_)) => Ok(()),
            Err(e) => Err(format!("Wrong error type: {}", e)),
            Ok(path) => Err(format!("Should be blocked! Got: {:?}", path.as_path())),
        }
    });

    // =========================================================================
    // Section 2: ExecutionContext with Tools
    // =========================================================================
    println!("\nSection 2: ExecutionContext Tool Integration");
    println!("------------------------------------------------------------------------");

    let security = SecurityContext::builder().root(&working_path).build()?;
    let ctx = ExecutionContext::new(security);

    // Test 7: Read tool - allowed file
    let read_tool = ReadTool;
    runner.check("Read allowed.txt via ReadTool", {
        let result = read_tool
            .execute(
                serde_json::json!({
                    "file_path": working_path.join("allowed.txt").to_str().unwrap()
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Success(content)
                if content.contains("This file is allowed") =>
            {
                Ok(())
            }
            _ => Err(format!("Unexpected result: {:?}", result)),
        }
    });

    // Test 8: Read tool - blocked file (outside sandbox)
    runner.check("Block reading ../secret.txt via ReadTool", {
        let result = read_tool
            .execute(
                serde_json::json!({
                    "file_path": parent_path.join("secret.txt").to_str().unwrap()
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Error(e)
                if e.to_string().contains("escape") || e.to_string().contains("outside") =>
            {
                Ok(())
            }
            branchforge::ToolOutput::Error(_) => Ok(()), // Any error is acceptable for blocked access
            _ => Err(format!("Should be blocked! Got: {:?}", result)),
        }
    });

    // Test 9: Glob tool - within sandbox
    let glob_tool = GlobTool;
    runner.check("Glob *.txt within sandbox", {
        let result = glob_tool
            .execute(
                serde_json::json!({
                    "pattern": "*.txt"
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Success(content) if content.contains("allowed.txt") => Ok(()),
            _ => Err(format!("Unexpected result: {:?}", result)),
        }
    });

    // Test 10: Glob with path traversal (should only find files within sandbox)
    runner.check("Glob ../*.txt should not find parent files", {
        let result = glob_tool
            .execute(
                serde_json::json!({
                    "pattern": "../*.txt"
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Success(content) => {
                if content.contains("secret.txt") {
                    Err("Should not find secret.txt!".into())
                } else {
                    Ok(()) // No matches or only sandbox files = OK
                }
            }
            branchforge::ToolOutput::Error(_) => Ok(()), // Error is also acceptable
            _ => Err(format!("Unexpected result: {:?}", result)),
        }
    });

    // Test 11: Write tool - within sandbox (should succeed)
    let write_tool = WriteTool;
    runner.check("Write new file within sandbox", {
        let new_file = working_path.join("new_file.txt");
        let result = write_tool
            .execute(
                serde_json::json!({
                    "file_path": new_file.to_str().unwrap(),
                    "content": "New content"
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Success(_) | branchforge::ToolOutput::Empty => {
                if new_file.exists() {
                    Ok(())
                } else {
                    Err("File was not created".into())
                }
            }
            _ => Err(format!("Unexpected result: {:?}", result)),
        }
    });

    // Test 12: Write tool - outside sandbox (should FAIL)
    runner.check("Block writing outside sandbox", {
        let result = write_tool
            .execute(
                serde_json::json!({
                    "file_path": parent_path.join("hacked.txt").to_str().unwrap(),
                    "content": "Should not be written"
                }),
                &ctx,
            )
            .await;

        if parent_path.join("hacked.txt").exists() {
            Err("File should NOT have been created outside sandbox!".into())
        } else {
            match &result.output {
                branchforge::ToolOutput::Error(_) => Ok(()),
                _ => Err(format!("Should return error: {:?}", result)),
            }
        }
    });

    // =========================================================================
    // Section 3: is_within() Boundary Check
    // =========================================================================
    println!("\nSection 3: Boundary Checks (is_within)");
    println!("------------------------------------------------------------------------");

    runner.check("is_within: working_dir/allowed.txt = true", {
        if ctx.is_within(&working_path.join("allowed.txt")) {
            Ok(())
        } else {
            Err("Should be within sandbox".into())
        }
    });

    runner.check("is_within: working_dir/subdir/nested.txt = true", {
        if ctx.is_within(&subdir.join("nested.txt")) {
            Ok(())
        } else {
            Err("Should be within sandbox".into())
        }
    });

    runner.check("is_within: parent_dir/secret.txt = false", {
        if !ctx.is_within(&parent_path.join("secret.txt")) {
            Ok(())
        } else {
            Err("Should NOT be within sandbox".into())
        }
    });

    runner.check("is_within: /etc/passwd = false", {
        if !ctx.is_within(&PathBuf::from("/etc/passwd")) {
            Ok(())
        } else {
            Err("Should NOT be within sandbox".into())
        }
    });

    // =========================================================================
    // Section 4: Grep tool sandbox check
    // =========================================================================
    println!("\nSection 4: Grep Tool Sandbox Verification");
    println!("------------------------------------------------------------------------");

    let grep_tool = GrepTool;

    runner.check("Grep within sandbox finds content", {
        let result = grep_tool
            .execute(
                serde_json::json!({
                    "pattern": "allowed",
                    "output_mode": "content"
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Success(content) if content.contains("allowed") => Ok(()),
            _ => Err(format!("Unexpected result: {:?}", result)),
        }
    });

    runner.check("Grep cannot search outside sandbox via path", {
        let result = grep_tool
            .execute(
                serde_json::json!({
                    "pattern": "SECRET",
                    "path": parent_path.to_str().unwrap()
                }),
                &ctx,
            )
            .await;

        match &result.output {
            branchforge::ToolOutput::Success(content) => {
                if content.contains("SECRET") {
                    Err("Should NOT find SECRET outside sandbox!".into())
                } else {
                    Ok(())
                }
            }
            branchforge::ToolOutput::Error(_) => Ok(()), // Error is acceptable
            _ => Err(format!("Unexpected result: {:?}", result)),
        }
    });

    // =========================================================================
    // Summary
    // =========================================================================
    let success = runner.summary();

    if success {
        println!("========================================================================");
        println!("              All sandbox verification tests passed!                   ");
        println!("========================================================================");
    } else {
        println!("========================================================================");
        println!("              Some sandbox verification tests failed!                  ");
        println!("========================================================================");
        std::process::exit(1);
    }

    Ok(())
}
