import { describe, expect, it, vi } from "vitest";
import {
  extractErrorMessage,
  translatePiProviderMutationError,
} from "@/utils/errorUtils";

describe("error utilities", () => {
  it("extracts Tauri string errors", () => {
    expect(extractErrorMessage("backend failed")).toBe("backend failed");
  });

  it("maps a simultaneous models.json write to a concise error", () => {
    const t = vi.fn((key: string) => key);

    expect(
      translatePiProviderMutationError(
        "Pi models.json changed outside CC Switch",
        t,
      ),
    ).toBe("pi.provider.writeConflict");
  });

  it("maps a duplicate Pi provider key to validation feedback", () => {
    const t = vi.fn((key: string) => key);

    expect(
      translatePiProviderMutationError(
        "无效输入: Pi provider key 'duplicate' already exists in models.json",
        t,
      ),
    ).toBe("pi.form.providerKeyDuplicate");
  });

  it("parses vault_* JSON and re-uses the same info from raw or translated text", async () => {
    const { parseVaultError, toastVaultError } = await import(
      "@/utils/errorUtils"
    );
    const payload = JSON.stringify({
      code: "vault_locked",
      message: "1Password 已锁定、未运行或授权被取消",
      messageEn: "1Password is locked",
    });

    // 原始 JSON 字符串（Tauri 命令直返）
    expect(parseVaultError(payload)?.code).toBe("vault_locked");
    // 被中文提示包装过的 JSON（spawn_blocking 外层 map_err）
    expect(
      parseVaultError(`打开终端任务失败: ${payload}`)?.code,
    ).toBe("vault_locked");
    // 非 vault 错误不误判
    expect(parseVaultError("供应商不存在: default")).toBeNull();
    // toastVaultError 命中后返回 true 且弹带重试的 toast
    const { toast } = await import("sonner");
    const spy = vi.spyOn(toast, "error").mockReturnValue("id" as never);
    expect(toastVaultError(payload, () => {})).toBe(true);
    expect(spy).toHaveBeenCalledTimes(1);
    spy.mockRestore();
  });
});
