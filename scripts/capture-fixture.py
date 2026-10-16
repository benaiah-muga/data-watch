#!/usr/bin/env python3
"""Real packets in an isolated user/network namespace; never sends to the internet."""
import ctypes
import multiprocessing
import pathlib
import socket
import subprocess
import sys
import threading
import time


def udp(family, name):
    ctypes.CDLL(None).prctl(15, name.encode(), 0, 0, 0)
    address = '::1' if family == socket.AF_INET6 else '127.0.0.1'
    with socket.socket(family, socket.SOCK_DGRAM) as server, socket.socket(family, socket.SOCK_DGRAM) as client:
        server.bind((address, 0))
        client.connect(server.getsockname())
        server.settimeout(2)
        for _ in range(64):
            assert client.send(b'u' * 1024) == 1024
            assert len(server.recv(2048)) == 1024
            time.sleep(.008)


def tcp():
    ctypes.CDLL(None).prctl(15, b'dw-tcp', 0, 0, 0)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        listener.listen()
        received = []
        def consume():
            with listener.accept()[0] as connection:
                connection.settimeout(3)
                data = b''
                while len(data) < 49152:
                    chunk = connection.recv(65536)
                    assert chunk
                    data += chunk
                received.append(len(data))
                time.sleep(.4)
        server = threading.Thread(target=consume)
        server.start()
        with socket.create_connection(listener.getsockname()) as client:
            client.sendall(b't' * 49152)
            time.sleep(.5)
        server.join()
        assert received == [49152], received


def main():
    binary, output = sys.argv[1:]
    subprocess.run(['ip', 'link', 'set', 'lo', 'up'], check=True)
    helper = subprocess.Popen([binary, 'packet-capture', output, 'lo'])
    try:
        # Two empty heartbeats establish the monitor's capture baseline.
        time.sleep(2.5)
        duplicate = subprocess.run([binary, 'packet-capture', output, 'lo'], capture_output=True, text=True, timeout=3)
        assert duplicate.returncode != 0 and 'already running' in duplicate.stderr, duplicate
        for function, args in [(udp, (socket.AF_INET, 'dw-udp')), (udp, (socket.AF_INET6, 'dw-udp6')), (tcp, ())]:
            worker = multiprocessing.Process(target=function, args=args)
            worker.start()
            worker.join(5)
            assert worker.exitcode == 0, (function.__name__, worker.exitcode)
        time.sleep(4)
        assert helper.poll() is None, 'packet helper exited'
        print('NATIVE_CAPTURE_TRAFFIC_SENT', flush=True)
    finally:
        helper.terminate()
        helper.wait(timeout=5)


if __name__ == '__main__':
    main()
