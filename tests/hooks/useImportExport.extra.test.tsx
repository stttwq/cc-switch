import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useImportExport } from "@/hooks/useImportExport";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

// S6-3：hook 内部用 useQueryClient 失效 settings 缓存，测试需要 Provider 包裹。
const renderHookWithClient = (
  callback: () => ReturnType<typeof useImportExport>,
) => {
  const queryClient = new QueryClient();
  return renderHook(callback, {
    wrapper: ({ children }: { children?: React.ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    ),
  });
};

const toastSuccessMock = vi.fn();
const toastErrorMock = vi.fn();
const toastWarningMock = vi.fn();

vi.mock("sonner", () => ({
  toast: {
    success: (...args: unknown[]) => toastSuccessMock(...args),
    error: (...args: unknown[]) => toastErrorMock(...args),
    warning: (...args: unknown[]) => toastWarningMock(...args),
  },
}));

const importViaDialogMock = vi.fn();
const exportViaDialogMock = vi.fn();
const syncCurrentProvidersLiveMock = vi.fn();

vi.mock("@/lib/api", () => ({
  settingsApi: {
    importConfigViaDialog: (...args: unknown[]) => importViaDialogMock(...args),
    exportConfigViaDialog: (...args: unknown[]) => exportViaDialogMock(...args),
    syncCurrentProvidersLive: (...args: unknown[]) =>
      syncCurrentProvidersLiveMock(...args),
  },
}));

describe("useImportExport Hook (edge cases)", () => {
  beforeEach(() => {
    importViaDialogMock.mockReset();
    exportViaDialogMock.mockReset();
    toastSuccessMock.mockReset();
    toastErrorMock.mockReset();
    toastWarningMock.mockReset();
    syncCurrentProvidersLiveMock.mockReset();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("resetStatus clears errors", async () => {
    importViaDialogMock.mockResolvedValue({
      success: false,
      message: "broken",
    });
    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    act(() => {
      result.current.resetStatus();
    });

    expect(result.current.status).toBe("idle");
    expect(result.current.errorMessage).toBeNull();
    expect(result.current.backupId).toBeNull();
  });

  it("does not call onImportSuccess when import fails", async () => {
    importViaDialogMock.mockResolvedValue({
      success: false,
      message: "invalid",
    });
    const onImportSuccess = vi.fn();
    const { result } = renderHookWithClient(() =>
      useImportExport({ onImportSuccess }),
    );

    await act(async () => {
      await result.current.importConfig();
    });

    expect(onImportSuccess).not.toHaveBeenCalled();
    expect(result.current.status).toBe("error");
  });

  it("propagates export success message to toast with saved path", async () => {
    exportViaDialogMock.mockResolvedValue({
      success: true,
      filePath: "/final/config.json",
    });
    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastSuccessMock).toHaveBeenCalledWith(
      expect.stringContaining("/final/config.json"),
      expect.objectContaining({ closeButton: true }),
    );
  });

  it("marks partial success when live sync fails after a successful import", async () => {
    importViaDialogMock.mockResolvedValue({ success: true, backupId: "b-1" });
    syncCurrentProvidersLiveMock.mockRejectedValue(new Error("sync down"));
    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("partial-success");
    expect(toastWarningMock).toHaveBeenCalledTimes(1);
  });
});
