// Controller checks with a minimal DOM facade. This does not verify browser layout.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
let document;
class Element {
  constructor(tag) { this.tagName = tag; this.children = []; this.dataset = {}; this.attributes = {}; this.listeners = {}; this.className = ''; this.style = {setProperty() {}}; this.scrollTop = 0; this.scrollLeft = 0; this.hidden = false; this.open = false; this.textContent = ''; }
  get classList() { const e = this; return {contains(c) {return e.className.split(' ').includes(c);}, add(c) {if (!this.contains(c)) e.className += ' ' + c;}, remove(c) {e.className = e.className.split(' ').filter(v => v !== c).join(' ');}, toggle(c, on) {if (on ?? !this.contains(c)) this.add(c); else this.remove(c);}}; }
  setAttribute(k, v) { this.attributes[k] = String(v); }
  getAttribute(k) { return this.attributes[k]; }
  addEventListener(k, f) { (this.listeners[k] ||= []).push(f); }
  emit(k, extra = {}) { const event = {target: this, clientX: 20, clientY: 20, ...extra}; for (let e = this; e; e = k === 'click' ? e.parent : null) for (const f of e.listeners[k] || []) f(event); }
  click() { this.emit('click'); }
  focus() { document.activeElement = this; }
  contains(e) { return this === e || this.children.some(c => c.contains(e)); }
  append(...items) { for (const e of items) { if (e.tagName === 'fragment') { this.append(...e.children); continue; } this.children.push(e); e.parent = this; } }
  replaceChildren(...items) { if (this.children.some(c => c.contains(document.activeElement))) document.activeElement = document.body; this.children.forEach(c => c.parent = null); this.children = []; this.append(...items); }
  matches(selector) { return selector.startsWith('.') ? this.classList.contains(selector.slice(1)) : selector === '[data-range]' ? Boolean(this.dataset.range) : selector === '[data-view]' ? Boolean(this.dataset.view) : selector === '[data-memory-range]' ? Boolean(this.dataset.memoryRange) : selector === '[data-memory-program]' ? Boolean(this.dataset.memoryProgram) : this.tagName === selector; }
  querySelectorAll(selector) { const found = []; const visit = e => { if (selector.split(',').some(part => e.matches(part.trim()))) found.push(e); e.children.forEach(visit); }; this.children.forEach(visit); return found; }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  closest(selector) { for(let e=this;e;e=e.parent) if(e.matches(selector)) return e; return null; }
  getBoundingClientRect() { return {left: 10, top: 10, right: 700, bottom: 700}; }
  showModal() { this.open = true; document.getElementById(this.id === 'memory-program-dialog' ? 'memory-program-close' : 'close-app-history').focus(); }
  close() { this.open = false; this.emit('close'); }
}
const ids = new Map();
document = {body: new Element('body'), activeElement: null, hidden: false, getElementById: id => ids.get(id), createElement: tag => new Element(tag), createDocumentFragment: () => new Element('fragment'), querySelectorAll(selector) {return this.body.querySelectorAll(selector);}, querySelector(selector) {return this.body.querySelector(selector);}, addEventListener() {}};
document.activeElement = document.body;
const html = fs.readFileSync('static/index.html', 'utf8');
for (const match of html.matchAll(/<([a-z][a-z0-9]*)\b[^>]*\bid="([^"]+)"[^>]*>/g)) { const e = new Element(match[1]); e.id = match[2]; ids.set(e.id, e); document.body.append(e); }
for (const range of ['today', 'hour', 'week', 'weeks', 'months', 'years']) {const e = new Element('button'); e.dataset.range = range; document.body.append(e);}
for (const view of ['network', 'memory']) ids.get(view + '-tab').dataset.view = view;
for (const range of ['hour','today','week']) {const e=new Element('button'); e.dataset.memoryRange=range; document.body.append(e);}
const statusDot = new Element('i'); statusDot.className = 'status-dot'; document.body.append(statusDot);
const window = {addEventListener() {}, setInterval() {}, matchMedia() {return {matches: true};}};
const fetches = [];
const context = {AbortSignal, history: {replaceState() {}}, location: {hash: '', pathname: '/'}, document, window, HTMLInputElement: Element, setTimeout() {}, clearTimeout() {}, fetch(url) {fetches.push(url); return Promise.reject(new Error('Test monitor is offline'));}};
const original = fs.readFileSync('static/app.js', 'utf8');
const tail = '  startMatrixRain();\n  activateView(location.hash === \"#memory\" ? \"memory\" : \"network\");';
assert(original.includes(tail));
vm.runInNewContext(original.replace(tail, '  window.ui = {state, renderApps, renderAppHistory, renderTimeline, renderOverview, memoryState, renderMemory, activateView};'), context);
const ui = window.ui;
const apps = Array.from({length: 15}, (_, i) => ({name: 'program-' + i, rx: 300 - i * 10, tx: 100, rate_rx: 8, rate_tx: 2, last_seen: 1792161000}));
const series = [{ts: 1792160100, end: 1792161000, rx: 200, tx: 100, apps: [{name: 'program-0', rx: 100, tx: 25}]}, {ts: 1792161000, end: 1792161900, rx: 800, tx: 200, apps: [{name: 'program-0', rx: 200, tx: 75}]}];
ui.state.data = {apps, series, totals: {rx: 10000, tx: 5000}, unattributed: {}, mode: 'tcp-estimate'};
ui.state.buckets = series;
const node = id => ids.get(id);
ui.renderApps();
assert.equal(node('apps').children.length, 10);
assert.equal(node('show-more-programs').hidden, false);
node('apps').children[0].children[2].click();
assert.equal(node('app-history-panel').open, true);
assert.equal(node('history-app-name').textContent, 'program-0');
assert.equal(node('history-total').textContent, '400 B');
assert.equal(node('history-current').textContent, '10 B/s');
assert.equal(node('history-share').textContent, '2.7%');
assert.equal(document.activeElement.id, 'close-app-history');
node('app-history-chart').children[0].click();
assert.equal(node('history-total').textContent, '125 B');
assert.equal(node('history-share').textContent, '41.7%');
node('close-app-history').click();
assert.equal(node('app-history-panel').open, false);
assert.equal(document.body.classList.contains('dialog-open'), false);
assert.equal(document.activeElement.dataset.program, 'program-0');
ui.state.selected = null;
ui.renderApps();
node('show-more-programs').click();
assert.equal(node('apps').children.length, 15);
assert.equal(node('programs-scroll').classList.contains('is-expanded'), true);
node('programs-scroll').scrollTop = 120;
node('programs-scroll').scrollLeft = 30;
ui.renderApps();
assert.equal(node('programs-scroll').scrollTop, 120);
assert.equal(node('programs-scroll').scrollLeft, 30);
assert.equal(node('apps').children.length, 15);
node('apps').children[14].children[3].click();
assert.equal(node('history-app-name').textContent, 'program-14');
node('app-history-panel').emit('click', {clientX: 0, clientY: 0});
assert.equal(node('app-history-panel').open, false);
assert.equal(document.activeElement.dataset.program, 'program-14');
assert.equal(node('programs-scroll').scrollTop, 120);
node('show-more-programs').click();
assert.equal(node('apps').children.length, 10);
assert.equal(node('programs-scroll').scrollTop, 0);
ui.state.data.apps = apps.slice(0, 3);
ui.renderApps();
assert.equal(node('apps').children.length, 3);
assert.equal(node('show-more-programs').hidden, true);
console.log('PROGRAM_FLOW_CHECK_PASS: full-row click, scoped details, local-only interactions, close and focus return, top ten, expansion, refresh scroll preservation, collapse, small lists (DOM simulation; native browser behavior not verified).');

