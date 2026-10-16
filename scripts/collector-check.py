#!/usr/bin/env python3
"""Exercise native AF_PACKET capture -> aggregate IPC -> API -> SQLite history."""
import json
import os
import pathlib
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request


def main():
    project = pathlib.Path(__file__).resolve().parents[1]
    binary = project / 'target/release/data-watch'
    assert binary.exists(), 'build the release binary first'
    with tempfile.TemporaryDirectory(prefix='data-watch-collector-') as directory:
        root = pathlib.Path(directory)
        data = root / 'history'
        with socket.socket() as port_socket:
            port_socket.bind(('127.0.0.1', 0))
            port = port_socket.getsockname()[1]
        env = dict(os.environ, DATA_WATCH_DATA=str(data), DATA_WATCH_PORT=str(port))
        monitor = subprocess.Popen([str(binary), 'serve'], env=env, stdout=subprocess.DEVNULL)
        raw = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        raw.bind(str(root / 'raw.sock'))
        raw.settimeout(.2)
        done = threading.Event()
        frames = []
        errors = []
        physical = None
        fixture = None
        def status():
            return json.load(urllib.request.urlopen(f'http://127.0.0.1:{port}/api/status?range=today', timeout=3))
        def await_condition(test, description, timeout=12):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if test():
                    return
                time.sleep(.1)
            raise AssertionError(description)
        def ready():
            nonlocal physical
            try:
                physical = status()['interface']
                return bool(physical)
            except Exception:
                return False
        def forward():
            while not done.is_set():
                try:
                    frame = json.loads(raw.recv(65536))
                    assert frame['interface'] == 'lo'
                    # Test namespace has only lo. Relabel solely the IPC interface
                    # to the physical interface the isolated history monitor selects.
                    frame['interface'] = physical
                    frames.append(frame)
                    raw.sendto(json.dumps(frame).encode(), str(data / 'capture.sock'))
                except socket.timeout:
                    pass
                except Exception as error:
                    errors.append(repr(error))
        try:
            await_condition(ready, 'isolated monitor did not start')
            thread = threading.Thread(target=forward)
            thread.start()
            expected = {'dw-udp': 65536, 'dw-udp6': 65536, 'dw-tcp': 49152}
            for run in range(2):
                baseline = {a['name']: a for a in status()['apps']}
                fixture = subprocess.Popen(['unshare', '-Urn', 'python3', str(project / 'scripts/capture-fixture.py'), str(binary), str(root / 'raw.sock')], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                await_condition(lambda: status()['mode'] == 'packet-capture', 'helper heartbeat did not activate TCP/UDP tracking')
                output, error = fixture.communicate(timeout=20)
                assert fixture.returncode == 0 and 'NATIVE_CAPTURE_TRAFFIC_SENT' in output, (output, error)
                assert not errors, errors
                assert frames, 'no native frames'
                latest = frames[-1]
                assert latest['stats']['dropped_packets'] == 0, latest
                assert latest['stats']['udp_rx'] >= 131072 and latest['stats']['udp_tx'] >= 131072, latest
                assert latest['stats']['tcp_rx'] >= 49152 and latest['stats']['tcp_tx'] >= 49152, latest
                observed = {a['name']: a for a in status()['apps']}
                for name, minimum in expected.items():
                    assert name in latest['apps'], (name, latest)
                    assert name in observed, (name, observed)
                    for direction in ('rx', 'tx'):
                        delta = observed[name][direction] - baseline.get(name, {}).get(direction, 0)
                        # Loopback delivers one send and one receive to processes
                        # sharing a program name; each direction includes IP headers.
                        assert minimum <= delta < minimum * 1.5, (run, name, direction, delta)
                # Duplicate/out-of-order datagrams cannot add traffic again.
                recorded = {n: (observed[n]['rx'], observed[n]['tx']) for n in expected}
                for _ in range(3):
                    raw.sendto(json.dumps(latest).encode(), str(data / 'capture.sock'))
                time.sleep(2.3)
                observed = {a['name']: a for a in status()['apps']}
                assert all((observed[n]['rx'], observed[n]['tx']) == value for n, value in recorded.items()), observed
                await_condition(lambda: status()['mode'] == 'tcp-estimate' and not status()['capture']['active'], 'helper loss did not return to limited mode')
                assert status()['packet_tracking_since'] > 0
            db = sqlite3.connect(data / 'usage.sqlite3')
            for name in expected:
                row = db.execute('SELECT SUM(rx), SUM(tx) FROM apps WHERE name=?', (name,)).fetchone()
                assert all(value >= expected[name]*2 for value in row), (name, row)
            db.close()
            print('COLLECTOR_CHECK_PASS: real IPv4/IPv6 UDP and short TCP, IPC, SQLite, replay, helper restart, fallback; no internet traffic')
        finally:
            done.set()
            if 'thread' in locals():
                thread.join(3)
            raw.close()
            if fixture and fixture.poll() is None:
                fixture.terminate()
                fixture.wait(timeout=5)
            monitor.terminate()
            monitor.wait(timeout=5)


if __name__ == '__main__':
    main()
