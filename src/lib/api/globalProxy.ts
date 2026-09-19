/**
 * 全局出站代理 API
 *
 * 提供获取、设置和测试全局代理的功能。
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * 代理测试结果
 */
export interface ProxyTestResult {
  success: boolean;
  latencyMs: number;
  error: string | null;
}

/**
 * 获取全局代理 URL
 *
 * @returns 代理 URL，null 表示未配置（直连）
 */
export async function getGlobalProxyUrl(): Promise<string | null> {
  return invoke<string | null>("get_global_proxy_url");
}

/**
 * 设置全局代理 URL
 *
 * @param url - 代理 URL（如 http://127.0.0.1:7890 或 socks5://127.0.0.1:1080）
 *              空字符串表示清除代理（直连）
 */
export async function setGlobalProxyUrl(url: string): Promise<void> {
  try {
    return await invoke("set_global_proxy_url", { url });
  } catch (error) {
    // Tauri invoke 错误可能是字符串
    throw new Error(typeof error === "string" ? error : String(error));
  }
}

/**
 * 测试代理连接
 *
 * @param url - 要测试的代理 URL
 * @returns 测试结果，包含是否成功、延迟和错误信息
 */
export async function testProxyUrl(url: string): Promise<ProxyTestResult> {
  return invoke<ProxyTestResult>("test_proxy_url", { url });
}
