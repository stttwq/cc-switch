import i18next from "i18next";
import { toast } from "sonner";

/**
 * 后端 vault_* 错误（F5-3）：后端把这些错误渲染成 JSON 字符串
 * `{"code":"vault_locked","message":"…"}`，前端按 code 映射 i18n
 * `vault.errors.<code>` 并提供「重试」。
 */
export interface VaultErrorInfo {
  code: string;
  message: string;
}

/** 从已提取的错误文本里解析 vault_* 结构化错误；未命中返回 null。 */
const parseVaultErrorText = (text: string): VaultErrorInfo | null => {
  if (!text.includes('"vault_')) return null;
  // 多数路径直接返回 JSON；少数路径会把 JSON 再包进中文提示里
  // （如「打开终端任务失败: {…}」），所以先整体 parse，失败再正则提取。
  const candidates = [
    text,
    /\{[^{}]*"code"\s*:\s*"vault_[a-z_]+"[^{}]*\}/.exec(text)?.[0] ?? "",
  ];
  for (const candidate of candidates) {
    if (!candidate) continue;
    try {
      const parsed = JSON.parse(candidate) as {
        code?: string;
        message?: string;
        messageEn?: string;
      };
      if (
        typeof parsed?.code === "string" &&
        parsed.code.startsWith("vault_")
      ) {
        return {
          code: parsed.code,
          message: parsed.messageEn || parsed.message || "",
        };
      }
    } catch {
      // 不是 JSON（或被截断），继续下一个候选
    }
  }
  return null;
};

/** 识别错误对象里的 vault_* 结构化错误；未命中返回 null。
 * 注意必须解析**原始**文本（extractRawErrorMessage）：extractErrorMessage
 * 会把 vault_* JSON 翻译成本地化文案，翻译后的文本里已经没有 code 了。 */
export const parseVaultError = (error: unknown): VaultErrorInfo | null => {
  if (!error) return null;
  return parseVaultErrorText(extractRawErrorMessage(error));
};

/** vault_* 错误码 → 本地化文案（vault.errors.<code>，缺失时回退后端消息）。
 * 用 i18next 全局单例（与 @/i18n 配置的是同一个实例），避免测试里对
 * react-i18next 的 mock 被 @/i18n 的模块初始化踩到。 */
export const translateVaultError = (info: VaultErrorInfo): string =>
  i18next.t(`vault.errors.${info.code}`, {
    defaultValue: info.message || info.code,
  });

/**
 * 从各种错误对象中提取错误信息
 * @param error 错误对象
 * @returns 提取的错误信息字符串
 */
export const extractErrorMessage = (error: unknown): string => {
  const raw = extractRawErrorMessage(error);
  const vault = parseVaultErrorText(raw);
  if (vault) {
    return translateVaultError(vault);
  }
  return raw;
};

/** S5-1：后端 sync.remote_ahead 上传冲突（远端有本机未下载的更新）。
 * 与 vault_* 相同的 JSON-in-string 约定，前端据此弹「先下载 / 强制覆盖」冲突框。 */
export interface RemoteAheadErrorInfo {
  code: string;
  message: string;
}

/** 从错误对象里解析 sync.remote_ahead 结构化错误；未命中返回 null。 */
export const parseRemoteAheadError = (
  error: unknown,
): RemoteAheadErrorInfo | null => {
  if (!error) return null;
  const raw = extractRawErrorMessage(error);
  if (!raw.includes('"sync.remote_ahead"')) return null;
  try {
    const parsed = JSON.parse(raw) as {
      code?: string;
      message?: string;
      messageEn?: string;
    };
    if (parsed?.code === "sync.remote_ahead") {
      return {
        code: parsed.code,
        message: parsed.messageEn || parsed.message || "",
      };
    }
  } catch {
    // 非 JSON（或被包装后截断），按未命中处理
  }
  return null;
};

const extractRawErrorMessage = (error: unknown): string => {
  if (!error) return "";
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }

  if (typeof error === "object") {
    const errObject = error as Record<string, unknown>;

    const candidate = errObject.message ?? errObject.error ?? errObject.detail;
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate;
    }

    const payload = errObject.payload;
    if (typeof payload === "string" && payload.trim()) {
      return payload;
    }
    if (payload && typeof payload === "object") {
      const payloadObj = payload as Record<string, unknown>;
      const payloadCandidate =
        payloadObj.message ?? payloadObj.error ?? payloadObj.detail;
      if (typeof payloadCandidate === "string" && payloadCandidate.trim()) {
        return payloadCandidate;
      }
    }
  }

  return "";
};

