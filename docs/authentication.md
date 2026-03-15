# Authentication

The runtime supports multiple credential sources and keeps authentication separate from prompt composition and session behavior.

## Supported Modes

- Anthropic API key
- Claude Code CLI credentials
- AWS Bedrock credentials
- Google Vertex AI credentials
- Azure AI Foundry credentials

## Recommended Entry Points

```rust
use branchforge::{Agent, Auth};

let direct = Agent::builder()
    .auth(Auth::api_key("sk-ant-..."))
    .await?
    .build()
    .await?;

let claude_code = Agent::builder()
    .from_claude_cli_workspace(".")
    .await?
    .build()
    .await?;
```

## Claude CLI Workspace Resources vs Credentials

`from_claude_cli_workspace()` combines credential resolution with project resource loading. If you want different credentials but still want `.claude/` resources, configure them separately.

```rust
Agent::builder()
    .auth(Auth::bedrock("us-east-1"))
    .await?
    .working_dir("./my-project")
    .project_resources()
    .build()
    .await?;
```

## Design Rules

- Authentication resolves credentials.
- Authentication does not own assistant behavior.
- Authentication does not define prompt content.
- Provider-specific headers and transport behavior belong in client adapters.

## Cloud Notes

- Bedrock uses AWS credential resolution.
- Vertex AI uses Google application credentials.
- Foundry uses Azure identity resolution.

See `cloud-providers.md` for provider-specific setup notes.
