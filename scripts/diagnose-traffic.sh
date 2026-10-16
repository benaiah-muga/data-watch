#!/usr/bin/env bash
# A bounded local diagnostic: packet summaries and socket owners, without payloads.
set -euo pipefail

if [[ ${1:-} == --help ]]; then
  cat <<'HELP'
Usage: bash scripts/diagnose-traffic.sh

Records 60 seconds on the default IPv4 network interface. Enter your Ubuntu
password only at sudo's terminal prompt. Uses installed tcpdump, ss, ip and
timeout; installs nothing and changes no network settings or services.

Saves text packet summaries (endpoints, protocol, lengths), socket owner
snapshots, interface counters and Data Watch status in a private directory
under ~/.local/share/data-watch/diagnostics. No packet payloads are saved.
HELP
  exit 0
fi
if (( $# != 0 )); then
  echo 'Use --help or run without arguments.' >&2
  exit 2
fi

for program in sudo tcpdump ss ip timeout python3; do
  command -v "$program" >/dev/null || { echo "Missing command: $program" >&2; exit 1; }
done

interface=$(ip -j -4 route show default | python3 -c 'import json,sys; routes=json.load(sys.stdin); print(min(routes,key=lambda r:r.get("metric",0)).get("dev","") if routes else "")')
if [[ -z $interface || ! -d /sys/class/net/$interface/statistics ]]; then
  echo 'No active default IPv4 interface found.' >&2
  exit 1
fi

echo "This records 60 seconds of traffic summaries on $interface."
echo 'Enter your Ubuntu password at the sudo prompt; it is not recorded.'
sudo -v

umask 077
diagnostic_dir=$(mktemp -d "${XDG_DATA_HOME:-$HOME/.local/share}/data-watch/diagnostics/capture-XXXXXXXX" 2>/dev/null) || {
  mkdir -p "${XDG_DATA_HOME:-$HOME/.local/share}/data-watch/diagnostics"
  diagnostic_dir=$(mktemp -d "${XDG_DATA_HOME:-$HOME/.local/share}/data-watch/diagnostics/capture-XXXXXXXX")
}
printf '%s\n' "$interface" > "$diagnostic_dir/interface.txt"
ip -j address show dev "$interface" > "$diagnostic_dir/addresses.json"
cat "/sys/class/net/$interface/statistics/"{rx,tx}_bytes > "$diagnostic_dir/counters-before.txt"
python3 -c 'import urllib.request; print(urllib.request.urlopen("http://127.0.0.1:8765/api/status?range=today",timeout=3).read().decode())' > "$diagnostic_dir/status-before.json" 2>/dev/null || true

sudo -n timeout --signal=INT --kill-after=5 60 tcpdump -i "$interface" -nn -q -l 'ip or ip6' > "$diagnostic_dir/packets.txt" 2> "$diagnostic_dir/capture.log" &
capture_pid=$!
started_seconds=$SECONDS
while kill -0 "$capture_pid" 2>/dev/null && (( SECONDS - started_seconds < 62 )); do
  printf 'SNAPSHOT %s\n' "$(date +%s.%N)" >> "$diagnostic_dir/sockets.txt"
  sudo -n ss -Htunape >> "$diagnostic_dir/sockets.txt"
  sleep 1
done
capture_status=0
wait "$capture_pid" || capture_status=$?
cat "/sys/class/net/$interface/statistics/"{rx,tx}_bytes > "$diagnostic_dir/counters-after.txt"
python3 -c 'import urllib.request; print(urllib.request.urlopen("http://127.0.0.1:8765/api/status?range=today",timeout=3).read().decode())' > "$diagnostic_dir/status-after.json" 2>/dev/null || true

if [[ $capture_status != 0 && $capture_status != 124 && $capture_status != 130 ]]; then
  cat "$diagnostic_dir/capture.log" >&2
  echo "Capture failed; diagnostic files: $diagnostic_dir" >&2
  exit "$capture_status"
fi
echo "Done. Diagnostic files saved in: $diagnostic_dir"
echo 'Tell Codex the capture is done so it can analyse these local files.'
