#!/usr/bin/env bash
set -e

# memd installation script for Linux
# Usage: curl -sSL https://raw.githubusercontent.com/fmschulz/memd/main/install.sh | bash

VERSION="${MEMD_VERSION:-0.1.0}"
INSTALL_DIR="${MEMD_INSTALL_DIR:-$HOME/.local/bin}"
CONFIG_DIR="$HOME/.config/memd"
DATA_DIR="$HOME/.local/share/memd"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Logging functions
info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    exit 1
}

# Detect platform
detect_platform() {
    local os=$(uname -s)
    local arch=$(uname -m)

    case "$os" in
        Linux*)
            case "$arch" in
                x86_64|amd64)
                    echo "linux-x64"
                    ;;
                aarch64|arm64)
                    echo "linux-arm64"
                    ;;
                *)
                    error "Unsupported architecture: $arch (Linux only supports x64 and arm64)"
                    ;;
            esac
            ;;
        *)
            error "Unsupported operating system: $os (only Linux is currently supported)"
            ;;
    esac
}

# Check dependencies
check_dependencies() {
    local missing=()

    if ! command -v curl &> /dev/null; then
        missing+=("curl")
    fi

    if ! command -v jq &> /dev/null; then
        warn "jq not found - MCP configuration will be manual"
    fi

    if [ ${#missing[@]} -ne 0 ]; then
        error "Missing required dependencies: ${missing[*]}"
    fi
}

# Download binary from GitHub releases
download_binary() {
    local platform=$1
    local binary_name="memd"
    local release_url="https://github.com/fmschulz/memd/releases/download/v${VERSION}/memd-${platform}"

    info "Downloading memd v${VERSION} for ${platform}..."

    # Create temp directory
    local tmp_dir=$(mktemp -d)
    local tmp_file="$tmp_dir/memd"

    # Try to download from releases
    if curl -sSL -f "$release_url" -o "$tmp_file" 2>/dev/null; then
        info "Downloaded from GitHub releases"
    else
        warn "GitHub release not found, attempting to build from source..."
        build_from_source "$tmp_file"
    fi

    echo "$tmp_file"
}

# Build from source as fallback
build_from_source() {
    local output_file=$1

    if ! command -v cargo &> /dev/null; then
        error "Rust toolchain not found. Please install Rust from https://rustup.rs/"
    fi

    info "Building memd from source..."

    # Clone or use current directory
    if [ -f "Cargo.toml" ] && grep -q "name = \"memd\"" Cargo.toml 2>/dev/null; then
        # Already in memd directory
        cargo build --release
        cp target/release/memd "$output_file"
    else
        # Need to clone
        local tmp_repo=$(mktemp -d)
        git clone https://github.com/fmschulz/memd.git "$tmp_repo"
        cd "$tmp_repo"
        cargo build --release
        cp target/release/memd "$output_file"
        cd - > /dev/null
        rm -rf "$tmp_repo"
    fi

    info "Built memd from source"
}

# Install binary
install_binary() {
    local binary_file=$1

    # Create install directory
    mkdir -p "$INSTALL_DIR"

    # Copy and make executable
    cp "$binary_file" "$INSTALL_DIR/memd"
    chmod +x "$INSTALL_DIR/memd"

    info "Installed memd to $INSTALL_DIR/memd"

    # Check if INSTALL_DIR is in PATH
    if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
        warn "$INSTALL_DIR is not in your PATH"
        info "Add this to your ~/.bashrc or ~/.zshrc:"
        echo ""
        echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
        echo ""
    fi
}

# Create default configuration
create_config() {
    mkdir -p "$CONFIG_DIR"

    if [ -f "$CONFIG_DIR/config.toml" ]; then
        info "Configuration already exists at $CONFIG_DIR/config.toml"
        return
    fi

    info "Creating default configuration..."

    cat > "$CONFIG_DIR/config.toml" <<'EOF'
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
semantic_cache_ttl = 2700  # 45 minutes

[compaction]
tombstone_threshold = 0.20
segment_threshold = 10
hnsw_staleness_threshold = 0.15
EOF

    info "Created configuration at $CONFIG_DIR/config.toml"
}

# Create data directory
create_data_dir() {
    mkdir -p "$DATA_DIR"
    info "Created data directory at $DATA_DIR"
}

# Configure MCP for Claude Code
configure_claude_code() {
    local mcp_config="$HOME/.config/claude/mcp_settings.json"

    if [ ! -f "$mcp_config" ]; then
        # Create new config
        mkdir -p "$(dirname "$mcp_config")"
        cat > "$mcp_config" <<EOF
{
  "mcpServers": {
    "memd": {
      "command": "$INSTALL_DIR/memd",
      "args": [],
      "env": {},
      "disabled": false
    }
  }
}
EOF
        info "Created MCP configuration for Claude Code at $mcp_config"
    else
        # Update existing config
        if command -v jq &> /dev/null; then
            local tmp_file=$(mktemp)
            jq --arg cmd "$INSTALL_DIR/memd" \
               '.mcpServers.memd = {"command": $cmd, "args": [], "env": {}, "disabled": false}' \
               "$mcp_config" > "$tmp_file"
            mv "$tmp_file" "$mcp_config"
            info "Updated MCP configuration for Claude Code"
        else
            warn "jq not installed - please manually add memd to $mcp_config"
            echo ""
            echo "Add this to the mcpServers object:"
            echo ""
            echo "  \"memd\": {"
            echo "    \"command\": \"$INSTALL_DIR/memd\","
            echo "    \"args\": [],"
            echo "    \"env\": {},"
            echo "    \"disabled\": false"
            echo "  }"
            echo ""
        fi
    fi
}

# Configure MCP for Codex CLI
configure_codex_cli() {
    local mcp_config="$HOME/.codex/mcp_config.json"

    if [ ! -d "$HOME/.codex" ]; then
        info "Codex CLI not found - skipping"
        return
    fi

    if [ ! -f "$mcp_config" ]; then
        # Create new config
        mkdir -p "$(dirname "$mcp_config")"
        cat > "$mcp_config" <<EOF
{
  "servers": {
    "memd": {
      "command": "$INSTALL_DIR/memd",
      "args": [],
      "env": {}
    }
  }
}
EOF
        info "Created MCP configuration for Codex CLI at $mcp_config"
    else
        # Update existing config
        if command -v jq &> /dev/null; then
            local tmp_file=$(mktemp)
            jq --arg cmd "$INSTALL_DIR/memd" \
               '.servers.memd = {"command": $cmd, "args": [], "env": {}}' \
               "$mcp_config" > "$tmp_file"
            mv "$tmp_file" "$mcp_config"
            info "Updated MCP configuration for Codex CLI"
        else
            warn "jq not installed - please manually add memd to $mcp_config"
        fi
    fi
}

# Verify installation
verify_installation() {
    info "Verifying installation..."

    if ! command -v memd &> /dev/null; then
        if [ -x "$INSTALL_DIR/memd" ]; then
            warn "memd installed but not in PATH. Run: export PATH=\"$INSTALL_DIR:\$PATH\""
        else
            error "Installation failed - memd not found"
        fi
    fi

    # Test memd runs
    if ! "$INSTALL_DIR/memd" --version &> /dev/null; then
        error "memd binary exists but fails to run"
    fi

    info "Installation verified successfully"
}

# Main installation flow
main() {
    echo ""
    echo "======================================"
    echo "  memd Installer v${VERSION}"
    echo "  Intelligent Memory for AI Agents"
    echo "======================================"
    echo ""

    # Check dependencies
    check_dependencies

    # Detect platform
    local platform=$(detect_platform)
    info "Detected platform: $platform"

    # Download or build binary
    local binary_file=$(download_binary "$platform")

    # Install binary
    install_binary "$binary_file"

    # Create configuration
    create_config
    create_data_dir

    # Configure MCP (optional)
    echo ""
    read -p "Configure MCP for Claude Code? [Y/n] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]] || [[ -z $REPLY ]]; then
        configure_claude_code
    fi

    read -p "Configure MCP for Codex CLI? [Y/n] " -n 1 -r
    echo ""
    if [[ $REPLY =~ ^[Yy]$ ]] || [[ -z $REPLY ]]; then
        configure_codex_cli
    fi

    # Verify installation
    verify_installation

    # Cleanup
    rm -f "$binary_file"

    echo ""
    echo "======================================"
    echo "  Installation Complete!"
    echo "======================================"
    echo ""
    info "Binary: $INSTALL_DIR/memd"
    info "Config: $CONFIG_DIR/config.toml"
    info "Data:   $DATA_DIR"
    echo ""
    info "Next steps:"
    echo "  1. Restart your AI agent (Claude Code or Codex CLI)"
    echo "  2. Verify memd tools are available"
    echo "  3. Try: memory.add and memory.search"
    echo ""
    info "Documentation: https://github.com/fmschulz/memd"
    echo ""
}

# Run main installation
main "$@"
