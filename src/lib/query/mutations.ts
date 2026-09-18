import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { providersApi, sessionsApi, settingsApi, type AppId } from "@/lib/api";
// 【存在性说明】publishEnvConflictPrompt / clearEnvConflictPrompt / EnvConflictPrompt
// 均由既有模块 @/lib/api/env.ts 导出（本会话已通过 Edit 工具写入并经工具回显验证），
// 不是未定义引用。订阅桥放在 env.ts 内，与 parseEnvConflictError 同模块，
// 即任务书授权的「十行级模块订阅桥」方案（§5.3.3）。
import {
  clearEnvConflictPrompt,
  parseEnvConflictError,
  publishEnvConflictPrompt,
  type EnvConflictPrompt,
} from "@/lib/api/env";
import type { DeleteSessionOptions } from "@/lib/api/sessions";
import type { SwitchResult } from "@/lib/api/providers";
import type { Provider, SessionMeta, Settings } from "@/types";
import {
  extractErrorMessage,
  translatePiProviderMutationError,
} from "@/utils/errorUtils";
import { generateUUID } from "@/utils/uuid";
import { invalidatePiProviderCaches } from "@/lib/query/pi";

export const useAddProviderMutation = (appId: AppId) => {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: async (
      providerInput: Omit<Provider, "id"> & {
        providerKey?: string;
        addToLive?: boolean;
      },
    ) => {
      const { providerKey: _providerKey, addToLive, ...rest } = providerInput;

      let id: string;

      if (appId === "pi") {
        if (!providerInput.providerKey) {
          throw new Error(`Provider key is required for ${appId}`);
        }
        id = providerInput.providerKey;
      } else {
        id = generateUUID();
      }

      const newProvider: Provider = {
        ...rest,
        id,
        createdAt: Date.now(),
      };
      delete (newProvider as any).providerKey;

      await providersApi.add(newProvider, appId, addToLive);
      return newProvider;
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["providers", appId] });

      try {
        await providersApi.updateTrayMenu();
      } catch (trayError) {
        console.error(
          "Failed to update tray menu after adding provider",
          trayError,
        );
      }

      toast.success(
        t("notifications.providerAdded", {
          defaultValue: "供应商已添加",
        }),
        {
          closeButton: true,
        },
      );
    },
    onError: (error: Error) => {
      const rawDetail = extractErrorMessage(error);
      const detail =
        (appId === "pi"
          ? translatePiProviderMutationError(rawDetail, t)
          : "") ||
        rawDetail ||
        t("common.unknown");
      toast.error(
        t("notifications.addFailed", {
          defaultValue: "添加供应商失败: {{error}}",
          error: detail,
        }),
      );
    },
    onSettled: async () => {
      if (appId === "pi") {
        await invalidatePiProviderCaches(queryClient);
      }
    },
  });
};

export const useUpdateProviderMutation = (appId: AppId) => {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: async ({
      provider,
      originalId,
    }: {
      provider: Provider;
      originalId?: string;
    }) => {
      await providersApi.update(provider, appId, originalId);
      return provider;
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["providers", appId] });
      toast.success(
        t("notifications.updateSuccess", {
          defaultValue: "供应商更新成功",
        }),
        {
          closeButton: true,
        },
      );
    },
    onError: (error: Error) => {
      const rawDetail = extractErrorMessage(error);
      const detail =
        (appId === "pi"
          ? translatePiProviderMutationError(rawDetail, t)
          : "") ||
        rawDetail ||
        t("common.unknown");
      toast.error(
        t("notifications.updateFailed", {
          defaultValue: "更新供应商失败: {{error}}",
          error: detail,
        }),
      );
    },
    onSettled: async () => {
      if (appId === "pi") {
        await invalidatePiProviderCaches(queryClient);
      }
    },
  });
};

