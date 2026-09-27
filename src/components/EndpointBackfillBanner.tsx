import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { toastVaultError } from "@/utils/errorUtils";
import { useTauriEvent } from "@/hooks/useTauriEvent";

/**
 * F1-2（D3-A）：端点回填提示横幅。
 *
 * 存量数据里 base_url 还托管在 vault（1Password / 凭据管理器）中时，读取每次要
 * 1 次 op（1P 模式还会弹解锁）。回填把它们逐个搬到本地端点表，之后读取 0 次 op。
 * 启动统计是纯本地查询（0 次 op）；点「回填」后每个约 7 秒并可能请求解锁。
 */
export function EndpointBackfillBanner() {
  const { t } = useTranslation();
  const [pending, setPending] = useState(0);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState({ done: 0, total: 0 });
  const [dismissed, setDismissed] = useState(() => {
    return sessionStorage.getItem("endpoint_backfill_banner_dismissed") === "true";
  });

  const refreshPending = useCallback(async () => {
    if (running) return;
    try {
      const s = await invoke<{ pending: number }>(
        "secrets_endpoint_backfill_status",
      );
      setPending(s.pending);
    } catch {
      // 状态查询失败不打扰用户（横幅只是提醒，读取路径本身有懒迁移兜底）
    }
  }, [running]);

  useEffect(() => {
    void refreshPending();
  }, [refreshPending]);

  useTauriEvent<{ done: number; total: number }>(
    "secrets-backfill-progress",
    (payload) => {
      setProgress(payload);
    },
  );

  const backfill = async () => {
    setRunning(true);
    try {
      const report = await invoke<{
        total: number;
        backfilled: number;
        failed: string[];
      }>("secrets_backfill_endpoints");
      if (report.failed.length > 0) {
        toast.warning(
          t("onepassword.backfillDoneWithFailures", {
            backfilled: report.backfilled,
            failed: report.failed.length,
          }),
        );
      } else {
        toast.success(
          t("onepassword.backfillDone", { backfilled: report.backfilled }),
        );
      }
      setPending(0);
      sessionStorage.setItem("endpoint_backfill_banner_dismissed", "true");
    } catch (error) {
      // F5-3：vault_* 错误（如 1Password 锁定）统一 toast + 「重试」。
      if (toastVaultError(error, () => void backfill())) return;
      toast.error(String(error));
    } finally {
      setRunning(false);
    }
  };

  const dismiss = () => {
    setDismissed(true);
    sessionStorage.setItem("endpoint_backfill_banner_dismissed", "true");
  };

  if (dismissed || (pending === 0 && !running)) {
    return null;
  }

  return (
    <div className="mx-4 mt-2 flex items-center justify-between gap-3 rounded-lg border border-border bg-muted/40 px-4 py-2 text-sm">
      <span>
        {running
          ? t("onepassword.backfillProgress", {
              done: progress.done,
              total: progress.total,
            })
          : t("onepassword.backfillPending", { count: pending })}
      </span>
      <div className="flex shrink-0 items-center gap-2">
        {running ? (
          <Loader2 className="h-4 w-4 animate-spin" />
        ) : (
          <>
            <Button size="sm" variant="outline" onClick={backfill}>
              {t("onepassword.backfillAction")}
            </Button>
            <Button size="sm" variant="ghost" onClick={dismiss}>
              {t("common.close")}
            </Button>
          </>
        )}
      </div>
    </div>
  );
}
