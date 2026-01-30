# Docker Development Environment

## Problem

The memd project uses ONNX Runtime (`ort-sys`) which requires glibc 2.38+ (C23 standard). Systems with older glibc (Ubuntu 22.04 has 2.35) cannot link the final binary.

## Solution

Use Docker container with Ubuntu 24.04 (glibc 2.39) for development and testing.

## Quick Start

```bash
# 1. Build the development container (first time only)
./docker-dev.sh build

# 2. Run tests
./docker-dev.sh test

# 3. Run eval suite
./docker-dev.sh eval --suite hybrid

# 4. Build release binary
./docker-dev.sh release

# 5. Interactive shell (for development)
./docker-dev.sh shell
```

## Available Commands

| Command | Purpose |
|---------|---------|
| `./docker-dev.sh build` | Build the Docker development image |
| `./docker-dev.sh shell` | Open interactive shell in container |
| `./docker-dev.sh test` | Run `cargo test` |
| `./docker-dev.sh check` | Run `cargo check` |
| `./docker-dev.sh eval <args>` | Run eval harness with arguments |
| `./docker-dev.sh release` | Build optimized release binary |

## How It Works

- **Container**: Ubuntu 24.04 with glibc 2.39
- **Rust**: Installed via rustup inside container
- **Volumes**:
  - `cargo-registry` - Caches downloaded crates
  - `cargo-git` - Caches git dependencies
  - `target` - Caches build artifacts
  - Project directory mounted at `/workspace`

## Performance

Build artifacts and dependencies are cached in Docker volumes, so subsequent builds are fast:

- **First build**: ~10 minutes (downloads all dependencies)
- **Incremental builds**: Seconds to minutes (depending on changes)

## Troubleshooting

### "Cannot connect to Docker daemon"
```bash
# Start Docker service
sudo systemctl start docker

# Or add your user to docker group (requires logout/login)
sudo usermod -aG docker $USER
```

### "Permission denied"
```bash
# Make script executable
chmod +x docker-dev.sh
```

### "Out of disk space"
```bash
# Clean up old Docker images and volumes
docker system prune -a --volumes
```

## Alternative: Native Development

If you prefer native development, upgrade to Ubuntu 24.04:

```bash
# Check current version
lsb_release -a

# Upgrade to 24.04 LTS (requires reboot)
sudo do-release-upgrade
```

## Binary Compatibility

Binaries built in the Docker container require glibc 2.38+. To run on systems with older glibc:

1. Build on target system directly
2. Use static linking (complex with ONNX Runtime)
3. Deploy with Docker in production
