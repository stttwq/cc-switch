/**
 * 全局出站代理 React Hooks（D10）
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  getGlobalProxyUrl,
  setGlobalProxyUrl,
  testProxyUrl,
  type ProxyTestResult,
} from "@/lib/api/globalProxy";

/**
 * 获取全局代理 URL
 */
export function useGlobalProxyUrl() {
  return useQuery({
    queryKey: ["globalProxyUrl"],
    queryFn: getGlobalProxyUrl,
    staleTime: 30 * 1000, // 30秒内不重新获取，避免展开时闪烁
  });
}

/**
 * 设置全局代理 URL
 */
export function useSetGlobalProxyUrl() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: setGlobalProxyUrl,
    onSuccess: () => {
      toast.success(t("settings.globalProxy.saved"));
      queryClient.invalidateQueries({ queryKey: ["globalProxyUrl"] });
    },
    onError: (error: unknown) => {
      const message =
        error instanceof Error
          ? error.message
          : typeof error === "string"
            ? error
            : "Unknown error";
      toast.error(t("settings.globalProxy.saveFailed", { error: message }));
    },
  });
}

/**
 * 测试代理连接
 */
export function useTestProxy() {
  const { t } = useTranslation();

  return useMutation({
    mutationFn: testProxyUrl,
    onSuccess: (result: ProxyTestResult) => {
      if (result.success) {
        toast.success(
          t("settings.globalProxy.testSuccess", { latency: result.latencyMs }),
        );
      } else {
        toast.error(
          t("settings.globalProxy.testFailed", { error: result.error }),
        );
      }
    },
    onError: (error: Error) => {
      toast.error(error.message);
    },
  });
}
