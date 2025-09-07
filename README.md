# Splitrail

**Fast, cross-platform, real-time Gemini CLI / Claude Code / Codex token usage tracker and cost monitor.**

The Splitrail CLI can automatically upload usage data to [Splitrail Cloud.](https://splitrail.dev)

> [!WARNING]
> While support for both Codex **is** implemented, Codex currently does not output enough information to its recorded chat files.  A PR is open on Codex, however: https://github.com/openai/codex/pull/1583.  React with :+1: on it to encourage it to be merged!

Also check out our developer-first agentic AI experience, [Piebald](https://piebald.ai/).

## Screenshots

### [Splitrail CLI](https://splitrail.dev)
<img width="750" alt="Screenshot of the Splitrail CLI" src="https://raw.githubusercontent.com/Piebald-AI/splitrail/main/screenshots/cli.gif" />

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
