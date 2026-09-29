import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Edit3,
  ExternalLink,
  Search,
  Server,
  ShieldAlert,
  Trash2,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  useAllMcpServers,
  useBulkToggleMcpApp,
  useToggleMcpApp,
  useDeleteMcpServer,
  useImportMcpFromApps,
  useMcpApprovalStates,
  useApproveMcpServer,
} from "@/hooks/useMcp";
import type { McpServer } from "@/types";
import type { AppId } from "@/lib/api/types";
import McpFormModal from "./McpFormModal";
import McpApprovalDialog from "./McpApprovalDialog";
import { ConfirmDialog } from "../ConfirmDialog";
import { settingsApi } from "@/lib/api";
import { mcpPresets } from "@/config/mcpPresets";
import { toast } from "sonner";
import { isMcpAppId, MCP_APP_IDS } from "@/config/appConfig";
import { AppCountBar } from "@/components/common/AppCountBar";
import { AppToggleGroup } from "@/components/common/AppToggleGroup";
import { ListItemRow } from "@/components/common/ListItemRow";
import { ManagementListSearch } from "@/components/common/ManagementListSearch";

function getMcpSearchText(id: string, server: McpServer): string {
  const spec = server.server ?? {};
  const values: unknown[] = [
    id,
    server.id,
    server.name,
    server.description,
    ...(Array.isArray(server.tags) ? server.tags : []),
    spec.type,
    spec.command,
    ...(Array.isArray(spec.args) ? spec.args : []),
    spec.cwd,
    spec.url,
    server.homepage,
    server.docs,
    server.source,
  ];

  // Keep this an explicit allow-list. In particular, env and headers may
  // contain credentials and must never become part of the searchable text.
  return values
    .filter((value): value is string => typeof value === "string")
    .join("\n")
    .toLowerCase();
}

interface UnifiedMcpPanelProps {
  onOpenChange: (open: boolean) => void;
  onInteractionBlockedChange?: (blocked: boolean) => void;
}

export interface UnifiedMcpPanelHandle {
  openAdd: () => void;
  openImport: () => void;
}

const UnifiedMcpPanel = React.forwardRef<
  UnifiedMcpPanelHandle,
  UnifiedMcpPanelProps