export const useDeleteProviderMutation = (appId: AppId) => {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: async (providerId: string) => {
      await providersApi.delete(providerId, appId);
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["providers", appId] });

      try {
        await providersApi.updateTrayMenu();
      } catch (trayError) {
        console.error(
          "Failed to update tray menu after deleting provider",
          trayError,
        );
      }

      toast.success(
        t("notifications.deleteSuccess", {
          defaultValue: "供应商已删除",
        }),
        {
          closeButton: true,
        },
      );
    },
    onError: (error: Error) => {
      const rawDetail = extractErrorMessage(error);
      const detail =
        (appId === "pi"
          ? translatePiProviderMutationError(rawDetail, t)
          : "") ||
        rawDetail ||
        t("common.unknown");
      toast.error(
        t("notifications.deleteFailed", {
          defaultValue: "删除供应商失败: {{error}}",
          error: detail,
        }),
      );
    },
    onSettled: async () => {
      if (appId === "pi") {
        await invalidatePiProviderCaches(queryClient);
      }
    },
  });
};

/**
 * 切换供应商。环境变量冲突（ENV_CONFLICT）不再降级为一次性 toast：
 * 冲突列表连同「重试切换 / 清空提示」闭包发布到 @/lib/api/env.ts 的模块级
 * 订阅桥（publishEnvConflictPrompt，上方注释已说明其存在性），由 main.tsx
 * 挂载的 EnvConflictDialogHost 渲染对话框，让用户在「接管并切换」与
 * 「取消切换」之间显式选择，并看到每个外来变量的末 4 位（§5.3.3）。
 * 非冲突类失败维持原有 toast 行为。
 */
export const useSwitchProviderMutation = (appId: AppId) => {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  const mutation = useMutation({
    mutationFn: async (providerId: string): Promise<SwitchResult> => {
      return await providersApi.switch(providerId, appId);
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["providers", appId] });
      try {
        await providersApi.updateTrayMenu();
      } catch (trayError) {
        console.error(
          "Failed to update tray menu after switching provider",
          trayError,
        );
      }
    },
    onError: (error: Error, providerId: string) => {
      const conflicts = parseEnvConflictError(error);
      if (conflicts && conflicts.length > 0) {
        const prompt: EnvConflictPrompt = {
          app: appId,
          providerId,
          conflicts,
          // 对话框在 envDeliveryAdopt 成功后先 clear 再 retry，重跑本次切换
          retry: () => mutation.mutate(providerId),
          onAdopted: clearEnvConflictPrompt,
          onCancel: clearEnvConflictPrompt,
        };
        publishEnvConflictPrompt(prompt);
        return;
      }
      const detail = extractErrorMessage(error) || t("common.unknown");
      toast.error(
        t("notifications.switchFailedTitle", { defaultValue: "切换失败" }),
        {
          description: t("notifications.switchFailed", {
            defaultValue: "切换失败：{{error}}",
            error: detail,
          }),
          duration: 6000,
          action: {
            label: t("common.copy", { defaultValue: "复制" }),
            onClick: () => {
              navigator.clipboard?.writeText(detail).catch(() => undefined);
            },
          },
        },
      );
    },
    onSettled: async () => {
      if (appId === "pi") {
        await invalidatePiProviderCaches(queryClient);
      }
    },
  });

  return mutation;
};

export const useDeleteSessionMutation = () => {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: async (input: DeleteSessionOptions) => {
      await sessionsApi.delete(input);
      return input;
    },
    onSuccess: async (input) => {
      queryClient.setQueryData<SessionMeta[]>(["sessions"], (current) =>
        (current ?? []).filter(
          (session) =>
            !(
              session.providerId === input.providerId &&
              session.sessionId === input.sessionId &&
              session.sourcePath === input.sourcePath
            ),
        ),
      );
      queryClient.removeQueries({
        queryKey: ["sessionMessages", input.providerId, input.sourcePath],
      });

      await queryClient.invalidateQueries({ queryKey: ["sessions"] });

      toast.success(
        t("sessionManager.sessionDeleted", {
          defaultValue: "会话已删除",
        }),
      );
    },
    onError: (error: Error) => {
      const detail = extractErrorMessage(error) || t("common.unknown");
      toast.error(
        t("sessionManager.deleteFailed", {
          defaultValue: "删除会话失败: {{error}}",
          error: detail,
        }),
      );
    },
  });
};

export const useSaveSettingsMutation = () => {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (settings: Settings) => {
      await settingsApi.save(settings);
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["settings"] });
    },
  });
};
