"use strict";

const assert = require("assert");
const fs = require("fs");
const http = require("http");
const net = require("net");
const os = require("os");
const path = require("path");
const { spawn } = require("child_process");

function request(port, route, method = "GET", body = null) {
  return new Promise((resolve, reject) => {
    const data = body == null ? null : Buffer.from(JSON.stringify(body));
    const req = http.request({
      hostname: "127.0.0.1", port, path: route, method,
      headers: data ? { "Content-Type": "application/json", "Content-Length": data.length, "X-Data-Watch": "1" } : {},
      timeout: 3000,
    }, (res) => {
      const chunks = [];
      res.on("data", (chunk) => chunks.push(chunk));
      res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks).toString("utf8") }));
    });
    req.on("timeout", () => req.destroy(new Error("request timed out")));
    req.on("error", reject);
    if (data) req.write(data);
    req.end();
  });
}

function availablePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      server.close((error) => error ? reject(error) : resolve(port));
    });
  });
}

function removeTree(target) {
  if (!fs.existsSync(target)) return;
  for (const item of fs.readdirSync(target)) {
    const full = path.join(target, item);
    if (fs.lstatSync(full).isDirectory()) removeTree(full);
    else fs.unlinkSync(full);
  }
  fs.rmdirSync(target);
}

async function main() {
  const project = path.resolve(__dirname, "..");
  const releaseBinary = path.join(project, "target", "release", "data-watch");
  assert(fs.existsSync(releaseBinary), "release binary is missing; build it first");
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "data-watch-single-binary-"));
  const binary = path.join(root, "data-watch");
  const data = path.join(root, "data");
  const fakeBin = path.join(root, "fake-bin");
  const notificationLog = path.join(data, "notifications.log");
  fs.mkdirSync(data);
  fs.mkdirSync(fakeBin);
  fs.copyFileSync(releaseBinary, binary);
  fs.chmodSync(binary, 0o755);
  const notifyStub = path.join(fakeBin, "notify-send");
  fs.writeFileSync(notifyStub, "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then exit 0; fi\nprintf '%s\\n' \"$*\" >> \"$DATA_WATCH_DATA/notifications.log\"\n", { mode: 0o755 });
  const port = await availablePort();
  const child = spawn(binary, ["serve"], {
    cwd: root,
    env: { ...process.env, DATA_WATCH_DATA: data, DATA_WATCH_PORT: String(port), PATH: `${fakeBin}:${process.env.PATH}` },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  child.stdout.on("data", (chunk) => { output += chunk; });
  child.stderr.on("data", (chunk) => { output += chunk; });
  try {
    let page;
    const deadline = Date.now() + 10000;
    while (Date.now() < deadline) {
      if (child.exitCode !== null) throw new Error(`server exited early: ${output}`);
      try { page = await request(port, "/"); break; } catch (_) { await new Promise((resolve) => setTimeout(resolve, 100)); }
    }
    assert(page && page.status === 200, `embedded dashboard did not start: ${output}`);
    assert(page.body.includes("Desktop data alert"), "embedded dashboard is missing alert controls");
    const js = await request(port, "/app.js");
    assert(js.status === 200 && js.body.includes("renderAppHistory"), "embedded JavaScript is missing app history");
    const css = await request(port, "/app.css");
    assert(css.status === 200 && css.body.includes("--green-bright") && css.body.includes("IBM Plex Sans"), "embedded stylesheet is missing the new theme or font");
    const favicon = await request(port, "/favicon.svg");
    assert(favicon.status === 200 && favicon.headers["content-type"].includes("image/svg+xml"), "embedded favicon is missing");
    const font = await request(port, "/fonts/ibm-plex-sans-latin.woff2");
    assert(font.status === 200 && font.headers["content-type"].includes("font/woff2"), "embedded font is missing");
    assert(!fs.existsSync(path.join(root, "static")), "smoke test unexpectedly has separate static files");
    const status = await request(port, "/api/status?range=today");
    assert(status.status === 200, "status API failed");
    const statusValue = JSON.parse(status.body);
    assert.strictEqual(statusValue.alerts.notifications_available, true, "notification command stub was not detected");
    const save = await request(port, "/api/alerts", "POST", { threshold_bytes: 536870912 });
    assert.strictEqual(save.status, 200, "could not save alert setting");
    const afterSave = JSON.parse((await request(port, "/api/status?range=today")).body);
    assert.strictEqual(afterSave.alerts.threshold_bytes, 536870912, "alert setting was not reflected by the API");
    const test = await request(port, "/api/alerts/test", "POST", {});
    assert.strictEqual(test.status, 200, "test notification did not dispatch");
    assert(fs.readFileSync(notificationLog, "utf8").includes("Test notification"), "notification command was not invoked");
    process.stdout.write("SINGLE_BINARY_SMOKE_PASS\n");
  } finally {
    child.kill("SIGTERM");
    await new Promise((resolve) => child.once("exit", resolve));
    removeTree(root);
  }
}

main().catch((error) => { console.error(error.stack || error); process.exitCode = 1; });
