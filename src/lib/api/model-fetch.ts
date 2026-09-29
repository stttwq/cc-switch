import { invoke } from "@tauri-apps/api/core";
import type { TFunction } from "i18next";
import { toast } from "sonner";

export interface FetchedModel {
  id: string;
  ownedBy: string | null;
}

export interface ModelFetchOptions {
  apiFormat?: string;
  requestHeaders?: Record<string, string>;
}

/**
 * 从供应商获取可用模型列表
 *
 * 使用 OpenAI 兼容的 GET /v1/models 端点。优先用 `modelsUrl` 精确覆写；
 * 否则后端会对 baseURL 生成候选列表并按序尝试（含"剥离 /anthropic 等兼容子路径"兜底）。
 */
export async function fetchModelsForConfig(
  baseUrl: string,
  apiKey: string,
  isFullUrl?: boolean,
  modelsUrl?: string,
  options?: ModelFetchOptions,
): Promise<FetchedModel[]> {
  return invoke("fetch_models_for_config", {
    baseUrl,
    apiKey,
    isFullUrl,
    modelsUrl,
    apiFormat: options?.apiFormat,
    requestHeaders: options?.requestHeaders,
  });
}

/**
 * 后端结构化错误载荷（SEC-C）
 *
 * 后端只返回稳定 code / retryable / status，不携带原始请求 URL。
 */
export interface ModelFetchErrorPayload {
  code: string;
  retryable?: boolean;
  status?: number;
}

function isModelFetchErrorPayload(err: unknown): err is ModelFetchErrorPayload {
  return (
    typeof err === "object" &&
    err !== null &&
    typeof (err as ModelFetchErrorPayload).code === "string"
  );
}

/** 后端 code → toast 文案；未知 code 走通用兜底 */
function showToastForCode(code: string, t: TFunction): void {
  switch (code) {
    case "auth_failed":
      toast.error(t("providerForm.fetchModelsAuthFailed"));
      return;
    // 单候选 404/405 或全部候选失败：供应商可能未开放 /models 接口
    case "endpoint_not_found":
    case "all_candidates_failed":
      toast.error(t("providerForm.fetchModelsEndpointNotFound"));
      return;
    case "timeout":
      toast.error(t("providerForm.fetchModelsTimeout"));
      return;
    case "parse_failed":
      toast.error(t("providerForm.fetchModelsNotSupported"));
      return;
    case "redirect_blocked":
      toast.error(t("providerForm.fetchModelsRedirectBlocked"));
      return;
    case "invalid_url":
    case "cross_origin_override":
      toast.error(t("providerForm.fetchModelsInvalidUrl"));
      return;
    default:
      toast.error(t("providerForm.fetchModelsFailed"));
  }
}

/** 迁移期兼容：旧后端返回英文明文错误串（SEC-C 之前的格式） */
function showToastForLegacyMessage(msg: string, t: TFunction): void {
  if (msg.includes("HTTP 401") || msg.includes("HTTP 403")) {
    toast.error(t("providerForm.fetchModelsAuthFailed"));
    return;
  }
  // 所有候选端点均返回 404/405：供应商可能未开放 /models 接口，或 Base URL 有误
  if (msg.includes("All candidates failed")) {
    toast.error(t("providerForm.fetchModelsEndpointNotFound"));
    return;
  }
  if (msg.includes("HTTP 404") || msg.includes("HTTP 405")) {
    toast.error(t("providerForm.fetchModelsEndpointNotFound"));
    return;
  }
  if (msg.includes("timeout") || msg.includes("timed out")) {
    toast.error(t("providerForm.fetchModelsTimeout"));
    return;
  }
  if (msg.includes("Failed to parse")) {
    toast.error(t("providerForm.fetchModelsNotSupported"));
    return;
  }
  toast.error(t("providerForm.fetchModelsFailed"));
}

/**
 * 根据错误类型显示对应的 toast 提示
 */
export function showFetchModelsError(
  err: unknown,
  t: TFunction,
  opts?: { hasApiKey: boolean; hasBaseUrl: boolean },
): void {
  // 前端预检：缺少必填字段
  if (opts && !opts.hasBaseUrl && !opts.hasApiKey) {
    toast.error(t("providerForm.fetchModelsNeedConfig"));
    return;
  }
  if (opts && !opts.hasApiKey) {
    toast.error(t("providerForm.fetchModelsNeedApiKey"));
    return;
  }
  if (opts && !opts.hasBaseUrl) {
    toast.error(t("providerForm.fetchModelsNeedEndpoint"));
    return;
  }

  // 后端结构化错误（SEC-C）：按 code 映射，不解析错误原文
  if (isModelFetchErrorPayload(err)) {
    showToastForCode(err.code, t);
    return;
  }

  // 迁移期兜底：旧后端的英文明文错误串
  showToastForLegacyMessage(String(err), t);
}
