// Copyright 2026 Chris Olson
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// The console example dashboard: one ES module that reads the console's read-only JSON API
// from another origin and renders it with DOM APIs only (no HTML strings).

// The API routes this page calls. The console's route guard test reads these literals.
const ROUTES = Object.freeze({
  breakdown: "/api/v1/usage/breakdown",
  trend: "/api/v1/usage/trend",
  topTraces: "/api/v1/usage/top-traces",
  sessions: "/api/v1/sessions",
  turns: "/api/v1/sessions/{id}/turns",
  users: "/api/v1/users",
  review: "/api/v1/review/pending",
});

const DEFAULT_CONSOLE = "http://127.0.0.1:8765";
const RANGE_SECONDS = Object.freeze({
  "1h": 3600,
  "6h": 21600,
  "24h": 86400,
  "7d": 604800,
  "30d": 2592000,
});
const TRUNCATED = "Results are cut at the console's caps.";
const NEEDS_BIGQUERY = "The console needs --traces bigquery://… for the JSON API.";
const SVG_NS = "http://www.w3.org/2000/svg";

// The page state: the console base URL, the picked range, whether the review queue was
// loaded once, and the sessions' next page.
const state = { console: DEFAULT_CONSOLE, range: "24h", reviewLoaded: false, next: null };

// An API error answer: the HTTP status and the body's `error` fields.
class ApiError extends Error {
  constructor(status, kind, message, hint) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.kind = kind;
    this.hint = hint;
  }
}

// A `fetch` rejection: the console is down, or the browser blocked the answer (CORS).
class NetworkError extends Error {
  constructor(url, cause) {
    super(`fetch ${url}: ${cause}`);
    this.name = "NetworkError";
  }
}

// `template` with each `{key}` replaced by `encodeURIComponent(params[key])`.
function path(template, params = {}) {
  return template.replace(/\{(\w+)\}/g, (_, key) => encodeURIComponent(params[key]));
}

// The absolute URL of `route` on the console, with the query `params`, each value encoded.
function apiUrl(route, params = {}) {
  const query = Object.entries(params)
    .map(([key, value]) => `${key}=${encodeURIComponent(value)}`)
    .join("&");
  return query === "" ? state.console + route : `${state.console}${route}?${query}`;
}

// GETs `url` and returns the envelope `{data, truncated, next}`, or throws an `ApiError` or a
// `NetworkError`.
async function api(url) {
  let resp;
  try {
    resp = await fetch(url, { method: "GET" });
  } catch (err) {
    throw new NetworkError(url, err);
  }
  let body = null;
  try {
    body = await resp.json();
  } catch {
    body = null;
  }
  if (!resp.ok) {
    const error = body && body.error;
    if (error && typeof error.kind === "string") {
      throw new ApiError(resp.status, error.kind, String(error.message), error.hint ?? null);
    }
    throw new ApiError(resp.status, "http", `HTTP ${resp.status} from ${url}`, null);
  }
  if (body === null || typeof body !== "object" || !("data" in body)) {
    throw new ApiError(resp.status, "invalid", `no JSON envelope from ${url}`, null);
  }
  return { data: body.data, truncated: body.truncated === true, next: body.next ?? null };
}

