import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";

interface LiveReapplyStatus {
  pending: boolean;
  failures: string[];
}

/**
 * §6.5：一次性迁移对话框被「知道了」之后，live 重写失败就再也没有入口了。
 * 这一行常驻在设置 → 高级，有待补项时列出失败原因并给「立即重试」。
 */
export function LiveReapplyMaintenance() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<LiveReapplyStatus | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setStatus(await invoke<LiveReapplyStatus>("get_live_reapply_status"));
    } catch (error) {
      console.error("[LiveReapplyMaintenance]", error);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (!status?.pending || status.failures.length === 0) {
    return null;
  }

  const retry = async () => {
    setBusy(true);
    try {
      const remaining = await invoke<string[]>("run_live_reapply_now");
      if (remaining.length === 0) {
        toast.success(t("secretsMigration.liveRetryDone"));
      } else {
        toast.error(t("secretsMigration.liveRetryStillFailing"));
      }
      await refresh();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-4 border-t border-border/50 pt-4">
      <div className="flex items-center justify-between gap-4">
        <p className="text-sm text-muted-foreground">
          {t("secretsMigration.livePendingHint", {
            count: status.failures.length,
          })}
        </p>
        <Button variant="outline" size="sm" disabled={busy} onClick={retry}>
          {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          {t("secretsMigration.livePendingRetry")}
        </Button>
      </div>
      <ul className="mt-2 space-y-1 text-xs text-muted-foreground">
        {status.failures.map((failure) => {
          const [target, code] = failure.split("|");
          return (
            <li key={failure}>
              {t(`secretsMigration.liveFail.${code || "other"}`, { target })}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
