export type ProviderCategory =
  | "official" // 官方
  | "cn_official" // 开源官方（原"国产官方"）
  | "cloud_provider" // 云服务商（AWS Bedrock 等）
  | "aggregator" // 聚合网站
  | "third_party" // 第三方供应商
  | "custom"; // 自定义

export interface Provider {
  id: string;
  name: string;
  settingsConfig: Record<string, any>; // 应用配置对象：Claude 为 settings.json；Codex 为 { auth, config }
  websiteUrl?: string;
  // 新增：供应商分类（用于差异化提示/能力开关）
  category?: ProviderCategory;
  createdAt?: number; // 添加时间戳（毫秒）
  sortIndex?: number; // 排序索引（用于自定义拖拽排序）
  // 备注信息
  notes?: string;
  // 新增：是否为商业合作伙伴
  isPartner?: boolean;
  // 可选：供应商元数据（仅存于 ~/.cc-switch/config.json，不写入 live 配置）
  meta?: ProviderMeta;
  // 图标配置
  icon?: string; // 图标名称（如 "openai", "anthropic"）
  iconColor?: string; // 图标颜色（Hex 格式，如 "#00A67E"）
  secretStatus?: {
    apiKey: { present: boolean };
    baseUrl: string | null;
    extraEnv: string[];
  };
}

export interface AppConfig {
  providers: Record<string, Provider>;
  current: string;
}

// 自定义端点配置（旧版端点数据的序列化兼容字段）
export interface CustomEndpoint {
  url: string;
  addedAt: number;
  lastUsed?: number;
}

// 供应商元数据（字段名与后端一致，保持 snake_case）
export interface ProviderMeta {
  // 自定义端点：以 URL 为键，值为端点信息（旧数据兼容，后端已不再消费）
  custom_endpoints?: Record<string, CustomEndpoint>;
  // 是否在切换/同步到 live 时应用通用配置片段
  commonConfigEnabled?: boolean;
  // 是否为官方合作伙伴
  isPartner?: boolean;
  // 合作伙伴促销 key（用于后端识别 PackyCode 等）
  partnerPromotionKey?: string;
  // API 格式（Claude / Codex 供应商使用）：只决定模型目录与字段形态判定，
  // cc-switch 不做请求级协议转换。
  apiFormat?: "anthropic" | "openai_chat" | "openai_responses";
  // Claude 认证字段名
  apiKeyField?: ClaudeApiKeyField;
  // 是否将 base_url 视为完整 API 端点（原样写入配置直接使用，不拼接路径）
  isFullUrl?: boolean;
  // Prompt cache key for OpenAI Responses-compatible endpoints (improves cache hit rate)
  promptCacheKey?: string;
  // Whether this provider is currently projected into an additive app's live config.
  liveConfigManaged?: boolean;
}

// Skill 同步方式
export type SkillSyncMethod = "auto" | "symlink" | "copy";

// Skill 存储位置
export type SkillStorageLocation = "cc_switch" | "unified";

// Claude API 格式类型：只决定端点填写提示与模型列表拉取方式，无协议转换
export type ClaudeApiFormat = "anthropic" | "openai_chat" | "openai_responses";

// Codex API 格式类型：只驱动模型目录与字段形态判定，无协议转换
export type CodexApiFormat = "openai_responses" | "openai_chat" | "anthropic";

export interface CodexCatalogModel {
  model: string;
  displayName?: string;
  contextWindow?: string | number;
  // Hidden provider capability metadata for the generated model catalog.
  // supportsParallelToolCalls is native-profile-only; inputModalities wins over
  // automatic text-only model detection for every profile.
  supportsParallelToolCalls?: boolean;
  inputModalities?: string[];
  // Vendor's OFFICIAL base_instructions (model identity / system preamble).
  // Codex requires this field in every catalog entry; when omitted the backend
  // falls back to a neutral default. e.g. MiMo "developed by Xiaomi".
  baseInstructions?: string;
  // Per-model reasoning effort levels exposed in the generated Codex catalog
  // (e.g. ["none", "low", "medium", "high", "xhigh", "max"]). When omitted the
  // backend keeps the template's conservative none/high default.
  reasoningLevels?: string[];
  // Per-model default reasoning effort. Only meaningful together with
  // reasoningLevels; when omitted the backend keeps the template default if it
  // is still in the list, otherwise the highest declared level.
  defaultReasoningLevel?: string;
}

