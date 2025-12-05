import * as vscode from "vscode";
import {
  JsonStatsResponse,
  summarizeAllAnalyzers,
} from "./usageView";
import { SplitrailDashboardProvider } from "./dashboardView";
import * as childProcess from "child_process";
import * as fs from "fs";
import * as path from "path";
import * as os from "os";

// Debounce timer for file watcher refreshes
let refreshDebounceTimer: NodeJS.Timeout | undefined;
const REFRESH_DEBOUNCE_MS = 1500;

// Track file watchers for cleanup
const fileWatchers: vscode.FileSystemWatcher[] = [];

export function activate(context: vscode.ExtensionContext) {
  // Status bar item
  const statusBar = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Left,
    100
  );
  statusBar.text = "Splitrail: ready";
  statusBar.command = "splitrail.showPopup";
  statusBar.show();

  // Dashboard (Activity Bar)
  const dashboardProvider = new SplitrailDashboardProvider(context);
  const dashboardDisposable = vscode.window.registerWebviewViewProvider(
    SplitrailDashboardProvider.viewType,
    dashboardProvider
  );

  // Command: open Splitrail TUI in terminal
  const openDashboard = vscode.commands.registerCommand(
    "splitrail.openDashboard",
    () => {
      const terminal = vscode.window.createTerminal("Splitrail");
      terminal.sendText("splitrail");
      terminal.show();
    }
  );

  const openCloud = vscode.commands.registerCommand(
    "splitrail.openCloud",
    () => {
      vscode.env.openExternal(
        vscode.Uri.parse("https://splitrail.dev")
      );
    }
  );

  const upload = vscode.commands.registerCommand(
    "splitrail.upload",
    async () => {
      const cliPath = resolveSplitrailPath();

      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: "Splitrail: Uploading to cloud...",
          cancellable: false,
        },
        async () => {
          return new Promise<void>((resolve) => {
            childProcess.execFile(
              cliPath,
              ["upload"],
              { maxBuffer: 10 * 1024 * 1024 },
              (error, stdout, stderr) => {
                if (error) {
                  const details = stderr || error.message || "unknown error";
                  vscode.window.showErrorMessage(
                    `Splitrail upload failed: ${details}`
                  );
                } else {
                  vscode.window.showInformationMessage(
                    "Splitrail: Successfully uploaded to cloud!"
                  );
                }
                resolve();
              }
            );
          });
        }
      );
    }
  );

  const showPopup = vscode.commands.registerCommand(
    "splitrail.showPopup",
    async () => {
      try {
        const stats = await runSplitrailStats();
        // Update dashboard when user clicks the status bar.
        dashboardProvider.updateStats(stats.analyzer_stats);
        const barSummary = summarizeAllAnalyzers(stats.analyzer_stats);
        const barTodayTokens = barSummary.todayTokens;
        const barTodayCost = barSummary.todayCost;
        const barLabelToday =
          barTodayTokens === 0 && barTodayCost === 0
            ? "no usage today"
            : `${shortNumber(barTodayTokens)} tok • $${barTodayCost.toFixed(
                2
              )}`;
        statusBar.text = `Splitrail: ${barLabelToday}`;

        const summary = summarizeAllAnalyzers(stats.analyzer_stats);
        const todayTokens = summary.todayTokens;
        const todayCost = summary.todayCost;
        const totalTokens = summary.totalTokens;
        const totalCost = summary.totalCost;

        const quickPick = vscode.window.createQuickPick();
        quickPick.title = "Splitrail – Today's Usage";
        quickPick.items = [
          {
            label: `$(graph) $${todayCost.toFixed(2)} · ${shortNumber(
              todayTokens
            )} tokens`,
            detail: "Today",
          },
          {
            label: `Total: $${totalCost.toFixed(
              2
            )} · ${shortNumber(totalTokens)} tokens`,
            detail: "All time",
          },
          {
            label: "Open Dashboard →",
            detail: "View full analytics",
          },
        ];

        quickPick.onDidChangeSelection((selection) => {
          const picked = selection[0];
          if (picked && picked.label.startsWith("Open Dashboard")) {
            vscode.commands.executeCommand(
              "workbench.view.extension.splitrail"
            );
          }
          quickPick.hide();
        });

        quickPick.onDidHide(() => quickPick.dispose());
        quickPick.show();
      } catch (err) {
        const message =
          err instanceof Error ? err.message : String(err);
        vscode.window.showWarningMessage(
          `Splitrail: unable to show popup (${message})`
        );
      }
    }
  );

  // Command: refresh usage (placeholder + hook for future CLI integration)
  const refresh = vscode.commands.registerCommand(
    "splitrail.refresh",
    async () => {
      statusBar.text = "Splitrail: refreshing...";

      try {
        const stats = await runSplitrailStats();
        dashboardProvider.updateStats(stats.analyzer_stats);
        const summary = summarizeAllAnalyzers(stats.analyzer_stats);
        const timestamp = new Date().toLocaleTimeString();
        const todayTokens = summary.todayTokens;
        const todayCost = summary.todayCost;
        const labelToday =
          todayTokens === 0 && todayCost === 0
            ? "no usage today"
            : `${shortNumber(todayTokens)} tok • $${todayCost.toFixed(
                2
              )}`;
        statusBar.text = `Splitrail: ${labelToday} (updated ${timestamp})`;
      } catch (err) {
        const message =
          err instanceof Error ? err.message : String(err);
        vscode.window.showWarningMessage(
          `Splitrail: unable to refresh usage (${message})`
        );
        statusBar.text = "Splitrail: ready (refresh failed)";
      }
    }
  );

  context.subscriptions.push(
    statusBar,
    openDashboard,
    showPopup,
    openCloud,
    upload,
    refresh,
    dashboardDisposable
  );

  // Setup live file watching if enabled
  setupFileWatchers(context, () => {
    debouncedRefresh(statusBar, dashboardProvider);
  });

  // Do an initial refresh on activation
  runSplitrailStats()
    .then((stats) => {
      dashboardProvider.updateStats(stats.analyzer_stats);
      const summary = summarizeAllAnalyzers(stats.analyzer_stats);
      const todayTokens = summary.todayTokens;
      const todayCost = summary.todayCost;
      const labelToday =
        todayTokens === 0 && todayCost === 0
          ? "no usage today"
          : `${shortNumber(todayTokens)} tok • $${todayCost.toFixed(2)}`;
      statusBar.text = `Splitrail: ${labelToday}`;
    })
    .catch(() => {
      // Silently fail on initial load - user can manually refresh
    });
}

