import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { extractErrorMessage } from "@/utils/errorUtils";
import { Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";

/** 1Password 孤儿条目候选（只有条目结构信息，不含值）。 */
interface OnePasswordOrphan {
  item_id: string;
  title: string;
  updated_at: string;
}

/**
 * §5.4「清理孤儿凭据」。
 *
 * Windows（凭据管理器）模式：按 DB 现有供应商 + known_secret_targets 比对，
 * 删掉已无归属的残留条目。
 *
 * 1Password 模式（F3-8）：先归档「删除供应商时记录的孤儿」，再列出 vault 里
 * 有、DB 里已无对应供应商的候选条目（`op item list --tags cc-switch`，不含值），
 * 用户确认后归档。不再触碰凭据管理器（残留由 1Password 区的专用清理处理）。
 */
export function SecretStoreMaintenance() {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [retitling, setRetitling] = useState(false);
  const [backend, setBackend] = useState<string>("windows");
  const [orphans, setOrphans] = useState<OnePasswordOrphan[] | null>(null);

  useEffect(() => {
    invoke<string>("secret_backend_name")
      .then(setBackend)
      .catch(() => setBackend("windows"));
  }, []);
  const isOnePassword = backend === "onepassword";

  const cleanupOrphans = async (confirmedItemIds?: string[]) => {
    setBusy(true);
    try {
      const removed = await invoke<number>("secrets_cleanup_orphans", {
        confirmedItemIds: confirmedItemIds ?? null,
      });
      toast.success(
        t("secretsMigration.cleanupOrphansDone", { count: removed }),
      );
    } catch (error) {
      toast.error(extractErrorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  const cleanupOrphans1P = async () => {
    setBusy(true);
    try {
      const candidates = await invoke<OnePasswordOrphan[]>(
        "secrets_list_onepassword_orphans",
      );
      if (candidates.length === 0) {
        // 没有候选也照常跑一次清理：顺带归档删除供应商时记录的孤儿。
        await cleanupOrphans([]);
        return;
      }
      setOrphans(candidates);
    } catch (error) {
      toast.error(extractErrorMessage(error));
    } finally {
      setBusy(false);
    }
  };

  const retitleItems = async () => {
    setRetitling(true);
    try {
      const result = await invoke<{ total: number; renamed: number }>(
        "onepassword_retitle_items",
      );
      toast.success(
        t("secretsMigration.retitleItemsDone", {
          renamed: result.renamed,
          total: result.total,
        }),
      );
    } catch (error) {
      toast.error(extractErrorMessage(error));
    } finally {
      setRetitling(false);
    }
  };

  const orphanMessage = (orphans ?? [])
    .map((o) => `${o.title} (${o.updated_at})`)
    .join("\n");

  return (
    <div className="flex items-center justify-between gap-4">
      <p className="text-sm text-muted-foreground">
        {isOnePassword
          ? t("secretsMigration.cleanupOrphansHint1P")
          : t("secretsMigration.cleanupOrphansHint")}
      </p>
      <div className="flex shrink-0 items-center gap-2">
        {isOnePassword && (
          <Button
            variant="outline"
            size="sm"
            disabled={busy || retitling}
            onClick={retitleItems}
          >
            {retitling && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {t("secretsMigration.retitleItems")}
          </Button>
        )}
        <Button
          variant="outline"
          size="sm"
          disabled={busy || retitling}
          onClick={() => (isOnePassword ? cleanupOrphans1P() : cleanupOrphans())}
        >
          {busy && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
          {t("secretsMigration.cleanupOrphans")}
        </Button>
      </div>

      {orphans !== null && (
        <ConfirmDialog
          isOpen
          title={t("secretsMigration.cleanupOrphansConfirmTitle", {
            count: orphans.length,
          })}
          message={`${t("secretsMigration.cleanupOrphansConfirmMessage")}\n${orphanMessage}`}
          confirmText={t("secretsMigration.cleanupOrphans")}
          pending={busy}
          onConfirm={() => {
            cleanupOrphans(orphans.map((o) => o.item_id))
              .finally(() => setOrphans(null));
          }}
          onCancel={() => setOrphans(null)}
        />
      )}
    </div>
  );
}
