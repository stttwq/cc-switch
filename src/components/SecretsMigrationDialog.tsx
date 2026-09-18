import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Shield } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";

interface MigratedProviderInfo {
  provider_id: string;
  provider_name: string;
  app_type: string;
  fields_count: number;
}

interface MigrationReport {
  migrated_providers: MigratedProviderInfo[];
  errors: string[];
  warnings?: string[];
  dropped_codex_oauth?: string[];
  live_reapply_failures?: string[];
}

interface BackupInfo {
  path: string;
  kind: string;
}

export function SecretsMigrationDialog() {
  const { t } = useTranslation();
  const [report, setReport] = useState<MigrationReport | null>(null);
  const [backups, setBackups] = useState<BackupInfo[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const next = await invoke<MigrationReport | null>(
          "get_secrets_migration_report",
        );
        setReport(next);
        if (next) {
          const listed = await invoke<BackupInfo[]>("list_plaintext_backups");
          setBackups(listed);
        }
      } catch (error) {
        console.error("[SecretsMigrationDialog]", error);
      }
    })();
  }, []);

  const confirm = async () => {
    setBusy(true);
    try {
      await invoke("confirm_secrets_migration");
      setReport(null);
    } catch (error) {
      console.error("[SecretsMigrationDialog] confirm", error);
    } finally {
      setBusy(false);
    }
  };

  const deleteBackups = async () => {
    setBusy(true);
    try {
      await invoke<number>("delete_plaintext_backups");
      setBackups([]);
    } catch (error) {
      console.error("[SecretsMigrationDialog] delete", error);
    } finally {
      setBusy(false);
    }
  };

  // §6.4 / §6.5：live 重写失败项提供重试——重新置位标志，下次启动自动补完。
  const retryLiveReapply = async () => {
    setBusy(true);
    try {
      await invoke("retry_live_reapply");
    } catch (error) {
      console.error("[SecretsMigrationDialog] retry", error);
    } finally {
      setBusy(false);
    }
  };

  if (!report) return null;

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) void confirm();
      }}
    >
      <DialogContent className="max-w-lg" zIndex="top">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Shield className="h-5 w-5 text-blue-500" />
            {t("secretsMigration.title")}
          </DialogTitle>
        </DialogHeader>
        <div className="space-y-3 px-6 py-4">
          <DialogDescription>
            {t("secretsMigration.summary", {
              numProviders: report.migrated_providers.length,
            })}
          </DialogDescription>
          <ul className="max-h-40 overflow-auto text-sm text-muted-foreground list-disc pl-5">
            {report.migrated_providers.slice(0, 20).map((p) => (
              <li key={`${p.app_type}-${p.provider_id}`}>
                {p.app_type} / {p.provider_name}
              </li>
            ))}
          </ul>
          {report.warnings && report.warnings.length > 0 && (
            <div className="space-y-1">
              <p className="text-sm text-muted-foreground">
                {t("secretsMigration.warningsTitle")}
              </p>
              <ul className="max-h-24 overflow-auto text-xs list-disc pl-5 text-muted-foreground">
                {report.warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            </div>
          )}
          {report.dropped_codex_oauth &&
            report.dropped_codex_oauth.length > 0 && (
              <p className="text-sm text-amber-600 dark:text-amber-400">
                {t("secretsMigration.codexLogin", {
                  numProviders: report.dropped_codex_oauth.length,
                })}
              </p>
            )}
          {report.live_reapply_failures &&
            report.live_reapply_failures.length > 0 && (
              <div className="space-y-1">
                <p className="text-sm text-destructive">
                  {t("secretsMigration.liveFailures", {
                    count: report.live_reapply_failures.length,
                  })}
                </p>
                <ul className="max-h-24 overflow-auto text-xs break-all list-disc pl-5 text-muted-foreground">
                  {report.live_reapply_failures.map((f) => (
                    <li key={f}>{f}</li>
                  ))}
                </ul>
              </div>
            )}
          {backups.length > 0 && (
            <div className="space-y-2">
              <p className="text-sm">{t("secretsMigration.backupsTitle")}</p>
              <ul className="max-h-24 overflow-auto text-xs break-all list-disc pl-5">
                {backups.map((b) => (
                  <li key={b.path}>{b.path}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
        <DialogFooter>
          {report.live_reapply_failures &&
            report.live_reapply_failures.length > 0 && (
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => void retryLiveReapply()}
              >
                {t("secretsMigration.retry")}
              </Button>
            )}
          {backups.length > 0 && (
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() => void deleteBackups()}
            >
              {t("secretsMigration.deleteBackups")}
            </Button>
          )}
          <Button disabled={busy} onClick={() => void confirm()}>
            {t("secretsMigration.gotIt")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
