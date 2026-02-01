# memd Skill for AI Agents

This directory contains skill documentation for integrating memd (Intelligent Memory Daemon) with AI coding agents.

## What This Skill Provides

Complete guide for:
- Installing memd locally (user-level, no sudo)
- Configuring AI agents (Claude Code, Codex CLI)
- Using all 13 MCP tools effectively
- Performance tuning and troubleshooting
- Integration patterns and best practices

## Files

- `skill.md` - Complete skill documentation
- `examples/` - Example configurations and usage patterns
- `mcp_config_claude.json` - Claude Code MCP configuration template
- `mcp_config_codex.json` - Codex CLI MCP configuration template

## Quick Start

### 1. Install memd

```bash
cd /path/to/memd
cargo build --release
mkdir -p ~/.local/bin
cp target/release/memd ~/.local/bin/
```

### 2. Configure AI Agent

**For Claude Code:**
```bash
cp memd-skill/mcp_config_claude.json ~/.config/claude/mcp_settings.json
```

**For Codex CLI:**
```bash
cp memd-skill/mcp_config_codex.json ~/.codex/mcp_config.json
```

### 3. Restart Agent

Restart Claude Code or Codex CLI to load the MCP server.

### 4. Verify

In your AI agent, you should see 13 new tools:
- memory.add, memory.add_batch, memory.search
- memory.get, memory.delete, memory.stats
- code.find_definition, code.find_references
- code.find_callers, code.find_imports
- debug.find_tool_calls, debug.find_errors
- memory.metrics, memory.compact

## Integration with Other Skills

memd works seamlessly with:
- `/commit` - Store commit decisions and search past work
- `/plan` - Reference architectural patterns from memory
- `/codex-review` - Search for similar code review findings
- `/gsd:*` - Persistent context across GSD phases
- Any skill that benefits from persistent memory

## Documentation

See `skill.md` for comprehensive documentation including:
- Detailed setup instructions
- All 13 tool usage examples
- Performance characteristics and tuning
- Troubleshooting common issues
- Best practices and integration patterns

## Support

- Issues: https://github.com/fmschulz/memd/issues
- Contact: fmschulz@gmail.com
