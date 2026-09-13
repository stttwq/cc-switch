import { useCallback } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { providersApi, settingsApi, type AppId } from "@/lib/api";
import type { Provider } from "@/types";
import {
  useAddProviderMutation,
  useUpdateProviderMutation,
  useDeleteProviderMutation,
  useSwitchProviderMutation,
} from "@/lib/query";
import { extractErrorMessage } from "@/utils/errorUtils";

/**
 * Hook for managing provider actions (add, update, delete, switch)
 * Extracts business logic from App.tsx
 */
export function useProviderActions(activeApp: AppId) {
  const { t } = useTranslation();

  const addProviderMutation = useAddProviderMutation(activeApp);
  const updateProviderMutation = useUpdateProviderMutation(activeApp);
  const deleteProviderMutation = useDeleteProviderMutation(activeApp);
  const switchProviderMutation = useSwitchProviderMutation(activeApp);

  // Claude 插件同步逻辑
  const syncClaudePlugin = useCallback(
    async (provider: Provider) => {
      if (activeApp !== "claude") return;

      try {
        const settings = await settingsApi.get();
        if (!settings?.enableClaudePluginIntegration) {
          return;
        }

        const isOfficial = provider.category === "official";
        await settingsApi.applyClaudePluginConfig({ official: isOfficial });

        // 静默执行，不显示成功通知
      } catch (error) {
        const detail =
          extractErrorMessage(error) ||
          t("notifications.syncClaudePluginFailed", {
            defaultValue: "同步 Claude 插件失败",
          });
        toast.error(detail, { duration: 4200 });
      }
    },
    [activeApp, t],
  );

  // 添加供应商
  const addProvider = useCallback(
    async (
      provider: Omit<Provider, "id"> & {
        providerKey?: string;
        addToLive?: boolean;
      },
    ) => {
      await addProviderMutation.mutateAsync(provider);
    },
    [addProviderMutation],
  );

  // 更新供应商
  const updateProvider = useCallback(
    async (provider: Provider, originalId?: string) => {
      await updateProviderMutation.mutateAsync({
        provider,
        originalId,
      });

      // 更新托盘菜单（失败不影响主操作）
      try {
        await providersApi.updateTrayMenu();
      } catch (trayError) {
        console.error(
          "Failed to update tray menu after updating provider",
          trayError,
        );
      }
    },
    [updateProviderMutation],
  );

  // 切换供应商
  const switchProvider = useCallback(
    async (provider: Provider) => {
      try {
        const result = await switchProviderMutation.mutateAsync(provider.id);
        await syncClaudePlugin(provider);

        // Surface switch warnings by code — a generic "backfill failed"
        // message for an auth-cleanup warning would point the user at the
        // wrong problem entirely.
        if (result?.warnings?.length) {
          const authCleanupFailed = result.warnings.some((warning) =>
            warning.startsWith("codex_auth_cleanup_failed"),
          );
          const hasOtherWarnings = result.warnings.some(
            (warning) => !warning.startsWith("codex_auth_cleanup_failed"),
          );
          if (authCleanupFailed) {
            toast.warning(
              t("notifications.codexAuthCleanupFailed", {
                defaultValue:
                  "切换成功，但未能删除 auth.json，官方登录凭据仍留在磁盘上；如需彻底移除请手动删除 Codex 配置目录中的 auth.json",
              }),
              { duration: 6000 },
            );
          }
          if (hasOtherWarnings) {
            toast.warning(
              t("notifications.backfillWarning", {
                defaultValue:
                  "切换成功，但旧供应商配置回填失败，您手动修改的配置可能未保存",
              }),
              { duration: 5000 },
            );
          }
        }

        // 若已弹过 proxyRequired 警告则不再弹 success
        {
          let messageKey = "notifications.switchSuccess";
          let defaultMessage = "切换成功！";
          if (activeApp === "codex") {
            messageKey = "notifications.codexRestartRequired";
            defaultMessage = "切换成功，请重启客户端以生效";
          }
          toast.success(t(messageKey, { defaultValue: defaultMessage }), {
            closeButton: true,
          });
        }
      } catch {
        // 错误提示由 mutation 处理
      }
    },
    [switchProviderMutation, syncClaudePlugin, activeApp, t],
  );

  // 删除供应商
  const deleteProvider = useCallback(
    async (id: string) => {
      await deleteProviderMutation.mutateAsync(id);
    },
    [deleteProviderMutation],
  );

  return {
    addProvider,
    updateProvider,
    switchProvider,
    deleteProvider,
    isLoading:
      addProviderMutation.isPending ||
      updateProviderMutation.isPending ||
      deleteProviderMutation.isPending ||
      switchProviderMutation.isPending,
  };
}
