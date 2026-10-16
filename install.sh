#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
cargo build --release --locked --offline
exec target/release/data-watch install
