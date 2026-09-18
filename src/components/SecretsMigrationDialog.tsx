import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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
}

interface BackupInfo {
  path: string;
  kind: string;
}

export function SecretsMigrationDialog() {
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
            密钥已迁入 Windows 凭据管理器
          </DialogTitle>
        </DialogHeader>
        <div className="space-y-3 px-6 py-4">
          <DialogDescription>
            已迁移 {report.migrated_providers.length} 个供应商的密钥。
            已打开的终端需要重开才能读到新环境变量。
          </DialogDescription>
          <ul className="max-h-40 overflow-auto text-sm text-muted-foreground list-disc pl-5">
            {report.migrated_providers.slice(0, 20).map((p) => (
              <li key={`${p.app_type}-${p.provider_id}`}>
                {p.app_type} / {p.provider_name}
              </li>
            ))}
          </ul>
          {backups.length > 0 && (
            <div className="space-y-2">
              <p className="text-sm">
                仍含明文的历史数据库备份（回滚用，可稍后删）：
              </p>
              <ul className="max-h-24 overflow-auto text-xs break-all list-disc pl-5">
                {backups.map((b) => (
                  <li key={b.path}>{b.path}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
        <DialogFooter>
          {backups.length > 0 && (
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() => void deleteBackups()}
            >
              全部删除备份
            </Button>
          )}
          <Button disabled={busy} onClick={() => void confirm()}>
            知道了
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
