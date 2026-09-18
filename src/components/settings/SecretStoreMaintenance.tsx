import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";

/**
 * §5.4「清理孤儿凭据」：凭据管理器无法枚举条目，这里按 DB 现有供应商 +
 * settings.known_secret_targets 比对，删掉已无归属的残留条目。
 */
export function SecretStoreMaintenance() {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);

  const cleanupOrphans = async () => {
    setBusy(true);
    try {
      const removed = await invoke<number>("secrets_cleanup_orphans");
      toast.success(
        t("secretsMigration.cleanupOrphansDone", { count: removed }),
      );
    } catch (error) {
      toast.error(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex items-center justify-between gap-4">
      <p className="text-sm text-muted-foreground">
        {t("secretsMigration.cleanupOrphansHint")}
      </p>
      <Button
        variant="outline"
        size="sm"
        disabled={busy}
        onClick={cleanupOrphans}
      >
        {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
        {t("secretsMigration.cleanupOrphans")}
      </Button>
    </div>
  );
}
