# branchforge

Rust로 작성된 stateful coding agent runtime입니다.

[![CI](https://github.com/junyeong-ai/branchforge/actions/workflows/ci.yml/badge.svg)](https://github.com/junyeong-ai/branchforge/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org)
[![Edition](https://img.shields.io/badge/edition-2024-blue.svg)](https://doc.rust-lang.org/edition-guide/)
[![License](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)

[English](README.md) | 한국어

## 소개

`branchforge`는 단순한 API 바인딩이 아니라, 장기적인 작업 흐름을 다루는 Rust 기반 agent runtime입니다.

이 프로젝트는 다음을 목표로 합니다.

- graph-first 세션 모델
- replay, export, bookmark, checkpoint를 포함한 지속 가능한 작업 기록
- Anthropic, Bedrock, Vertex AI, Azure AI Foundry 지원
- 안전한 로컬 도구 실행과 권한 제어
- Claude CLI의 `.claude/` 레이아웃과 호환되는 워크스페이스 리소스 활용

## 핵심 가치

- `SessionGraph`를 canonical state로 사용합니다.
- `Session.messages`는 message-based API를 위한 projection으로 유지합니다.
- 세션은 분기, replay, export가 가능한 작업 그래프로 관리됩니다.
- JSONL, PostgreSQL, Redis persistence를 지원합니다.
- built-in tools, MCP, subagents, skills를 같은 runtime 안에서 조합할 수 있습니다.

## 빠른 시작

### 설치

```toml
[dependencies]
branchforge = "0.2"
tokio = { version = "1", features = ["full"] }
```

### 간단한 질의

```rust
use branchforge::query;

#[tokio::main]
async fn main() -> branchforge::Result<()> {
    let response = query("Explain the benefits of Rust").await?;
    println!("{response}");
    Ok(())
}
```

### 에이전트 생성

```rust
use branchforge::{Agent, Auth, ToolSurface};

#[tokio::main]
async fn main() -> branchforge::Result<()> {
    let agent = Agent::builder()
        .auth(Auth::from_env()).await?
        .tools(ToolSurface::core())
        .build()
        .await?;

    let result = agent.execute("Summarize this repository").await?;
    println!("{}", result.text());
    Ok(())
}
```

## 인증

지원되는 인증 방식은 다음과 같습니다.

- Anthropic API key
- Claude Code CLI credentials
- AWS Bedrock
- Google Vertex AI
- Azure AI Foundry

예시:

```rust
use branchforge::Auth;

let agent = branchforge::Agent::builder()
    .auth(Auth::api_key("sk-ant-..."))
    .await?
    .build()
    .await?;
```

상세 내용은 `docs/authentication.md`, `docs/cloud-providers.md`를 참고하세요.

## 세션과 리플레이

세션은 graph-first 구조로 관리됩니다.

- branch
- replay
- export
- bookmark
- checkpoint

이 구조 덕분에 긴 코딩 세션을 단순 로그가 아니라 재개 가능한 작업 기록으로 다룰 수 있습니다.

상세 내용은 `docs/session.md`를 참고하세요.

## 도구 시스템

기본 런타임은 최소 코어 도구 표면만 노출하고, 필요할 때 워크플로우 도구를 추가로 켤 수 있습니다.

- File: Read, Write, Edit, Glob, Grep
- Execution: Bash, KillShell
- Extension: Skill
- Optional workflow: Task, TaskOutput, TodoWrite, Plan, GraphHistory
- Server tools: WebFetch, WebSearch, ToolSearch

상세 내용은 `docs/tools.md`를 참고하세요.

## 문서

- `docs/architecture.md`
- `docs/authentication.md`
- `docs/cloud-providers.md`
- `docs/session.md`
- `docs/tools.md`
- `docs/security.md`
- `docs/authorization.md`
- `docs/subagents.md`
- `docs/skills.md`
- `docs/memory-system.md`
- `docs/backend-selection.md`
- `docs/audit-export.md`

## 품질 기준

이 저장소는 다음 품질 게이트를 기준으로 유지됩니다.

```bash
cargo nextest run --all-features
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```