export function deactivate() {
  // Clean up file watchers
  for (const watcher of fileWatchers) {
    watcher.dispose();
  }
  fileWatchers.length = 0;

  // Clear any pending debounce timer
  if (refreshDebounceTimer) {
    clearTimeout(refreshDebounceTimer);
    refreshDebounceTimer = undefined;
  }
}

function runSplitrailStats(): Promise<JsonStatsResponse> {
  const cliPath = resolveSplitrailPath();

  return new Promise((resolve, reject) => {
    const args = ["stats"];
    childProcess.execFile(
      cliPath,
      args,
      { maxBuffer: 50 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (stdout && stdout.trim().length > 0) {
          try {
            const parsed = parseStatsJson(stdout);
            resolve(parsed);
            return;
          } catch (parseErr) {
            // Fall through to error handling below.
            const parseMessage = `Failed to parse JSON from splitrail stats: ${String(
              parseErr
            )}`;
            if (!error) {
              reject(new Error(parseMessage));
              return;
            }
          }
        }

        if (error) {
          const err: NodeJS.ErrnoException = error as NodeJS.ErrnoException;
          if (err.code === "ENOENT") {
            reject(
              new Error(
                `splitrail CLI not found at '${cliPath}'. Set 'splitrail.cliPath' in settings or install splitrail on PATH.`
              )
            );
            return;
          }

          const details = stderr || error.message || "unknown error";
          reject(new Error(`splitrail stats failed: ${details}`));
          return;
        }

        reject(
          new Error(
            "splitrail stats produced no output. Check the CLI installation."
          )
        );
      }
    );
  });
}

function parseStatsJson(stdout: string): JsonStatsResponse {
  // First try a direct parse.
  try {
    return JSON.parse(stdout) as JsonStatsResponse;
  } catch {
    // Fall through to attempt to strip any leading warnings or noise.
  }

  const firstBrace = stdout.indexOf("{");
  const lastBrace = stdout.lastIndexOf("}");
  if (firstBrace !== -1 && lastBrace !== -1 && lastBrace > firstBrace) {
    const slice = stdout.slice(firstBrace, lastBrace + 1);
    return JSON.parse(slice) as JsonStatsResponse;
  }

  // If we get here, we really don't have valid JSON.
  throw new Error("No valid JSON object found in splitrail stats output.");
}

