#!/bin/bash
# Helper script for Docker development environment

set -e

case "$1" in
    build)
        echo "Building Docker development environment..."
        docker compose build dev
        ;;
    shell)
        echo "Starting interactive shell in dev container..."
        docker compose run --rm dev bash
        ;;
    test)
        echo "Running tests in dev container..."
        docker compose run --rm dev cargo test
        ;;
    check)
        echo "Running cargo check in dev container..."
        docker compose run --rm dev cargo check
        ;;
    eval)
        shift
        echo "Running eval harness in dev container..."
        docker compose run --rm dev cargo run --package memd-evals -- "$@"
        ;;
    release)
        echo "Building release binary in dev container..."
        docker compose run --rm dev cargo build --release
        ;;
    *)
        echo "Usage: $0 {build|shell|test|check|eval|release}"
        echo ""
        echo "Commands:"
        echo "  build    - Build the Docker development image"
        echo "  shell    - Open interactive shell in container"
        echo "  test     - Run cargo test"
        echo "  check    - Run cargo check"
        echo "  eval     - Run eval harness (pass args: --suite hybrid)"
        echo "  release  - Build release binary"
        echo ""
        echo "Example:"
        echo "  $0 build              # First time setup"
        echo "  $0 eval --suite hybrid   # Run hybrid eval suite"
        exit 1
        ;;
esac
