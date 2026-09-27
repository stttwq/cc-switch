import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { settingsApi } from "@/lib/api/settings";
import type { Settings } from "@/types";
import { toastVaultError } from "@/utils/errorUtils";

/**
 * F5-5：1Password 模式下的明文待处理提示横幅。
 *
 * 后端在 1P 模式检测到无法立即安全收进 vault 的明文时不丢弃（原则 3），而是
 * 记入本机设置（settings.json，不随云同步）并在此提示：
 * - `piPlaintextPending`：Pi `models.json` 里的明文 apiKey（F1-4），点「导入」
 *   收进 vault 并把 `models.json` 改写为引用；
 * - `livePlaintextPending`：live 文件里的明文钥匙（F1-8），点「导入」收进
 *   vault 并就地剥离；
 * - `secretsImportPending`：导入/恢复时写 vault 失败、明文暂留 DB 的行
 *   （F1-5），需要解锁 1Password 后重新执行一次导入。
 *
 * 状态读取是纯本机查询（0 次 op）；「导入」按钮会触发 op（可能弹解锁）。
 */
export function PlaintextPendingBanner() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<Settings | null>(null);
  const [importing, setImporting] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setSettings(await settingsApi.get());
    } catch {
      // 设置读取失败不打扰用户（导入动作本身会报错）
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const importAll = async () => {
    setImporting(true);
    try {
      const counts: number[] = [];
      if ((settings?.livePlaintextPending?.length ?? 0) > 0) {
        counts.push(
          await invoke<number>("import_live_plaintext_to_onepassword"),
        );
      }
      if ((settings?.piPlaintextPending?.length ?? 0) > 0) {
        counts.push(await invoke<number>("import_pi_plaintext_to_onepassword"));
      }
      toast.success(
        t("onepassword.plaintextImportDone", {
          count: counts.reduce((a, b) => a + b, 0),
        }),
      );
      await refresh();
    } catch (error) {
      // F5-3：vault_* 错误（如 1Password 锁定）统一 toast + 「重试」。
      if (toastVaultError(error, () => void importAll())) return;
      toast.error(String(error));
    } finally {
      setImporting(false);
    }
  };

  if (!settings || settings.secretBackend !== "onepassword") {
    return null;
  }
  const piCount = settings.piPlaintextPending?.length ?? 0;
  const liveCount = settings.livePlaintextPending?.length ?? 0;
  const importPendingCount = settings.secretsImportPending?.length ?? 0;
  if (piCount + liveCount + importPendingCount === 0) {
    return null;
  }

  return (
    <div className="mx-4 mt-2 flex items-center justify-between gap-3 rounded-lg border border-border bg-muted/40 px-4 py-2 text-sm">
      <span>
        {importing
          ? t("onepassword.requesting")
          : t("onepassword.plaintextPending", {
              pi: piCount,
              live: liveCount,
              imports: importPendingCount,
            })}
      </span>
      <div className="flex shrink-0 items-center gap-2">
        {importing ? (
          <Loader2 className="h-4 w-4 animate-spin" />
        ) : (
          (piCount > 0 || liveCount > 0) && (
            <Button size="sm" variant="outline" onClick={importAll}>
              {t("onepassword.plaintextImportAction")}
            </Button>
          )
        )}
      </div>
    </div>
  );
}
