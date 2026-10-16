#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
cargo fmt --check
cargo test --locked --offline
cargo clippy --locked --offline -- -D warnings
cargo build --release --locked --offline
node --check static/app.js
node scripts/opensource-check.js
node scripts/smoke-binary.js
python3 scripts/browser-check.py
printf '%s\n' COLLECTOR_VERIFICATION_PASS
