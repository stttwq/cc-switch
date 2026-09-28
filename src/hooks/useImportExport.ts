import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { settingsApi } from "@/lib/api";
import type { SqlImportPreview } from "@/lib/api/settings";

export type ImportStatus =
  | "idle"
  | "importing"
  | "success"
  | "partial-success"
  | "error";

/** S6-2：导入结果里要展示给用户的后处理信息。 */
export interface ImportResultInfo {
  warning?: string;
  adoptedRefs?: number;
  unlinkedProviders?: number;
}

export interface UseImportExportOptions {
  onImportSuccess?: () => void | Promise<void>;
}

export interface UseImportExportResult {
  status: ImportStatus;
  errorMessage: string | null;
  backupId: string | null;
  isImporting: boolean;
  /** S6-2：预览通过后待确认的导入信息，非空时组件渲染确认框。 */
  pendingPreview: SqlImportPreview | null;
  /** S6-2：导入结果的后处理信息（warning / 采纳引用 / 未关联数）。 */
  importResult: ImportResultInfo | null;
  importConfig: () => Promise<void>;
  confirmImport: () => Promise<void>;
  cancelImport: () => void;
  exportConfig: () => Promise<void>;
  resetStatus: () => void;
}

export function useImportExport(
  options: UseImportExportOptions = {},
): UseImportExportResult {
  const { t } = useTranslation();
  const { onImportSuccess } = options;
  const queryClient = useQueryClient();

  const [status, setStatus] = useState<ImportStatus>("idle");
  const [errorMessage, setErrorMessage] = useState<string | null>(null);
  const [backupId, setBackupId] = useState<string | null>(null);
  const [isImporting, setIsImporting] = useState(false);
  const [pendingPreview, setPendingPreview] = useState<SqlImportPreview | null>(
    null,
  );
  const [importResult, setImportResult] = useState<ImportResultInfo | null>(
    null,
  );

  // S6-2：第一步只做预览——选文件、读文件头、拿到 meta 和一次性 pathToken
  //（路径只在 Rust 侧，不经前端往返，令牌 10 分钟过期）。确认后走 confirmImport。
  const importConfig = useCallback(async () => {
    if (isImporting) return;

    setErrorMessage(null);
    setStatus("idle");

    try {
      const preview = await settingsApi.previewSqlImportViaDialog();
      if (preview === null) {
        // 用户取消：静默回到 idle，不当作失败
        return;
      }
      setPendingPreview(preview);
    } catch (error) {
      console.error("[useImportExport] Failed to preview config", error);
      setStatus("error");
      const message =
        typeof error === "string"
          ? error
          : error instanceof Error
            ? error.message
            : String(error ?? "");
      setErrorMessage(message);
      toast.error(
        t("settings.importFailedError", {
          defaultValue: "导入配置失败: {{message}}",
          message,
        }),
      );
    }
  }, [isImporting, t]);

  // S6-2：确认导入——消费 pathToken 执行真正的导入。
  const confirmImport = useCallback(async () => {
    if (!pendingPreview || isImporting) return;

    setIsImporting(true);
    setStatus("importing");
    setErrorMessage(null);

    try {
      const result = await settingsApi.importConfigConfirmed(
        pendingPreview.pathToken,
      );
      setPendingPreview(null);
      if (result === null) {
        setStatus("idle");
        return;
      }
      if (!result.success) {
        setStatus("error");
        const message =
          result.message ||
          t("settings.configCorrupted", {
            defaultValue: "SQL 文件已损坏或格式不正确",
          });
        setErrorMessage(message);
        toast.error(message);
        return;
      }

      setBackupId(result.backupId ?? null);
      setImportResult({
        warning: result.warning,
        adoptedRefs: result.adoptedRefs,
        unlinkedProviders: result.unlinkedProviders,
      });
      // 导入成功后立即触发外部刷新（与 live 同步结果解耦）
      // - 避免 sync 失败时 UI 不刷新
      // - 避免依赖 setTimeout（组件卸载会取消）
      void onImportSuccess?.();
      // S6-3：失效 settings 缓存，明文暂留横幅随之刷新（后处理的 scrub 结果
      // 会改 secrets_import_pending / onepassword_unlinked 清单）。
      void queryClient.invalidateQueries({ queryKey: ["settings"] });
      // S6-2（P2-3）：后端 run_post_import_sync 已刷新 live 配置，前端不再
      // 重复调用 sync_current_providers_live；失败信息降级在 result.warning。
      setStatus("success");
      toast.success(
        t("settings.importSuccess", {
          defaultValue: "配置导入成功",
        }),
        { closeButton: true },
      );
      if (result.warning) {
        toast.warning(result.warning, { closeButton: true });
      }
    } catch (error) {
      console.error("[useImportExport] Failed to import config", error);
      setPendingPreview(null);
      setStatus("error");
      const message =
        typeof error === "string"
          ? error
          : error instanceof Error
            ? error.message
            : String(error ?? "");
      setErrorMessage(message);
      toast.error(
        t("settings.importFailedError", {
          defaultValue: "导入配置失败: {{message}}",
          message,
        }),
      );
    } finally {
      setIsImporting(false);
    }
  }, [pendingPreview, isImporting, onImportSuccess, queryClient, t]);

  // S6-2：用户在确认框选择取消。
  const cancelImport = useCallback(() => {
    setPendingPreview(null);
    setStatus("idle");
  }, []);

  const exportConfig = useCallback(async () => {
    try {
      // S6-1：默认文件名体现「配置导出」语义（完整快照请用数据库备份）。
      const now = new Date();
      const stamp = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, "0")}${String(now.getDate()).padStart(2, "0")}`;
      const defaultName = `cc-switch-config-${stamp}.sql`;

      const result = await settingsApi.exportConfigViaDialog(defaultName);
      if (result === null) {
        // 用户取消保存对话框
        return;
      }

      if (result.success) {
        toast.success(
          t("settings.configExported", {
            defaultValue: "配置已导出",
          }) + (result.filePath ? `\n${result.filePath}` : ""),
          { closeButton: true },
        );
      } else {
        toast.error(
          t("settings.exportFailed", {
            defaultValue: "导出配置失败",
          }) + (result.message ? `: ${result.message}` : ""),
        );
      }
    } catch (error) {
      console.error("[useImportExport] Failed to export config", error);
      toast.error(
        t("settings.exportFailedError", {
          defaultValue: "导出配置失败: {{message}}",
          message: error instanceof Error ? error.message : String(error ?? ""),
        }),
      );
    }
  }, [t]);

  const resetStatus = useCallback(() => {
    setStatus("idle");
    setErrorMessage(null);
    setBackupId(null);
    setPendingPreview(null);
    setImportResult(null);
  }, []);

  return {
    status,
    errorMessage,
    backupId,
    isImporting,
    pendingPreview,
    importResult,
    importConfig,
    confirmImport,
    cancelImport,
    exportConfig,
    resetStatus,
  };
}