/**
 * vault_* 错误的统一 toast：本地化文案 + 可选「重试」按钮（F5-3）。
 * 命中时返回 true，调用方应跳过原有错误 toast，避免重复弹。
 */
export const toastVaultError = (
  error: unknown,
  retry?: () => void,
): boolean => {
  const info = parseVaultError(error);
  if (!info) return false;
  const label = i18next.t("vault.errors.retry");
  toast.error(translateVaultError(info), {
    action: retry ? { label, onClick: retry } : undefined,
  });
  return true;
};

export const translatePiProviderMutationError = (
  message: string,
  t: (key: string, options?: Record<string, unknown>) => string,
): string => {
  if (!message) return "";

  if (
    message.includes("models.json changed") ||
    message.includes("changed outside CC Switch") ||
    message.includes("no longer present in models.json") ||
    message.includes("another value now owns the key")
  ) {
    return t("pi.provider.writeConflict");
  }

  if (message.includes("Pi provider") && message.includes("already exists")) {
    return t("pi.form.providerKeyDuplicate");
  }

  return "";
};

/**
 * 将已知的 MCP 相关后端错误（通常为中文硬编码）映射为 i18n 文案
 * 采用包含式匹配，尽量稳健地覆盖不同上下文的相似消息。
 * 若无法识别，返回空字符串以便调用方回退到原始 detail 或默认 i18n。
 */
export const translateMcpBackendError = (
  message: string,
  t: (key: string, opts?: any) => string,
): string => {
  if (!message) return "";
  const msg = String(message).trim();

  // 基础字段与结构校验相关
  if (msg.includes("MCP 服务器 ID 不能为空")) {
    return t("mcp.error.idRequired");
  }
  if (
    msg.includes("MCP 服务器定义必须为 JSON 对象") ||
    msg.includes("MCP 服务器条目必须为 JSON 对象") ||
    msg.includes("MCP 服务器条目缺少 server 字段") ||
    msg.includes("MCP 服务器 server 字段必须为 JSON 对象") ||
    msg.includes("MCP 服务器连接定义必须为 JSON 对象") ||
    msg.includes("MCP 服务器 '" /* 不是对象 */) ||
    msg.includes("不是对象") ||
    msg.includes("服务器配置必须是对象") ||
    msg.includes("MCP 服务器 name 必须为字符串") ||
    msg.includes("MCP 服务器 description 必须为字符串") ||
    msg.includes("MCP 服务器 homepage 必须为字符串") ||
    msg.includes("MCP 服务器 docs 必须为字符串") ||
    msg.includes("MCP 服务器 tags 必须为字符串数组") ||
    msg.includes("MCP 服务器 enabled 必须为布尔值")
  ) {
    return t("mcp.error.jsonInvalid");
  }
  if (msg.includes("MCP 服务器 type 必须是")) {
    return t("mcp.error.jsonInvalid");
  }

  // 必填字段
  if (
    msg.includes("stdio 类型的 MCP 服务器缺少 command 字段") ||
    msg.includes("必须包含 command 字段")
  ) {
    return t("mcp.error.commandRequired");
  }
  if (
    msg.includes("http 类型的 MCP 服务器缺少 url 字段") ||
    msg.includes("sse 类型的 MCP 服务器缺少 url 字段") ||
    msg.includes("必须包含 url 字段") ||
    msg === "URL 不能为空"
  ) {
    return t("mcp.wizard.urlRequired");
  }

  // 文件解析/序列化
  if (
    msg.includes("解析 ~/.claude.json 失败") ||
    msg.includes("解析 config.toml 失败") ||
    msg.includes("无法识别的 TOML 格式") ||
    msg.includes("TOML 内容不能为空")
  ) {
    return t("mcp.error.tomlInvalid");
  }
  if (msg.includes("序列化 config.toml 失败")) {
    return t("mcp.error.tomlInvalid");
  }

  return "";
};
