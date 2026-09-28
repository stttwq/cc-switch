import { useQuery, type QueryClient } from "@tanstack/react-query";
import { piApi } from "@/lib/api/pi";

export const piKeys = {
  all: ["pi"] as const,
  currentState: ["pi", "currentState"] as const,
  sessionDiscovery: ["pi", "sessionDiscovery"] as const,
};

export const invalidatePiProviderCaches = async (queryClient: QueryClient) => {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: piKeys.currentState }),
    queryClient.invalidateQueries({ queryKey: ["providers", "pi"] }),
  ]);
};

export const invalidatePiDirectoryCaches = async (queryClient: QueryClient) => {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: piKeys.all }),
    queryClient.invalidateQueries({ queryKey: ["providers", "pi"] }),
    queryClient.invalidateQueries({ queryKey: ["skills", "installed"] }),
    queryClient.invalidateQueries({ queryKey: ["sessions"] }),
  ]);
};

export function usePiCurrentState(enabled = true) {
  return useQuery({
    queryKey: piKeys.currentState,
    queryFn: () => piApi.getCurrentState(),
    // S1-6（施工方案 §4.2）：与 useProvidersQuery 一致的 5 秒去重；
    // refetchOnWindowFocus 保留，外部改 models.json 后切回窗口即可看到。
    staleTime: 5_000,
    enabled,
  });
}
