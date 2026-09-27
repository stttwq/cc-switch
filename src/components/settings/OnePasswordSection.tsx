import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Eraser, KeyRound, Loader2, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { settingsApi } from "@/lib/api/settings";
import { extractErrorMessage, toastVaultError } from "@/utils/errorUtils";
import { useTauriEvent } from "@/hooks/useTauriEvent";

interface OnePasswordStatus {
  installed: boolean;
  opPath: string | null;
  version: string | null;
  signedIn: boolean;
  signatureOk: boolean | null;
  backend: string;
  account: string | null;
  vault: string | null;
  verifySignature: boolean;
}

interface OpAccount {
  url: string;
  email: string;
  account_uuid: string;
}

interface OpVault {
  id: string;
  name: string;
}

/**
 * F5-3：vault_* 错误（锁定/断网/超时等）统一 toast + 「重试」；其余显示原始信息。
 * 模块级纯展示辅助，不依赖组件状态。
 */
const showVaultAwareError = (error: unknown, retry: () => void) => {
  if (toastVaultError(error, retry)) return;
  toast.error(extractErrorMessage(error) || String(error));
};

/**
 * 1Password 后端设置（§8）：显示状态（未安装/未登录/正常 + op 版本/路径/签名），
 * 选择 account/vault，测试取钥匙。真正切换后端在迁移向导里完成（P4）。
 */
