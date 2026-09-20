# CC Switch 用户手册

> Claude Code / Codex / Pi 全方位辅助工具

## 目录结构

```
📚 CC Switch 用户手册
│
├── 1. 快速入门
│   ├── 1.1 软件介绍
│   ├── 1.2 安装指南
│   ├── 1.3 界面概览
│   ├── 1.4 快速上手
│   └── 1.5 个性化配置
│
├── 2. 供应商管理
│   ├── 2.1 添加供应商
│   ├── 2.2 切换供应商
│   ├── 2.3 编辑供应商
│   └── 2.4 排序与复制
│
├── 3. 扩展功能
│   ├── 3.1 MCP 服务器管理
│   ├── 3.2 Prompts 提示词管理
│   ├── 3.3 Skills 技能管理
│   └── 3.4 会话管理器
│
└── 5. 常见问题
    ├── 5.1 配置文件说明
    ├── 5.2 FAQ
    └── 5.4 环境变量冲突
```

## 文件列表

### 1. 快速入门

| 文件 | 内容 |
|------|------|
| [1.1-introduction.md](./1-getting-started/1.1-introduction.md) | 软件介绍、核心功能、支持平台 |
| [1.2-installation.md](./1-getting-started/1.2-installation.md) | Windows 安装指南 |
| [1.3-interface.md](./1-getting-started/1.3-interface.md) | 界面布局、导航栏、供应商卡片说明 |
| [1.4-quickstart.md](./1-getting-started/1.4-quickstart.md) | 5 分钟快速上手教程 |
| [1.5-settings.md](./1-getting-started/1.5-settings.md) | 语言、主题、目录、云同步配置 |

### 2. 供应商管理

| 文件 | 内容 |
|------|------|
| [2.1-add.md](./2-providers/2.1-add.md) | 使用预设、自定义配置 |
| [2.2-switch.md](./2-providers/2.2-switch.md) | 主界面切换、托盘切换、生效方式 |
| [2.3-edit.md](./2-providers/2.3-edit.md) | 编辑配置、修改 API Key、回填机制 |
| [2.4-sort-duplicate.md](./2-providers/2.4-sort-duplicate.md) | 拖拽排序、复制供应商、删除 |

### 3. 扩展功能

| 文件 | 内容 |
|------|------|
| [3.1-mcp.md](./3-extensions/3.1-mcp.md) | MCP 协议、添加服务器、应用绑定 |
| [3.2-prompts.md](./3-extensions/3.2-prompts.md) | 创建预设、激活切换、智能回填 |
| [3.3-skills.md](./3-extensions/3.3-skills.md) | 发现技能、安装卸载、仓库管理 |
| [3.4-sessions.md](./3-extensions/3.4-sessions.md) | 会话浏览、搜索过滤、恢复与删除 |

### 4. 凭据与投递

密钥只保存在 Windows 凭据管理器，切换供应商时通过用户级环境变量投递给 CLI；SQLite、`settings.json`、live 配置文件、导出与同步载荷中都没有密钥值。本地 HTTP 代理与用量看板已移除；应用内也不再有更新提示，升级到新版本请下载新的 Windows 安装包覆盖安装（GitHub Releases）。

### 5. 常见问题

| 文件 | 内容 |
|------|------|
| [5.1-config-files.md](./5-faq/5.1-config-files.md) | CC Switch 存储、CLI 配置文件格式 |
| [5.2-questions.md](./5-faq/5.2-questions.md) | 常见问题解答 |
| [5.4-env-conflict.md](./5-faq/5.4-env-conflict.md) | 环境变量冲突检测与处理 |

## 快速链接

- **新用户**：从 [1.1 软件介绍](./1-getting-started/1.1-introduction.md) 开始
- **安装问题**：查看 [1.2 安装指南](./1-getting-started/1.2-installation.md)
- **配置供应商**：查看 [2.1 添加供应商](./2-providers/2.1-add.md)
- **密钥与环境变量**：见上文「凭据与投递」
- **遇到问题**：查看 [5.2 FAQ](./5-faq/5.2-questions.md)

## 版本信息

- 文档版本：v2.0.2
- 最后更新：2026-09-20
- 适用于 CC Switch v2.0.0 及以上

### 2.0.0 关键变化

本 Windows 专版分支相对上游 3.x 的主要变化（完整清单见根目录 [`CHANGELOG.md`](../../../CHANGELOG.md)）：

- **凭据全部进 Windows 凭据管理器**：API Key、Base URL、Pi 请求头、同步口令不再以明文写入 SQLite、`settings.json` 或 live 配置文件；切换时通过用户级环境变量投递给 CLI。
- **移除本地代理、路由与用量看板**：不再监听端口、不再转发请求。
- **移除六个应用与自动更新**：仅保留 Claude Code、Codex、Pi；不再检查或安装更新，升级请下载新的 Windows 安装包覆盖安装。
- **仅发布 Windows 版本**：不再提供其他操作系统的构建。
- **首次启动自动迁移**：旧版明文凭据自动抽取入凭据管理器并重写配置，报告列出迁移项与失败的 live 重写并提供重试。

## 贡献

欢迎提交 Issue 或 PR 改进文档：

- [GitHub Issues](https://github.com/stttwq/cc-switch/issues)
- [GitHub Repository](https://github.com/stttwq/cc-switch)
