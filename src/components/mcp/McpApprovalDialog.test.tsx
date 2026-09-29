import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import McpApprovalDialog from "./McpApprovalDialog";

// SEC-A（方案 §4.3-9）：审批确认框必须完整展示执行语义字段；env/header 的
// 值默认掩码（可能含凭据），用户主动开启后才显示。

const server = {
  id: "ext",
  name: "External Server",
  server: {
    type: "stdio",
    command: "node",
    args: ["server.js", "--port", "8080"],
    cwd: "/srv/agent",
    env: { API_TOKEN: "sk-super-secret", DEBUG: "1" },
  },
  apps: { claude: true, codex: false },
} as never;

function renderDialog(overrides?: { onConfirm?: () => void }) {
  return render(
    <McpApprovalDialog
      isOpen
      server={server}
      app="claude"
      pending={false}
      onConfirm={overrides?.onConfirm ?? vi.fn()}
      onCancel={vi.fn()}
    />,
  );
}

describe("McpApprovalDialog", () => {
  it("shows executable fields in plain text but masks env values by default", () => {
    renderDialog();

    // 执行语义字段必须可见，不能以脱敏为由隐藏
    expect(screen.getByText("node")).toBeInTheDocument();
    expect(screen.getByText("server.js")).toBeInTheDocument();
    expect(screen.getByText("8080")).toBeInTheDocument();
    expect(screen.getByText("/srv/agent")).toBeInTheDocument();

    // env 值默认掩码，但键名可见
    expect(screen.getByText("API_TOKEN")).toBeInTheDocument();
    expect(screen.queryByText("sk-super-secret")).not.toBeInTheDocument();
    expect(screen.getAllByText("••••••••").length).toBe(2);
  });

  it("reveals env values only after the user opts in", async () => {
    const user = userEvent.setup();
    renderDialog();

    await user.click(screen.getByRole("switch"));
    expect(screen.getByText("sk-super-secret")).toBeInTheDocument();
    expect(screen.getByText("1")).toBeInTheDocument();
  });

  it("invokes onConfirm when the approve button is clicked", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    renderDialog({ onConfirm });

    await user.click(screen.getByText("mcp.approval.approveAndEnable"));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });
});