export function OnePasswordSection() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<OnePasswordStatus | null>(null);
  const [accounts, setAccounts] = useState<OpAccount[]>([]);
  const [vaults, setVaults] = useState<OpVault[]>([]);
  const [account, setAccount] = useState<string>("");
  const [vault, setVault] = useState<string>("");
  const [verifySignature, setVerifySignature] = useState(true);
  const [restartFailed, setRestartFailed] = useState(false);
  const [migrateProgress, setMigrateProgress] = useState({ done: 0, total: 0 });
  const [rebuildProgress, setRebuildProgress] = useState({ done: 0, total: 0 });
  const [busy, setBusy] = useState<
    "status" | "accounts" | "vaults" | "save" | "test" | "migrate" | "cleanup" | "rebuild" | null
  >(null);

  // F2-3：已处于 1Password 后端时锁定 account / vault（运行中的 vault 不跟随设置变化，
  // 换 vault 会让所有已迁移条目变成孤儿）。
  const is1pActive = status?.backend === "onepassword";

  // F2-1：迁移进度（每组完成一个事件，约 7 秒/个）。
  useTauriEvent<{ done: number; total: number }>(
    "onepassword-migrate-progress",
    (payload) => {
      setMigrateProgress(payload);
    },
  );

  // F4-5：重建引用进度（N+1 次 op，逐条上报）。
  useTauriEvent<{ done: number; total: number }>(
    "onepassword-rebuild-refs-progress",
    (payload) => {
      setRebuildProgress(payload);
    },
  );

  // F4-5（D14）：从 1Password 重建 secret_refs——云同步恢复 / 换设备后引用会
  // 脱节（引用不随云同步）。用户显式动作，N+1 次 op（可能弹解锁）。
  const rebuildRefs = async () => {
    setBusy("rebuild");
    try {
      const r = await invoke<{ total: number; rebuilt: number; skipped: string[] }>(
        "onepassword_rebuild_refs",
      );
      toast.success(t("onepassword.rebuildRefsDone", r));
    } catch (error) {
      showVaultAwareError(error, () => void rebuildRefs());
    } finally {
      setBusy(null);
    }
  };

  const refreshStatus = useCallback(async () => {
    setBusy("status");
    try {
      const s = await invoke<OnePasswordStatus>("onepassword_status");
      setStatus(s);
      setVerifySignature(s.verifySignature);
      if (s.account) setAccount(s.account);
      if (s.vault) setVault(s.vault);
    } catch (error) {
      showVaultAwareError(error, () => void refreshStatus());
    } finally {
      setBusy(null);
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
  }, [refreshStatus]);

  const loadAccounts = async () => {
    setBusy("accounts");
    try {
      const list = await invoke<OpAccount[]>("onepassword_list_accounts");
      setAccounts(list);
      if (!account && list.length > 0) {
        setAccount(list[0].account_uuid || list[0].email);
      }
    } catch (error) {
      showVaultAwareError(error, () => void loadAccounts());
    } finally {
      setBusy(null);
    }
  };

  // 列 vault 需要解锁：会触发 1Password 授权弹窗。
  const loadVaults = async () => {
    setBusy("vaults");
    try {
      const list = await invoke<OpVault[]>("onepassword_list_vaults", {
        account: account || null,
      });
      setVaults(list);
      if (!vault && list.length > 0) setVault(list[0].id);
    } catch (error) {
      showVaultAwareError(error, () => void loadVaults());
    } finally {
      setBusy(null);
    }
  };

  const save = async () => {
    setBusy("save");
    try {
      await invoke("onepassword_save_config", {
        account: account || null,
        vault: vault || null,
        verifySignature,
      });
      toast.success(t("onepassword.saved"));
      await refreshStatus();
    } catch (error) {
      showVaultAwareError(error, () => void save());
    } finally {
      setBusy(null);
    }
  };

  const testFetch = async () => {
    setBusy("test");
    try {
      await invoke("onepassword_test_fetch");
      toast.success(t("onepassword.testOk"));
    } catch (error) {
      showVaultAwareError(error, () => void testFetch());
    } finally {
      setBusy(null);
    }
  };

  // F2-1：清理凭据管理器残留（迁移后 1Password 是唯一真源）。
  const cleanupResidue = async () => {
    if (!window.confirm(t("onepassword.cleanupConfirm"))) return;
    setBusy("cleanup");
    try {
      const r = await invoke<{ deleted: number; failed: number }>(
        "onepassword_cleanup_credential_residue",
      );
      if (r.failed > 0) {
        toast.warning(t("onepassword.cleanupDoneWithFailures", r));
      } else {
        toast.success(t("onepassword.cleanupDone", r));
      }
    } catch (error) {
      showVaultAwareError(error, () => void cleanupResidue());
    } finally {
      setBusy(null);
    }
  };

  const migrate = async () => {
    if (!window.confirm(t("onepassword.migrateConfirm"))) return;
    setBusy("migrate");
    try {
      const report = await invoke<{
        migratedGroups: number;
        migratedFields: number;
        deletedTargets: number;
      }>("onepassword_migrate");
      toast.success(
        t("onepassword.migrateDone", {
          groups: report.migratedGroups,
          fields: report.migratedFields,
        }),
      );
      // 迁移提交后运行中的旧后端已失效（F1-6），必须重启才能继续使用，
      // 不再提供「稍后」选项；重启失败时显示常驻横幅。
      try {
        await settingsApi.restart();
      } catch {
        setRestartFailed(true);
      }
    } catch (error) {
      showVaultAwareError(error, () => void migrate());
    } finally {
      setBusy(null);
    }
  };

  const statusLine = () => {
    if (!status) return t("onepassword.status.checking");
    if (!status.installed) return t("onepassword.status.notInstalled");
    if (!status.signedIn) return t("onepassword.status.notSignedIn");
    return t("onepassword.status.ready");
  };

  return (
    <div className="space-y-4">
      {restartFailed && (
        <div className="rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
          {t("onepassword.restartRequiredBanner")}
        </div>
      )}
      <p className="text-sm text-muted-foreground">{t("onepassword.hint")}</p>

      <div className="rounded-lg border border-border/50 p-3 space-y-1 text-sm">
        <div className="flex items-center justify-between">
          <span className="font-medium">{statusLine()}</span>
          <Button
            variant="ghost"
            size="sm"
            disabled={busy !== null}
            onClick={refreshStatus}
          >
            {busy === "status" ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <RefreshCw className="h-4 w-4" />
            )}
          </Button>
        </div>
        {status?.version && (
          <p className="text-xs text-muted-foreground">
            {t("onepassword.version")}: {status.version}
          </p>
        )}
        {status?.opPath && (
          <p className="text-xs text-muted-foreground break-all">
            {t("onepassword.path")}: {status.opPath}
          </p>
        )}
        {status?.signatureOk === false && (
          <p className="text-xs text-destructive">
            {t("onepassword.signatureFailed")}
          </p>
        )}
      </div>

      {/* 账户 */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <label className="text-sm font-medium">
            {t("onepassword.account")}
          </label>
          <Button
            variant="outline"
            size="sm"
            disabled={busy !== null || !status?.installed || is1pActive}
            onClick={loadAccounts}
          >
            {busy === "accounts" ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            {t("onepassword.loadAccounts")}
          </Button>
        </div>
        <select
          className="w-full rounded-md border border-border bg-background px-3 py-2 text-sm disabled:opacity-60"
          value={account}
          disabled={is1pActive}
          onChange={(e) => setAccount(e.target.value)}
        >
          <option value="">{t("onepassword.selectAccount")}</option>
          {accounts.map((a) => (
            <option key={a.account_uuid || a.email} value={a.account_uuid || a.email}>
              {a.email} ({a.url})
            </option>
          ))}
          {account && !accounts.some((a) => (a.account_uuid || a.email) === account) && (
            <option value={account}>{account}</option>
          )}
        </select>
        {is1pActive && (
          <p className="text-xs text-muted-foreground">
            {t("onepassword.accountVaultLocked")}
          </p>
        )}
      </div>

      {/* Vault（需解锁） */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <label className="text-sm font-medium">
            {t("onepassword.vault")}
          </label>
          <Button
            variant="outline"
            size="sm"
            disabled={busy !== null || !account || is1pActive}
            onClick={loadVaults}
          >
            {busy === "vaults" ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            {t("onepassword.loadVaults")}
          </Button>
        </div>
        <select
          className="w-full rounded-md border border-border bg-background px-3 py-2 text-sm disabled:opacity-60"
          value={vault}
          disabled={is1pActive}
          onChange={(e) => setVault(e.target.value)}
        >
          <option value="">{t("onepassword.selectVault")}</option>
          {vaults.map((v) => (
            <option key={v.id} value={v.id}>
              {v.name}
            </option>
          ))}
          {vault && !vaults.some((v) => v.id === vault) && (
            <option value={vault}>{vault}</option>
          )}
        </select>
        <p className="text-xs text-muted-foreground">
          {t("onepassword.vaultUnlockHint")}
        </p>
      </div>

      {/* 签名校验 */}
      <label className="flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={verifySignature}
          onChange={(e) => setVerifySignature(e.target.checked)}
        />
        {t("onepassword.verifySignature")}
      </label>

      <div className="flex flex-wrap gap-2">
        <Button size="sm" disabled={busy !== null} onClick={save}>
          {busy === "save" ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : null}
          {t("common.save")}
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={busy !== null || !account || !vault}
          onClick={testFetch}
        >
          {busy === "test" ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : (
            <KeyRound className="mr-2 h-4 w-4" />
          )}
          {t("onepassword.testFetch")}
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={
            busy !== null ||
            !account ||
            !vault ||
            status?.backend === "onepassword"
          }
          onClick={migrate}
        >
          {busy === "migrate" ? (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          ) : null}
          {t("onepassword.migrate")}
        </Button>
        {/* F2-1：迁移后清理凭据管理器残留（1Password 是唯一真源）。 */}
        {is1pActive && (
          <>
            <Button
              variant="outline"
              size="sm"
              disabled={busy !== null}
              onClick={cleanupResidue}
            >
              {busy === "cleanup" ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <Eraser className="mr-2 h-4 w-4" />
              )}
              {t("onepassword.cleanupResidue")}
            </Button>
            {/* F4-5（D14）：换设备 / 云同步恢复后重建 secret_refs。 */}
            <Button
              variant="outline"
              size="sm"
              disabled={busy !== null}
              onClick={rebuildRefs}
            >
              {busy === "rebuild" ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : null}
              {t("onepassword.rebuildRefs")}
            </Button>
          </>
        )}
      </div>
      {busy === "migrate" && migrateProgress.total > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("onepassword.migrateProgress", migrateProgress)}
        </p>
      )}
      {busy === "rebuild" && rebuildProgress.total > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("onepassword.rebuildRefsProgress", rebuildProgress)}
        </p>
      )}
      {/* F5-4：可能触发 op 的操作进行中，统一提示可能弹解锁。 */}
      {(busy === "accounts" ||
        busy === "vaults" ||
        busy === "test" ||
        busy === "migrate" ||
        busy === "rebuild") && (
        <p className="text-xs text-muted-foreground">
          {t("onepassword.requesting")}
        </p>
      )}
    </div>
  );
}
