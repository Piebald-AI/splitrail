<div>
<div align="right">
<a href="https://piebald.ai"><img width="200" top="20" align="right" src="https://github.com/Piebald-AI/.github/raw/main/Wordmark.svg"></a>
</div>

<div align="left">

### Check out Piebald
We've released **Piebald**, the ultimate agentic AI developer experience. \
Download it and try it out for free!  **https://piebald.ai/**

<a href="https://piebald.ai/discord"><img src="https://img.shields.io/badge/Join%20our%20Discord-5865F2?style=flat&logo=discord&logoColor=white" alt="Join our Discord"></a>
<a href="https://x.com/PiebaldAI"><img src="https://img.shields.io/badge/Follow%20%40PiebaldAI-000000?style=flat&logo=x&logoColor=white" alt="X"></a>

<sub>[**Scroll down for Splitrail.**](#splitrail) :point_down:</sub>

</div>
</div>

<div align="left">
<a href="https://piebald.ai">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://piebald.ai/screenshot-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="https://piebald.ai/screenshot-light.png">
  <img alt="hero" width="800" src="https://piebald.ai/screenshot-light.png">
</picture>
</a>
</div>

# Splitrail

Splitrail is a **fast, cross-platform, real-time token usage tracker and cost monitor for**:
- [Gemini CLI](https://github.com/google-gemini/gemini-cli) (and [Qwen Code](https://github.com/qwenlm/qwen-code))
- [Claude Code](https://github.com/anthropics/claude-code)
- [Codex CLI](https://github.com/openai/codex)
- [Cline](https://github.com/cline/cline) / [Roo Code](https://github.com/RooCodeInc/Roo-Code) / [Zoo Code](https://github.com/Zoo-Code-Org/Zoo-Code/) / [Kilo Code](https://github.com/Kilo-Org/kilocode) (VS Code extension + CLI)
- [GitHub Copilot](https://github.com/features/copilot) (VS Code)
- [GitHub Copilot CLI](https://github.com/features/copilot)
- [OpenCode](https://github.com/sst/opencode)
- [Pi Agent](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent)
- Grok
- [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)

Run one command to instantly review all of your CLI coding agent usage.  Upload your usage data to your private account on the [Splitrail Cloud](https://splitrail.dev) for safe-keeping and cross-machine usage aggregation.  From the team behind [<img src="https://github.com/Piebald-AI/piebald-issues/raw/main/assets/logo.svg" width="15"> **Piebald.**](https://piebald.ai/)


> [!note]
> ⭐ **If you find Splitrail useful, please consider [starring the repository](https://github.com/Piebald-AI/splitrail) to show your support!** ⭐


**Download the binary for your platform on the [Releases](https://github.com/Piebald-AI/splitrail/releases) page.**

## Screenshots

### [Splitrail CLI](https://splitrail.dev)
<img width="750" alt="Screenshot of the Splitrail CLI" src="https://raw.githubusercontent.com/Piebald-AI/splitrail/main/screenshots/cli.gif" />

### [Splitrail VS Code Extension](https://splitrail.dev)
<img width="750" alt="Screenshot of the Splitrail VS Code Extension" src="https://raw.githubusercontent.com/Piebald-AI/splitrail/main/screenshots/extension.png" />

### [Splitrail Cloud](https://splitrail.dev)
<img width="750" alt="Screenshot of Splitrail Cloud" src="https://raw.githubusercontent.com/Piebald-AI/splitrail/main/screenshots/cloud.png" />

## Local Web Dashboard

Run `splitrail web` and open `http://127.0.0.1:8765`. Use `splitrail web --port 9000` to choose another port. The server listens only on `127.0.0.1` and accepts that IP in the Host header. It reads local usage through the existing analyzers without uploading it. Static assets are embedded in the binary; no frontend build or Node installation is needed to run it.

The overview shows estimated cost, total tokens, replies (model responses), session count, and cache share, with comparisons against the previous period. Filter by today, 7, 30, or 90 days, or all history, and select tools, models, or projects. Selections within one field combine; selections across fields narrow the result. Linked worktrees, repository subdirectories, and paths connected by an analyzer's project hash are grouped into one project. Hover a project to see its merged paths.

Switch between cost, tokens, and replies, and group the stacked trend by model, tool, or project. The chart supports hourly, daily, weekly, and monthly intervals; click a daily bar to inspect that day by hour. Focus the chart and use the arrow keys to read values, or Enter to drill into a day. Model, tool, and project rankings also filter on click. The five entities with the highest cost in the selected period keep their colors while filters change; the rest are grouped as Other.

Usage snapshots are saved in the system cache directory under `splitrail/web-usage-v1.json`. On restart, the page immediately shows the saved snapshot while a background scan refreshes it, then updates automatically. The first run without a valid cache waits for a scan. Click Refresh to scan again. Concurrent refreshes share one scan, cached pages remain available during scanning, and a failed refresh preserves the previous snapshot. Cache files with a different application version or timezone are ignored. Dates and hours use the machine's local time. Total tokens match the TUI: input + output + cached; reasoning tokens are not added again. Costs use public API prices and may differ from subscription spending.

Frontend aggregation tests run with `node --test src/web/data.test.mjs`.

Model aliases configured in `~/.splitrail.toml` also determine dashboard names and grouping, including filters, charts, rankings, and CSV exports. Session details show the original names beneath mapped names. Unmapped models keep their original names. Restart `splitrail web` after changing aliases; saved snapshots retain the original names and use the current aliases on load.

The Sessions tab provides search, sorting by recency/cost/tokens/replies/active time, and a paginated list. A compact summary shows total active time, replies, cost and tokens per reply, cache share, and project count. Above the list, the session shape card buckets the filtered sessions by reply count, active time, or tool calls, with the median and p90 beside it; the work activity card shows recorded tool calls, file reads, file operations, and shell commands. Active time counts gaps under five minutes between messages, so long idle periods do not inflate session duration. Select a session to see its whole-session totals, reply and tool-call counts, file reads and edits, token mix, per-model replies, tokens, cache share and cost, and its hourly or daily timeline. Shared filters select sessions; the detail panel shows the entire selected session. File and command counts are tool-operation counts, not unique file counts, and are available only where the source analyzer records them.

The Breakdown tab groups the filtered usage by period, project, model, or tool, and includes tool calls, file reads, file operations, and shell commands. Export CSV downloads the same groups, columns, ordering, and total row with exact numeric values. Shares are fractions between 0 and 1; text labels are escaped for spreadsheet use. View and filter selections are kept in the URL hash for bookmarks.

English is the default language; choose Simplified or Traditional Chinese in the header. The preference is saved locally. The dashboard follows the system's light/dark appearance and adapts to phone widths, with horizontally scrollable charts and tables. Overview also includes a weekday/hour activity heatmap and the largest sessions.

## MCP Server

Splitrail can run as an [MCP (Model Context Protocol)](https://modelcontextprotocol.io/) server, allowing AI assistants to query your usage statistics programmatically.

```bash
splitrail mcp
```

### Available Tools

- `get_daily_stats` - Query usage statistics with date filtering
- `get_model_usage` - Analyze model usage distribution
- `get_cost_breakdown` - Get cost breakdown over a date range
- `get_file_operations` - Get file operation statistics
- `compare_tools` - Compare usage across different AI coding tools
- `list_analyzers` - List available analyzers

### Resources

- `splitrail://summary` - Daily summaries across all dates
- `splitrail://models` - Model usage breakdown

## Configuration

Splitrail stores its configuration at `~/.splitrail.toml`:

```toml
[server]
url = "https://splitrail.dev"
api_token = "your-api-token"

[upload]
auto_upload = false
upload_today_only = false

[formatting]
number_comma = false
number_human = false
locale = "en"
decimal_places = 2

[logging]
level = "warn"
```

## Keeping usage from deleted Piebald chats

Splitrail reads Piebald's database live, so permanently deleting a chat in Piebald also removes its usage from Splitrail, including from any upload that hasn't happened yet. To keep counting a chat after you delete it, save a usage archive of it first, in Splitrail's state directory:

- Linux: `~/.local/state/splitrail/piebald-archive/` (or `$XDG_STATE_HOME/splitrail/piebald-archive/`)
- macOS: `~/Library/Application Support/splitrail/piebald-archive/`
- Windows: `%LOCALAPPDATA%\splitrail\piebald-archive\`

An archive is a SQLite file (`.db`, `.sqlite`, or `.sqlite3`) in Piebald's database layout. Splitrail reads it with the same code it uses for Piebald's own database and merges the two:

- Each message counts once, even while its chat still exists in Piebald or when it appears in several archives.
- Archived messages keep the date, model, project, price, and identity they had while their chat existed, so they upload to Splitrail Cloud exactly as they would have before the deletion.
- Archives belong to the Piebald installation on the same machine and are merged only with its database.
- An archive that can't be read is skipped with a warning; it never hides your live usage.

A complete copy of Piebald's database works as an archive, but it doesn't need to be one. Splitrail only needs each chat, its messages and their token counts, its tool-call markers, and the profile and project rows they refer to; message text and tool output can be left out. Write the file under another name (such as `*.db.partial`) and rename it into place when it's complete, because Splitrail reads whatever is in the folder each time it refreshes.

## Development

### Windows

On Windows, we use `lld-link.exe` from LLVM to significantly speed up compilation, so you'll need to install it to compile Splitrail.  Example for `winget`:

```shell
winget install --id LLVM.LLVM
```

Then add it to your system PATH:
```cmd
:: Command prompt
setx /M PATH "%PATH%;C:\Program Files\LLVM\bin\"
set "PATH=%PATH%;C:\Program Files\LLVM\bin"
```
or
```pwsh
# PowerShell
setx /M PATH "$env:PATH;C:\Program Files\LLVM\bin\"
$env:PATH = "$env:PATH;C:\Program Files\LLVM\bin\"
```

Then use standard Cargo commands to build and run:

```shell
cargo run
```

### macOS/Linux

Build as normal:
```
cargo run
```


-----

## License

[MIT](https://github.com/Piebald-AI/splitrail/blob/main/LICENSE)

Copyright © 2026 [Piebald LLC](https://piebald.ai).
