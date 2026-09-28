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

const previewSqlImportMock = vi.fn();
const importConfigConfirmedMock = vi.fn();
const exportViaDialogMock = vi.fn();

vi.mock("@/lib/api", () => ({
  settingsApi: {
    previewSqlImportViaDialog: (...args: unknown[]) =>
      previewSqlImportMock(...args),
    importConfigConfirmed: (...args: unknown[]) =>
      importConfigConfirmedMock(...args),
    exportConfigViaDialog: (...args: unknown[]) => exportViaDialogMock(...args),
  },
}));

const previewPayload = {
  pathToken: "token-456",
  meta: null,
  sizeBytes: 2048,
};

describe("useImportExport Hook (edge cases)", () => {
  beforeEach(() => {
    previewSqlImportMock.mockReset();
    importConfigConfirmedMock.mockReset();
    exportViaDialogMock.mockReset();
    toastSuccessMock.mockReset();
    toastErrorMock.mockReset();
    toastWarningMock.mockReset();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("resetStatus clears errors", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
      success: false,
      message: "broken",
    });
    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    await act(async () => {
      await result.current.confirmImport();
    });

    act(() => {
      result.current.resetStatus();
    });

    expect(result.current.status).toBe("idle");
    expect(result.current.errorMessage).toBeNull();
    expect(result.current.backupId).toBeNull();
  });

  it("does not call onImportSuccess when import fails", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
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
    await act(async () => {
      await result.current.confirmImport();
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

  it("does not re-import with a consumed token when confirm runs twice", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
      success: true,
      backupId: "b-1",
    });
    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    await act(async () => {
      await result.current.confirmImport();
    });
    // S6-2：确认一次后 pendingPreview 已清空，重复确认不应再发命令。
    await act(async () => {
      await result.current.confirmImport();
    });

    expect(importConfigConfirmedMock).toHaveBeenCalledTimes(1);
    expect(result.current.status).toBe("success");
  });
});