function resolveSplitrailPath(): string {
  const config = vscode.workspace.getConfiguration("splitrail");
  const configured = config.get<string>("cliPath");
  if (configured && configured.trim().length > 0) {
    return configured.trim();
  }

  const workspaceFolders = vscode.workspace.workspaceFolders;
  if (workspaceFolders && workspaceFolders.length > 0) {
    const ws = workspaceFolders[0].uri.fsPath;
    const exeName = process.platform === "win32" ? "splitrail.exe" : "splitrail";
    const debugPath = path.join(ws, "..", "target", "debug", exeName);
    if (fs.existsSync(debugPath)) {
      return debugPath;
    }
  }

  return "splitrail";
}

function shortNumber(value: number): string {
  if (value >= 1_000_000_000) {
    return (value / 1_000_000_000).toFixed(1) + "b";
  }
  if (value >= 1_000_000) {
    return (value / 1_000_000).toFixed(1) + "m";
  }
  if (value >= 1_000) {
    return (value / 1_000).toFixed(1) + "k";
  }
  return value.toString();
}

function getDataDirectories(): string[] {
  const home = os.homedir();
  const dirs: string[] = [];

  // Claude Code: ~/.claude/projects
  const claudeDir = path.join(home, ".claude", "projects");
  if (fs.existsSync(claudeDir)) {
    dirs.push(claudeDir);
  }

  // Codex CLI: ~/.codex/sessions
  const codexDir = path.join(home, ".codex", "sessions");
  if (fs.existsSync(codexDir)) {
    dirs.push(codexDir);
  }

  // Gemini CLI: ~/.gemini/tmp
  const geminiDir = path.join(home, ".gemini", "tmp");
  if (fs.existsSync(geminiDir)) {
    dirs.push(geminiDir);
  }

  // GitHub Copilot: ~/.vscode/extensions/github.copilot-chat-*
  const vscodeExtDir = path.join(home, ".vscode", "extensions");
  if (fs.existsSync(vscodeExtDir)) {
    try {
      const entries = fs.readdirSync(vscodeExtDir);
      for (const entry of entries) {
        if (entry.startsWith("github.copilot-chat-")) {
          const sessionsDir = path.join(vscodeExtDir, entry, "sessions");
          if (fs.existsSync(sessionsDir)) {
            dirs.push(sessionsDir);
          }
        }
      }
    } catch {
      // Ignore errors reading directory
    }
  }

  // OpenCode: ~/.opencode
  const opencodeDir = path.join(home, ".opencode");
  if (fs.existsSync(opencodeDir)) {
    dirs.push(opencodeDir);
  }

  // Pi Agent: ~/.pi/agent/sessions
  const piAgentDir = path.join(home, ".pi", "agent", "sessions");
  if (fs.existsSync(piAgentDir)) {
    dirs.push(piAgentDir);
  }

  return dirs;
}

function setupFileWatchers(
  context: vscode.ExtensionContext,
  onFileChange: () => void
): void {
  const config = vscode.workspace.getConfiguration("splitrail");
  const liveEnabled = config.get<boolean>("liveUpdates", true);

  if (!liveEnabled) {
    return;
  }

  const dataDirs = getDataDirectories();

  for (const dir of dataDirs) {
    // Watch for JSONL and JSON files in these directories
    const pattern = new vscode.RelativePattern(dir, "**/*.{jsonl,json}");
    const watcher = vscode.workspace.createFileSystemWatcher(pattern);

    watcher.onDidCreate(onFileChange);
    watcher.onDidChange(onFileChange);
    watcher.onDidDelete(onFileChange);

    fileWatchers.push(watcher);
    context.subscriptions.push(watcher);
  }

  if (dataDirs.length > 0) {
    console.log(`Splitrail: watching ${dataDirs.length} data directories for live updates`);
  }
}

function debouncedRefresh(
  statusBar: vscode.StatusBarItem,
  dashboardProvider: SplitrailDashboardProvider
): void {
  // Clear any existing timer
  if (refreshDebounceTimer) {
    clearTimeout(refreshDebounceTimer);
  }

  // Set a new debounced refresh
  refreshDebounceTimer = setTimeout(async () => {
    refreshDebounceTimer = undefined;

    try {
      const stats = await runSplitrailStats();
      dashboardProvider.updateStats(stats.analyzer_stats);

      const summary = summarizeAllAnalyzers(stats.analyzer_stats);
      const todayTokens = summary.todayTokens;
      const todayCost = summary.todayCost;
      const timestamp = new Date().toLocaleTimeString();
      const labelToday =
        todayTokens === 0 && todayCost === 0
          ? "no usage today"
          : `${shortNumber(todayTokens)} tok • $${todayCost.toFixed(2)}`;
      statusBar.text = `Splitrail: ${labelToday} (${timestamp})`;
    } catch {
      // Silently fail on auto-refresh to avoid spamming the user
    }
  }, REFRESH_DEBOUNCE_MS);
}