const GiB = 1024 ** 3;
const now = Date.now() / 1000;
const memoryPrograms = Array.from({length: 14}, (_, index) => ({name: `memory-program-${index}`, rss: 1000-index, swap: index, processes: 2}));
const memory = {
 live: {ts: now, total: 16*GiB, used: 6*GiB, available: 10*GiB, cache: 8*GiB, swap_total: 2*GiB, swap_used: 0, psi_some: 0, psi_full: 0, programs: memoryPrograms, oom_total: 0, oom_kills: 0, paging_available: true, swap_in_rate: 0, swap_out_rate: 0},
 series: [{ts: Math.floor(now/300)*300, end: Math.floor(now/300)*300+300, total: 16*GiB, used_peak: 13*GiB, available_min: 3*GiB, swap_total: 2*GiB, swap_peak: GiB, psi_some_peak: 12, psi_full_peak: 2, samples: 4, oom_kills: 1, swap_in: 1000, swap_out: 2000, apps: [{name: 'historical-editor', rss: 2*GiB, swap: 100, processes: 4}]}],
 database_size_bytes: 16384, error: ''
};
ui.renderMemory(memory);
assert.equal(node('memory-used').textContent, '6 GiB');
assert.equal(node('memory-available').textContent, '10 GiB');
assert.equal(node('memory-health').dataset.level, 'normal');
assert.equal(node('memory-programs').children.length, 10);
node('memory-show-more').click();
assert.equal(node('memory-programs').children.length, 14);
assert.equal(node('memory-programs-scroll').classList.contains('is-expanded'), true);
node('memory-programs-scroll').scrollTop = 120;
ui.renderMemory(memory);
assert.equal(node('memory-programs-scroll').scrollTop, 120);
node('memory-programs').children[0].children[2].click();
assert.equal(node('memory-program-dialog').open, true);
assert.equal(node('memory-program-title').textContent, 'memory-program-0');
node('memory-program-close').click();
assert.equal(document.activeElement.closest('[data-memory-program]').dataset.memoryProgram, 'memory-program-0');
node('memory-timeline').children[0].click();
assert.equal(node('memory-used').textContent, '13 GiB');
assert.equal(node('memory-available').textContent, '3 GiB');
assert.equal(node('memory-health').dataset.level, 'critical');
assert.equal(node('memory-live').hidden, false);
assert.equal(node('memory-programs').children.length, 1);
assert.equal(node('memory-programs').children[0].children[0].children[0].textContent, 'historical-editor');
node('memory-programs').children[0].children[3].click();
assert.equal(node('memory-program-title').textContent, 'historical-editor');
assert.equal(node('memory-program-rss').textContent, '2 GiB');
node('memory-program-close').click();
assert.equal(fetches.length, 0, 'History and program dialogs must reuse loaded data');
ui.memoryState.selected = null;
memory.live.psi_some = null; memory.live.psi_full = null;
ui.renderMemory(memory);
assert.equal(node('memory-pressure').textContent, 'Unavailable');
memory.live.ts = now - 60;
ui.renderMemory(memory);
assert.equal(node('memory-error').hidden, false);
assert.equal(node('memory-health').textContent, 'SAMPLE IS STALE');
node('memory-tab').click();
assert.equal(node('memory-view').hidden, false);
assert.equal(node('network-view').hidden, true);
assert.equal(node('memory-tab').getAttribute('aria-selected'), 'true');
assert.equal(fetches[0], '/api/memory?range=hour');
node('memory-tab').emit('keydown', {key:'Home', preventDefault() {}});
assert.equal(node('network-tab').getAttribute('aria-selected'), 'true');
assert.equal(document.activeElement, node('network-tab'));
console.log('MEMORY_UI_CONTROLLER_CHECK_PASS: live vs historical metrics, pressure, missing data, stale readings, whole-row dialogs, focus return, top ten, scroll preservation, tab keyboard navigation (DOM simulation; browser layout not verified).');