>(({ onOpenChange: _onOpenChange, onInteractionBlockedChange }, ref) => {
  const { t } = useTranslation();
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [writePending, setWritePending] = useState(false);
  const writeLockRef = React.useRef(false);
  const [confirmDialog, setConfirmDialog] = useState<{
    isOpen: boolean;
    title: string;
    message: string;
    onConfirm: () => void;
  } | null>(null);

  const { data: serversMap, isLoading } = useAllMcpServers();
  const { data: approvalStates } = useMcpApprovalStates();
  const toggleAppMutation = useToggleMcpApp();
  const bulkToggleAppMutation = useBulkToggleMcpApp();
  const deleteServerMutation = useDeleteMcpServer();
  const importMutation = useImportMcpFromApps();
  const approveMutation = useApproveMcpServer();

  // SEC-A：待审批映射 serverId -> app -> 当前内容修订（!approved，无论启用位
  // ——禁用的未批准条目在启用前同样必须先过审批）。
  const pendingRevisions = useMemo(() => {
    const map: Record<string, Partial<Record<AppId, string>>> = {};
    (approvalStates ?? []).forEach(({ serverId, apps }) => {
      Object.entries(apps).forEach(([app, state]) => {
        if (!state.approved) {
          map[serverId] = map[serverId] ?? {};
          map[serverId][app as AppId] = state.revision;
        }
      });
    });
    return map;
  }, [approvalStates]);

  // SEC-A：对未批准内容的启用操作不放行到 toggle，改为打开审批确认框，
  // 用户查看完整配置并确认后由 approve 命令批准、启用并投影（§4.3-5/9）。
  const [approvalDialog, setApprovalDialog] = useState<{
    serverId: string;
    app: AppId;
    revision: string;
  } | null>(null);

  const mutationPending =
    toggleAppMutation.isPending ||
    bulkToggleAppMutation.isPending ||
    deleteServerMutation.isPending ||
    importMutation.isPending;
  const interactionBlocked =
    writePending ||
    mutationPending ||
    isFormOpen ||
    confirmDialog !== null ||
    approvalDialog !== null;

  React.useEffect(() => {
    onInteractionBlockedChange?.(interactionBlocked);
  }, [interactionBlocked, onInteractionBlockedChange]);

  React.useEffect(
    () => () => onInteractionBlockedChange?.(false),
    [onInteractionBlockedChange],
  );

  const beginWrite = (allowOpenConfirmation = false) => {
    if (
      writeLockRef.current ||
      mutationPending ||
      isFormOpen ||
      (!allowOpenConfirmation && confirmDialog !== null)
    ) {
      return false;
    }
    writeLockRef.current = true;
    setWritePending(true);
    return true;
  };

  const endWrite = () => {
    writeLockRef.current = false;
    setWritePending(false);
  };

  // SEC-A：徽标点击 = 打开审批确认框（pending 且启用位已为 true 的条目，
  // 开关点击是「关闭」而非「启用」，所以审批入口放在徽标上）。
  const openApprovalDialog = (serverId: string) => {
    const apps = pendingRevisions[serverId];
    const firstApp = MCP_APP_IDS.find((app) => apps?.[app]);
    if (!firstApp) return;
    setApprovalDialog({
      serverId,
      app: firstApp,
      revision: apps[firstApp] as string,
    });
  };

  const serverEntries = useMemo((): Array<[string, McpServer]> => {
    if (!serversMap) return [];
    return Object.entries(serversMap);
  }, [serversMap]);

  const normalizedSearchQuery = searchQuery.trim().toLowerCase();
  const filteredServerEntries = useMemo(() => {
    if (!normalizedSearchQuery) return serverEntries;
    return serverEntries.filter(([id, server]) =>
      getMcpSearchText(id, server).includes(normalizedSearchQuery),
    );
  }, [normalizedSearchQuery, serverEntries]);

  const enabledCounts = useMemo(() => {
    const counts = {
      claude: 0,
      codex: 0,
    };
    serverEntries.forEach(([_, server]) => {
      for (const app of MCP_APP_IDS) {
        if (server.apps[app]) counts[app]++;
      }
    });
    return counts;
  }, [serverEntries]);

  const pendingApp = bulkToggleAppMutation.isPending
    ? (bulkToggleAppMutation.variables?.app ?? null)
    : toggleAppMutation.isPending
      ? (toggleAppMutation.variables?.app ?? null)
      : null;

  const handleToggleApp = async (
    serverId: string,
    app: AppId,
    enabled: boolean,
  ) => {
    if (!isMcpAppId(app)) return;
    // SEC-A：启用未批准内容 → 打开审批确认框，不直接 toggle
    if (enabled && pendingRevisions[serverId]?.[app]) {
      setApprovalDialog({
        serverId,
        app,
        revision: pendingRevisions[serverId][app] as string,
      });
      return;
    }
    if (!beginWrite()) return;
    try {
      await toggleAppMutation.mutateAsync({ serverId, app, enabled });
    } catch (error) {
      toast.error(t("common.error"), { description: String(error) });
    } finally {
      endWrite();
    }
  };

  const handleToggleAll = async (app: AppId, enabled: boolean) => {
    if (!isMcpAppId(app)) return;
    if (!beginWrite()) return;

    // AppCountBar summarizes the complete collection, so its bulk action must
    // use the complete collection too, even while a search filter is active.
    const serverIds = serverEntries
      .filter(([_, server]) => Boolean(server.apps[app]) !== enabled)
      .map(([id]) => id);
    if (serverIds.length === 0) {
      endWrite();
      return;
    }

    // SEC-A：批量启用不夹带未批准条目——它们由用户逐个审批后再启用。
    const skippedPending = enabled
      ? serverIds.filter((id) => pendingRevisions[id]?.[app])
      : [];
    const actionableIds = serverIds.filter(
      (id) => !skippedPending.includes(id),
    );
    if (actionableIds.length === 0) {
      endWrite();
      if (skippedPending.length > 0) {
        toast.info(
          t("mcp.approval.bulkSkipped", { count: skippedPending.length }),
        );
      }
      return;
    }

    try {
      const result = await bulkToggleAppMutation.mutateAsync({
        serverIds: actionableIds,
        app,
        enabled,
      });
      if (result.failed.length > 0) {
        toast.error(
          t("common.bulkToggleFailed", { count: result.failed.length }),
          { closeButton: true },
        );
      } else if (skippedPending.length > 0) {
        toast.info(
          t("mcp.approval.bulkSkipped", { count: skippedPending.length }),
        );
      }
    } catch (error) {
      toast.error(
        t("common.bulkToggleFailed", { count: actionableIds.length }),
        {
          description: String(error),
          closeButton: true,
        },
      );
    } finally {
      endWrite();
    }
  };

  const handleEdit = (id: string) => {
    if (writeLockRef.current || interactionBlocked) return;
    setEditingId(id);
    setIsFormOpen(true);
  };

  const handleAdd = () => {
    if (writeLockRef.current || interactionBlocked) return;
    setEditingId(null);
    setIsFormOpen(true);
  };

  const handleImport = async () => {
    if (!beginWrite()) return;
    try {
      const count = await importMutation.mutateAsync();
      if (count === 0) {
        toast.success(t("mcp.unifiedPanel.noImportFound"), {
          closeButton: true,
        });
      } else {
        toast.success(t("mcp.unifiedPanel.importSuccess", { count }), {
          closeButton: true,
        });
      }
    } catch (error) {
      toast.error(t("common.error"), { description: String(error) });
    } finally {
      endWrite();
    }
  };

  React.useImperativeHandle(ref, () => ({
    openAdd: handleAdd,
    openImport: handleImport,
  }));

  const handleDelete = (id: string) => {
    if (writeLockRef.current || interactionBlocked) return;
    setConfirmDialog({
      isOpen: true,
      title: t("mcp.unifiedPanel.deleteServer"),
      message: t("mcp.unifiedPanel.deleteConfirm", { id }),
      onConfirm: async () => {
        if (!beginWrite(true)) return;
        try {
          await deleteServerMutation.mutateAsync(id);
          setConfirmDialog(null);
          toast.success(t("common.success"), { closeButton: true });
        } catch (error) {
          toast.error(t("common.error"), { description: String(error) });
        } finally {
          endWrite();
        }
      },
    });
  };

  const handleCloseForm = () => {
    setIsFormOpen(false);
    setEditingId(null);
  };

  return (
    <div className="px-6 flex flex-col flex-1 min-h-0 overflow-hidden">
      <AppCountBar
        totalLabel={t("mcp.serverCount", { count: serverEntries.length })}
        counts={enabledCounts}
        appIds={MCP_APP_IDS}
        totalCount={serverEntries.length}
        onToggleAll={handleToggleAll}
        pendingApp={pendingApp}
        disabled={interactionBlocked}
      />

      <ManagementListSearch
        value={searchQuery}
        onValueChange={setSearchQuery}
        placeholder={t("mcp.unifiedPanel.searchPlaceholder")}
        ariaLabel={t("mcp.unifiedPanel.searchAriaLabel")}
        clearLabel={t("common.clear")}
      />

      <ScrollArea type="auto" className="-mr-3 flex-1 min-h-0">
        <div className="pb-24 pr-3">
          {isLoading ? (
            <div className="text-center py-12 text-muted-foreground">
              {t("mcp.loading")}
            </div>
          ) : serverEntries.length === 0 ? (
            <div className="text-center py-12">
              <div className="w-16 h-16 mx-auto mb-4 bg-muted rounded-full flex items-center justify-center">
                <Server size={24} className="text-muted-foreground" />
              </div>
              <h3 className="text-lg font-medium text-foreground mb-2">
                {t("mcp.unifiedPanel.noServers")}
              </h3>
              <p className="text-muted-foreground text-sm">
                {t("mcp.emptyDescription")}
              </p>
            </div>
          ) : filteredServerEntries.length === 0 ? (
            <div className="flex flex-col items-center justify-center py-12 text-center text-muted-foreground">
              <Search className="mb-4 h-10 w-10 opacity-40" />
              <p className="text-sm">{t("mcp.unifiedPanel.noSearchResults")}</p>
            </div>
          ) : (
            <TooltipProvider delayDuration={300}>
              <div className="rounded-xl border border-border-default overflow-hidden">
                {filteredServerEntries.map(([id, server], index) => (
                  <UnifiedMcpListItem
                    key={id}
                    id={id}
                    server={server}
                    pendingApps={
                      Object.keys(pendingRevisions[id] ?? {}).filter((app) =>
                        isMcpAppId(app as AppId),
                      ) as AppId[]
                    }
                    onOpenApproval={openApprovalDialog}
                    onToggleApp={handleToggleApp}
                    onEdit={handleEdit}
                    onDelete={handleDelete}
                    disabled={interactionBlocked}
                    isLast={index === filteredServerEntries.length - 1}
                  />
                ))}
              </div>
            </TooltipProvider>
          )}
        </div>
      </ScrollArea>

      {isFormOpen && (
        <McpFormModal
          editingId={editingId || undefined}
          initialData={
            editingId && serversMap ? serversMap[editingId] : undefined
          }
          existingIds={serversMap ? Object.keys(serversMap) : []}
          defaultFormat="json"
          onSave={async () => {
            setIsFormOpen(false);
            setEditingId(null);
          }}
          onClose={handleCloseForm}
        />
      )}

      {confirmDialog && (
        <ConfirmDialog
          isOpen={confirmDialog.isOpen}
          title={confirmDialog.title}
          message={confirmDialog.message}
          pending={writePending}
          onConfirm={confirmDialog.onConfirm}
          onCancel={() => setConfirmDialog(null)}
        />
      )}

      {approvalDialog && (
        <McpApprovalDialog
          isOpen
          server={serversMap?.[approvalDialog.serverId]}
          app={approvalDialog.app}
          pending={approveMutation.isPending}
          onConfirm={async () => {
            try {
              await approveMutation.mutateAsync({
                serverId: approvalDialog.serverId,
                app: approvalDialog.app,
                expectedRevision: approvalDialog.revision,
              });
              setApprovalDialog(null);
              toast.success(t("mcp.approval.approved"), { closeButton: true });
            } catch (error) {
              // 内容在确认前被再次改动等情形：后端拒绝，保持对话框让用户重看
              toast.error(t("common.error"), {
                description: String(error),
                closeButton: true,
              });
            }
          }}
          onCancel={() => setApprovalDialog(null)}
        />
      )}
    </div>
  );
});

