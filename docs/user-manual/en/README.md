# CC Switch User Manual

> All-in-One Assistant for Claude Code / Codex / Pi

## Table of Contents

```
CC Switch User Manual
│
├── 1. Getting Started
│   ├── 1.1 Introduction
│   ├── 1.2 Installation Guide
│   ├── 1.3 Interface Overview
│   ├── 1.4 Quick Start
│   └── 1.5 Personalization
│
├── 2. Provider Management
│   ├── 2.1 Add Provider
│   ├── 2.2 Switch Provider
│   ├── 2.3 Edit Provider
│   └── 2.4 Sort & Duplicate
│
├── 3. Extensions
│   ├── 3.1 MCP Server Management
│   ├── 3.2 Prompts Management
│   ├── 3.3 Skills Management
│   └── 3.4 Session Manager
│
└── 5. FAQ
    ├── 5.1 Configuration Files
    ├── 5.2 FAQ
    └── 5.4 Environment Variable Conflicts
```

## File List

### 1. Getting Started

| File | Description |
|------|-------------|
| [1.1-introduction.md](./1-getting-started/1.1-introduction.md) | Introduction, core features, supported platforms |
| [1.2-installation.md](./1-getting-started/1.2-installation.md) | Windows/macOS/Linux installation guide |
| [1.3-interface.md](./1-getting-started/1.3-interface.md) | Interface layout, navigation bar, provider cards |
| [1.4-quickstart.md](./1-getting-started/1.4-quickstart.md) | 5-minute quick start tutorial |
| [1.5-settings.md](./1-getting-started/1.5-settings.md) | Language, theme, directories, cloud sync settings |

### 2. Provider Management

| File | Description |
|------|-------------|
| [2.1-add.md](./2-providers/2.1-add.md) | Using presets, custom configuration |
| [2.2-switch.md](./2-providers/2.2-switch.md) | Main UI switching, tray switching, activation methods |
| [2.3-edit.md](./2-providers/2.3-edit.md) | Edit configuration, modify API Key, backfill mechanism |
| [2.4-sort-duplicate.md](./2-providers/2.4-sort-duplicate.md) | Drag-to-reorder, duplicate provider, delete |

### 3. Extensions

| File | Description |
|------|-------------|
| [3.1-mcp.md](./3-extensions/3.1-mcp.md) | MCP protocol, add servers, app binding |
| [3.2-prompts.md](./3-extensions/3.2-prompts.md) | Create presets, activate/switch, smart backfill |
| [3.3-skills.md](./3-extensions/3.3-skills.md) | Discover skills, install/uninstall, repository management |
| [3.4-sessions.md](./3-extensions/3.4-sessions.md) | Session Manager: browse, search, resume, delete sessions |

### 4. Credentials

Secrets live in Windows Credential Manager and are delivered via user environment variables. The local HTTP proxy and usage dashboard are removed.

### 5. FAQ

| File | Description |
|------|-------------|
| [5.1-config-files.md](./5-faq/5.1-config-files.md) | CC Switch storage, CLI configuration file formats |
| [5.2-questions.md](./5-faq/5.2-questions.md) | Frequently asked questions |
| [5.4-env-conflict.md](./5-faq/5.4-env-conflict.md) | Environment variable conflict detection and resolution |

## Quick Links

- **New users**: Start with [1.1 Introduction](./1-getting-started/1.1-introduction.md)
- **Installation issues**: See [1.2 Installation Guide](./1-getting-started/1.2-installation.md)
- **Configure providers**: See [2.1 Add Provider](./2-providers/2.1-add.md)
- **Credentials**: See “Credentials” above
- **Having trouble**: See [5.2 FAQ](./5-faq/5.2-questions.md)

## Version Information

- Documentation version: v3.16.0
- Last updated: 2026-05-29
- Applicable to CC Switch v3.16.0+

### v3.16.0 Highlights

- **Codex Chat Completions routing**: route Chat-only providers such as Baidu Qianfan, StepFun, and SiliconFlow through Codex. See [2.1 Add Provider](./2-providers/2.1-add.md)
- **Managed CLI tool lifecycle**: install, update, update all, and diagnose Claude / Codex / Gemini / OpenCode / OpenClaw / Hermes from Settings / About. See [1.5 Personalization](./1-getting-started/1.5-settings.md)
- **Provider and model refresh**: new partner presets, refreshed default models and pricing, Claude Opus 4.8 defaults, and GPT 5.5 defaults where applicable
- **Routing support badges**: Claude Code / Codex provider cards indicate whether a provider can be served through Local Routing
- **Codex OAuth live model discovery**: ChatGPT Codex providers fetch available models from the ChatGPT backend on demand
- **Lightweight Mode**: Destroys the main window when minimizing to tray — near-zero idle footprint. See [1.5 Personalization](./1-getting-started/1.5-settings.md)
- **Codex OAuth Reverse Proxy**: Reuse your ChatGPT account's Codex service inside Claude Code — see [2.1 Add Provider](./2-providers/2.1-add.md)
- **Per-App Tray Submenus**: Claude / Codex / Gemini submenus show the current provider and available usage summaries — see [2.2 Switch Provider](./2-providers/2.2-switch.md)
- **Skills Discovery & Batch Updates**: SHA-256 update detection, batch updates, skills.sh public registry search — see [3.3 Skills Management](./3-extensions/3.3-skills.md)
- **Full URL Endpoint Mode**: Advanced option to treat `base_url` as the full upstream endpoint — see [2.1 Add Provider](./2-providers/2.1-add.md)

## Contributing

Feel free to submit Issues or PRs to improve the documentation:

- [GitHub Issues](https://github.com/farion1231/cc-switch/issues)
- [GitHub Repository](https://github.com/farion1231/cc-switch)
