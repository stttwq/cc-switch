import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
import { settingsApi } from "@/lib/api/settings";
import type { SyncE2eStatus } from "@/types";

type Transport = "webdav" | "s3";

/**
 * 端到端同步加密（2.1 方案第 2 部分 / P3）。挂在"设置 → 高级 → 云同步"下方。
 *
 * 口令只写进 Windows 凭据管理器、永不上传；忘口令 = 远端不可恢复（红字提示）。
 * 这里只翻开关与设口令；生成加密快照由用户在同步区手动点"上传"完成。
 */
export function SyncEncryptionSection() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<SyncE2eStatus | null>(null);
  const [passphrase, setPassphrase] = useState("");
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await settingsApi.syncE2eGetStatus());
    } catch (error) {
      console.error("[SyncEncryptionSection]", error);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const savePassphrase = async () => {
    if (!passphrase) return;
    setBusy(true);
    try {
      await settingsApi.syncE2eSetPassphrase(passphrase);
      setPassphrase("");
      toast.success(t("settings.syncEncryption.passphraseSaved"));
      await refresh();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (transport: Transport, enabled: boolean) => {
    setBusy(true);
    try {
      await settingsApi.syncE2eSetEnabled(transport, enabled);
      toast.success(
        enabled
          ? t("settings.syncEncryption.enabledToast")
          : t("settings.syncEncryption.disabledToast"),
      );
      await refresh();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  const toggleInsecure = async (transport: Transport, allow: boolean) => {
    setBusy(true);
    try {
      await settingsApi.syncE2eSetEnabled(
        transport,
        status?.[transport].e2eEnabled ?? false,
        allow,
      );
      await refresh();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  const rows: Array<{
    transport: Transport;
    label: string;
    on: boolean;
    insecure: boolean;
  }> = [
    {
      transport: "webdav",
      label: t("settings.syncEncryption.webdav"),
      on: status?.webdav.e2eEnabled ?? false,
      insecure: status?.webdav.allowInsecure ?? false,
    },
    {
      transport: "s3",
      label: t("settings.syncEncryption.s3"),
      on: status?.s3.e2eEnabled ?? false,
      insecure: status?.s3.allowInsecure ?? false,
    },
  ];

  return (
    <div className="mt-4 border-t border-border/50 pt-4 space-y-4">
      <div>
        <h4 className="text-sm font-semibold">
          {t("settings.syncEncryption.title")}
        </h4>
        <p className="text-xs text-muted-foreground">
          {t("settings.syncEncryption.description")}
        </p>
      </div>

      {/* 口令：内联输入框，保存到凭据管理器；永不回显 */}
      <div className="space-y-1">
        <div className="flex items-center gap-2">
          <Input
            type="password"
            value={passphrase}
            onChange={(e) => setPassphrase(e.target.value)}
            placeholder={
              status?.passphraseSet
                ? t("settings.syncEncryption.passphraseUpdatePlaceholder")
                : t("settings.syncEncryption.passphrasePlaceholder")
            }
            className="max-w-xs"
            autoComplete="new-password"
          />
          <Button
            variant="outline"
            size="sm"
            disabled={busy || !passphrase}
            onClick={savePassphrase}
          >
            {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {t("settings.syncEncryption.savePassphrase")}
          </Button>
        </div>
        <p className="text-xs text-destructive">
          {status?.passphraseSet
            ? t("settings.syncEncryption.passphraseSetWarning")
            : t("settings.syncEncryption.passphraseRequiredWarning")}
        </p>
      </div>

      {/* 两传输各自的加密开关 + 允许不安全连接 */}
      {rows.map((row) => (
        <div
          key={row.transport}
          className="flex items-center justify-between gap-4"
        >
          <div className="text-left">
            <span className="text-sm">{row.label}</span>
            <p className="text-xs text-muted-foreground">
              {t("settings.syncEncryption.allowInsecure")}
            </p>
          </div>
          <div className="flex items-center gap-4">
            <label className="flex items-center gap-2 text-xs text-muted-foreground">
              <Switch
                checked={row.insecure}
                onCheckedChange={(checked: boolean) =>
                  toggleInsecure(row.transport, checked)
                }
                disabled={busy}
                aria-label={t("settings.syncEncryption.allowInsecure")}
              />
            </label>
            <Switch
              checked={row.on}
              onCheckedChange={(checked: boolean) =>
                toggle(row.transport, checked)
              }
              disabled={busy}
              aria-label={t("settings.syncEncryption.title")}
            />
          </div>
        </div>
      ))}

      <p className="text-xs text-muted-foreground">
        {t("settings.syncEncryption.migrateHint")}
      </p>
    </div>
  );
}
