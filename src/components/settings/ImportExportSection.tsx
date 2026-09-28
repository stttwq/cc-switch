import {
  AlertCircle,
  CheckCircle2,
  FolderOpen,
  Loader2,
  Save,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { useTranslation } from "react-i18next";
import { useSettingsQuery } from "@/lib/query";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { SqlImportPreview } from "@/lib/api/settings";
import type { ImportResultInfo, ImportStatus } from "@/hooks/useImportExport";

interface ImportExportSectionProps {
  status: ImportStatus;
  errorMessage: string | null;
  backupId: string | null;
  isImporting: boolean;
  /** S6-2：预览通过后待确认的导入信息，非空时渲染确认框。 */
  pendingPreview: SqlImportPreview | null;
  /** S6-2：导入结果的后处理信息（warning / 采纳引用 / 未关联数）。 */
  importResult: ImportResultInfo | null;
  onImport: () => Promise<void>;
  onConfirmImport: () => Promise<void>;
  onCancelImport: () => void;
  onExport: () => Promise<void>;
}

export function ImportExportSection({
  status,
  errorMessage,
  backupId,
  isImporting,
  pendingPreview,
  importResult,
  onImport,
  onConfirmImport,
  onCancelImport,
  onExport,
}: ImportExportSectionProps) {
  const { t } = useTranslation();
  // S6-1：导出语义随凭据后端模式不同，按钮旁的说明文案也分两套。
  const { data: settings } = useSettingsQuery();
  const isOnePassword = settings?.secretBackend === "onepassword";

  return (
    <section className="space-y-4">
      <header className="space-y-2">
        <h3 className="text-base font-semibold text-foreground">
          {t("settings.importExport")}
        </h3>
        <p className="text-sm text-muted-foreground">
          {t("settings.importExportHint")}
        </p>
      </header>

      <div className="space-y-4 rounded-lg border border-border bg-muted/40 p-6">
        {/* Import and Export Buttons Side by Side */}
        <div className="grid grid-cols-2 gap-4 items-stretch">
          {/* Import Button：选择文件与导入合成一步（计划 4.2.1 S-2） */}
          <div>
            <Button
              type="button"
              className="w-full h-full py-3 px-4 bg-blue-500 hover:bg-blue-600 dark:bg-blue-600 dark:hover:bg-blue-700 text-white items-center"
              onClick={onImport}
              disabled={isImporting}
            >
              {isImporting ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin flex-shrink-0" />
              ) : (
                <FolderOpen className="mr-2 h-4 w-4 flex-shrink-0" />
              )}
              <span className="font-medium">
                {isImporting ? t("settings.importing") : t("settings.import")}
              </span>
            </Button>
          </div>

          {/* Export Button */}
          <div className="flex flex-col gap-1.5">
            <Button
              type="button"
              className="w-full h-full py-3 px-4 bg-blue-500 hover:bg-blue-600 dark:bg-blue-600 dark:hover:bg-blue-700 text-white items-center"
              onClick={onExport}
            >
              <Save className="mr-2 h-4 w-4" />
              {t("settings.exportConfig")}
            </Button>
            <p className="text-xs text-muted-foreground leading-relaxed">
              {isOnePassword
                ? t("settings.exportHintOnePassword")
                : t("settings.exportHintCredentialManager")}
            </p>
          </div>
        </div>

        <ImportStatusMessage
          status={status}
          errorMessage={errorMessage}
          backupId={backupId}
          importResult={importResult}
        />
      </div>

      {/* S6-2：导入确认框——显示来源信息与影响范围，确认后才执行导入。 */}
      <Dialog
        open={!!pendingPreview}
        onOpenChange={(open) => !open && onCancelImport()}
      >
        <DialogContent className="max-w-md" zIndex="alert">
          <DialogHeader>
            <DialogTitle>{t("settings.importPreview.title")}</DialogTitle>
            <DialogDescription>
              {t("settings.importPreview.body")}
            </DialogDescription>
          </DialogHeader>
          {pendingPreview?.meta ? (
            <dl className="space-y-1.5 rounded-lg bg-muted/40 p-3 text-sm">
              <div className="flex justify-between gap-3">
                <dt className="text-muted-foreground">
                  {t("settings.importPreview.device")}
                </dt>
                <dd className="truncate">{pendingPreview.meta.device}</dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-muted-foreground">
                  {t("settings.importPreview.exportedAt")}
                </dt>
                <dd>{formatExportedAt(pendingPreview.meta.exportedAt)}</dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-muted-foreground">
                  {t("settings.importPreview.backend")}
                </dt>
                <dd>
                  {pendingPreview.meta.backend === "onepassword"
                    ? t("settings.importPreview.backendOnePassword")
                    : t("settings.importPreview.backendCredentialManager")}
                </dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-muted-foreground">
                  {t("settings.importPreview.endpoints")}
                </dt>
                <dd>
                  {pendingPreview.meta.endpoints
                    ? t("settings.importPreview.endpointsYes")
                    : t("settings.importPreview.endpointsNo")}
                </dd>
              </div>
              <div className="flex justify-between gap-3">
                <dt className="text-muted-foreground">
                  {t("settings.importPreview.refs")}
                </dt>
                <dd>{pendingPreview.meta.refs}</dd>
              </div>
            </dl>
          ) : (
            <p className="rounded-lg bg-muted/40 p-3 text-sm text-muted-foreground">
              {t("settings.importPreview.metaNone")}
            </p>
          )}
          <DialogFooter>
            <Button
              variant="outline"
              onClick={onCancelImport}
              disabled={isImporting}
            >
              {t("common.cancel")}
            </Button>
            <Button onClick={onConfirmImport} disabled={isImporting}>
              {isImporting
                ? t("settings.importing")
                : t("settings.importPreview.confirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}

/** S6-2：导出时间（RFC 3339）按本机格式显示，解析失败原样回显。 */
function formatExportedAt(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

interface ImportStatusMessageProps {
  status: ImportStatus;
  errorMessage: string | null;
  backupId: string | null;
  /** S6-2：导入结果的后处理信息。 */
  importResult: ImportResultInfo | null;
}

function ImportStatusMessage({
  status,
  errorMessage,
  backupId,
  importResult,
}: ImportStatusMessageProps) {
  const { t } = useTranslation();

  if (status === "idle") {
    return null;
  }

  const baseClass =
    "flex items-start gap-3 rounded-xl border p-4 text-sm leading-relaxed backdrop-blur-sm";

  if (status === "importing") {
    return (
      <div
        className={`${baseClass} border-blue-500/30 bg-blue-500/10 text-blue-600 dark:text-blue-400`}
      >
        <Loader2 className="mt-0.5 h-5 w-5 flex-shrink-0 animate-spin" />
        <div>
          <p className="font-semibold">{t("settings.importing")}</p>
          <p className="text-blue-600/80 dark:text-blue-400/80">
            {t("common.loading")}
          </p>
        </div>
      </div>
    );
  }

  if (status === "success") {
    return (
      <div
        className={`${baseClass} border-green-500/30 bg-green-500/10 text-green-700 dark:text-green-400`}
      >
        <CheckCircle2 className="mt-0.5 h-5 w-5 flex-shrink-0" />
        <div className="space-y-1.5">
          <p className="font-semibold">{t("settings.importSuccess")}</p>
          {backupId ? (
            <p className="text-xs text-green-600/80 dark:text-green-400/80">
              {t("settings.backupId")}: {backupId}
            </p>
          ) : null}
          {/* S6-2：后处理统计——采纳了几条 1P 关联、几个供应商未关联。 */}
          {(importResult?.adoptedRefs ?? 0) > 0 ? (
            <p className="text-xs text-green-600/80 dark:text-green-400/80">
              {t("settings.importAdoptedRefs", {
                count: importResult?.adoptedRefs,
              })}
            </p>
          ) : null}
          {(importResult?.unlinkedProviders ?? 0) > 0 ? (
            <p className="text-xs text-yellow-600/80 dark:text-yellow-400/80">
              {t("settings.importUnlinkedProviders", {
                count: importResult?.unlinkedProviders,
              })}
            </p>
          ) : null}
          {importResult?.warning ? (
            <p className="text-xs text-yellow-600/80 dark:text-yellow-400/80">
              {importResult.warning}
            </p>
          ) : null}
          <p className="text-green-600/80 dark:text-green-400/80">
            {t("settings.autoReload")}
          </p>
        </div>
      </div>
    );
  }

  if (status === "partial-success") {
    return (
      <div
        className={`${baseClass} border-yellow-500/30 bg-yellow-500/10 text-yellow-700 dark:text-yellow-400`}
      >
        <AlertCircle className="mt-0.5 h-5 w-5 flex-shrink-0" />
        <div className="space-y-1.5">
          <p className="font-semibold">{t("settings.importPartialSuccess")}</p>
          <p className="text-yellow-600/80 dark:text-yellow-400/80">
            {t("settings.importPartialHint")}
          </p>
        </div>
      </div>
    );
  }

  const message = errorMessage || t("settings.importFailed");

  return (
    <div
      className={`${baseClass} border-red-500/30 bg-red-500/10 text-red-600 dark:text-red-400`}
    >
      <AlertCircle className="mt-0.5 h-5 w-5 flex-shrink-0" />
      <div className="space-y-1.5">
        <p className="font-semibold">{t("settings.importFailed")}</p>
        <p className="text-red-600/80 dark:text-red-400/80">{message}</p>
      </div>
    </div>
  );
}
