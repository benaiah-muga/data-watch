"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const clockTimeFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", second: "2-digit" });
  const clockZoneFormat = new Intl.DateTimeFormat(undefined, { timeZoneName: "short" });
  function updateClock() {
    const now = new Date();
    const clock = $("local-clock");
    if (!clock) return;
    clock.dateTime = now.toISOString();
    clock.textContent = clockTimeFormat.format(now).replace(/\u202f/g, " ");
    const zone = clockZoneFormat.formatToParts(now).find((part) => part.type === "timeZoneName");
    $("clock-zone").textContent = zone ? zone.value : "LOCAL";
  }

  updateClock();
  window.setInterval(updateClock, 1000);
  const state = { range: "today", sort: "total", data: null, selected: null, selectedApp: null, returnProgram: null, appsExpanded: false, buckets: [], signalHistory: [], busy: false, timer: null, queued: false };
  const labels = { today: "today", hour: "in the last hour", week: "in the last 7 days", weeks: "in the last 12 weeks", months: "in the last 12 months", years: "in the last 5 years" };
  const number = (value) => Math.max(0, Number(value) || 0);
  const bytes = (value) => {
    let n = number(value);
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) { n /= 1024; i += 1; }
    return `${n.toLocaleString(undefined, { maximumFractionDigits: i ? 1 : 0 })} ${units[i]}`;
  };
  const date = (ts, full = false) => new Date(number(ts) * 1000).toLocaleString(undefined, full ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" } : { hour: "2-digit", minute: "2-digit" });
  const calendarDate = (ts, options) => new Date(number(ts) * 1000).toLocaleDateString(undefined, options);
  function bucketAxisLabel(ts, range = state.range) {
    if (range === "today" || range === "hour") return calendarDate(ts, { hour: "2-digit", minute: "2-digit" });
    if (range === "week") return calendarDate(ts, { weekday: "short" });
    if (range === "weeks") return calendarDate(ts, { month: "short", day: "numeric" });
    if (range === "months") return calendarDate(ts, { month: "short" });
    return calendarDate(ts, { year: "numeric" });
  }
  function bucketRangeLabel(bucket, range = state.range) {
    if (range === "today" || range === "hour") return `${date(bucket.ts, true)} to ${date(bucket.end, true)}`;
    if (range === "week") return calendarDate(bucket.ts, { weekday: "long", month: "long", day: "numeric" });
    if (range === "weeks") return `${calendarDate(bucket.ts, { month: "short", day: "numeric" })} to ${calendarDate(bucket.end - 1, { month: "short", day: "numeric" })}`;
    if (range === "months") return calendarDate(bucket.ts, { month: "long", year: "numeric" });
    return calendarDate(bucket.ts, { year: "numeric" });
  }
  const set = (id, value) => { $(id).textContent = value; };
  const notice = (id, message) => { set(id, message); $(id).hidden = !message; };

  function startMatrixRain() {
    const canvas = $("matrix-rain");
    if (!canvas || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const context = canvas.getContext("2d", { alpha: true });
    if (!context) return;
    const glyphs = Array.from("01アイウエオカキクケコサシスセソ");
    const cell = 18;
    let width = 0;
    let height = 0;
    let drops = [];
    let timer = null;
    const resize = () => {
      const scale = Math.min(window.devicePixelRatio || 1, 1.5);
      width = window.innerWidth;
      height = window.innerHeight;
      canvas.width = Math.floor(width * scale);
      canvas.height = Math.floor(height * scale);
      context.setTransform(scale, 0, 0, scale, 0, 0);
      context.font = `500 12px "IBM Plex Mono", monospace`;
      drops = Array.from({ length: Math.ceil(width / cell) }, () => Math.random() * (height / cell));
    };
    const draw = () => {
      context.fillStyle = "rgba(7, 11, 8, .2)";
      context.fillRect(0, 0, width, height);
      for (let column = 0; column < drops.length; column += 1) {
        const y = drops[column] * cell;
        context.fillStyle = Math.random() > .97 ? "#a7ffb2" : "#3b9e54";
        context.fillText(glyphs[Math.floor(Math.random() * glyphs.length)], column * cell, y);
        if (y > height && Math.random() > .98) drops[column] = -Math.random() * 24;
        else drops[column] += .8 + Math.random() * .9;
      }
    };
    const animate = () => { if (timer === null) { draw(); timer = window.setInterval(draw, 115); } };
    const pause = () => { if (timer !== null) { window.clearInterval(timer); timer = null; } };
    resize();
    animate();
    window.addEventListener("resize", resize, { passive: true });
    document.addEventListener("visibilitychange", () => { if (document.hidden) pause(); else animate(); });
  }

  function mergeApps(apps) {
    const merged = new Map();
    for (const app of apps) {
      const name = String(app.name || "Unknown process");
      const item = merged.get(name) || { name, rx: 0, tx: 0, rate_rx: 0, rate_tx: 0 };
      for (const key of ["rx", "tx", "rate_rx", "rate_tx"]) item[key] += number(app[key]);
      merged.set(name, item);
    }
    return [...merged.values()];
  }

  function makeBuckets(series) {
    return [...series].sort((a, b) => number(a.ts) - number(b.ts)).map((bucket) => {
      const rx = number(bucket.rx);
      const tx = number(bucket.tx);
      return { ...bucket, ts: number(bucket.ts), end: number(bucket.end), rx, tx,
        gap: rx + tx === 0, apps: mergeApps(Array.isArray(bucket.apps) ? bucket.apps : []) };
    });
  }

  function renderTimeline() {
    const buckets = makeBuckets(Array.isArray(state.data.series) ? state.data.series : []);
    state.buckets = buckets;
    const previous = state.selected;
    state.selected = previous == null ? null : buckets.findIndex((b) => b.ts === previous);
    if (state.selected === -1) state.selected = null;
    if (state.selected !== null) state.selected = buckets[state.selected].ts;
    const max = Math.max(1, ...buckets.map((b) => b.rx + b.tx));
    const fragment = document.createDocumentFragment();
    for (const b of buckets) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "time-bucket";
      button.dataset.ts = String(b.ts);
      button.setAttribute("aria-pressed", String(state.selected === b.ts));
      const description = `${bucketRangeLabel(b)}: ${b.gap ? "No readings recorded" : `${bytes(b.rx)} downloaded, ${bytes(b.tx)} uploaded`}`;
      button.setAttribute("aria-label", description);
      button.title = description;
      if (b.gap) button.classList.add("recording-gap");
      for (const [key, className] of [["tx", "bar-upload"], ["rx", "bar-download"]]) {
        const bar = document.createElement("span");
        bar.className = className;
        bar.style.height = `${number(b[key]) / max * 92}%`;
        bar.hidden = number(b[key]) === 0;
        bar.setAttribute("aria-hidden", "true");
        button.append(bar);
      }
      button.addEventListener("click", () => {
        state.selected = state.selected === b.ts ? null : b.ts;
        state.appsExpanded = false;
        document.querySelectorAll(".time-bucket").forEach((item) => item.setAttribute("aria-pressed", String(item === button && state.selected !== null)));
        renderOverview();
        renderApps();
        describeTimeline();
      });
      fragment.append(button);
    }
    const focusedTimestamp = document.activeElement && document.activeElement.classList.contains("time-bucket") ? document.activeElement.dataset.ts : null;
    $("timeline").replaceChildren(fragment);
    if (focusedTimestamp) {
      const focusedButton = [...$("timeline").children].find((el) => el.dataset.ts === focusedTimestamp);
      if (focusedButton) focusedButton.focus({ preventScroll: true });
    }
    set("axis-start", buckets.length ? bucketAxisLabel(buckets[0].ts) : "");
    set("axis-mid", buckets.length > 2 ? bucketAxisLabel(buckets[Math.floor(buckets.length / 2)].ts) : "");
    set("axis-end", buckets.length ? bucketAxisLabel(buckets[buckets.length - 1].ts) : "");
    describeTimeline();
  }

  function describeTimeline() {
    const b = state.buckets.find((item) => item.ts === state.selected);
    if (b) set("timeline-description", `Selected ${bucketRangeLabel(b)}: ${bytes(b.rx + b.tx)} used. The summary and program list show this interval.`);
    else if (state.buckets.length) {
      const peak = state.buckets.reduce((a, item) => item.rx + item.tx > a.rx + a.tx ? item : a);
      set("timeline-description", `Busiest recorded time: ${bucketRangeLabel(peak)}, ${bytes(peak.rx + peak.tx)}. All times use your PC's local time.`);
    } else set("timeline-description", "History starts when monitoring begins. Leave Data Watch running to build a picture of your usage.");
  }

  function renderOverview() {
    if (!state.data) return;
    const selected = state.buckets.find((bucket) => bucket.ts === state.selected);
    const totals = selected || state.data.totals || {};
    const rx = number(totals.rx);
    const tx = number(totals.tx);
    const total = rx + tx;
    set("total-label", selected ? "Data used in selected interval" : `Data used ${labels[state.range]}`);
    set("total", bytes(total));
    set("download", bytes(rx));
    set("upload", bytes(tx));
    const downloadShare = total > 0 ? rx / total * 100 : 0;
    set("download-share", `${downloadShare.toFixed(1)}% of total`);
    set("upload-share", `${(total > 0 ? 100 - downloadShare : 0).toFixed(1)}% of total`);
    set("since", selected
      ? bucketRangeLabel(selected)
      : state.data.first_recorded_at ? `History since ${date(state.data.first_recorded_at, true)}` : "History begins with the first reading");
    $("return-current").hidden = !selected && state.range === "today";
    $("metrics").classList.toggle("interval-selected", Boolean(selected));
  }

  function programButton(name) {
    return [...$("apps").querySelectorAll(".app-history-trigger")].find((button) => button.dataset.program === name);
  }

  function openProgramDetails(name) {
    state.selectedApp = name;
    state.returnProgram = name;
    renderAppHistory();
    const dialog = $("app-history-panel");
    if (!dialog.open) dialog.showModal();
    document.body.classList.add("dialog-open");
    $("apps").querySelectorAll(".app-history-trigger").forEach((button) => button.setAttribute("aria-expanded", String(button.dataset.program === name)));
  }

  function renderApps() {
    if (!state.data) return;
    const scroll = $("programs-scroll");
    const scrollTop = scroll.scrollTop;
    const scrollLeft = scroll.scrollLeft;
    const focusedProgram = document.activeElement && document.activeElement.classList.contains("app-history-trigger") ? document.activeElement.dataset.program : null;
    const selected = state.buckets.find((b) => b.ts === state.selected);
    const apps = mergeApps(selected ? selected.apps : (Array.isArray(state.data.apps) ? state.data.apps : []));
    const sortCurrent = state.sort === "current" && !selected;
    apps.sort((a, b) => (sortCurrent ? b.rate_rx + b.rate_tx - a.rate_rx - a.rate_tx : b.rx + b.tx - a.rx - a.tx) || a.name.localeCompare(b.name));
    if (apps.length <= 10) state.appsExpanded = false;
    const visibleApps = state.appsExpanded ? apps : apps.slice(0, 10);
    const max = Math.max(1, ...apps.map((app) => app.rx + app.tx));
    const fragment = document.createDocumentFragment();
    for (const app of visibleApps) {
      const row = document.createElement("tr");
      row.className = "program-row";
      row.title = `View ${app.name} details`;
      row.addEventListener("click", () => openProgramDetails(app.name));
      const program = document.createElement("td");
      program.className = "program";
      const name = document.createElement("button");
      name.type = "button";
      name.className = "program-name app-history-trigger";
      name.dataset.program = app.name;
      name.textContent = app.name;
      name.setAttribute("aria-label", `View ${app.name} details`);
      name.setAttribute("aria-haspopup", "dialog");
      name.setAttribute("aria-controls", "app-history-panel");
      name.setAttribute("aria-expanded", String(state.selectedApp === app.name && $("app-history-panel").open));
      const track = document.createElement("span");
      track.className = "usage-track";
      track.setAttribute("aria-hidden", "true");
      const fill = document.createElement("span");
      fill.className = "usage-fill";
      fill.style.width = `${(app.rx + app.tx) / max * 100}%`;
      track.append(fill);
      program.append(name, track);
      row.append(program);
      for (const [value, extra] of [[bytes(app.rx), ""], [bytes(app.tx), ""], [bytes(app.rx + app.tx), "total-cell"], [selected ? "N/A" : `${bytes(app.rate_rx + app.rate_tx)}/s`, "current-cell"]]) {
        const cell = document.createElement("td");
        cell.className = `numeric ${extra}`;
        cell.textContent = value;
        row.append(cell);
      }
      fragment.append(row);
    }
    $("apps").replaceChildren(fragment);
    scroll.classList.toggle("is-expanded", state.appsExpanded);
    scroll.tabIndex = state.appsExpanded ? 0 : -1;
    scroll.scrollTop = state.appsExpanded ? scrollTop : 0;
    scroll.scrollLeft = scrollLeft;
    if (focusedProgram) (programButton(focusedProgram) || $("sort")).focus({ preventScroll: true });
    const more = $("show-more-programs");
    more.hidden = apps.length <= 10;
    more.textContent = state.appsExpanded ? "Show top 10" : apps.length > 10 ? `Show ${apps.length - 10} more programs` : "Show more programs";
    more.setAttribute("aria-expanded", String(state.appsExpanded));
    set("program-count", !apps.length ? "" : state.appsExpanded ? `${apps.length} programs. Scroll inside the list to see more.` : `Showing ${visibleApps.length} of ${apps.length} programs`);
    $("apps-empty").hidden = apps.length > 0;
    set("apps-empty", selected ? "No programs could be attributed in this time window. The connection may still have used data." : "No program traffic recorded yet. Use your connection and check back in a moment.");
    $("clear-selection").hidden = !selected;
    $("sort").disabled = Boolean(selected);
    set("apps-description", selected ? `Traffic from ${bucketRangeLabel(selected)}. Current speed is only available for the whole period.` : `Observed program traffic ${labels[state.range]}.`);
    const total = selected || state.data.totals || {};
    const estimated = selected ? { rx: Math.max(0, total.rx - apps.reduce((n, app) => n + app.rx, 0)), tx: Math.max(0, total.tx - apps.reduce((n, app) => n + app.tx, 0)) } : state.data.unattributed || {};
    const difference = number(estimated.rx) + number(estimated.tx);
    $("unattributed").hidden = !difference;
    const packetTracking = state.data.mode === "packet-capture" && state.data.capture && state.data.capture.active;
    const explanation = packetTracking
      ? "Earlier TCP-only history, unmatched sockets, link overhead, VPNs and containers can contribute to this gap."
      : "TCP-only tracking misses UDP/QUIC, some short connections and privileged services. Enable continuous packet tracking to improve future attribution.";
    set("unattributed", `${bytes(difference)} of connection traffic was not attributed to a program. ${explanation}`);
  }

  function renderAppHistory() {
    const panel = $("app-history-panel");
    if (!state.selectedApp || !state.data) { if (panel.open) panel.close(); return; }
    const appName = state.selectedApp;
    const selected = state.buckets.find((bucket) => bucket.ts === state.selected);
    const periodApps = Array.isArray(state.data.apps) ? state.data.apps : [];
    const periodApp = mergeApps(periodApps).find((app) => app.name === appName);
    const app = (selected ? selected.apps.find((item) => item.name === appName) : periodApp) || { rx: 0, tx: 0 };
    const connection = selected || state.data.totals || {};
    const connectionTotal = number(connection.rx) + number(connection.tx);
    const lastSeen = periodApps.filter((item) => item.name === appName).reduce((latest, item) => Math.max(latest, number(item.last_seen)), 0);
    const rate = periodApp ? periodApp.rate_rx + periodApp.rate_tx : 0;
    set("history-app-name", appName);
    set("history-description", selected ? `Usage during ${bucketRangeLabel(selected)}.` : `Usage ${labels[state.range]}.`);
    set("history-download", bytes(app.rx));
    set("history-upload", bytes(app.tx));
    set("history-total", bytes(number(app.rx) + number(app.tx)));
    set("history-current", `${bytes(rate)}/s`);
    set("history-status", rate > 0 ? "Transferring" : "No current traffic");
    set("history-share", connectionTotal ? `${((number(app.rx) + number(app.tx)) / connectionTotal * 100).toFixed(1)}%` : "0%");
    set("history-last-seen", lastSeen ? new Date(lastSeen * 1000).toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }) : "Not recorded");
    set("history-chart-scope", `Activity ${labels[state.range]}. Select a bar to inspect that interval.`);
    const rows = state.buckets.map((bucket) => {
      const app = bucket.apps.find((item) => item.name === appName) || { rx: 0, tx: 0 };
      return { ...bucket, rx: number(app.rx), tx: number(app.tx) };
    });
    const rx = rows.reduce((sum, bucket) => sum + bucket.rx, 0);
    const tx = rows.reduce((sum, bucket) => sum + bucket.tx, 0);
    const max = Math.max(1, ...rows.map((bucket) => bucket.rx + bucket.tx));
    const chart = $("app-history-chart");
    const fragment = document.createDocumentFragment();
    for (const bucket of rows) {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "history-bucket";
      button.dataset.ts = String(bucket.ts);
      button.setAttribute("aria-pressed", String(state.selected === bucket.ts));
      button.title = `${bucketRangeLabel(bucket)}: ${bytes(bucket.rx)} down, ${bytes(bucket.tx)} up`;
      button.setAttribute("aria-label", button.title);
      const down = document.createElement("span");
      down.className = "history-bar-download";
      down.style.height = `${bucket.rx / max * 100}%`;
      const up = document.createElement("span");
      up.className = "history-bar-upload";
      up.style.height = `${bucket.tx / max * 100}%`;
      button.append(down, up);
      button.addEventListener("click", () => {
        state.selected = state.selected === bucket.ts ? null : bucket.ts;
        state.appsExpanded = false;
        renderTimeline();
        renderOverview();
        renderApps();
        renderAppHistory();
      });
      fragment.append(button);
    }
    const focusedTimestamp = chart.contains(document.activeElement) ? document.activeElement.dataset.ts : null;
    chart.replaceChildren(fragment);
    if (focusedTimestamp) {
      const button = [...chart.children].find((item) => item.dataset.ts === focusedTimestamp);
      if (button) button.focus({ preventScroll: true });
    }
    const hasTraffic = rx + tx > 0;
    $("history-empty").hidden = hasTraffic;
    chart.hidden = !hasTraffic;
    set("history-axis-start", rows.length ? bucketAxisLabel(rows[0].ts) : "");
    set("history-axis-mid", rows.length > 2 ? bucketAxisLabel(rows[Math.floor(rows.length / 2)].ts) : "");
    set("history-axis-end", rows.length ? bucketAxisLabel(rows[rows.length - 1].ts) : "");
  }

  function render(data) {
    state.data = data;
    const live = data.live || {};
    const totalRate = number(live.rx) + number(live.tx);
    set("live", `${bytes(totalRate)}/s`);
    set("live-detail", `Down ${bytes(live.rx)}/s  /  Up ${bytes(live.tx)}/s`);
    set("signal-rate", `${bytes(totalRate)}/s`);
    set("signal-directions", `↓ ${bytes(live.rx)}/s  ·  ↑ ${bytes(live.tx)}/s`);
    state.signalHistory.push(totalRate);
    if (state.signalHistory.length > 12) state.signalHistory.shift();
    const signalState = $("signal-state");
    signalState.textContent = totalRate > 0 ? "TRANSFERRING" : data.interface ? "IDLE" : "OFFLINE";
    signalState.dataset.active = String(totalRate > 0);
    const peakRate = Math.max(1024, ...state.signalHistory);
    const bars = [...$("signal-bars").children];
    bars.forEach((bar, index) => {
      const offset = index - (bars.length - state.signalHistory.length);
      const value = offset >= 0 ? state.signalHistory[offset] : 0;
      bar.style.setProperty("--level", `${Math.max(2, 2 + value / peakRate * 24)}px`);
    });
    const options = [{ name: "auto", label: "Automatic" }, ...(data.interfaces || []).filter((item) => item.name !== "lo").map((item) => ({ name: item.name, label: `${item.name}${item.active ? " (connected)" : ""}` }))];
    const optionsGroup = $("interface-options");
    const selection = data.interface_selection || data.selected_interface || (optionsGroup.dataset.saved || "auto");
    const focused = optionsGroup.contains(document.activeElement);
    if (!focused) {
      optionsGroup.replaceChildren(...options.map((item) => {
        const label = document.createElement("label");
        label.className = "interface-choice";
        const radio = document.createElement("input");
        radio.type = "radio";
        radio.name = "interface";
        radio.value = item.name;
        radio.checked = item.name === selection;
        const text = document.createElement("span");
        text.textContent = item.label;
        label.append(radio, text);
        return label;
      }));
    }
    set("connection-note", data.interface ? `Recording ${data.interface} locally` : data.history_interface ? `Offline · Showing saved history for ${data.history_interface}` : "Offline · Waiting for a connection; history is saved locally");
    const packetTracking = data.mode === "packet-capture" && data.capture && data.capture.active;
    const warning = packetTracking
      ? `Program monitoring: continuous TCP + UDP packet tracking. Short connections are captured; some sockets may still close before their program can be identified.${data.packet_tracking_since ? ` Enabled ${date(data.packet_tracking_since, true)}.` : ""}`
      : "Limited program monitoring: TCP estimates only. UDP/QUIC, connections shorter than the 2-second scan and some privileged services may be missing. Enable continuous packet tracking below.";
    $("mode-notice").classList.toggle("success", Boolean(packetTracking));
    $("tracking-help").hidden = Boolean(packetTracking);
    notice("mode-notice", data.warning ? `${warning} ${data.warning}` : warning);
    const alerts = data.alerts || {};
    if (document.activeElement !== $("alert-limit") && document.activeElement !== $("alert-enabled")) {
      const threshold = number(alerts.threshold_bytes);
      $("alert-enabled").checked = threshold > 0;
      if (threshold > 0) $("alert-limit").value = (threshold / 1073741824).toFixed(1);
    }
    set("notification-badge", alerts.notifications_available ? "SYSTEM NOTIFICATIONS READY" : "NOTIFICATIONS UNAVAILABLE");
    $("notification-badge").classList.toggle("unavailable", !alerts.notifications_available);
    if (!alerts.notifications_available) set("alert-message", "Install libnotify-bin, then restart Data Watch: sudo apt install libnotify-bin && systemctl --user restart data-watch.service");
    else if (number(alerts.threshold_bytes) > 0) set("alert-message", `Daily alert is on at ${bytes(alerts.threshold_bytes)} for the selected connection. It plays a system sound once per local day.`);
    else set("alert-message", "Set a daily limit to receive a desktop notification with a system sound.");
    notice("collector-error", data.collector_error ? `Program monitoring needs attention: ${data.collector_error}` : "");
    set("updated", `Updated ${date(data.now || Date.now() / 1000)}`);
    set("database-size", bytes(data.database_size_bytes));
    document.querySelector(".status-dot").classList.add("connected");
    renderTimeline();
    renderOverview();
    renderApps();
    renderAppHistory();
  }

  async function refresh() {
    if (state.busy) { state.queued = true; return; }
    if (document.hidden) return;
    clearTimeout(state.timer);
    state.busy = true;
    const range = state.range;
    try {
      const response = await fetch(`/api/status?range=${encodeURIComponent(range)}`, { cache: "no-store", signal: AbortSignal.timeout(10000) });
      if (!response.ok) throw new Error(`Monitor returned HTTP ${response.status}`);
      const data = await response.json();
      if (range === state.range) render(data);
      notice("network-error", "");
    } catch (error) {
      notice("network-error", `Cannot reach the local monitor. ${error.message}. Check that Data Watch is still running. Displayed readings may be out of date.`);
      document.querySelector(".status-dot").classList.remove("connected");
    } finally {
      state.busy = false;
      if (!document.hidden) { const delay = state.queued ? 0 : 3000; state.queued = false; state.timer = setTimeout(refresh, delay); }
    }
  }

  document.querySelectorAll("[data-range]").forEach((button) => button.addEventListener("click", () => {
    if (state.range === button.dataset.range) return;
    state.range = button.dataset.range;
    state.selected = null;
    state.selectedApp = null;
    state.appsExpanded = false;
    renderAppHistory();
    document.querySelectorAll("[data-range]").forEach((item) => item.setAttribute("aria-pressed", String(item === button)));
    $("export").href = `/api/export?range=${state.range}`;
    set("apps-description", "Loading selected period...");
    refresh();
  }));
  $("sort").addEventListener("change", () => { state.sort = $("sort").value; if (state.data) renderApps(); });
  $("clear-selection").addEventListener("click", () => { state.selected = null; state.appsExpanded = false; renderTimeline(); renderOverview(); renderApps(); renderAppHistory(); });
  $("return-current").addEventListener("click", () => {
    state.range = "today";
    state.selected = null;
    state.selectedApp = null;
    state.appsExpanded = false;
    renderAppHistory();
    document.querySelectorAll("[data-range]").forEach((item) => item.setAttribute("aria-pressed", String(item.dataset.range === "today")));
    $("export").href = "/api/export?range=today";
    set("apps-description", "Loading today's readings...");
    refresh();
  });
  $("show-more-programs").addEventListener("click", () => {
    state.appsExpanded = !state.appsExpanded;
    renderApps();
    if (state.appsExpanded) $("programs-scroll").focus({ preventScroll: true });
  });
  $("close-app-history").addEventListener("click", () => $("app-history-panel").close());
  $("app-history-panel").addEventListener("close", () => {
    const name = state.returnProgram;
    state.selectedApp = null;
    state.returnProgram = null;
    document.body.classList.remove("dialog-open");
    renderApps();
    (programButton(name) || ($("show-more-programs").hidden ? $("sort") : $("show-more-programs"))).focus({ preventScroll: true });
  });
  $("app-history-panel").addEventListener("click", (event) => {
    const dialog = $("app-history-panel");
    if (event.target !== dialog) return;
    const bounds = dialog.getBoundingClientRect();
    if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) dialog.close();
  });
  $("alert-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const enabled = $("alert-enabled").checked;
    const gib = Number($("alert-limit").value);
    if (enabled && (!Number.isFinite(gib) || gib < 0.1 || gib > 1024)) {
      set("alert-message", "Choose a daily limit between 0.1 and 1024 GiB.");
      return;
    }
    const threshold = enabled ? Math.round(gib * 1073741824) : 0;
    try {
      const response = await fetch("/api/alerts", { method: "POST", headers: { "Content-Type": "application/json", "X-Data-Watch": "1" }, body: JSON.stringify({ threshold_bytes: threshold }), signal: AbortSignal.timeout(10000) });
      const result = await response.json();
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      if (state.data) { state.data.alerts = { ...state.data.alerts, threshold_bytes: threshold }; render(state.data); }
      set("alert-message", enabled ? `Daily alert saved at ${bytes(threshold)} for the selected connection. It includes a system sound.` : "Daily alert disabled.");
    } catch (error) { set("alert-message", `Could not save the alert: ${error.message}`); }
  });
  $("test-alert").addEventListener("click", async () => {
    try {
      const response = await fetch("/api/alerts/test", { method: "POST", headers: { "X-Data-Watch": "1" }, signal: AbortSignal.timeout(10000) });
      const result = await response.json();
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      set("alert-message", "Test notification sent with a system alert sound. Check your desktop notification area.");
    } catch (error) { set("alert-message", `Could not send a notification: ${error.message}`); }
  });
  $("interface-options").addEventListener("change", async (event) => {
    if (!(event.target instanceof HTMLInputElement) || event.target.name !== "interface") return;
    const optionsGroup = $("interface-options");
    const radios = [...optionsGroup.querySelectorAll('input[name="interface"]')];
    const selected = event.target.value;
    radios.forEach((radio) => { radio.disabled = true; });
    try {
      const response = await fetch("/api/interface", { method: "POST", headers: { "Content-Type": "application/json", "X-Data-Watch": "1" }, body: JSON.stringify({ interface: selected }), signal: AbortSignal.timeout(10000) });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      optionsGroup.dataset.saved = selected;
      state.selected = null;
      state.selectedApp = null;
      state.appsExpanded = false;
      renderAppHistory();
      refresh();
    } catch (error) { notice("network-error", `Could not change the watched connection: ${error.message}. Try selecting it again.`); }
    finally { radios.forEach((radio) => { radio.disabled = false; }); }
  });
  document.addEventListener("visibilitychange", () => { clearTimeout(state.timer); if (!document.hidden) refresh(); });
  startMatrixRain();
  refresh();
})();
