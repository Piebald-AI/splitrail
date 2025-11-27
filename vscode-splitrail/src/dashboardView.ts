import * as vscode from "vscode";
import { JsonAnalyzerStats, summarizeAllAnalyzers } from "./usageView";

export interface DashboardData {
  analyzers: JsonAnalyzerStats[];
}

export class SplitrailDashboardProvider implements vscode.WebviewViewProvider {
  public static readonly viewType = "splitrailDashboard";

  private view?: vscode.WebviewView;
  private latestData: DashboardData | null = null;

  constructor(private readonly context: vscode.ExtensionContext) {}

  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken
  ): void | Thenable<void> {
    this.view = webviewView;
    const webview = webviewView.webview;

    webview.options = {
      enableScripts: true,
    };

    webview.html = this.getHtml(webview);

    webview.onDidReceiveMessage((message) => {
      if (!message || typeof message !== "object") {
        return;
      }

      switch (message.type) {
        case "requestRefresh":
          vscode.commands.executeCommand("splitrail.refresh");
          break;
        case "upload":
          vscode.commands.executeCommand("splitrail.upload");
          break;
        case "openCloud":
          vscode.commands.executeCommand("splitrail.openCloud");
          break;
        case "openSettings":
          vscode.commands.executeCommand(
            "workbench.action.openSettings",
            "@ext:splitrail-vscode splitrail"
          );
          break;
        case "openTui":
          vscode.commands.executeCommand("splitrail.openDashboard");
          break;
        default:
          break;
      }
    });

    // Send any cached data once the webview is ready.
    if (this.latestData) {
      this.postStats(this.latestData);
    }
  }

  updateStats(analyzerStats: JsonAnalyzerStats[]): void {
    this.latestData = { analyzers: analyzerStats };
    this.postStats(this.latestData);
  }

  private postStats(data: DashboardData): void {
    if (!this.view) {
      return;
    }

    this.view.webview.postMessage({
      type: "stats",
      payload: data,
    });
  }

  private getHtml(webview: vscode.Webview): string {
    const nonce = getNonce();

    const styles = /* html */ `
      <style>
        :root {
          color-scheme: var(--vscode-color-scheme, dark light);
          --foreground: var(--vscode-foreground);
          --background: var(--vscode-editor-background);
          --muted: var(--vscode-descriptionForeground);
          --accent: var(--vscode-editorHoverWidget-border, #569cd6);
        }

        body {
          margin: 0;
          padding: 0;
          font-family: var(--vscode-font-family);
          color: var(--foreground);
          background-color: var(--background);
        }

        .root {
          display: flex;
          flex-direction: column;
          height: 100vh;
          box-sizing: border-box;
          padding: 8px;
          gap: 8px;
        }

        .header {
          display: flex;
          align-items: center;
          justify-content: space-between;
          font-size: 11px;
          text-transform: uppercase;
          letter-spacing: 0.08em;
          font-weight: 600;
        }

        .header-left {
          display: flex;
          align-items: center;
          gap: 6px;
        }

        .logo-dot {
          width: 10px;
          height: 10px;
          border-radius: 2px;
          background: var(--accent);
        }

        .scope-select {
          margin-left: 8px;
          font-size: 11px;
          padding: 2px 6px;
          border-radius: 4px;
          border: 1px solid var(--vscode-input-border, transparent);
          background: var(--vscode-input-background);
          color: inherit;
        }

        .header-actions {
          display: flex;
          align-items: center;
          gap: 6px;
        }

        .icon-button {
          width: 22px;
          height: 22px;
          border-radius: 4px;
          border: 1px solid transparent;
          display: flex;
          align-items: center;
          justify-content: center;
          cursor: pointer;
          background: transparent;
          color: inherit;
        }

        .icon-button:hover {
          border-color: var(--vscode-editorHoverWidget-border, #555);
          background: var(--vscode-list-hoverBackground, rgba(255,255,255,0.04));
        }

        .content {
          display: grid;
          grid-template-rows: auto auto 1fr;
          gap: 8px;
          min-height: 0;
        }

        .hero {
          border-radius: 6px;
          padding: 10px 12px;
          border: 1px solid var(--vscode-editorHoverWidget-border, #3c3c3c);
          display: flex;
          align-items: center;
          justify-content: space-between;
        }

        .hero-main {
          display: flex;
          flex-direction: column;
          gap: 2px;
        }

        .hero-cost {
          font-size: 20px;
          font-weight: 600;
        }

        .hero-sub {
          font-size: 11px;
          color: var(--muted);
        }

        .hero-meta {
          font-size: 11px;
          text-align: right;
          color: var(--muted);
        }

        .grid {
          display: grid;
          grid-template-columns: 1fr 1fr;
          gap: 8px;
        }

        .card {
          border-radius: 6px;
          padding: 8px 10px;
          border: 1px solid var(--vscode-editorHoverWidget-border, #3c3c3c);
          font-size: 11px;
        }

        .card-header {
          font-weight: 600;
          margin-bottom: 6px;
          text-transform: uppercase;
          letter-spacing: 0.06em;
        }

        .row {
          display: flex;
          justify-content: space-between;
          align-items: center;
          padding: 2px 0;
        }

        .row-label {
          color: var(--muted);
        }

        .row-value {
          font-variant-numeric: tabular-nums;
        }

        .badge {
          font-size: 10px;
          padding: 2px 4px;
          border-radius: 3px;
          border: 1px solid var(--vscode-editorHoverWidget-border, #3c3c3c);
          color: var(--muted);
        }

        .muted {
          color: var(--muted);
        }

        .footer {
          font-size: 10px;
          color: var(--muted);
          display: flex;
          justify-content: space-between;
          align-items: center;
          padding-top: 2px;
        }

        .footer-link {
          text-decoration: underline;
          cursor: pointer;
        }
      </style>
    `;

    const script = /* html */ `
      <script nonce="${nonce}">
        const vscode = window.vscode = acquireVsCodeApi();

        const state = {
          scope: "today",
          data: null,
        };

        function shortNumber(value) {
          if (value >= 1_000_000_000) return (value / 1_000_000_000).toFixed(1) + "b";
          if (value >= 1_000_000) return (value / 1_000_000).toFixed(1) + "m";
          if (value >= 1_000) return (value / 1_000).toFixed(1) + "k";
          return value.toString();
        }

        function onScopeChange(event) {
          state.scope = event.target.value;
          render();
        }

        function applyScope(analyzers) {
          if (!analyzers) return [];

          const now = new Date();
          const todayStr = toDateString(now);

          const start = (() => {
            const d = new Date(now);
            switch (state.scope) {
              case "week":
                d.setDate(d.getDate() - 6);
                return d;
              case "month":
                d.setMonth(d.getMonth() - 1);
                return d;
              case "all":
                return null;
              case "today":
              default:
                return todayStr;
            }
          })();

          function inRange(dateStr) {
            if (!start) return true;
            if (typeof start === "string") return dateStr === start;
            const d = new Date(dateStr + "T00:00:00");
            return d >= start;
          }

          return analyzers.map(a => {
            const filteredDaily = {};
            for (const [date, daily] of Object.entries(a.daily_stats || {})) {
              if (inRange(date)) {
                filteredDaily[date] = daily;
              }
            }
            return { ...a, daily_stats: filteredDaily };
          });
        }

        function toDateString(d) {
          const year = d.getFullYear();
          const month = String(d.getMonth() + 1).padStart(2, "0");
          const day = String(d.getDate()).padStart(2, "0");
          return \`\${year}-\${month}-\${day}\`;
        }

        function render() {
          const root = document.querySelector(".root");
          if (!root) return;

          const data = state.data;
          if (!data || !data.analyzers || data.analyzers.length === 0) {
            root.querySelector(".hero-cost").textContent = "$0.00";
            root.querySelector(".hero-sub").textContent = "No usage yet";
            root.querySelector(".hero-meta").textContent = "";
            root.querySelector(".by-tool-body").innerHTML =
              '<div class="muted">No data for this range.</div>';
            root.querySelector(".by-model-body").innerHTML =
              '<div class="muted">Model breakdown coming soon.</div>';
            root.querySelector(".details-body").innerHTML =
              '<div class="muted">No detailed metrics for this range.</div>';
            root.querySelector(".projects-body").innerHTML =
              '<div class="muted">Recent projects coming soon.</div>';
            return;
          }

          const scopedAnalyzers = applyScope(data.analyzers);
          const summary = (() => {
            // Recompute like summarizeAllAnalyzers but scoped
            let totalTokens = 0;
            let totalCost = 0;
            let messages = 0;
            let todayTokens = 0;
            let todayCost = 0;
            const today = toDateString(new Date());

            for (const analyzer of scopedAnalyzers) {
              for (const daily of Object.values(analyzer.daily_stats || {})) {
                const stats = daily.stats || {};
                const input = stats.inputTokens ?? 0;
                const output = stats.outputTokens ?? 0;
                const tokens = input + output;
                const cost = stats.cost ?? 0;

                totalTokens += tokens;
                totalCost += cost;
                messages += (daily.ai_messages ?? 0) + (daily.user_messages ?? 0);

                if (daily.date === today) {
                  todayTokens += tokens;
                  todayCost += cost;
                }
              }
            }

            return { totalTokens, totalCost, messages, todayTokens, todayCost };
          })();

          root.querySelector(".hero-cost").textContent =
            "$" + summary.totalCost.toFixed(2);
          root.querySelector(".hero-sub").textContent =
            shortNumber(summary.totalTokens) +
            " tokens • " +
            summary.messages.toLocaleString() +
            " messages";

          const scopeLabel =
            state.scope === "today"
              ? "Today"
              : state.scope === "week"
              ? "Last 7 days"
              : state.scope === "month"
              ? "Last 30 days"
              : "All time";

          root.querySelector(".hero-meta").textContent =
            scopeLabel + " • updated " + new Date().toLocaleTimeString();

          // By tool
          const byTool = [];
          for (const analyzer of scopedAnalyzers) {
            let tokens = 0;
            let cost = 0;
            for (const daily of Object.values(analyzer.daily_stats || {})) {
              const stats = daily.stats || {};
              const input = stats.inputTokens ?? 0;
              const output = stats.outputTokens ?? 0;
              tokens += input + output;
              cost += stats.cost ?? 0;
            }
            byTool.push({ name: analyzer.analyzer_name, tokens, cost });
          }
          byTool.sort((a, b) => b.cost - a.cost);

          const byToolBody = root.querySelector(".by-tool-body");
          byToolBody.innerHTML = "";
          if (byTool.length === 0) {
            byToolBody.innerHTML = '<div class="muted">No tool usage.</div>';
          } else {
            for (const row of byTool) {
              const percent =
                summary.totalCost > 0
                  ? Math.round((row.cost / summary.totalCost) * 100)
                  : 0;
              const div = document.createElement("div");
              div.className = "row";
              div.innerHTML =
                '<span class="row-label">' +
                row.name +
                "</span>" +
                '<span class="row-value">$' +
                row.cost.toFixed(2) +
                ' <span class="badge">' +
                percent +
                "%</span></span>";
              byToolBody.appendChild(div);
            }
          }

          // By model (approximate using daily model counts, proportional by messages)
          const modelMap = new Map();
          for (const analyzer of scopedAnalyzers) {
            for (const daily of Object.values(analyzer.daily_stats || {})) {
              const models = daily.models || {};
              const dailyCost = daily.stats?.cost ?? 0;
              const totalModelMessages = Object.values(models).reduce(
                (a, b) => a + Number(b),
                0
              );
              for (const [model, count] of Object.entries(models)) {
                const prev = modelMap.get(model) || { cost: 0, messages: 0 };
                const share =
                  totalModelMessages > 0
                    ? Number(count) / totalModelMessages
                    : 0;
                prev.cost += dailyCost * share;
                prev.messages += Number(count);
                modelMap.set(model, prev);
              }
            }
          }

          const models = Array.from(modelMap.entries()).map(
            ([name, value]) => ({
              name,
              cost: value.cost,
              messages: value.messages,
            })
          );
          models.sort((a, b) => b.cost - a.cost);

          const byModelBody = root.querySelector(".by-model-body");
          byModelBody.innerHTML = "";
          if (models.length === 0) {
            byModelBody.innerHTML =
              '<div class="muted">No model usage.</div>';
          } else {
            for (const row of models.slice(0, 6)) {
              const percent =
                summary.totalCost > 0
                  ? Math.round((row.cost / summary.totalCost) * 100)
                  : 0;
              const div = document.createElement("div");
              div.className = "row";
              div.innerHTML =
                '<span class="row-label">' +
                row.name +
                "</span>" +
                '<span class="row-value">$' +
                row.cost.toFixed(2) +
                ' <span class="badge">' +
                percent +
                "%</span></span>";
              byModelBody.appendChild(div);
            }
          }

          // Details (aggregate simple stats)
          const detailsBody = root.querySelector(".details-body");
          const totals = {
            inputTokens: 0,
            outputTokens: 0,
            cacheReadTokens: 0,
            fileReads: 0,
            fileWrites: 0,
            toolCalls: 0,
          };

          for (const analyzer of scopedAnalyzers) {
            for (const daily of Object.values(analyzer.daily_stats || {})) {
              const stats = daily.stats || {};
              totals.inputTokens += stats.inputTokens ?? 0;
              totals.outputTokens += stats.outputTokens ?? 0;
              totals.cacheReadTokens += stats.cacheReadTokens ?? 0;
              totals.fileReads += stats.filesRead ?? 0;
              totals.fileWrites +=
                (stats.filesAdded ?? 0) +
                (stats.filesEdited ?? 0) +
                (stats.filesDeleted ?? 0);
              totals.toolCalls += stats.toolCalls ?? 0;
            }
          }

          detailsBody.innerHTML = "";
          const detailRows = [
            ["Input tokens", totals.inputTokens],
            ["Output tokens", totals.outputTokens],
            ["Cache read tokens", totals.cacheReadTokens],
            ["File reads", totals.fileReads],
            ["File writes", totals.fileWrites],
            ["Tool calls", totals.toolCalls],
          ];

          for (const [label, value] of detailRows) {
            const div = document.createElement("div");
            div.className = "row";
            div.innerHTML =
              '<span class="row-label">' +
              label +
              "</span>" +
              '<span class="row-value">' +
              (typeof value === "number"
                ? value.toLocaleString()
                : String(value)) +
              "</span>";
            detailsBody.appendChild(div);
          }

          // Projects (placeholder for now)
          const projectsBody = root.querySelector(".projects-body");
          projectsBody.innerHTML =
            '<div class="muted">Recent projects coming in a future update.</div>';
        }

        window.addEventListener("message", (event) => {
          const message = event.data;
          if (!message || typeof message !== "object") return;
          if (message.type === "stats") {
            state.data = message.payload;
            render();
          }
        });

        window.addEventListener("load", () => {
          const select = document.querySelector(".scope-select");
          if (select) {
            select.addEventListener("change", onScopeChange);
          }

          // Attach button event listeners (CSP blocks inline onclick)
          document.getElementById("btn-upload")?.addEventListener("click", () => {
            vscode.postMessage({ type: "upload" });
          });
          document.getElementById("btn-tui")?.addEventListener("click", () => {
            vscode.postMessage({ type: "openTui" });
          });
          document.getElementById("btn-settings")?.addEventListener("click", () => {
            vscode.postMessage({ type: "openSettings" });
          });
          document.getElementById("btn-cloud")?.addEventListener("click", () => {
            vscode.postMessage({ type: "openCloud" });
          });

          vscode.postMessage({ type: "requestRefresh" });
        });
      </script>
    `;

    const body = /* html */ `
      <body>
        <div class="root">
          <div class="header">
            <div class="header-left">
              <div class="logo-dot"></div>
              <span>SPLITRAIL</span>
              <select class="scope-select">
                <option value="today">Today</option>
                <option value="week">This Week</option>
                <option value="month">This Month</option>
                <option value="all">All Time</option>
              </select>
            </div>
            <div class="header-actions">
              <button class="icon-button" id="btn-upload" title="Upload to Splitrail Cloud">☁️</button>
              <button class="icon-button" id="btn-tui" title="Open Splitrail TUI">🖥️</button>
              <button class="icon-button" id="btn-settings" title="Splitrail Settings">⚙️</button>
            </div>
          </div>
          <div class="content">
            <div class="hero">
              <div class="hero-main">
                <div class="hero-cost">$0.00</div>
                <div class="hero-sub">No usage yet</div>
              </div>
              <div class="hero-meta"></div>
            </div>
            <div class="grid">
              <div class="card">
                <div class="card-header">By Tool</div>
                <div class="by-tool-body"></div>
              </div>
              <div class="card">
                <div class="card-header">By Model</div>
                <div class="by-model-body"></div>
              </div>
            </div>
            <div class="grid">
              <div class="card">
                <div class="card-header">Today's Details</div>
                <div class="details-body"></div>
              </div>
              <div class="card">
                <div class="card-header">Recent Projects</div>
                <div class="projects-body"></div>
              </div>
            </div>
            <div class="footer">
              <span class="muted">Powered by Splitrail CLI</span>
              <span class="footer-link" id="btn-cloud">Open Cloud Dashboard →</span>
            </div>
          </div>
        </div>
      </body>
    `;

    return `<!DOCTYPE html>
      <html lang="en">
        <head>
          <meta charset="UTF-8" />
          <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';" />
          <meta name="viewport" content="width=device-width, initial-scale=1.0" />
          <title>Splitrail</title>
          ${styles}
        </head>
        ${body}
        ${script}
      </html>`;
  }
}

function getNonce(): string {
  let text = "";
  const possible =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  for (let i = 0; i < 32; i++) {
    text += possible.charAt(Math.floor(Math.random() * possible.length));
  }
  return text;
}

