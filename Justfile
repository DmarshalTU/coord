# Common dev tasks for agentd.
#
# Install: brew install just
# Run any target with `just <name>` (e.g. `just demo`).

set shell := ["bash", "-cu"]

# Default daemon URL the helpers point at.
url := "http://127.0.0.1:7788/"
db  := "/tmp/agentd-dev.db"

# List available tasks.
default:
    @just --list

# Build everything in dev mode.
build:
    cargo build

# Build optimized release binaries.
release:
    cargo build --release

# Run unit + integration tests.
test:
    cargo test

# Run clippy with the strictest sane lints.
lint:
    cargo clippy --all-targets -- -D warnings

# Format the workspace.
fmt:
    cargo fmt

# Boot a fresh dev daemon on port 7788 with a clean DB.
serve:
    rm -f {{db}} {{db}}-wal {{db}}-shm
    cargo run --bin agentd -- serve --addr 127.0.0.1:7788 --db {{db}}

# Open the live TUI against the dev daemon.
top:
    cargo run --bin agentctl -- --url {{url}} top

# Tear down the dev daemon (matches by --db path so we don't kill prod).
kill:
    pkill -f "agentd serve --addr 127.0.0.1:7788" || true

# Run the 3-agent demo script (assumes daemon is already up).
demo:
    ./scripts/demo.sh
