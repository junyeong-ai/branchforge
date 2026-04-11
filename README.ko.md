# branchforge

Rust로 작성된 stateful agent runtime — 순수 API 에이전트, 로컬 머신 지식 에이전트, 풀 코딩 에이전트까지 모두 지원하며, 4개 레이어 아키텍처로 필요한 기능만 골라 쓸 수 있도록 설계되었습니다.

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
- Anthropic, Bedrock, Vertex AI, Azure AI Foundry, OpenAI, Gemini 지원
- 안전한 로컬 도구 실행과 인가 제어
- Claude CLI의 `.claude/` 레이아웃과 호환되는 워크스페이스 리소스 활용

## 4-레이어 아키텍처

`branchforge`는 4개의 레이어로 구성되어 있으며, 각 레이어는 별도의 Cargo feature로 게이팅됩니다. 배포 환경에 필요한 surface만 선택할 수 있습니다.

| 레이어 | Feature | 추가되는 기능 | 대표 사용 사례 |
|-------|---------|--------------|-----------|
| **1 — Pure core** | (항상 켜짐) | Agent runtime, IR, provider client, session graph, hooks, budget, observability, 네트워크 egress 샌드박스. 파일/셸 의존성 없음. | 서버 사이드 API 에이전트, 고객 지원 봇, 워크플로 오케스트레이터. |
| **2a — Local FS** | `local-fs` | `Workspace`, `SecureFs` (TOCTOU-safe), Read/Write/Edit/Glob/Grep, Landlock/Seatbelt 경로 샌드박스, 범용 markdown memory loader, `explore`/`plan` 서브에이전트. | 연구 에이전트, 지식 노동자, 로컬 데이터 분석가. |
| **2b — Coding tools** | `coding-tools` | tree-sitter AST 검증을 거치는 Bash, 프로세스 스케줄러, 컨테이너 감지, CLAUDE.md 디스커버리, git 컨텍스트, `bash` 서브에이전트. Layer 2a에 의존. | Claude Code급 코딩 에이전트. |
| **3 — Cloud providers** | `aws` / `gcp` / `azure` / `cloud-all` | Bedrock, Vertex (Gemini + Anthropic), Azure AI Foundry transport. Layer 1에 의존. | 멀티 클라우드 / 엔터프라이즈 배포. |

기본 features는 `coding-tools` (Layer 2a 자동 활성화). Pure API 사용자는 `default-features = false`로 끄고 `anthropic-direct`만 켜면 됩니다. 자세한 의존성 계약은 [`docs/architecture/layering.md`](docs/architecture/layering.md) 참고.

## 문서

| 가이드 | 설명 |
|--------|------|
| [아키텍처](docs/architecture.md) | 시스템 경계와 설계 원칙 |
| [레이어링](docs/architecture/layering.md) | 4-레이어 feature 게이팅 계약 |
| [세션 & 그래프](docs/session.md) | Graph-first 세션 모델과 퍼시스턴스 |
| [도구](docs/tools.md) | 내장 도구, 접근 제어, 커스텀 도구 |
| [스킬](docs/skills.md) | Progressive disclosure와 스킬 시스템 |
| [서브에이전트](docs/subagents.md) | 위임, 도구 제한, 모델 해석 |
| [인가](docs/authorization.md) | 모드, 규칙, 스코프 패턴 |
| [보안](docs/security.md) | SecureFs, bash 분석, 샌드박싱 |
| [인증](docs/authentication.md) | OAuth, API 키, 클라우드 프로바이더 |
| [백엔드 선택](docs/backend-selection.md) | Memory, JSONL, PostgreSQL, Redis |

## 핵심 가치

- `SessionGraph`를 canonical state로 사용합니다. 메시지 리스트는 `Session::current_branch_messages()`로 그래프에서 매번 재구성되며, 별도의 `messages` 필드는 존재하지 않습니다.
- 세션은 분기, replay, export가 가능한 작업 그래프로 관리됩니다.
- JSONL, PostgreSQL, Redis persistence를 지원합니다.
- built-in tools, MCP, subagents, skills를 같은 runtime 안에서 조합할 수 있습니다.
- 구조화된 출력은 provider-neutral `JsonSchemaSpec` 으로 IR 에 담기고, 5 개 codec 이 공유하는 `SchemaPolicy` 파이프라인을 통해 encode 시점에 provider 별 subset 으로 변환됩니다. 탈락된 키워드는 `ModelWarning::LossyEncode` 로 사용자에게 표면화됩니다.

## 빠른 시작

### 설치

```toml
[dependencies]
branchforge = "0.9"
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
- OpenAI (GPT-4o, o3, 호환 엔드포인트)
- Google Gemini

예시:

```rust
use branchforge::Auth;

let agent = branchforge::Agent::builder()
    .auth(Auth::api_key("sk-ant-..."))
    .await?
    .build()
    .await?;
```

상세 내용은 [인증](docs/authentication.md), [클라우드 프로바이더](docs/cloud-providers.md)를 참고하세요.

## 세션과 리플레이

세션은 graph-first 구조로 관리됩니다.

- branch
- replay
- export
- bookmark
- checkpoint

이 구조 덕분에 긴 코딩 세션을 단순 로그가 아니라 재개 가능한 작업 기록으로 다룰 수 있습니다.

상세 내용은 [세션 & 그래프](docs/session.md)를 참고하세요.

## 런타임 아키텍처

에이전트는 공유 인프라와 세션별 상태를 분리합니다.

```rust
use branchforge::{Agent, AgentRuntime, RunConfig};
use std::sync::Arc;

// AgentRuntime은 client, config, tools, hooks를 보유 — 세션 간 공유 가능
let agent = Agent::builder()
    .auth(Auth::from_env()).await?
    .tools(ToolSurface::core())
    .build()
    .await?;

// RunConfig로 실행별 오버라이드 — 에이전트를 재생성할 필요 없음
let result = agent
    .execute_with(
        "이 파일을 요약해줘",
        RunConfig::new()
            .model("claude-haiku-4-5-20251001")
            .max_iterations(3)
            .system_prompt("간결하게 답해."),
    )
    .await?;

// CancellationToken을 통한 graceful shutdown
agent.shutdown_token().cancel();
```

주요 기능:

- **AgentRuntime**: 공유 인프라(`client`, `config`, `tools`, `hooks`, `budget`)를 `Arc`로 감싸 멀티세션 사용
- **RunConfig**: 실행별 `model`, `max_tokens`, `max_iterations`, `timeout`, `system_prompt`, `execution_mode` 오버라이드
- **Graceful shutdown**: `CancellationToken` 기반 협력적 취소, 세션 상태 저장 후 종료
- **EventBus 구독**: `SubscriptionHandle`의 RAII 패턴으로 Drop 시 자동 구독 해제

## 도구 시스템

기본 런타임은 최소 코어 도구 표면만 노출하고, 필요할 때 워크플로우 도구를 추가로 켤 수 있습니다.

- File: Read, Write, Edit, Glob, Grep
- Execution: Bash, KillShell
- Extension: Skill
- Optional workflow: Task, TaskOutput, TodoWrite, Plan, GraphHistory
- Server tools: WebFetch, WebSearch, ToolSearch

상세 내용은 [도구](docs/tools.md)를 참고하세요.

## 품질 기준

이 저장소는 다음 품질 게이트를 기준으로 유지됩니다.

```bash
cargo nextest run --all-features
cargo clippy --all-features -- -D warnings
cargo fmt --all -- --check
```
