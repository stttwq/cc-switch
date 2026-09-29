import { describe, expect, it } from "vitest";
import {
  isPhaseAfterVaultCommitError,
  parseVaultError,
} from "@/utils/errorUtils";

describe("isPhaseAfterVaultCommitError（P5 §9.2-6 阶段化重试）", () => {
  it("识别 vault_saved_local_failed 阶段错误", () => {
    const error = new Error(
      JSON.stringify({
        code: "vault_saved_local_failed",
        message: "1Password 已更新，但本地保存失败",
        messageEn: "1Password was updated, but saving locally failed",
      }),
    );
    expect(isPhaseAfterVaultCommitError(error)).toBe(true);
    expect(parseVaultError(error)?.code).toBe("vault_saved_local_failed");
  });

  it("识别 vault_saved_pending_live 阶段错误", () => {
    const error = new Error(
      JSON.stringify({
        code: "vault_saved_pending_live",
        message: "配置已保存，但应用 live 配置失败",
        messageEn: "Configuration saved, but applying the live config failed",
      }),
    );
    expect(isPhaseAfterVaultCommitError(error)).toBe(true);
  });

  it("识别被文案包裹的阶段错误 JSON", () => {
    const error = new Error(
      '保存供应商失败: {"code":"vault_saved_pending_live","message":"配置已保存","messageEn":"saved"}',
    );
    expect(isPhaseAfterVaultCommitError(error)).toBe(true);
  });

  it("vault_locked 等普通 vault 错误不是阶段错误（重试保留原凭据意图）", () => {
    const error = new Error(
      JSON.stringify({
        code: "vault_locked",
        message: "1Password 已锁定",
        messageEn: "1Password is locked",
      }),
    );
    expect(isPhaseAfterVaultCommitError(error)).toBe(false);
    expect(parseVaultError(error)?.code).toBe("vault_locked");
  });

  it("普通文本错误不是阶段错误", () => {
    expect(isPhaseAfterVaultCommitError(new Error("boom"))).toBe(false);
    expect(isPhaseAfterVaultCommitError(null)).toBe(false);
  });
});
