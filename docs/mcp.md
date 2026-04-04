# MCP

The runtime can connect to MCP servers to expose external tools and resources.

## Transports

- stdio — local process communication (default)
- SSE — Server-Sent Events over HTTP (Streamable HTTP)

## Responsibilities

- manage multiple MCP server connections
- surface tools and resources into the runtime
- keep MCP-specific behavior isolated from built-in tools
- cache tool listings per server with configurable TTL
- reconnect with exponential backoff on transient failures

## Configuration

MCP servers are configured through Claude-style settings files or programmatic configuration.

Timeouts are configurable via `McpTimeouts`:

- `connection` — default 30s
- `tool_call` — default 60s
- `resource_read` — default 30s

Tool listing cache TTL defaults to 5 minutes and can be set via `McpManager::cache_ttl()`.

## Related Guides

- `tools.md`
- `plugins.md`
