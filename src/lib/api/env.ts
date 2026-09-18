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
 * 检查指定应用的环境变量冲突
 * @param appType 应用类型 ("claude" | "codex")
 * @returns 环境变量冲突列表（值已脱敏为末 4 位）
 */
export async function checkEnvConflicts(
  appType: string,
): Promise<EnvConflict[]> {
  return invoke<EnvConflict[]>("check_env_conflicts", { app: appType });
}

/**
 * 按名字删除选中的冲突环境变量（原值不做备份、不保留）
 * @param conflicts 要删除的环境变量冲突列表
 * @returns 实际删除的变量数量
 */
export async function deleteEnvVars(conflicts: EnvConflict[]): Promise<number> {
  return invoke<number>("delete_env_vars", { conflicts });
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
        results[app] = await checkEnvConflicts(app);
      } catch (error) {
        console.error(`检查 ${app} 环境变量失败:`, error);
        results[app] = [];
      }
    }),
  );

  return results;
}
