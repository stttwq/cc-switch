import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useSettingsQuery } from "@/lib/query";
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
 *   （F1-5），点「重试导入」走 `retry_secrets_import_pending`（S6-3），无需
 *   重新执行整个导入；同时后端在明文暂留未处理前会拒绝手动导入/恢复；
 * - `onepasswordUnlinked`：导入后仍没有 1P 引用行的供应商（S4-3，D-S3）。它们
 *   在 1P 里本来就有钥匙，只是本机没关联上——点「从 1Password 关联」走
 *   `onepassword_rebuild_refs`（只针对这批），**不要**让用户重新输入密钥。
 *
 * 状态读取是纯本机查询（0 次 op）；「导入」「关联」按钮会触发 op（可能弹解锁）。
 * S6-3：改用 useSettingsQuery——手动导入 / `.db` 恢复 / 云同步下载成功后前端
 * 都会失效 ["settings"] 缓存，横幅随之刷新，不再只在挂载时读取一次。
 */
export function PlaintextPendingBanner() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { data: settings, refetch } = useSettingsQuery();
  const [importing, setImporting] = useState(false);
  const [writing, setWriting] = useState(false);
  const [linking, setLinking] = useState(false);
  const [retrying, setRetrying] = useState(false);

  const refresh = useCallback(async () => {
    try {
      await refetch();
    } catch {
      // 设置读取失败不打扰用户（导入动作本身会报错）
    }
  }, [refetch]);

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

  // S1-2（D-S9）：Pi 端点改动已进本机缓存，1P 侧还是旧值——用户主动触发写回。
  const writeEndpoints = async () => {
    setWriting(true);
    try {
      const count = await invoke<number>(
        "flush_pi_endpoint_vault_to_onepassword",
      );
      toast.success(t("onepassword.endpointWriteDone", { count }));
      await refresh();
    } catch (error) {
      if (toastVaultError(error, () => void writeEndpoints())) return;
      toast.error(String(error));
    } finally {
      setWriting(false);
    }
  };

  // S6-3：重试导入「明文暂留 DB」的行（解锁 1Password 后无需重跑整个导入）。
  // 整表重跑后端 scrub：已干净的行零 op，只有 pending 的行真正写 vault。
  const retryPendingImports = async () => {
    setRetrying(true);
    try {
      const remaining = await invoke<string[]>("retry_secrets_import_pending");
      if (remaining.length === 0) {
        toast.success(t("onepassword.retryImportDone"));
      } else {
        toast.warning(
          t("onepassword.retryImportPartial", { count: remaining.length }),
        );
      }
      await refresh();
      // 剥离过的行 settings_config 变了，供应商列表的钥匙状态要跟着刷新。
      await queryClient.invalidateQueries({ queryKey: ["providers"] });
    } catch (error) {
      if (toastVaultError(error, () => void retryPendingImports())) return;
      toast.error(String(error));
    } finally {
      setRetrying(false);
    }
  };

  // S4-3：把「未关联」的供应商与 1P 条目对上。走 `only` 精确定位，
  // 避免为了几个供应商扫一遍整个保险箱（N+1 次 op）。
  const linkProviders = async () => {
    const only = settings?.onepasswordUnlinked ?? [];
    if (only.length === 0) return;
    setLinking(true);
    try {
      const r = await invoke<{
        total: number;
        rebuilt: number;
        skipped: string[];
      }>("onepassword_rebuild_refs", { only });
      toast.success(t("onepassword.linkProvidersDone", r));
      await refresh();
      // 列表徽标的「未关联」要跟着变（secretStatus.linked）。
      await queryClient.invalidateQueries({ queryKey: ["providers"] });
    } catch (error) {
      if (toastVaultError(error, () => void linkProviders())) return;
      toast.error(String(error));
    } finally {
      setLinking(false);
    }
  };

  if (!settings || settings.secretBackend !== "onepassword") {
    return null;
  }
  const piCount = settings.piPlaintextPending?.length ?? 0;
  const liveCount = settings.livePlaintextPending?.length ?? 0;
  const importPendingCount = settings.secretsImportPending?.length ?? 0;
  const endpointCount = settings.piEndpointVaultPending?.length ?? 0;
  const unlinkedCount = settings.onepasswordUnlinked?.length ?? 0;
  if (
    piCount + liveCount + importPendingCount + endpointCount + unlinkedCount ===
    0
  ) {
    return null;
  }

  return (
    <div className="mx-4 mt-2 flex flex-col gap-2">
      {(piCount > 0 || liveCount > 0 || importPendingCount > 0) && (
        <div className="flex items-center justify-between gap-3 rounded-lg border border-border bg-muted/40 px-4 py-2 text-sm">
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
            {importing || retrying ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <>
                {/* S6-3：明文暂留行单独重试，无需重新执行整个导入。 */}
                {importPendingCount > 0 && (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={retryPendingImports}
                  >
                    {t("onepassword.retryImportAction")}
                  </Button>
                )}
                {(piCount > 0 || liveCount > 0) && (
                  <Button size="sm" variant="outline" onClick={importAll}>
                    {t("onepassword.plaintextImportAction")}
                  </Button>
                )}
              </>
            )}
          </div>
        </div>
      )}
      {endpointCount > 0 && (
        <div className="flex items-center justify-between gap-3 rounded-lg border border-border bg-muted/40 px-4 py-2 text-sm">
          <span>
            {writing
              ? t("onepassword.requesting")
              : t("onepassword.endpointVaultPending", { count: endpointCount })}
          </span>
          <div className="flex shrink-0 items-center gap-2">
            {writing ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Button size="sm" variant="outline" onClick={writeEndpoints}>
                {t("onepassword.endpointWriteAction")}
              </Button>
            )}
          </div>
        </div>
      )}
      {/* S4-3：未关联的供应商——这些在 1P 里已有钥匙，只是本机没建立引用。 */}
      {unlinkedCount > 0 && (
        <div className="flex items-center justify-between gap-3 rounded-lg border border-border bg-muted/40 px-4 py-2 text-sm">
          <span>
            {linking
              ? t("onepassword.requesting")
              : t("onepassword.unlinkedPending", { count: unlinkedCount })}
          </span>
          <div className="flex shrink-0 items-center gap-2">
            {linking ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Button size="sm" variant="outline" onClick={linkProviders}>
                {t("onepassword.linkProvidersAction")}
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
