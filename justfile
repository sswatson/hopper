# Hopper - Justfile for common operations
#
# Quick start:
#   just demo-release    # Run in demo mode (no AWS credentials needed)
#   just install         # Install to ~/.local/bin

# Default recipe to display help
default:
    @just --list

# Build the project in release mode
build:
    cargo build --release

# Run the app in demo mode (debug build)
demo:
    cargo run -- --demo

# Run the app in demo mode (release build, faster)
demo-release:
    cargo run --release -- --demo

# Run with a specific default AWS profile
run PROFILE:
    cargo run -- --profile {{PROFILE}}

install: build
    install target/release/hopper ~/.local/bin/hopper

# Run tests
test:
    cargo test

# Run clippy with auto-fix
lint-fix:
    cargo clippy --fix --allow-dirty --allow-staged

# Format code with rustfmt
fmt:
    cargo fmt
