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
    onImport: vi.fn(),
    onExport: vi.fn(),
  };

  beforeEach(() => {
    tMock.mockImplementation((key: string) => key);
    baseProps.onImport.mockReset();
    baseProps.onExport.mockReset();
    mockedUseSettingsQuery.mockReturnValue({
      data: { secretBackend: "onepassword" },
    } as never);
  });

  it("triggers import and export from the two buttons", () => {
    render(<ImportExportSection {...baseProps} />);

    // 计划 4.2.1 S-2：选文件与导入合成一步，按钮直接触发导入
    fireEvent.click(screen.getByRole("button", { name: /settings\.import/ }));
    expect(baseProps.onImport).toHaveBeenCalledTimes(1);

    fireEvent.click(
      screen.getByRole("button", { name: "settings.exportConfig" }),
    );
    expect(baseProps.onExport).toHaveBeenCalledTimes(1);
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
