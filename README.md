<div>
<div align="right">
<a href="https://piebald.ai"><img width="200" top="20" align="right" src="https://github.com/Piebald-AI/.github/raw/main/Wordmark.svg"></a>
</div>

<div align="left">

### Announcement: Piebald is released!
We've released **Piebald**, the ultimate agentic AI developer experience. \
Download it and try it out for free!  **https://piebald.ai/**

<sub>[Scroll down for Splitrail.](#splitrail) :point_down:</sub>

</div>
</div>

<div align="left">
<a href="https://piebald.ai">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://github.com/user-attachments/assets/79c18689-e2f0-4008-a13f-61c80756286a">
  <source media="(prefers-color-scheme: light)" srcset="https://github.com/user-attachments/assets/25cb5df8-cf15-4cae-9c1e-e88645f06ee1">
  <img alt="hero" width="600" src="https://github.com/user-attachments/assets/25cb5df8-cf15-4cae-9c1e-e88645f06ee1">
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

Copyright © 2025 [Piebald LLC](https://piebald.ai).