// Claude 认证字段类型
export type ClaudeApiKeyField = "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";

// 主页面显示的应用配置
export interface VisibleApps {
  claude: boolean;
  codex: boolean;
  pi: boolean;
}

// WebDAV 同步状态
export interface WebDavSyncStatus {
  lastSyncAt?: number | null;
  lastError?: string | null;
  lastErrorSource?: string | null;
  lastRemoteEtag?: string | null;
  lastLocalManifestHash?: string | null;
  lastRemoteManifestHash?: string | null;
  // 端到端加密的设备级序号（方案 2.4.4）
  lastAppliedSeq?: number | null;
  lastUploadedSeq?: number | null;
}

// WebDAV 同步配置
// 注意：password 只存在于 Windows 凭据管理器（SecretTarget::app("webdav","password")），
// 绝不回显、绝不随本结构提交；写入走 webdav_sync_save_settings 的独立参数。
export interface WebDavSyncSettings {
  enabled?: boolean;
  autoSync?: boolean;
  baseUrl?: string;
  username?: string;
  remoteRoot?: string;
  profile?: string;
  e2eEnabled?: boolean;
  allowInsecure?: boolean;
  status?: WebDavSyncStatus;
}

// S3 同步配置
// 注意：accessKeyId / secretAccessKey 同样只在凭据管理器里，不回显、不随本结构提交。
export interface S3SyncSettings {
  enabled?: boolean;
  autoSync?: boolean;
  region?: string;
  bucket?: string;
  endpoint?: string;
  remoteRoot?: string;
  profile?: string;
  e2eEnabled?: boolean;
  allowInsecure?: boolean;
  status?: WebDavSyncStatus;
}

// 端到端加密状态（sync_e2e_get_status）
export interface SyncE2eTransportStatus {
  e2eEnabled: boolean;
  allowInsecure: boolean;
  lastAppliedSeq?: number | null;
  lastUploadedSeq?: number | null;
}

export interface SyncE2eStatus {
  passphraseSet: boolean;
  webdav: SyncE2eTransportStatus;
  s3: SyncE2eTransportStatus;
}

export type RemoteSnapshotLayout = "current" | "legacy";

// 远端快照信息（下载前预览）
export interface RemoteSnapshotInfo {
  deviceName: string;
  createdAt: string;
  snapshotId: string;
  version: number;
  protocolVersion: number;
  dbCompatVersion?: number | null;
  compatible: boolean;
  artifacts: string[];
  layout: RemoteSnapshotLayout;
  remotePath: string;
}

// 应用设置类型（用于设置对话框与 Tauri API）
// 存储在本地 ~/.cc-switch/settings.json，不随数据库同步
export interface Settings {
  // ===== 设备级 UI 设置 =====
  // 是否在系统托盘（macOS 菜单栏）显示图标
  showInTray: boolean;
  // 点击关闭按钮时是否最小化到托盘而不是关闭应用
  minimizeToTrayOnClose: boolean;
  // 是否启用应用级窗口控制按钮（最小化/最大化/关闭）
  useAppWindowControls?: boolean;
  // 启用 Claude 插件联动（写入 ~/.claude/config.json 的 primaryApiKey）
  enableClaudePluginIntegration?: boolean;
  // 跳过 Claude Code 初次安装确认（写入 ~/.claude.json 的 hasCompletedOnboarding）
  skipClaudeOnboarding?: boolean;
  // 是否开机自启
  launchOnStartup?: boolean;
  // 静默启动（程序启动时不显示主窗口）
  silentStartup?: boolean;
  // Whether to show the project profile switcher on the main page header
  showProfileSwitcher?: boolean;
  // Run official Codex under the shared "custom" provider id so future
  // sessions share one resume-history bucket with third-party providers
  unifyCodexSessionHistory?: boolean;
  // User opted in (enable dialog checkbox) to migrate existing official sessions
  unifyCodexMigrateExisting?: boolean;
  // User has confirmed the first-run welcome notice
  firstRunNoticeConfirmed?: boolean;
  // User has confirmed the auto-sync traffic warning
  autoSyncConfirmed?: boolean;
  // User has confirmed the common config first-run notice
  commonConfigConfirmed?: boolean;
  // 首选语言（可选，默认中文）
  language?: "en" | "zh" | "zh-TW" | "ja";