// A new element: `attrs` become attributes, or listeners when named `on…`; string and number
// children become text nodes; `null` children are skipped.
function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [name, value] of Object.entries(attrs)) {
    if (name.startsWith("on")) {
      node.addEventListener(name.slice(2), value);
    } else {
      node.setAttribute(name, value);
    }
  }
  for (const child of children) {
    if (child === null || child === undefined) {
      continue;
    }
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

// A table row of cells (each a string, a number or a node).
function tr(cells, attrs = {}) {
  return el("tr", attrs, ...cells.map((cell) => el("td", {}, cell)));
}

// A table: `rows` are cell arrays or `tr` elements.
function table(headers, rows) {
  return el(
    "table",
    {},
    el("thead", {}, el("tr", {}, ...headers.map((h) => el("th", { scope: "col" }, h)))),
    el("tbody", {}, ...rows.map((row) => (Array.isArray(row) ? tr(row) : row))),
  );
}

// The lines that explain `err` (R11).
function errorLines(err) {
  if (err instanceof NetworkError) {
    return [`Cannot reach ${state.console}: is it running with --api-origin ${location.origin}?`];
  }
  if (err instanceof ApiError) {
    if (err.status === 404 && err.kind === "needs_bigquery") {
      return [NEEDS_BIGQUERY];
    }
    return err.hint === null ? [err.message] : [err.message, err.hint];
  }
  return [String(err)];
}

// Appends an error banner for `err` to `panel`.
function banner(panel, err) {
  const [message, hint] = errorLines(err);
  panel.append(
    el(
      "div",
      { class: "banner", role: "alert" },
      el("p", {}, message),
      hint === undefined ? null : el("p", { class: "hint" }, hint),
    ),
  );
}

// Appends a note to `panel`.
function note(panel, text) {
  panel.append(el("p", { class: "note" }, text));
}

// The R3 cost of `rows`: `$` and six decimals, `*` when partial, `—` when nothing is priced.
function money(rows) {
  const costs = rows.map((row) => row.cost_usd ?? null);
  const priced = costs.filter((cost) => cost !== null);
  if (priced.length === 0) {
    return "—";
  }
  const partial = rows.some((row) => row.cost_partial === true) || priced.length < costs.length;
  const total = priced.reduce((sum, cost) => sum + cost, 0);
  return `$${total.toFixed(6)}${partial ? "*" : ""}`;
}

// The console's token total of one usage: input + output + reasoning (cached is part of input).
function tokens(usage) {
  return usage.input + usage.output + usage.reasoning;
}

// The token total of a `[[model, usage]]` list.
function sumTokens(list) {
  return list.reduce((sum, [, usage]) => sum + tokens(usage), 0);
}

const COUNT = new Intl.NumberFormat("en-US");

// A count with thousands separators.
function num(n) {
  return COUNT.format(n);
}

// An RFC 3339 timestamp as ISO 8601 UTC to the second, or `—` for `null`.
function when(ts) {
  return ts === null || ts === undefined ? "—" : new Date(ts).toISOString().replace(/\.\d{3}Z$/, "Z");
}

// The range the sessions, turns, users and review routes accept: `30d` becomes `7d` (R2a).
function r7(range) {
  return range === "30d" ? "7d" : range;
}

// A link to the console's trace page, or `—` without a trace id.
function traceLink(id) {
  if (id === null || id === undefined) {
    return "—";
  }
  const href = `${state.console}/traces/${encodeURIComponent(id)}`;
  return el("a", { href, target: "_blank", rel: "noopener" }, id);
}

// The panel body with this id.
function panel(id) {
  return document.getElementById(id);
}

// The latest load of each panel body, so that a slow answer of an older load is dropped.
const loads = new WeakMap();

// Starts a load of `node` (clearing it unless `keep`) and returns a check that stays true until
// a newer load of `node` starts.
function begin(node, keep = false) {
  const token = Symbol("load");
  loads.set(node, token);
  if (!keep) {
    node.replaceChildren(el("p", { class: "loading" }, "Loading…"));
  }
  return () => loads.get(node) === token;
}

// Loads `url` into `node`: `render(res, node)` on success, then the truncation note; a banner
// on failure.
async function load(node, url, render) {
  const live = begin(node);
  try {
    const res = await api(url);
    if (!live()) {
      return;
    }
    node.replaceChildren();
    render(res, node);
    if (res.truncated) {
      note(node, TRUNCATED);
    }
  } catch (err) {
    if (!live()) {
      return;
    }
    node.replaceChildren();
    banner(node, err);
  }
}

// R3: the totals of the usage breakdown.
async function loadUsage() {
  await load(panel("usage-body"), apiUrl(ROUTES.breakdown, { range: state.range }), (res, node) => {
    const cells = res.data.cells;
    const sum = { calls: 0, input: 0, cached: 0, output: 0, reasoning: 0 };
    for (const cell of cells) {
      sum.calls += cell.calls.calls;
      for (const key of ["input", "cached", "output", "reasoning"]) {
        sum[key] += cell.calls.usage[key];
      }
    }
    node.append(
      table(
        ["Calls", "Input", "Cached", "Output", "Reasoning", "Cost"],
        [
          [
            num(sum.calls),
            num(sum.input),
            num(sum.cached),
            num(sum.output),
            num(sum.reasoning),
            money(cells),
          ],
        ],
      ),
    );
  });
}

// An SVG element with these attributes.
function svg(tag, attrs = {}) {
  const node = document.createElementNS(SVG_NS, tag);
  for (const [name, value] of Object.entries(attrs)) {
    node.setAttribute(name, String(value));
  }
  return node;
}

// R4: the trend bar chart of `data` for `range`: cost per bucket, or calls when nothing is
// priced.
function trendChart(data, range) {
  const width = 720;
  const height = 160;
  const buckets = new Map();
  let priced = false;
  for (const c of data.cells) {
    const b = buckets.get(c.bucket) ?? { cost: 0, calls: 0 };
    b.cost += c.cost_usd ?? 0;
    b.calls += c.cell.calls.calls;
    priced = priced || (c.cost_usd !== null && c.cost_usd !== undefined);
    buckets.set(c.bucket, b);
  }
  const last = data.cells.reduce((max, c) => Math.max(max, c.bucket), -1);
  const count = Math.max(Math.ceil(RANGE_SECONDS[range] / data.bucket_seconds), last + 1);
  const values = [];
  for (let i = 0; i < count; i += 1) {
    const b = buckets.get(i);
    values.push(b === undefined ? 0 : priced ? b.cost : b.calls);
  }
  const max = values.reduce((m, v) => Math.max(m, v), 0);
  const start = Date.parse(data.start);
  const bar = width / count;
  const chart = svg("svg", {
    viewBox: `0 0 ${width} ${height}`,
    preserveAspectRatio: "none",
    role: "img",
    "aria-label": priced ? "Cost per bucket" : "LLM calls per bucket",
  });
  values.forEach((value, i) => {
    const h = max > 0 ? (value / max) * height : 0;
    const rect = svg("rect", { class: "bar", x: i * bar, y: height - h, width: bar, height: h });
    const title = svg("title");
    const at = new Date(start + i * data.bucket_seconds * 1000).toISOString();
    title.textContent = `${at}: ${priced ? `$${value.toFixed(6)}` : `${num(value)} calls`}`;
    rect.append(title);
    chart.append(rect);
  });
  const caption = priced
    ? `Cost (USD) per ${data.bucket_seconds} s bucket`
    : `LLM calls per ${data.bucket_seconds} s bucket (no prices)`;
  return el("figure", {}, chart, el("figcaption", {}, caption));
}

// R4: the trend chart.
async function loadTrend() {
  const range = state.range;
  await load(panel("trend-body"), apiUrl(ROUTES.trend, { range }), (res, node) => {
    node.append(trendChart(res.data, range));
  });
}

// R5: the top traces by tokens.
async function loadTopTraces() {
  await load(panel("top-traces-body"), apiUrl(ROUTES.topTraces, { range: state.range }), (res, node) => {
    if (res.data.length === 0) {
      note(node, "No traces in this range.");
      return;
    }
    const rows = res.data.map((t) => [
      traceLink(t.trace_id),
      num(t.total),
      num(t.models.reduce((sum, m) => sum + m.calls, 0)),
      money([t]),
    ]);
    node.append(table(["Trace", "Tokens", "LLM calls", "Cost"], rows));
  });
}

// R6: the sessions; with `append`, the next page is added below the rows already shown.
async function loadSessions(append = false) {
  panel("sessions-body").replaceChildren();
}

// Hides the turns detail and drops any load of it still running.
function clearTurns() {
  document.getElementById("turns").hidden = true;
  const node = panel("turns-body");
  begin(node, true);
  node.replaceChildren();
}

// R8: the users.
async function loadUsers() {
  panel("users-body").replaceChildren();
}

// R9: the review queue of the score in the score field.
async function loadReview() {
  state.reviewLoaded = true;
  panel("review-body").replaceChildren();
}

// Reads the header controls into `state`.
function readControls() {
  const value = document.getElementById("console-url").value.trim().replace(/\/+$/, "");
  state.console = value === "" ? DEFAULT_CONSOLE : value;
  state.range = document.getElementById("range").value;
}

// R2: reloads every loaded panel with the header's console URL and range.
async function reload() {
  readControls();
  document.getElementById("clamp-note").hidden = state.range !== "30d";
  clearTurns();
  const loaders = [loadUsage(), loadTrend(), loadTopTraces(), loadSessions(), loadUsers()];
  if (state.reviewLoaded) {
    loaders.push(loadReview());
  }
  await Promise.allSettled(loaders);
}

// Wires the controls and runs the first load.
function init() {
  const params = new URLSearchParams(location.search);
  document.getElementById("console-url").value = params.get("console") || DEFAULT_CONSOLE;
  document.getElementById("console-url").addEventListener("change", reload);
  document.getElementById("range").addEventListener("change", reload);
  document.getElementById("controls").addEventListener("submit", (event) => {
    event.preventDefault();
    reload();
  });
  document.getElementById("review-form").addEventListener("submit", (event) => {
    event.preventDefault();
    loadReview();
  });
  document.getElementById("sessions-more").addEventListener("click", () => loadSessions(true));
  reload();
}

init();
