# Testing memd Installation Locally

Since the repository is currently private, you can test the installation process using the local binary.

## Test Installation Script

The `test-install.sh` script simulates the production installation process using your local binary:

```bash
./test-install.sh
```

**What it does:**
1. Uses the pre-built binary from `dist/memd-linux-x64`
2. Installs to `~/.local/bin/memd` (or `$MEMD_INSTALL_DIR`)
3. Creates configuration at `~/.config/memd/config.toml`
4. Prompts to configure MCP for Claude Code
5. Prompts to configure MCP for Codex CLI
6. Verifies the installation

## Prerequisites

Build the binary first if you haven't:

```bash
cargo build --release
mkdir -p dist
cp target/release/memd dist/memd-linux-x64
```

## Testing MCP Integration

### For Claude Code

1. Run the test installer:
   ```bash
   ./test-install.sh
   ```

2. When prompted, choose "Y" for Claude Code configuration

3. Restart Claude Code completely

4. Test in Claude Code:
   ```markdown
   User: "List available tools"

   # You should see memd tools:
   - memory.add
   - memory.search
   - code.find_definition
   # ... etc
   ```

5. Test a tool:
   ```markdown
   User: "Use memory.add to store: Test installation works"
   ```

### For Codex CLI

1. Run the test installer:
   ```bash
   ./test-install.sh
   ```

2. When prompted, choose "Y" for Codex CLI configuration

3. Test with Codex:
   ```bash
   codex -p "What memory tools are available?"
   ```

## Manual Testing

If you want to test without running the installer:

### 1. Install Binary Manually

```bash
mkdir -p ~/.local/bin
cp dist/memd-linux-x64 ~/.local/bin/memd
chmod +x ~/.local/bin/memd
```

### 2. Create Config

```bash
mkdir -p ~/.config/memd
cat > ~/.config/memd/config.toml <<'EOF'
[server]
mode = "mcp"

[storage]
data_dir = "~/.local/share/memd"

[embeddings]
model = "all-MiniLM-L6-v2"
dimension = 384
pooling_strategy = "mean"

[index]
hnsw_m = 16
hnsw_ef_construction = 200
hnsw_ef_search = 50

[cache]
hot_tier_size = 1000
semantic_cache_ttl = 2700

[compaction]
tombstone_threshold = 0.20
segment_threshold = 10
hnsw_staleness_threshold = 0.15
EOF
```

### 3. Configure MCP (Claude Code)

Add to `~/.config/claude/mcp_settings.json`:

```json
{
  "mcpServers": {
    "memd": {
      "command": "/home/fschulz/.local/bin/memd",
      "args": [],
      "env": {},
      "disabled": false
    }
  }
}
```

### 4. Test memd Directly

```bash
# Test binary runs
~/.local/bin/memd

# It should wait for MCP input on stdin
# Press Ctrl+C to exit
```

## Verification Checklist

After installation, verify:

- [ ] Binary exists: `ls -la ~/.local/bin/memd`
- [ ] Binary is executable: `file ~/.local/bin/memd`
- [ ] Config created: `cat ~/.config/memd/config.toml`
- [ ] Data dir created: `ls -la ~/.local/share/memd`
- [ ] MCP config added: `cat ~/.config/claude/mcp_settings.json | grep memd`
- [ ] memd runs: `memd` (should start, press Ctrl+C)
- [ ] Claude Code shows memd tools (after restart)
- [ ] Can use memory.add tool successfully

## Troubleshooting

### Binary not in PATH

```bash
export PATH="$HOME/.local/bin:$PATH"
# Add to ~/.bashrc to make permanent
```

### MCP config not working

Check Claude Code picks up the config:
```bash
cat ~/.config/claude/mcp_settings.json
```

Ensure memd is not disabled:
```json
"disabled": false
```

### memd fails to start

Check binary is executable:
```bash
chmod +x ~/.local/bin/memd
ldd ~/.local/bin/memd  # Check dependencies
```

### Tools not showing in Claude Code

1. Verify MCP config is correct
2. Restart Claude Code completely
3. Check memd binary runs: `~/.local/bin/memd`
4. Check logs if available

## Cleanup

To remove the test installation:

```bash
rm ~/.local/bin/memd
rm -rf ~/.config/memd
rm -rf ~/.local/share/memd

# Remove from MCP config manually or:
jq 'del(.mcpServers.memd)' ~/.config/claude/mcp_settings.json > /tmp/mcp.json
mv /tmp/mcp.json ~/.config/claude/mcp_settings.json
```

## Production Testing

Once the repository is public, test the production install script:

```bash
curl -sSL https://raw.githubusercontent.com/fmschulz/memd/main/install.sh | bash
```

This will download the binary from GitHub releases instead of using a local copy.
