#!/usr/bin/env python3
"""Verify the generated service and installation privilege boundary, without sudo."""
import os
import pathlib
import subprocess
import tempfile


def main():
    project = pathlib.Path(__file__).resolve().parents[1]
    binary = project / 'target/release/data-watch'
    service = subprocess.check_output([str(binary), 'capture-service', '1000', '/home/example user/100%/capture.sock'], text=True)
    command = next(line for line in service.splitlines() if line.startswith('ExecStart='))
    assert command == 'ExecStart=/usr/local/libexec/data-watch-capture-1000 packet-capture "/home/example user/100%%/capture.sock"', command
    assert ' serve' not in command and '/.local/bin/' not in command
    expected = ['User=1000', 'NoNewPrivileges=true', 'AmbientCapabilities=CAP_NET_RAW CAP_DAC_READ_SEARCH CAP_SYS_PTRACE', 'CapabilityBoundingSet=CAP_NET_RAW CAP_DAC_READ_SEARCH CAP_SYS_PTRACE', 'ProtectSystem=strict', 'ProtectHome=read-only', 'RestrictAddressFamilies=AF_PACKET AF_UNIX', 'WantedBy=multi-user.target', 'Restart=always']
    assert all(line in service.splitlines() for line in expected), service
    for user, path in [('0', '/tmp/a'), ('1000', 'relative'), ('1000', '/tmp/a\nExecStart=bad')]:
        result = subprocess.run([str(binary), 'capture-service', user, path], capture_output=True, text=True)
        assert result.returncode != 0, (user, path, result)
    if os.geteuid() != 0:
        result = subprocess.run([str(binary), 'install-capture', str(os.getuid()), '/tmp/not-installed.sock'], capture_output=True, text=True)
        assert result.returncode != 0 and 'needs sudo' in result.stderr, result
    with tempfile.TemporaryDirectory(prefix='data-watch-service-') as directory:
        unit = pathlib.Path(directory) / 'data-watch-capture-check.service'
        # Substitute only the executable location for syntax verification before
        # the administrator installs the root-owned copy into /usr/local/libexec.
        unit.write_text(service.replace('/usr/local/libexec/data-watch-capture-1000', str(binary)))
        result = subprocess.run(['systemd-analyze', 'verify', str(unit)], capture_output=True, text=True)
        assert result.returncode == 0, result.stdout + result.stderr
    print('CAPTURE_INSTALL_CHECK_PASS: systemd syntax, fixed root-owned executable, capabilities, argument validation, non-root rejection')


if __name__ == '__main__':
    main()
