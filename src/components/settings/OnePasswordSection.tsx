import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { KeyRound, Loader2, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";

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
  const [busy, setBusy] = useState<
    "status" | "accounts" | "vaults" | "save" | "test" | "migrate" | null
  >(null);

  const refreshStatus = useCallback(async () => {
    setBusy("status");
    try {
      const s = await invoke<OnePasswordStatus>("onepassword_status");
      setStatus(s);
      setVerifySignature(s.verifySignature);
      if (s.account) setAccount(s.account);
      if (s.vault) setVault(s.vault);
    } catch (error) {
      toast.error(String(error));
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
      toast.error(String(error));
    } finally {
      setBusy(null);
    }
  };

  // 列 vault 需要解锁：会触发 1Password 授权弹窗。
  const loadVaults = async () => {
    setBusy("vaults");
    try {
      const list = await invoke<OpVault[]>("onepassword_list_vaults");
      setVaults(list);
      if (!vault && list.length > 0) setVault(list[0].id);
    } catch (error) {
      toast.error(String(error));
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
      toast.error(String(error));
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
      toast.error(String(error));
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
      await refreshStatus();
    } catch (error) {
      toast.error(String(error));
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
            disabled={busy !== null || !status?.installed}
            onClick={loadAccounts}
          >
            {busy === "accounts" ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            {t("onepassword.loadAccounts")}
          </Button>
        </div>
        <select
          className="w-full rounded-md border border-border bg-background px-3 py-2 text-sm"
          value={account}
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
            disabled={busy !== null || !account}
            onClick={loadVaults}
          >
            {busy === "vaults" ? (
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            ) : null}
            {t("onepassword.loadVaults")}
          </Button>
        </div>
        <select
          className="w-full rounded-md border border-border bg-background px-3 py-2 text-sm"
          value={vault}
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
      </div>
    </div>
  );
}
