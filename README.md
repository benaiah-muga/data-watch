<p align="center">
  <img src="https://raw.githubusercontent.com/benaiah-muga/data-watch/main/docs/assets/data-watch-logo.svg" alt="Data Watch logo" width="80" height="80">
</p>

<h1 align="center">Data Watch</h1>

<p align="center">Your hotspot data, accounted for.</p>

My phone's hotspot report showed that my PC was using about 80% of my mobile data. I was mostly using Codex in VS Code and searching the web, so I wanted a small tool that could answer two questions: when is the data being used, and which programs can be tied to it? Data Watch records connection totals and best-effort program estimates locally, then shows them in a private dashboard.

Data Watch is currently for **Ubuntu Linux**. Windows and macOS support are future work because each operating system exposes network and desktop notification features differently.

Read the [development story](docs/development-story.md) for the investigation that led to Data Watch and the problems the collector and interface had to solve.

![Data Watch dashboard showing live throughput, connection selection, download and upload totals, and a usage timeline](https://raw.githubusercontent.com/benaiah-muga/data-watch/main/docs/assets/dashboard.png)

*The dashboard shows live traffic, historical usage, and collection status, including warnings when program attribution may be incomplete.*

## Install

The Linux release is one executable plus a README, license notices, and the development story. The dashboard, font, favicon, and SQLite engine are embedded in the executable, so the installed application does not need a separate web server, database package, or static asset folder. The executable uses Ubuntu's system libraries and `ip`/`ss` commands. Desktop alerts use `notify-send` from `libnotify-bin`.

Download the ready-to-run **Ubuntu 22.04+ x86-64** archive from [GitHub Releases](https://github.com/benaiah-muga/data-watch/releases/latest). It contains the executable, documentation, and license notices; no Rust toolchain is needed to run it.

```sh
curl -fLO https://github.com/benaiah-muga/data-watch/releases/latest/download/data-watch-linux-x86_64.tar.gz
curl -fLO https://github.com/benaiah-muga/data-watch/releases/latest/download/SHA256SUMS
sha256sum -c SHA256SUMS
tar -xzf data-watch-linux-x86_64.tar.gz
cd data-watch-linux-x86_64
./data-watch install
```

To build from source, install Rust and a C compiler, then run:

```sh
cargo build --release --locked
./target/release/data-watch install
```

Or run `./install.sh`, which builds and installs the same release binary. The installer copies the executable to `~/.local/bin/data-watch`, creates a **Data Watch** application-menu entry with its own icon, installs a systemd user service, enables it at boot, and keeps it running after logout. The menu entry opens a standalone Chrome/Chromium-style app window when available, then falls back to your default browser. Usage history stays in `~/.local/share/data-watch/usage.sqlite3` with user-only permissions.

Open <http://127.0.0.1:8765> or launch **Data Watch** from Applications. The monitor records without an open browser tab. The PC must be awake to collect traffic. System notifications appear when the graphical desktop session is available, including while the dashboard is closed.

For desktop alerts, install the Ubuntu notification command if it is not already present:

```sh
sudo apt install libnotify-bin
systemctl --user restart data-watch.service
```

## What it shows

- Connection totals for today, the last hour, seven daily bars, twelve weekly bars, twelve monthly bars, or five yearly bars, with downloads and uploads separated.
- Download and upload cards show each direction's share of the selected period.
- The dashboard header shows local time and timezone beside current throughput and connection status.
- Selecting a timeline bar updates the summary and program list to that interval. Use **Return to today** to reload the current readings.
- Choose a network connection with accessible radio buttons in the live header.
- A live throughput meter in the header and a low-contrast Matrix-style background that stays behind the dashboard content.
- A timeline of usage. Select a period to inspect its attributed programs.
- Program totals and current rates. Select a program to see its usage history over the chosen period.
- The current on-disk history database size, including pending SQLite WAL writes.
- A configurable daily data limit. Data Watch sends one Linux desktop notification with the system alarm sound per day when the selected connection reaches it. Use **Send test** to check notifications and sound.
- CSV export for further analysis.

Times use the PC's local timezone and sizes use binary units (MiB, GiB). The collector samples every two seconds and stores detailed data by minute for 30 days, then compacts older traffic into daily summaries retained for up to five years.

## Attribution and privacy

Connection totals come from Linux interface counters and include background services and local network traffic, so they can differ from carrier billing. Basic program attribution uses TCP socket counters. It can miss UDP/QUIC traffic, short-lived connections, some privileged processes, and VPN inner traffic. Browser traffic is grouped under Chrome or Firefox; tabs and VS Code extensions sharing a process cannot be separated. The dashboard shows traffic that could not be attributed instead of assigning it to a guessed program.

Enable continuous TCP and UDP packet tracking, including Chrome QUIC and short connections:

```sh
~/.local/bin/data-watch full
```

Enter your Ubuntu password locally when requested. The same binary installs a root-owned copy under `/usr/local/libexec/` and enables `data-watch-capture-<uid>.service`. It restarts automatically and runs after logout and reboot; you can close the terminal. No NetHogs, tcpdump, extra library, or package download is needed. The capture helper reads packet headers and process/socket metadata, then sends aggregate byte counts to the unprivileged monitor. It never opens the history database or serves HTTP. Its systemd unit limits its capabilities and filesystem access.

The dashboard explicitly shows **continuous TCP + UDP packet tracking** while it receives helper heartbeats, or **limited TCP estimates** if the helper stops. Packet counts include IP and transport headers and retransmissions. Interface totals also include link overhead and other traffic, so a small gap can remain. Unmatched sockets stay unattributed; the monitor does not guess their owner. Historical TCP-only estimates remain unchanged and cannot be reconstructed.

To update the privileged helper after upgrading the binary, run `data-watch full` again. To stop or remove it:

```sh
sudo systemctl disable --now "data-watch-capture-$(id -u).service"
sudo rm "/etc/systemd/system/data-watch-capture-$(id -u).service" "/usr/local/libexec/data-watch-capture-$(id -u)"
sudo systemctl daemon-reload
```

Recorded history contains timestamps, connection names, program names, and byte counts. Data Watch does not save URLs, remote addresses, command arguments, or packet contents. It makes no analytics or cloud requests. Desktop alerts use the local Linux notification service.

## Manage the service

```sh
~/.local/bin/data-watch status
~/.local/bin/data-watch stop
~/.local/bin/data-watch start
journalctl --user -u data-watch.service -n 30
```

For an isolated foreground run:

```sh
DATA_WATCH_DATA=/tmp/data-watch-test DATA_WATCH_PORT=8876 ./target/release/data-watch serve
```

Only one collector can use a history directory. The HTTP server listens on loopback, rejects unexpected Host headers, and requires a custom header for settings changes. No firewall changes are needed.

To remove the system integration while preserving history and the project:

```sh
systemctl --user disable --now data-watch.service
rm ~/.config/systemd/user/data-watch.service
rm ~/.local/share/applications/data-watch.desktop ~/.local/bin/data-watch
rm ~/.local/share/icons/hicolor/scalable/apps/data-watch.svg
systemctl --user daemon-reload
```

## Development

```sh
cargo fmt --check
cargo test --locked --offline
cargo clippy --locked --offline -- -D warnings
node --check static/app.js
```

The project is licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0). The bundled IBM Plex Sans font is licensed separately under the SIL Open Font License in `static/fonts/OFL.txt`. See [LICENSE](LICENSE) and [CONTRIBUTING.md](CONTRIBUTING.md).