  // 主页面显示的应用（默认全部显示）
  visibleApps?: VisibleApps;

  // ===== 设备级目录覆盖 =====
  // 覆盖 Claude Code 配置目录（可选）
  claudeConfigDir?: string;
  // 覆盖 Codex 配置目录（可选）
  codexConfigDir?: string;
  // 覆盖 Pi agent 配置目录（可选）
  piConfigDir?: string;

  // ===== 当前供应商 ID（设备级）=====
  // 当前 Claude 供应商 ID（优先于数据库 is_current）
  currentProviderClaude?: string;
  // 当前 Codex 供应商 ID（优先于数据库 is_current）
  currentProviderCodex?: string;

  // ===== Skill 同步设置 =====
  // Skill 同步方式：auto（默认，优先 symlink）、symlink、copy
  skillSyncMethod?: SkillSyncMethod;
  // Skill 存储位置：cc_switch（默认）或 unified（~/.agents/skills/）
  skillStorageLocation?: SkillStorageLocation;

  // ===== WebDAV v2 同步设置 =====
  webdavSync?: WebDavSyncSettings;

  // ===== S3 同步设置 =====
  s3Sync?: S3SyncSettings;

  // ===== 备份策略设置 =====
  // Auto-backup interval in hours (0=disabled, default 24)
  backupIntervalHours?: number;
  // Maximum backup files to retain (default 10)
  backupRetainCount?: number;

  // ===== 终端设置 =====
  // 首选终端应用（可选，默认使用系统默认终端）
  // macOS: "terminal" | "iterm2" | "warp" | "alacritty" | "kitty" | "ghostty" | "otty" | "wezterm" | "kaku"
  // Windows: "cmd" | "powershell" | "wt"
  // Linux: "gnome-terminal" | "konsole" | "xfce4-terminal" | "alacritty" | "kitty" | "ghostty"
  preferredTerminal?: string;

  // ===== 环境变量投递（B5 严格模式）=====
  // 开启后切换供应商不把密钥写入 HKCU\Environment，密钥仅经 cc-switch「打开终端」注入。
  envDeliveryStrictMode?: boolean;
  // P2 分级：全局关时，按应用严格的 app 子集（"claude" | "codex" | "pi"）。
  envDeliveryStrictApps?: string[] | null;

  // ===== 本机自动迁移状态 =====
  localMigrations?: {
    codexThirdPartyHistoryProviderBucketV1?: {
      completedAt: string;
      targetProviderId: string;
      sourceProviderIds?: string[];
      migratedJsonlFiles?: number;
      migratedStateRows?: number;
    };
  };
}

export interface SessionMeta {
  providerId: string;
  sessionId: string;
  title?: string;
  summary?: string;
  projectDir?: string | null;
  createdAt?: number;
  lastActiveAt?: number;
  sourcePath?: string;
  resumeCommand?: string;
}

export interface SessionMessage {
  role: string;
  content: string;
  ts?: number;
}

// MCP 服务器连接参数（宽松：允许扩展字段）
export interface McpServerSpec {
  // 可选：社区常见 .mcp.json 中 stdio 配置可不写 type
  type?: "stdio" | "http" | "sse";
  // stdio 字段
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
  // http 和 sse 字段
  url?: string;
  headers?: Record<string, string>;
  // 通用字段
  [key: string]: any;
}

// v3.7.0: MCP 服务器应用启用状态
export interface McpApps {
  claude: boolean;
  codex: boolean;
}

// MCP 服务器条目（v3.7.0 统一结构）
export interface McpServer {
  id: string;
  name: string;
  server: McpServerSpec;
  apps: McpApps; // v3.7.0: 标记应用到哪些客户端
  description?: string;
  tags?: string[];
  homepage?: string;
  docs?: string;
  // 兼容旧字段（v3.6.x 及以前）
  enabled?: boolean; // 已废弃，v3.7.0 使用 apps 字段
  source?: string;
  [key: string]: any;
}

// MCP 服务器映射（id -> McpServer）
export type McpServersMap = Record<string, McpServer>;

// MCP 配置状态
export interface McpStatus {
  userConfigPath: string;
  userConfigExists: boolean;
  serverCount: number;
}

// 新：来自 config.json 的 MCP 列表响应
export interface McpConfigResponse {
  configPath: string;
  servers: Record<string, McpServer>;
}
