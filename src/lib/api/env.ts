import { invoke } from "@tauri-apps/api/core";
import type { EnvConflict } from "@/types/env";

/**
 * 环境变量管理 API
 */

export interface DeliveryConflict {
  name: string;
  owner: string;
  maskedValue: string;
}

/**
 * 列出切换该供应商会撞上的外来环境变量。
 */
export async function envDeliveryConflicts(
  app: string,
  providerId: string,
): Promise<DeliveryConflict[]> {
  return invoke<DeliveryConflict[]>("env_delivery_conflicts", {
    app,
    providerId,
  });
}

/**
 * 接管外来环境变量（登记为 cc-switch 托管）。
 */
export async function envDeliveryAdopt(
  app: string,
  providerId: string,
  names: string[],
): Promise<void> {
  return invoke<void>("env_delivery_adopt", { app, providerId, names });
}

/**
 * 从切换失败的错误里解析 ENV_CONFLICT。
 */
export function parseEnvConflictError(
  error: unknown,
): DeliveryConflict[] | null {
  const text =
    typeof error === "string"
      ? error
      : error instanceof Error
        ? error.message
        : "";
  if (!text.includes("ENV_CONFLICT")) return null;
  try {
    const parsed = JSON.parse(text) as {
      code?: string;
      conflicts?: DeliveryConflict[];
    };
    if (parsed?.code === "ENV_CONFLICT") {
      return parsed.conflicts ?? [];
    }
  } catch {
    return null;
  }
  return null;
}

/**
 * 切换失败时发布的「环境变量冲突」提示（§5.3.3）。
 *
 * 这里同时充当任务书授权的模块级订阅桥：
 * - 生产者：useSwitchProviderMutation 的 onError 解析到 ENV_CONFLICT 后调用
 *   publishEnvConflictPrompt()，连同「重试切换」闭包一起登记；
 * - 消费者：顶层挂载的 EnvConflictDialogHost 通过 useSyncExternalStore
 *   订阅 getEnvConflictPromptSnapshot()/subscribeEnvConflictPrompt()，
 *   渲染对话框让用户在「接管并切换」与「取消切换」间显式选择。
 */
export interface EnvConflictPrompt {
  /** 目标应用（claude / codex），env_delivery_adopt 入参 */
  app: string;
  /** 切换目标供应商 ID */
  providerId: string;
  /** 冲突列表（契约上只有末 4 位 maskedValue，没有完整值） */
  conflicts: DeliveryConflict[];
  /** 接管成功后重跑本次切换 */
  retry: () => void;
  /** 接管成功后由对话框回调：宿主清空提示（clearEnvConflictPrompt） */
  onAdopted: () => void;
  /** 用户取消：宿主清空提示（clearEnvConflictPrompt），放弃本次切换 */
  onCancel: () => void;
}

let currentPrompt: EnvConflictPrompt | null = null;
const promptListeners = new Set<() => void>();

function emitPromptChange() {
  promptListeners.forEach((listener) => listener());
}

/** 订阅冲突提示变化，返回退订函数 */
export function subscribeEnvConflictPrompt(listener: () => void): () => void {
  promptListeners.add(listener);
  return () => {
    promptListeners.delete(listener);
  };
}

/** 读取当前冲突提示（useSyncExternalStore 的 getSnapshot） */
export function getEnvConflictPromptSnapshot(): EnvConflictPrompt | null {
  return currentPrompt;
}

/** 生产者入口：切换失败解析出冲突时发布提示 */
export function publishEnvConflictPrompt(prompt: EnvConflictPrompt): void {
  currentPrompt = prompt;
  emitPromptChange();
}

/** 用户「取消」或「接管并切换」后清空提示 */
export function clearEnvConflictPrompt(): void {
  if (currentPrompt === null) return;
  currentPrompt = null;
  emitPromptChange();
}

/**
 * 检查指定应用的环境变量冲突
 * @param appType 应用类型 ("claude" | "codex")
 * @returns 环境变量冲突列表（值已脱敏为末 4 位）
 */
export async function envDeliveryScan(appType: string): Promise<EnvConflict[]> {
  return invoke<EnvConflict[]>("env_delivery_scan", { app: appType });
}

/**
 * 按名字删除选中的冲突环境变量（原值不做备份、不保留）
 * @param conflicts 要删除的环境变量冲突列表
 * @returns 实际删除的变量数量
 */
export async function envDeliveryRemove(
  conflicts: EnvConflict[],
): Promise<number> {
  return invoke<number>("env_delivery_remove", { conflicts });
}

/**
 * 检查所有应用的环境变量冲突
 * @returns 按应用类型分组的环境变量冲突
 */
export async function checkAllEnvConflicts(): Promise<
  Record<string, EnvConflict[]>
> {
  const apps = ["claude", "codex"];
  const results: Record<string, EnvConflict[]> = {};

  await Promise.all(
    apps.map(async (app) => {
      try {
        results[app] = await envDeliveryScan(app);
      } catch (error) {
        console.error(`检查 ${app} 环境变量失败:`, error);
        results[app] = [];
      }
    }),
  );

  return results;
}
