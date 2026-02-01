# Installing the memd Skill

This guide shows how to install the memd skill for use with AI agents.

## For Individual Users

### Option 1: Copy Entire Skill Directory

```bash
# Copy skill to your Claude Code skills directory
cp -r memd-skill ~/.claude/skills/memd

# Verify installation
ls -la ~/.claude/skills/memd
```

### Option 2: Symlink (Development)

```bash
# Create symlink for easier updates
ln -s /path/to/memd/memd-skill ~/.claude/skills/memd

# Verify
ls -la ~/.claude/skills/memd
```

## For Codex CLI Users

```bash
# Copy to Codex skills directory
cp -r memd-skill ~/.codex/skills/memd

# Verify
ls -la ~/.codex/skills/memd
```

## Verify Skill Installation

The skill directory should contain:
```
~/.claude/skills/memd/
├── SKILL.md                    # Main skill documentation
├── README.md                   # Quick reference
├── INSTALL.md                  # This file
├── mcp_config_claude.json      # Claude Code MCP config template
├── mcp_config_codex.json       # Codex CLI MCP config template
└── examples/
    ├── INDEX.md                # Examples overview
    ├── session_tracking.md     # Session context tracking example
    ├── codebase_indexing.md    # Codebase search example
    └── decision_tracking.md    # ADR tracking example
```

## Configure MCP Server

After installing the skill, configure your AI agent to use memd:

### For Claude Code

```bash
# Copy MCP config template
cp ~/.claude/skills/memd/mcp_config_claude.json ~/.config/claude/mcp_settings.json

# Or add manually to existing config:
{
  "mcpServers": {
    "memd": {
      "command": "memd",
      "args": [],
      "env": {},
      "disabled": false
    }
  }
}
```

### For Codex CLI

```bash
# Copy MCP config template
cp ~/.claude/skills/memd/mcp_config_codex.json ~/.codex/mcp_config.json

# Or add manually:
{
  "servers": {
    "memd": {
      "command": "memd",
      "args": [],
      "env": {}
    }
  }
}
```

**Important:** Restart your AI agent after updating MCP configuration.

## Verify MCP Integration

After restarting your agent, verify memd tools are available:

### In Claude Code
```markdown
User: "List available memory tools"

Claude: I can see these memd tools:
- memory.add
- memory.search
- memory.get
- memory.delete
- code.find_definition
- (... 8 more tools)
```

### In Codex CLI
```bash
codex -p "What memory tools are available?"
```

## Update Skill

To update the skill when new versions are released:

```bash
# If you copied the directory
cd /path/to/memd
git pull origin main
cp -r memd-skill ~/.claude/skills/memd

# If you symlinked (no action needed)
cd /path/to/memd
git pull origin main
# Symlink automatically points to updated version
```

## Team Installation

For teams sharing memd configurations:

### 1. Clone Repository
```bash
git clone https://github.com/fmschulz/memd.git
cd memd
```

### 2. Install for Each Team Member
```bash
# Each developer runs:
cp -r memd-skill ~/.claude/skills/memd
cp memd-skill/mcp_config_claude.json ~/.config/claude/mcp_settings.json
```

### 3. Shared Tenant Configuration
```bash
# Team agrees on tenant_id naming convention
# Example: Use repository name as tenant_id

# Project A
tenant_id: "backend-api"

# Project B
tenant_id: "frontend-app"

# Shared decisions
tenant_id: "team-architecture"
```

## Troubleshooting Installation

### Skill Not Found

**Problem:** Agent doesn't recognize memd skill

**Solution:**
```bash
# Check skill directory exists
ls -la ~/.claude/skills/memd

# Check permissions
chmod -R u+r ~/.claude/skills/memd
```

### MCP Tools Not Available

**Problem:** Agent doesn't show memd tools

**Solution:**
1. Verify memd binary is in PATH: `which memd`
2. Check MCP config syntax: `cat ~/.config/claude/mcp_settings.json | jq`
3. Restart agent completely
4. Check memd logs: `~/.local/share/memd/memd.log`

### Permission Errors

**Problem:** Can't write to skill directory

**Solution:**
```bash
# Fix permissions
chmod -R u+w ~/.claude/skills

# Try installation again
cp -r memd-skill ~/.claude/skills/memd
```

## Uninstallation

To remove the memd skill:

```bash
# Remove skill directory
rm -rf ~/.claude/skills/memd

# Remove MCP configuration
# Edit ~/.config/claude/mcp_settings.json
# Remove the "memd" entry from mcpServers

# Restart agent
```

## Next Steps

After installation:
1. Read `SKILL.md` for complete documentation
2. Review examples in `examples/` directory
3. Start with `examples/session_tracking.md`
4. Configure memd: `~/.config/memd/config.toml`
5. Begin using memory tools in your agent

## Support

- Repository: https://github.com/fmschulz/memd
- Issues: https://github.com/fmschulz/memd/issues
- Contact: fmschulz@gmail.com