UnifiedMcpPanel.displayName = "UnifiedMcpPanel";

interface UnifiedMcpListItemProps {
  id: string;
  server: McpServer;
  /** SEC-A：待审批的应用列表（enabled && !approved） */
  pendingApps: AppId[];
  /** SEC-A：打开审批确认框（点击待审批徽标） */
  onOpenApproval: (serverId: string) => void;
  onToggleApp: (serverId: string, app: AppId, enabled: boolean) => void;
  onEdit: (id: string) => void;
  onDelete: (id: string) => void;
  disabled?: boolean;
  isLast?: boolean;
}

const UnifiedMcpListItem: React.FC<UnifiedMcpListItemProps> = ({
  id,
  server,
  pendingApps,
  onOpenApproval,
  onToggleApp,
  onEdit,
  onDelete,
  disabled,
  isLast,
}) => {
  const { t } = useTranslation();
  const name = server.name || id;
  const description = server.description || "";

  const meta = mcpPresets.find((p) => p.id === id);
  const docsUrl = server.docs || meta?.docs;
  const homepageUrl = server.homepage || meta?.homepage;
  const tags = server.tags || meta?.tags;

  const openDocs = async () => {
    const url = docsUrl || homepageUrl;
    if (!url) return;
    try {
      await settingsApi.openExternal(url);
    } catch {
      // ignore
    }
  };

  return (
    <ListItemRow isLast={isLast}>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5">
          <span className="font-medium text-sm text-foreground truncate">
            {name}
          </span>
          {pendingApps.length > 0 && (
            <Tooltip>
              <TooltipTrigger asChild>
                <button
                  type="button"
                  onClick={() => onOpenApproval(id)}
                  className="flex items-center gap-0.5 rounded-full bg-amber-100 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:bg-amber-500/15 dark:text-amber-400 flex-shrink-0 cursor-pointer"
                  data-testid="mcp-pending-badge"
                >
                  <ShieldAlert size={10} />
                  {t("mcp.approval.pendingBadge")}
                </button>
              </TooltipTrigger>
              <TooltipContent>
                {t("mcp.approval.pendingApps", {
                  apps: pendingApps.join(", "),
                })}
              </TooltipContent>
            </Tooltip>
          )}
          {docsUrl && (
            <button
              type="button"
              onClick={openDocs}
              className="text-muted-foreground/60 hover:text-foreground flex-shrink-0"
              title={t("mcp.presets.docs")}
            >
              <ExternalLink size={12} />
            </button>
          )}
        </div>
        {description && (
          <p
            className="text-xs text-muted-foreground truncate"
            title={description}
          >
            {description}
          </p>
        )}
        {!description && tags && tags.length > 0 && (
          <p className="text-xs text-muted-foreground/60 truncate">
            {tags.join(", ")}
          </p>
        )}
      </div>

      <AppToggleGroup
        apps={server.apps}
        onToggle={(app, enabled) => onToggleApp(id, app, enabled)}
        appIds={MCP_APP_IDS}
        disabled={disabled}
      />

      <div className="flex items-center gap-0.5 flex-shrink-0 opacity-0 group-hover:opacity-100 transition-opacity">
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-7 w-7 disabled:opacity-100"
          onClick={() => onEdit(id)}
          disabled={disabled}
          title={t("common.edit")}
        >
          <Edit3 size={14} />
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-7 w-7 hover:text-red-500 hover:bg-red-100 disabled:opacity-100 dark:hover:text-red-400 dark:hover:bg-red-500/10"
          onClick={() => onDelete(id)}
          disabled={disabled}
          title={t("common.delete")}
        >
          <Trash2 size={14} />
        </Button>
      </div>
    </ListItemRow>
  );
};

export default UnifiedMcpPanel;
