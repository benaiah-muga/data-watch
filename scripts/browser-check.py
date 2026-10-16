#!/usr/bin/env python3
"""Linux-only headless UI smoke test; the application runtime has no Python dependency."""

import base64
import hashlib
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import struct
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def physical_interface():
    route = subprocess.run(["ip", "-4", "route", "show", "default"], text=True, capture_output=True)
    if route.returncode == 0:
        fields = route.stdout.split()
        if "dev" in fields:
            name = fields[fields.index("dev") + 1]
            if pathlib.Path("/sys/class/net", name, "device").exists():
                return name
    for entry in pathlib.Path("/sys/class/net").iterdir():
        if entry.name != "lo" and (entry / "device").exists():
            return entry.name
    raise RuntimeError("browser check needs an active physical network interface")


class DevTools:
    def __init__(self, url):
        parsed = urllib.parse.urlparse(url)
        self.sock = socket.create_connection((parsed.hostname, parsed.port), timeout=8)
        self.sock.settimeout(8)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((
            f"GET {parsed.path} HTTP/1.1\r\nHost: {parsed.hostname}:{parsed.port}\r\n"
            f"Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        ).encode())
        response = b""
        while b"\r\n\r\n" not in response:
            response += self.sock.recv(4096)
        headers = response.split(b"\r\n\r\n", 1)[0].decode().lower()
        expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode().lower()
        if "http/1.1 101 " not in headers or expected not in headers:
            raise RuntimeError(f"Chrome DevTools WebSocket handshake failed: {headers}")
        self.sequence = 0
        self.exceptions = []

    def _read(self, count):
        value = b""
        while len(value) < count:
            part = self.sock.recv(count - len(value))
            if not part:
                raise RuntimeError("Chrome closed the DevTools connection")
            value += part
        return value

    def _send(self, payload, opcode=1):
        payload = payload if isinstance(payload, bytes) else payload.encode()
        mask = os.urandom(4)
        length = len(payload)
        prefix = bytes([0x80 | opcode])
        if length < 126:
            prefix += bytes([0x80 | length])
        elif length < 65536:
            prefix += bytes([0x80 | 126]) + struct.pack("!H", length)
        else:
            prefix += bytes([0x80 | 127]) + struct.pack("!Q", length)
        self.sock.sendall(prefix + mask + bytes(item ^ mask[i % 4] for i, item in enumerate(payload)))

    def _receive(self):
        first, second = self._read(2)
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = struct.unpack("!H", self._read(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self._read(8))[0]
        mask = self._read(4) if second & 0x80 else None
        payload = self._read(length)
        if mask:
            payload = bytes(item ^ mask[i % 4] for i, item in enumerate(payload))
        if opcode == 9:
            self._send(payload, 10)
            return self._receive()
        if opcode == 8:
            raise RuntimeError("Chrome closed the DevTools WebSocket")
        return json.loads(payload.decode())

    def call(self, method, params=None):
        self.sequence += 1
        request_id = self.sequence
        self._send(json.dumps({"id": request_id, "method": method, "params": params or {}}))
        while True:
            message = self._receive()
            if message.get("method") == "Runtime.exceptionThrown":
                self.exceptions.append(message)
            if message.get("id") == request_id:
                if "error" in message:
                    raise RuntimeError(f"Chrome DevTools {method}: {message['error']}")
                return message.get("result", {})

    def evaluate(self, expression):
        result = self.call("Runtime.evaluate", {
            "expression": expression,
            "returnByValue": True,
            "awaitPromise": True,
        })["result"]
        if result.get("exceptionDetails"):
            raise RuntimeError(result["exceptionDetails"].get("text", "JavaScript evaluation failed"))
        return result.get("value")

    def close(self):
        self.sock.close()


def main():
    project = pathlib.Path(__file__).resolve().parents[1]
    binary = project / "target/release/data-watch"
    chrome = shutil.which("google-chrome") or shutil.which("chromium") or shutil.which("chromium-browser")
    if not binary.exists() or not chrome:
        raise RuntimeError("build target/release/data-watch and install Google Chrome or Chromium first")
    interface = physical_interface()
    root = pathlib.Path(tempfile.mkdtemp(prefix="data-watch-browser-"))
    data = root / "data"
    fake_bin = root / "fake-bin"
    data.mkdir()
    fake_bin.mkdir()
    log = data / "notifications.log"
    notifier = fake_bin / "notify-send"
    notifier.write_text("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nprintf '%s\\n' \"$*\" >> \"$DATA_WATCH_DATA/notifications.log\"\n")
    notifier.chmod(0o755)
    now = int(time.time())
    minute = now // 60 * 60
    db = sqlite3.connect(data / "usage.sqlite3")
    db.executescript("""
      CREATE TABLE traffic (ts INTEGER, interface TEXT, rx INTEGER, tx INTEGER, PRIMARY KEY(ts, interface));
      CREATE TABLE apps (ts INTEGER, interface TEXT, name TEXT, rx INTEGER, tx INTEGER, last_seen REAL, PRIMARY KEY(ts, interface, name));
      CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
    """)
    for ts, rx, tx in ((minute - 1800, 6400000, 240000), (minute, 1800000, 62000)):
        db.execute("INSERT INTO traffic VALUES(?,?,?,?)", (ts, interface, rx + 2400000, tx + 310000))
        db.execute("INSERT INTO apps VALUES(?,?,?,?,?,?)", (ts, interface, "browser-demo", rx, tx, now))
        db.execute("INSERT INTO apps VALUES(?,?,?,?,?,?)", (ts, interface, "editor-demo", 2400000, 310000, now))
    for index in range(15):
        db.execute("INSERT INTO apps VALUES(?,?,?,?,?,?)", (minute, interface, f"program-demo-{index:02}", 1000 + index, 100, now))
    db.execute("INSERT INTO settings VALUES('interface', ?)", (interface,))
    db.commit()
    db.close()

    app_port, devtools_port = free_port(), free_port()
    env = dict(os.environ, DATA_WATCH_DATA=str(data), DATA_WATCH_PORT=str(app_port), PATH=str(fake_bin) + os.pathsep + os.environ.get("PATH", ""))
    service = subprocess.Popen([str(binary), "serve"], cwd=root, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    browser = None
    client = None
    try:
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{app_port}/api/status?range=today", timeout=1).read()
                break
            except Exception:
                if service.poll() is not None:
                    raise RuntimeError(service.stderr.read().decode(errors="replace"))
                time.sleep(.1)
        else:
            raise RuntimeError("Data Watch did not start")

        browser = subprocess.Popen([
            chrome, "--headless=new", "--no-sandbox", "--disable-gpu", "--no-first-run",
            f"--remote-debugging-port={devtools_port}", f"--user-data-dir={root / 'chrome-profile'}",
            "--window-size=1440,1400", "about:blank",
        ], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.time() + 10
        version = None
        while time.time() < deadline:
            try:
                version = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{devtools_port}/json/version", timeout=1).read())
                break
            except Exception:
                if browser.poll() is not None:
                    raise RuntimeError("Chrome exited before opening DevTools")
                time.sleep(.1)
        target_url = urllib.parse.quote(f"http://127.0.0.1:{app_port}/", safe="")
        target = json.loads(urllib.request.urlopen(urllib.request.Request(
            f"http://127.0.0.1:{devtools_port}/json/new?{target_url}", method="PUT"), timeout=3).read())
        client = DevTools(target["webSocketDebuggerUrl"])
        client.call("Page.enable")
        client.call("Runtime.enable")
        client.call("Emulation.setEmulatedMedia", {"features": [{"name": "prefers-reduced-motion", "value": "no-preference"}]})
        client.call("Page.navigate", {"url": f"http://127.0.0.1:{app_port}/"})
        deadline = time.time() + 10
        while time.time() < deadline:
            if client.evaluate("document.readyState") == "complete" and client.evaluate("document.querySelectorAll('.app-history-trigger').length") >= 2:
                break
            time.sleep(.1)
        assert "Data Watch" in client.evaluate("document.title")
        assert client.evaluate("document.querySelector('#network-error').hidden")
        assert client.evaluate("document.querySelector('#notification-badge').textContent.includes('READY')")
        radio_state = client.evaluate("(() => { const radios=[...document.querySelectorAll('#interface-options input[type=radio]')]; return {count:radios.length,checked:radios.filter(x=>x.checked).length,selected:radios.find(x=>x.checked)?.value}; })()")
        assert radio_state["count"] >= 2 and radio_state["checked"] == 1 and radio_state["selected"] == interface, radio_state
        client.evaluate("document.querySelector('#interface-options input[value=auto]').click()")
        deadline = time.time() + 5
        while time.time() < deadline and client.evaluate("fetch('/api/status?range=today').then(r=>r.json()).then(d=>d.interface_selection)") != "auto":
            time.sleep(.1)
        assert client.evaluate("fetch('/api/status?range=today').then(r=>r.json()).then(d=>d.interface_selection)") == "auto"
        client.evaluate(f"document.querySelector('#interface-options input[value={json.dumps(interface)}]').click()")
        deadline = time.time() + 5
        while time.time() < deadline and client.evaluate("fetch('/api/status?range=today').then(r=>r.json()).then(d=>d.interface_selection)") != interface:
            time.sleep(.1)
        assert client.evaluate("fetch('/api/status?range=today').then(r=>r.json()).then(d=>d.interface_selection)") == interface
        theme = client.evaluate("(() => { const c=document.querySelector('#matrix-rain'),x=c.getContext('2d'),d=x.getImageData(0,0,c.width,c.height).data; let ink=0; for(let i=0;i<d.length;i+=16) if(d[i+1]>45 && d[i+1]>d[i]*1.4 && d[i+1]>d[i+2]*1.1) ink++; return {font:getComputedStyle(document.body).fontFamily,fontLoaded:document.fonts.check('14px \\\"IBM Plex Sans\\\"'),canvas:getComputedStyle(c).opacity,behind:Number(getComputedStyle(c).zIndex)<Number(getComputedStyle(document.querySelector('main')).zIndex),ink,upload:getComputedStyle(document.querySelector('.bar-upload')).backgroundImage,favicon:document.querySelector('link[rel=icon]').getAttribute('href')}; })()")
        assert "IBM Plex Sans" in theme["font"] and theme["fontLoaded"], theme
        assert 0 < float(theme["canvas"]) <= .28 and theme["behind"] and theme["favicon"] == "/favicon.svg", theme
        assert theme["ink"] > 0, f"Matrix background has not drawn any characters: {theme}"
        assert "rgb(133, 220, 255)" in theme["upload"], theme
        signal = client.evaluate("({bars:document.querySelectorAll('#signal-bars i').length, rate:document.querySelector('#signal-rate').textContent, state:document.querySelector('#signal-state').textContent})")
        assert signal["bars"] == 12 and signal["rate"].endswith("/s") and signal["state"] in ("IDLE", "TRANSFERRING"), signal
        shares = client.evaluate("Number.parseFloat(document.querySelector('#download-share').textContent) + Number.parseFloat(document.querySelector('#upload-share').textContent)")
        assert abs(shares - 100) < .01, shares
        timeline = client.evaluate("(() => { const bars=[...document.querySelectorAll('.time-bucket')]; return {count:bars.length, clear:bars.every(b=>getComputedStyle(b).backgroundColor==='rgba(0, 0, 0, 0)'), quiet:bars.some(b=>b.querySelectorAll('.bar-download:not([hidden]), .bar-upload:not([hidden])').length===0)}; })()")
        assert timeline["count"] >= 3 and timeline["clear"] and timeline["quiet"], timeline
        assert client.evaluate("document.querySelectorAll('#apps tr').length") == 10
        client.evaluate("document.querySelector('#show-more-programs').click()")
        expanded = client.evaluate("(() => { const s=document.querySelector('#programs-scroll'); return {rows:document.querySelectorAll('#apps tr').length,expanded:document.querySelector('#show-more-programs').getAttribute('aria-expanded'),scrollable:s.scrollHeight>s.clientHeight}; })()")
        assert expanded["rows"] >= 17 and expanded["expanded"] == "true" and expanded["scrollable"], expanded
        client.evaluate("document.querySelector('#show-more-programs').click()")
        assert client.evaluate("document.querySelectorAll('#apps tr').length") == 10
        app_history = client.evaluate("(() => { const b=[...document.querySelectorAll('.app-history-trigger')].find(x=>x.textContent==='browser-demo'); const row=b.closest('tr'); const total=row.querySelector('.total-cell').textContent; row.children[2].click(); return {open:document.querySelector('#app-history-panel').open,bars:document.querySelectorAll('#app-history-chart .history-bucket').length,total:document.querySelector('#history-total').textContent,rowTotal:total,focus:document.activeElement.id}; })()")
        assert app_history["open"] and app_history["bars"] > 0 and app_history["total"] == app_history["rowTotal"] and app_history["focus"] == "close-app-history", app_history
        client.call("Input.dispatchKeyEvent", {"type": "keyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})
        client.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})
        assert not client.evaluate("document.querySelector('#app-history-panel').open")
        assert client.evaluate("document.activeElement.dataset.program") == "browser-demo"
        client.call("Input.dispatchKeyEvent", {"type": "keyDown", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13})
        client.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13})
        assert client.evaluate("document.querySelector('#app-history-panel').open")
        client.evaluate("document.querySelector('#close-app-history').click()")
        client.evaluate("(() => { document.querySelector('#alert-enabled').checked=true; document.querySelector('#alert-limit').value='0.5'; document.querySelector('#alert-form').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true})); })()")
        deadline = time.time() + 5
        while time.time() < deadline:
            if "saved" in client.evaluate("document.querySelector('#alert-message').textContent.toLowerCase()"):
                break
            time.sleep(.1)
        assert "saved" in client.evaluate("document.querySelector('#alert-message').textContent.toLowerCase()")
        client.evaluate("document.querySelector('#test-alert').click()")
        deadline = time.time() + 5
        while time.time() < deadline and not log.exists():
            time.sleep(.1)
        assert log.exists() and "Test notification" in log.read_text()
        capture_socket = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
        frame = {"version": 2, "session": "browser-check", "sequence": 1, "ts": time.time(), "interface": interface, "apps": {}, "stats": {"dropped_packets": 0}}
        capture_socket.sendto(json.dumps(frame).encode(), str(data / "capture.sock"))
        deadline = time.time() + 5
        while time.time() < deadline and not client.evaluate("document.querySelector('#mode-notice').textContent.includes('continuous TCP + UDP')"):
            time.sleep(.1)
        assert client.evaluate("document.querySelector('#mode-notice').textContent.includes('continuous TCP + UDP')")
        assert client.evaluate("document.querySelector('#mode-notice').classList.contains('success') && document.querySelector('#tracking-help').hidden")
        frame.update(sequence=2, ts=time.time(), apps={"browser-demo": [4096, 1024]})
        capture_socket.sendto(json.dumps(frame).encode(), str(data / "capture.sock"))
        client.call("Emulation.setDeviceMetricsOverride", {"width": 1440, "height": 1200, "deviceScaleFactor": 1, "mobile": False})
        time.sleep(.5)
        pathlib.Path(project / ".test-artifacts").mkdir(exist_ok=True)
        desktop_image = client.call("Page.captureScreenshot", {"format": "png", "captureBeyondViewport": True})["data"]
        (project / ".test-artifacts/dashboard-desktop.png").write_bytes(base64.b64decode(desktop_image))
        client.call("Emulation.setDeviceMetricsOverride", {"width": 390, "height": 844, "deviceScaleFactor": 1, "mobile": True})
        time.sleep(.3)
        assert client.evaluate("document.documentElement.scrollWidth <= window.innerWidth"), "mobile layout overflows horizontally"
        client.evaluate("document.querySelector('#apps tr').children[2].click()")
        assert client.evaluate("(() => { const d=document.querySelector('#app-history-panel'); const r=d.getBoundingClientRect(); return d.open && r.left>=0 && r.right<=innerWidth && r.top>=0 && r.bottom<=innerHeight && d.scrollWidth<=d.clientWidth; })()"), "mobile program dialog exceeds the viewport"
        client.evaluate("document.querySelector('#close-app-history').click()")
        assert not client.exceptions, f"JavaScript exceptions: {client.exceptions}"
        image = client.call("Page.captureScreenshot", {"format": "png", "captureBeyondViewport": True})["data"]
        (project / ".test-artifacts/dashboard.png").write_bytes(base64.b64decode(image))
        deadline = time.time() + 9
        while time.time() < deadline and not client.evaluate("document.querySelector('#mode-notice').textContent.includes('Limited program monitoring')"):
            time.sleep(.1)
        assert client.evaluate("document.querySelector('#mode-notice').textContent.includes('Limited program monitoring') && !document.querySelector('#tracking-help').hidden")
        capture_socket.close()
        print("BROWSER_CHECKS_PASS: capture and fallback mode, interface radios, Matrix canvas, live signal, directional shares, app history, alerts, responsive layout, no JavaScript exceptions")
    finally:
        if client:
            client.close()
        if browser:
            browser.terminate()
            browser.wait(timeout=5)
        service.terminate()
        service.wait(timeout=5)
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
