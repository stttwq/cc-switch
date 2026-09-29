import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import UnifiedMcpPanel from "./UnifiedMcpPanel";

// SEC-A（方案 §4.3-5/9）：未批准内容的启用操作必须先经审批确认框，
// 不得直接 toggle；已批准条目的开关与禁用操作不受影响。

const toggleMutateAsync = vi.fn();
const approveMutateAsync = vi.fn();

vi.mock("@/hooks/useMcp", () => ({
  useAllMcpServers: () => ({
    data: {
      ext: {
        id: "ext",
        name: "Pending Server",
        server: { type: "stdio", command: "echo", args: ["hi"] },
        apps: { claude: true, codex: false },
      },
      off: {
        id: "off",
        name: "Pending Disabled Server",
        server: { type: "stdio", command: "echo", args: ["off"] },
        apps: { claude: false, codex: false },
      },
      ok: {
        id: "ok",
        name: "Approved Server",
        server: { type: "stdio", command: "echo" },
        apps: { claude: true, codex: false },
      },
    },
    isLoading: false,
  }),
  useMcpApprovalStates: () => ({
    data: [
      {
        serverId: "ext",
        apps: {
          claude: { enabled: true, approved: false, revision: "rev-1" },
          codex: { enabled: false, approved: false, revision: "rev-1" },
        },
      },
      {
        serverId: "off",
        apps: {
          claude: { enabled: false, approved: false, revision: "rev-3" },
          codex: { enabled: false, approved: false, revision: "rev-3" },
        },
      },
      {
        serverId: "ok",
        apps: {
          claude: { enabled: true, approved: true, revision: "rev-2" },
          codex: { enabled: false, approved: true, revision: "rev-2" },
        },
      },
    ],
  }),
  useToggleMcpApp: () => ({
    mutateAsync: toggleMutateAsync,
    isPending: false,
    variables: undefined,
  }),
  useBulkToggleMcpApp: () => ({
    mutateAsync: vi.fn(),
    isPending: false,
    variables: undefined,
  }),
  useDeleteMcpServer: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useImportMcpFromApps: () => ({ mutateAsync: vi.fn(), isPending: false }),
  useApproveMcpServer: () => ({
    mutateAsync: approveMutateAsync,
    isPending: false,
  }),
}));

vi.mock("@/lib/api", () => ({
  settingsApi: { openExternal: vi.fn() },
}));

function renderPanel() {
  return render(<UnifiedMcpPanel onOpenChange={vi.fn()} />);
}

describe("UnifiedMcpPanel MCP 审批拦截", () => {
  beforeEach(() => {
    toggleMutateAsync.mockClear();
    approveMutateAsync.mockClear();
  });

  it("shows a pending badge for unapproved servers", () => {
    renderPanel();
    // ext 与 off 未批准（ok 已批准），两枚徽标
    expect(screen.getAllByTestId("mcp-pending-badge").length).toBe(2);
  });

  it("opens the approval dialog from the pending badge click", async () => {
    const user = userEvent.setup();
    renderPanel();

    await user.click(screen.getAllByTestId("mcp-pending-badge")[0]);

    expect(
      screen.getByText("mcp.approval.title"),
      "必须打开审批确认框",
    ).toBeInTheDocument();
    expect(toggleMutateAsync).not.toHaveBeenCalled();
  });

  it("opens the approval dialog instead of enabling a pending-and-disabled server", async () => {
    const user = userEvent.setup();
    renderPanel();

    // 行序：ext、off、ok。「off」行的 Claude 开关当前禁用，点击=启用，
    // 必须被拦截到审批框而不是直接 toggle
    await user.click(screen.getAllByRole("button", { name: "Claude" })[1]);

    expect(screen.getByText("mcp.approval.title")).toBeInTheDocument();
    expect(toggleMutateAsync).not.toHaveBeenCalled();
  });

  it("toggles an approved server directly without the dialog", async () => {
    const user = userEvent.setup();
    toggleMutateAsync.mockResolvedValue(undefined);
    renderPanel();

    // 「ok」行已批准，点击开关直接 toggle
    await user.click(screen.getAllByRole("button", { name: "Claude" })[2]);

    expect(toggleMutateAsync).toHaveBeenCalledTimes(1);
    expect(screen.queryByText("mcp.approval.title")).not.toBeInTheDocument();
  });
});
