import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useImportExport } from "@/hooks/useImportExport";

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

describe("useImportExport Hook", () => {
  it("should set success status, record backup ID, and call callback on successful import", async () => {
    importViaDialogMock.mockResolvedValue({
      success: true,
      backupId: "backup-123",
    });
    const onImportSuccess = vi.fn();

    const { result } = renderHook(() => useImportExport({ onImportSuccess }));

    await act(async () => {
      await result.current.importConfig();
    });

    // 计划 4.2.1 S-2：命令自己弹对话框，前端不再传路径
    expect(importViaDialogMock).toHaveBeenCalledWith();
    expect(result.current.status).toBe("success");
    expect(result.current.backupId).toBe("backup-123");
    expect(toastSuccessMock).toHaveBeenCalledTimes(1);
    expect(onImportSuccess).toHaveBeenCalledTimes(1);
  });

  it("should stay idle and silent when the user cancels the import dialog", async () => {
    importViaDialogMock.mockResolvedValue(null);

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("idle");
    expect(result.current.errorMessage).toBeNull();
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("should show error message when import result fails", async () => {
    importViaDialogMock.mockResolvedValue({
      success: false,
      message: "Config corrupted",
    });

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("error");
    expect(result.current.errorMessage).toBe("Config corrupted");
    expect(toastErrorMock).toHaveBeenCalledWith("Config corrupted");
  });

  it("should catch and display error when import process throws exception", async () => {
    importViaDialogMock.mockRejectedValue(new Error("Import failed"));

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("error");
    expect(result.current.errorMessage).toBe("Import failed");
    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining("导入配置失败:"),
    );
  });

  it("should export successfully with a generated default filename and show path in toast", async () => {
    exportViaDialogMock.mockResolvedValue({
      success: true,
      filePath: "/backup/export.json",
    });

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(exportViaDialogMock).toHaveBeenCalledTimes(1);
    expect(exportViaDialogMock.mock.calls[0][0]).toMatch(
      /^cc-switch-export-\d{8}_\d{6}\.sql$/,
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

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining("Write failed"),
    );
  });

  it("should catch and show error when export throws exception", async () => {
    exportViaDialogMock.mockRejectedValue(new Error("Disk read-only"));

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastErrorMock).toHaveBeenCalledWith(
      expect.stringContaining("Disk read-only"),
    );
  });

  it("should stay silent when the user cancels the save dialog during export", async () => {
    exportViaDialogMock.mockResolvedValue(null);

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.exportConfig();
    });

    expect(toastSuccessMock).not.toHaveBeenCalled();
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("should restore initial values when resetting status", async () => {
    importViaDialogMock.mockResolvedValue({
      success: false,
      message: "Config corrupted",
    });

    const { result } = renderHook(() => useImportExport());

    await act(async () => {
      await result.current.importConfig();
    });

    expect(result.current.status).toBe("error");

    act(() => {
      result.current.resetStatus();
    });

    expect(result.current.status).toBe("idle");
    expect(result.current.errorMessage).toBeNull();
    expect(result.current.backupId).toBeNull();
  });
});
