#!/usr/bin/env python3
"""Read-only check that the installed helper is current, enabled and collecting."""
import hashlib
import json
import os
import pathlib
import stat
import subprocess
import time
import urllib.request


def main():
    uid = os.getuid()
    service = f'data-watch-capture-{uid}.service'
    for action, unit in [('is-active', service), ('is-enabled', service), ('is-active', 'data-watch.service')]:
        args = ['systemctl'] + (['--user'] if unit == 'data-watch.service' else []) + [action, unit]
        subprocess.run(args, check=True, capture_output=True, text=True)
    helper = pathlib.Path(f'/usr/local/libexec/data-watch-capture-{uid}')
    unit = pathlib.Path('/etc/systemd/system', service)
    for path in (helper, unit):
        metadata = path.lstat()
        assert stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and not metadata.st_mode & 0o022, path
    installed = pathlib.Path.home() / '.local/bin/data-watch'
    assert hashlib.sha256(helper.read_bytes()).digest() == hashlib.sha256(installed.read_bytes()).digest(), 'capture helper has not been upgraded to the installed binary'
    pid = int(subprocess.check_output(['systemctl', 'show', service, '-p', 'MainPID', '--value'], text=True).strip())
    assert pid > 0
    fields = dict(line.split(':', 1) for line in pathlib.Path(f'/proc/{pid}/status').read_text().splitlines() if ':' in line)
    assert int(fields['Uid'].split()[0]) == uid, 'capture helper should run as the desktop UID with limited capabilities'
    capabilities = (1 << 2) | (1 << 13) | (1 << 19)
    assert int(fields['CapEff'].strip(), 16) == capabilities, fields['CapEff']
    started = time.monotonic()
    status = None
    while time.monotonic() - started < 45:
        status = json.load(urllib.request.urlopen('http://127.0.0.1:8765/api/status?range=today', timeout=3))
        capture = status.get('capture', {})
        stats = capture.get('stats', {})
        if status['mode'] == 'packet-capture' and capture.get('active') and stats.get('tcp_rx', 0) + stats.get('tcp_tx', 0) > 0 and stats.get('udp_rx', 0) + stats.get('udp_tx', 0) > 0:
            break
        time.sleep(1)
    else:
        raise AssertionError(('expected live TCP and UDP capture heartbeats', status))
    assert stats['dropped_packets'] == 0, stats
    assert status['packet_tracking_since'] > 0
    assert any(a['rx'] + a['tx'] > 0 for a in status['apps'])
    print('INSTALLED_CAPTURE_CHECK_PASS:', json.dumps({'pid': pid, 'mode': status['mode'], 'stats': stats, 'memory_rss': fields['VmRSS'].strip()}))


if __name__ == '__main__':
    main()
