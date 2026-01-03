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
- [Cline](https://github.com/cline/cline) / [Roo Code](https://github.com/RooCodeInc/Roo-Code) / [Kilo Code](https://github.com/Kilo-Org/kilocode)
- [GitHub Copilot](https://github.com/features/copilot)
- [OpenCode](https://github.com/sst/opencode)
- [Pi Agent](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent)

Run one command to instantly review all of your CLI coding agent usage.  Upload your usage data to your private account on the [Splitrail Cloud](https://splitrail.dev) for safe-keeping and cross-machine usage aggregation.  From the team behind [<img src="https://github.com/Piebald-AI/piebald/raw/main/assets/logo.svg" width="15"> **Piebald.**](https://piebald.ai/)


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
```

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
