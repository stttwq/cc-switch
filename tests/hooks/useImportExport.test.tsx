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
  pathToken: "token-123",
  meta: null,
  sizeBytes: 1024,
};

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

describe("useImportExport Hook", () => {
  it("previews first, then imports with the one-time token on confirmation", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
      success: true,
      backupId: "backup-123",
    });
    const onImportSuccess = vi.fn();

    const { result } = renderHookWithClient(() =>
      useImportExport({ onImportSuccess }),
    );

    // S6-2：第一步只做预览，不执行导入。
    await act(async () => {
      await result.current.importConfig();
    });
    expect(previewSqlImportMock).toHaveBeenCalledWith();
    expect(importConfigConfirmedMock).not.toHaveBeenCalled();
    expect(result.current.pendingPreview).toEqual(previewPayload);
    expect(result.current.status).toBe("idle");

    // 确认后执行导入，消费 pathToken。
    await act(async () => {
      await result.current.confirmImport();
    });

    expect(importConfigConfirmedMock).toHaveBeenCalledWith("token-123");
    expect(result.current.status).toBe("success");
    expect(result.current.backupId).toBe("backup-123");
    expect(result.current.pendingPreview).toBeNull();
    expect(toastSuccessMock).toHaveBeenCalledTimes(1);
    expect(onImportSuccess).toHaveBeenCalledTimes(1);
  });

  it("records post-import result fields (warning / adoptedRefs / unlinkedProviders)", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
      success: true,
      backupId: "backup-123",
      warning: "部分导入后同步失败: boom",
      adoptedRefs: 2,
      unlinkedProviders: 1,
    });

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    await act(async () => {
      await result.current.confirmImport();
    });

    expect(result.current.status).toBe("success");
    expect(result.current.importResult).toEqual({
      warning: "部分导入后同步失败: boom",
      adoptedRefs: 2,
      unlinkedProviders: 1,
    });
    expect(toastWarningMock).toHaveBeenCalledWith(
      "部分导入后同步失败: boom",
      expect.anything(),
    );
  });

  it("should stay idle and silent when the user cancels the preview dialog", async () => {
    previewSqlImportMock.mockResolvedValue(null);

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("idle");
    expect(result.current.pendingPreview).toBeNull();
    expect(result.current.errorMessage).toBeNull();
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("cancelling the confirm dialog clears the pending preview without importing", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    act(() => {
      result.current.cancelImport();
    });

    expect(result.current.pendingPreview).toBeNull();
    expect(result.current.status).toBe("idle");
    expect(importConfigConfirmedMock).not.toHaveBeenCalled();
  });

  it("should show error message when the confirmed import fails", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
      success: false,
      message: "Config corrupted",
    });

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    await act(async () => {
      await result.current.confirmImport();
    });

    expect(result.current.status).toBe("error");
    expect(result.current.errorMessage).toBe("Config corrupted");
    expect(toastErrorMock).toHaveBeenCalledWith("Config corrupted");
  });

  it("should catch and display error when the confirmed import throws", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockRejectedValue(
      "import.plaintext_pending: boom",
    );

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    await act(async () => {
      await result.current.confirmImport();
    });

    expect(result.current.status).toBe("error");
    expect(result.current.errorMessage).toBe("import.plaintext_pending: boom");
    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining("导入配置失败:"),
    );
    // 导入失败后确认框关闭，避免用户在旧令牌上重复确认。
    expect(result.current.pendingPreview).toBeNull();
  });

  it("surfaces preview errors (unreadable or foreign file)", async () => {
    previewSqlImportMock.mockRejectedValue("打开文件失败: EPERM");

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("error");
    expect(result.current.errorMessage).toBe("打开文件失败: EPERM");
    expect(importConfigConfirmedMock).not.toHaveBeenCalled();
  });

  it("should export successfully with a generated default filename and show path in toast", async () => {
    exportViaDialogMock.mockResolvedValue({
      success: true,
      filePath: "/backup/export.json",
    });

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(exportViaDialogMock).toHaveBeenCalledTimes(1);
    // S6-1：默认文件名体现「配置导出」语义，只到日期粒度。
    expect(exportViaDialogMock.mock.calls[0][0]).toMatch(
      /^cc-switch-config-\d{8}\.sql$/,
    );
    expect(toastSuccessMock).toHaveBeenCalledWith(
      expect.stringContaining("/backup/export.json"),
      expect.objectContaining({ closeButton: true }),
    );
  });

  it("should show error message when export fails", async () => {
    exportViaDialogMock.mockResolvedValue({
      success: false,
      message: "Write failed",
    });

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining("Write failed"),
    );
  });

  it("should catch and show error when export throws exception", async () => {
    exportViaDialogMock.mockRejectedValue(new Error("Disk read-only"));

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining("Disk read-only"),
    );
  });

  it("should stay silent when the user cancels the save dialog during export", async () => {
    exportViaDialogMock.mockResolvedValue(null);

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastSuccessMock).not.toHaveBeenCalled();
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("should restore initial values when resetting status", async () => {
    previewSqlImportMock.mockResolvedValue(previewPayload);
    importConfigConfirmedMock.mockResolvedValue({
      success: false,
      message: "Config corrupted",
    });

    const { result } = renderHookWithClient(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });
    await act(async () => {
      await result.current.confirmImport();
    });

    expect(result.current.status).toBe("error");

    act(() => {
      result.current.resetStatus();
    });

    expect(result.current.status).toBe("idle");
    expect(result.current.errorMessage).toBeNull();
    expect(result.current.backupId).toBeNull();
    expect(result.current.pendingPreview).toBeNull();
    expect(result.current.importResult).toBeNull();
  });
});
