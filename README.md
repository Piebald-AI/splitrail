# Splitrail

Splitrail is a **fast, cross-platform, real-time Claude Code / Codex CLI / Gemini CLI / Qwen Code / Cline / Roo Code / Kilo Code / GitHub Copilot / OpenCode token usage tracker and cost monitor.**  Run one command to instantly review all of your CLI coding agent usage.  Upload your usage data to your private account on the [Splitrail Cloud](https://splitrail.dev) for safe-keeping and cross-machine usage aggregation.

**Download the binary for your platform on the [Releases](https://github.com/Piebald-AI/splitrail/releases) page.**

Also check out our developer-first agentic AI experience, [Piebald](https://piebald.ai/).

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

© 2025 [Piebald LLC](https://piebald.ai). All rights reserved.
