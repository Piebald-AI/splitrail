// Exercise compiled extension code and its emitted webview script without a
// VS Code host. Only host/DOM plumbing is stubbed; aggregation and rendering
// execute unchanged so a JSON field mismatch cannot hide behind a helper test.
const assert = require("node:assert/strict");
const { readFileSync } = require("node:fs");
const path = require("node:path");
const { test } = require("node:test");
const vm = require("node:vm");

function loadExtensionModule(name) {
  const filename = path.join(__dirname, "../out", `${name}.js`);
  const module = { exports: {} };
  const wrapper = vm.runInThisContext(
    `(function(require, module, exports) {${readFileSync(filename, "utf8")}\n})`,
    { filename }
  );
  wrapper((id) => {
    if (id === "vscode") return { TreeItem: class {} };
    if (id === "./usageView") return loadExtensionModule("usageView");
    throw new Error(`Unexpected extension dependency: ${id}`);
  }, module, module.exports);
  return module.exports;
}

const { summarizeAnalyzer, summarizeAllAnalyzers } = loadExtensionModule("usageView");
const { SplitrailDashboardProvider } = loadExtensionModule("dashboardView");
const now = new Date();
const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;

function daily(date, costCents) {
  return {
    date, user_messages: 1, ai_messages: 2, conversations: 1,
    models: { "test-model": 2 },
    stats: { inputTokens: 86, outputTokens: 24845, ...(costCents === undefined ? {} : { costCents }) },
  };
}

function analyzer(name, days) {
  return {
    analyzer_name: name, num_conversations: days.length,
    daily_stats: Object.fromEntries(days.map(day => [day.date, day])),
  };
}

// The issue's 118-cent payload must display as $1.18, not $0 or $118.
const analyzers = [
  analyzer("Tool A", [daily(today, 118), daily("2000-01-01", 250)]),
  analyzer("Tool B", [daily(today, 82)]),
];

test("summaries convert CLI cents and preserve today/all-time and token totals", () => {
  const summary = summarizeAnalyzer(analyzers[0]);
  assert.equal(summary.totalTokens, 49862);
  // Dollar sums use binary floats; assert the precision shown by summary consumers.
  assert.equal(summary.totalCost.toFixed(4), "3.6800");
  assert.deepEqual(summarizeAllAnalyzers(analyzers), {
    totalTokens: 74793, totalCost: 4.5, todayTokens: 49862, todayCost: 2,
  });
});

test("summaries retain zero defaults for missing costs and empty data", () => {
  for (const cost of [0, undefined]) {
    const item = analyzer("Tool", [daily(today, cost)]);
    assert.equal(summarizeAnalyzer(item).totalCost, 0);
    assert.equal(summarizeAllAnalyzers([item]).todayCost, 0);
  }
  assert.deepEqual(summarizeAllAnalyzers([]), {
    totalTokens: 0, totalCost: 0, todayTokens: 0, todayCost: 0,
  });
});

function dashboard() {
  const elements = new Map();
  function element() {
    return {
      textContent: "", children: [],
      set innerHTML(value) { this.html = value; this.children = []; },
      get innerHTML() { return this.html; },
      appendChild(child) { this.children.push(child); },
      querySelector(selector) {
        if (!elements.has(selector)) elements.set(selector, element());
        return elements.get(selector);
      },
    };
  }
  const root = element();
  const listeners = new Map();
  const webview = { onDidReceiveMessage() {}, postMessage() {} };
  new SplitrailDashboardProvider({}).resolveWebviewView({ webview }, {}, {});
  const script = webview.html.match(/<script nonce="[^"]+">([\s\S]*?)<\/script>/)[1];
  const context = vm.createContext({
    document: { querySelector: () => root, createElement: element },
    window: { addEventListener: (name, callback) => listeners.set(name, callback) },
    acquireVsCodeApi: () => ({ postMessage() {} }),
  });
  vm.runInContext(script, context);
  return {
    elements,
    send(items) { listeners.get("message")({ data: { type: "stats", payload: { analyzers: items } } }); },
    scope(value) { vm.runInContext(`onScopeChange({ target: { value: ${JSON.stringify(value)} } })`, context); },
  };
}

test("dashboard renders dollar costs in hero, tools and models across scopes", () => {
  const view = dashboard();
  view.send(analyzers);
  assert.equal(view.elements.get(".hero-cost").textContent, "$2.00");
  const tools = view.elements.get(".by-tool-body").children;
  assert.match(tools[0].innerHTML, /Tool A.*\$1\.18.*59%/);
  assert.match(tools[1].innerHTML, /Tool B.*\$0\.82.*41%/);
  assert.match(view.elements.get(".by-model-body").children[0].innerHTML, /test-model.*\$2\.00.*100%/);
  view.scope("all");
  assert.equal(view.elements.get(".hero-cost").textContent, "$4.50");
  assert.match(view.elements.get(".by-tool-body").children[0].innerHTML, /Tool A.*\$3\.68/);
  assert.match(view.elements.get(".by-model-body").children[0].innerHTML, /\$4\.50/);
});

test("dashboard keeps proportional model allocation and handles zero/missing costs", () => {
  const view = dashboard();
  const day = daily(today, 118);
  day.models = { "Model A": 3, "Model B": 1 };
  view.send([analyzer("Tool", [day])]);
  const models = view.elements.get(".by-model-body").children;
  assert.match(models[0].innerHTML, /Model A.*\$0\.89.*75%/);
  assert.match(models[1].innerHTML, /Model B.*\$0\.29.*25%/);
  for (const cost of [0, undefined]) {
    view.send([analyzer("Tool", [daily(today, cost)])]);
    assert.equal(view.elements.get(".hero-cost").textContent, "$0.00");
    assert.match(view.elements.get(".by-tool-body").children[0].innerHTML, /\$0\.00/);
    assert.match(view.elements.get(".by-model-body").children[0].innerHTML, /\$0\.00/);
  }
  view.send([]);
  assert.equal(view.elements.get(".hero-cost").textContent, "$0.00");
});
