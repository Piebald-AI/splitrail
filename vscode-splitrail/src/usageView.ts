import * as vscode from "vscode";

export interface JsonStatsResponse {
  analyzer_stats: JsonAnalyzerStats[];
}

export interface JsonAnalyzerStats {
  analyzer_name: string;
  num_conversations: number;
  daily_stats: { [date: string]: JsonDailyStats };
  messages?: JsonMessage[];
}

export interface JsonDailyStats {
  date: string;
  user_messages: number;
  ai_messages: number;
  conversations: number;
  models: { [model: string]: number };
  stats: JsonInnerStats;
}

export interface JsonInnerStats {
  inputTokens?: number;
  outputTokens?: number;
  cost?: number;
  // Other fields are allowed but not explicitly modeled
  [key: string]: unknown;
}

export interface JsonMessage {
  application: string;
  date: string;
  project_hash: string;
  conversation_hash: string;
  local_hash?: string | null;
  global_hash: string;
  model?: string | null;
  stats: JsonInnerStats;
  role: string;
  uuid?: string | null;
  session_name?: string | null;
}

export class UsageTreeItem extends vscode.TreeItem {
  constructor(
    public readonly label: string,
    public readonly collapsibleState: vscode.TreeItemCollapsibleState,
    public readonly description?: string
  ) {
    super(label, collapsibleState);
    this.description = description;
  }
}

export class UsageTreeDataProvider
  implements vscode.TreeDataProvider<UsageTreeItem>
{
  private analyzerStats: JsonAnalyzerStats[] = [];

  private _onDidChangeTreeData: vscode.EventEmitter<
    UsageTreeItem | undefined | null | void
  > = new vscode.EventEmitter<UsageTreeItem | undefined | null | void>();

  readonly onDidChangeTreeData: vscode.Event<
    UsageTreeItem | undefined | null | void
  > = this._onDidChangeTreeData.event;

  setAnalyzerStats(stats: JsonAnalyzerStats[]): void {
    this.analyzerStats = stats;
    this.refresh();
  }

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: UsageTreeItem): vscode.TreeItem {
    return element;
  }

  getChildren(element?: UsageTreeItem): Thenable<UsageTreeItem[]> {
    if (this.analyzerStats.length === 0) {
      return Promise.resolve([]);
    }

    if (!element) {
      // Root items: one per analyzer
      const items = this.analyzerStats.map((analyzer) => {
        const { totalTokens, totalCost } = summarizeAnalyzer(analyzer);
        const description =
          totalTokens === 0 && totalCost === 0
            ? "No usage yet"
            : `${totalTokens.toLocaleString()} tokens • $${totalCost.toFixed(
                4
              )}`;
        const item = new UsageTreeItem(
          analyzer.analyzer_name,
          vscode.TreeItemCollapsibleState.Collapsed,
          description
        );
        item.iconPath = new vscode.ThemeIcon("graph");
        item.tooltip = new vscode.MarkdownString(
          `**${analyzer.analyzer_name}**  \n` +
            `${totalTokens.toLocaleString()} tokens total  \n` +
            `$${totalCost.toFixed(4)} total cost`
        );
        return item;
      });
      return Promise.resolve(items);
    }

    // Child items: one per day for the selected analyzer
    const analyzer = this.analyzerStats.find(
      (a) => a.analyzer_name === element.label
    );
    if (!analyzer) {
      return Promise.resolve([]);
    }

    const dates = Object.keys(analyzer.daily_stats).sort();
    const items = dates.map((date) => {
      const daily = analyzer.daily_stats[date];
      const stats = daily.stats || {};
      const tokens =
        (stats.inputTokens ?? 0) + (stats.outputTokens ?? 0);
      const description = `${tokens.toLocaleString()} tokens • ${daily.ai_messages} AI / ${daily.user_messages} user`;
      const item = new UsageTreeItem(
        date,
        vscode.TreeItemCollapsibleState.None,
        description
      );
      item.iconPath = new vscode.ThemeIcon("calendar");
      item.tooltip = new vscode.MarkdownString(
        `**${date}**  \n` +
          `${tokens.toLocaleString()} tokens  \n` +
          `${daily.ai_messages} AI / ${daily.user_messages} user messages`
      );
      return item;
    });

    return Promise.resolve(items);
  }
}

export function summarizeAnalyzer(analyzer: JsonAnalyzerStats): {
  totalTokens: number;
  totalCost: number;
} {
  let totalTokens = 0;
  let totalCost = 0;

  for (const daily of Object.values(analyzer.daily_stats)) {
    const stats = daily.stats || {};
    const input = stats.inputTokens ?? 0;
    const output = stats.outputTokens ?? 0;
    totalTokens += input + output;
    totalCost += stats.cost ?? 0;
  }

  return { totalTokens, totalCost };
}

export function summarizeAllAnalyzers(
  analyzers: JsonAnalyzerStats[]
): {
  totalTokens: number;
  totalCost: number;
  todayTokens: number;
  todayCost: number;
} {
  let totalTokens = 0;
  let totalCost = 0;
  let todayTokens = 0;
  let todayCost = 0;

  const today = localDateString();

  for (const analyzer of analyzers) {
    for (const daily of Object.values(analyzer.daily_stats)) {
      const stats = daily.stats || {};
      const input = stats.inputTokens ?? 0;
      const output = stats.outputTokens ?? 0;
      const tokens = input + output;
      const cost = stats.cost ?? 0;

      totalTokens += tokens;
      totalCost += cost;

      if (daily.date === today) {
        todayTokens += tokens;
        todayCost += cost;
      }
    }
  }

  return { totalTokens, totalCost, todayTokens, todayCost };
}

function localDateString(): string {
  const d = new Date();
  const year = d.getFullYear();
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}
