# MCP

The runtime can connect to MCP servers to expose external tools and resources.

## Current Support

- stdio-based MCP servers

## Responsibilities

- manage multiple MCP server connections
- surface tools and resources into the runtime
- keep MCP-specific behavior isolated from built-in tools

## Typical Configuration

MCP servers are usually configured through Claude-style settings files or programmatic configuration.

## Related Guides

- `tools.md`
- `plugins.md`
