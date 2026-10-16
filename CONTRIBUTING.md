# Contributing

Thanks for helping improve Data Watch. The first release target is Ubuntu Linux. Keep network collection local and explain attribution limits when changing estimates; interface totals and per-program totals measure different things.

## Build and verify

Install a current stable Rust toolchain, a C compiler, and Node.js. Build and run the checks with:

```sh
cargo build --release --locked
cargo fmt --check
cargo test --locked --offline
cargo clippy --locked --offline -- -D warnings
node --check static/app.js
node scripts/ui-check.js
node scripts/smoke-binary.js
python3 scripts/collector-check.py
python3 scripts/capture-install-check.py
bash scripts/verify-collector.sh
```

To exercise the single-file executable, copy `target/release/data-watch` to a temporary directory and run it there with `DATA_WATCH_DATA` and `DATA_WATCH_PORT` set. It should serve the dashboard without reading files from the source tree.

The collector check uses an unprivileged Linux user/network namespace and loopback packets. It requires `unshare`, `ip`, and Python 3, sends no internet traffic, and does not need sudo. It covers real IPv4/IPv6 UDP and TCP transfers shorter than one second, helper restarts, duplicate frames, API output, and SQLite persistence. The browser check requires Chrome or Chromium. After enabling packet tracking locally, `python3 scripts/installed-capture-check.py` verifies the installed services and live TCP/UDP capture.

## Changes

- Keep stored history compatible when evolving the SQLite schema.
- Memory collection must keep working offline. Treat RSS as an estimate, distinguish missing PSI from zero, and store sampled peaks rather than summing memory over time.
- Do not collect URLs, remote addresses, command arguments, or packet contents.
- Keep local HTTP endpoints bound to loopback and protect state-changing requests.
- Test desktop notifications with a fake `notify-send` in automated checks; reserve real desktop popups for a deliberate manual test.
- Update the README when user-visible behavior or platform requirements change.

By submitting a contribution, you agree that it is offered under the [Apache License, Version 2.0](LICENSE). The project does not require a separate contributor agreement.
