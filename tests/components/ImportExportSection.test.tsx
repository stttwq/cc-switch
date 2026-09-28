import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { ImportExportSection } from "@/components/settings/ImportExportSection";
import { useSettingsQuery } from "@/lib/query";

const tMock = vi.fn((key: string) => key);

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: tMock }),
}));

// S6-1：导出说明文案随凭据后端模式切换，mock 掉 settings 查询。
vi.mock("@/lib/query", () => ({
  useSettingsQuery: vi.fn(),
}));

const mockedUseSettingsQuery = vi.mocked(useSettingsQuery);

describe("ImportExportSection Component", () => {
  const baseProps = {
    status: "idle" as const,
    errorMessage: null,
    backupId: null,
    isImporting: false,
    pendingPreview: null,
    importResult: null,
    onImport: vi.fn(),
    onConfirmImport: vi.fn(),
    onCancelImport: vi.fn(),
    onExport: vi.fn(),
  };

  beforeEach(() => {
    tMock.mockImplementation((key: string) => key);
    baseProps.onImport.mockReset();
    baseProps.onConfirmImport.mockReset();
    baseProps.onCancelImport.mockReset();
    baseProps.onExport.mockReset();
    mockedUseSettingsQuery.mockReturnValue({
      data: { secretBackend: "onepassword" },
    } as never);
  });

  it("triggers import and export from the two buttons", () => {
    render(<ImportExportSection {...baseProps} />);

    // 计划 4.2.1 S-2：按钮直接触发导入（S6-2 起为「预览」，确认在弹框里）
    fireEvent.click(screen.getByRole("button", { name: /settings\.import/ }));
    expect(baseProps.onImport).toHaveBeenCalledTimes(1);

    fireEvent.click(
      screen.getByRole("button", { name: "settings.exportConfig" }),
    );
    expect(baseProps.onExport).toHaveBeenCalledTimes(1);
  });

  // S6-2：确认框展示 meta 来源信息；确认/取消分别回调。
  it("shows the import confirm dialog with meta and confirms on click", () => {
    render(
      <ImportExportSection
        {...baseProps}
        pendingPreview={{
          pathToken: "token-1",
          meta: {
            purpose: "config",
            backend: "onepassword",
            endpoints: false,
            refs: 3,
            device: "PC-1",
            exportedAt: "2026-09-28T00:00:00Z",
          },
          sizeBytes: 4096,
        }}
      />,
    );

    expect(
      screen.getByText("settings.importPreview.title"),
    ).toBeInTheDocument();
    expect(screen.getByText("PC-1")).toBeInTheDocument();
    expect(
      screen.getByText("settings.importPreview.backendOnePassword"),
    ).toBeInTheDocument();
    // 不含端点（1P 模式导出）时显示否定文案并给出原因。
    expect(
      screen.getByText("settings.importPreview.endpointsNo"),
    ).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "settings.importPreview.confirm" }),
    );
    expect(baseProps.onConfirmImport).toHaveBeenCalledTimes(1);
  });

  it("shows the legacy-file note when the export has no meta", () => {
    render(
      <ImportExportSection
        {...baseProps}
        pendingPreview={{
          pathToken: "token-2",
          meta: null,
          sizeBytes: 4096,
        }}
      />,
    );

    expect(
      screen.getByText("settings.importPreview.metaNone"),
    ).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", { name: "settings.importPreview.confirm" }),
    );
    expect(baseProps.onConfirmImport).toHaveBeenCalledTimes(1);
  });

  it("calls onCancelImport when the confirm dialog is cancelled", () => {
    render(
      <ImportExportSection
        {...baseProps}
        pendingPreview={{
          pathToken: "token-3",
          meta: null,
          sizeBytes: 4096,
        }}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "common.cancel" }));
    expect(baseProps.onCancelImport).toHaveBeenCalledTimes(1);
    expect(baseProps.onConfirmImport).not.toHaveBeenCalled();
  });

  it("shows adopted refs and unlinked providers in the success message", () => {
    render(
      <ImportExportSection
        {...baseProps}
        status="success"
        backupId="backup-001"
        importResult={{ adoptedRefs: 2, unlinkedProviders: 1 }}
      />,
    );

    expect(screen.getByText("settings.importSuccess")).toBeInTheDocument();
    expect(screen.getByText(/backup-001/)).toBeInTheDocument();
    // tMock 只回显键名；此处断言统计行确实渲染。
    expect(screen.getByText("settings.importAdoptedRefs")).toBeInTheDocument();
    expect(
      screen.getByText("settings.importUnlinkedProviders"),
    ).toBeInTheDocument();
  });

  it("should show loading text and disable import button during import", () => {
    render(
      <ImportExportSection {...baseProps} isImporting status="importing" />,
    );

    const importingLabels = screen.getAllByText("settings.importing");
    expect(importingLabels.length).toBeGreaterThanOrEqual(2);
    expect(
      screen.getByRole("button", { name: "settings.importing" }),
    ).toBeDisabled();
    expect(screen.getByText("common.loading")).toBeInTheDocument();
  });

  it("should display backup information on successful import", () => {
    render(
      <ImportExportSection
        {...baseProps}
        status="success"
        backupId="backup-001"
      />,
    );

    expect(screen.getByText("settings.importSuccess")).toBeInTheDocument();
    expect(screen.getByText(/backup-001/)).toBeInTheDocument();
    expect(screen.getByText("settings.autoReload")).toBeInTheDocument();
  });

  it("should display error message when import fails", () => {
    render(
      <ImportExportSection
        {...baseProps}
        status="error"
        errorMessage="Parse failed"
      />,
    );

    expect(screen.getByText("settings.importFailed")).toBeInTheDocument();
    expect(screen.getByText("Parse failed")).toBeInTheDocument();
  });

  it("shows the 1Password export hint in 1P mode", () => {
    mockedUseSettingsQuery.mockReturnValue({
      data: { secretBackend: "onepassword" },
    } as never);
    render(<ImportExportSection {...baseProps} />);

    expect(
      screen.getByText("settings.exportHintOnePassword"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("settings.exportHintCredentialManager"),
    ).not.toBeInTheDocument();
  });

  it("shows the credential-manager export hint in credential-manager mode", () => {
    mockedUseSettingsQuery.mockReturnValue({
      data: { secretBackend: "credential_manager" },
    } as never);
    render(<ImportExportSection {...baseProps} />);

    expect(
      screen.getByText("settings.exportHintCredentialManager"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("settings.exportHintOnePassword"),
    ).not.toBeInTheDocument();
  });
});
