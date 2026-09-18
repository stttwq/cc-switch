import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { EnvConflictDialog } from "@/components/env/EnvConflictDialog";
import type { EnvConflictPrompt } from "@/lib/api/env";

// 只拦截对话框实际调用的 API；类型导入会被编译期擦除，不受该 mock 影响。
const envDeliveryAdoptMock = vi.fn();
vi.mock("@/lib/api/env", () => ({
  envDeliveryAdopt: (...args: unknown[]) => envDeliveryAdoptMock(...args),
}));

// 与 AddProviderDialog.test.tsx 相同的轻量 Dialog mock，绕开 Radix 传送门。
vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => (
    <div role="dialog">{children}</div>
  ),
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <h2>{children}</h2>
  ),
  DialogDescription: ({ children }: { children: React.ReactNode }) => (
    <p>{children}</p>
  ),
  DialogFooter: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
}));

function makePrompt(
  overrides: Partial<EnvConflictPrompt> = {},
): EnvConflictPrompt {
  return {
    app: "claude",
    providerId: "p-1",
    conflicts: [
      { name: "ANTHROPIC_AUTH_TOKEN", owner: "foreign", maskedValue: "abcd" },
      { name: "ANTHROPIC_BASE_URL", owner: "foreign", maskedValue: "wxyz" },
    ],
    retry: vi.fn(),
    onAdopted: vi.fn(),
    onCancel: vi.fn(),
    ...overrides,
  };
}

describe("EnvConflictDialog", () => {
  beforeEach(() => {
    envDeliveryAdoptMock.mockReset();
    envDeliveryAdoptMock.mockResolvedValue(undefined);
  });

  it("逐行显示变量名与末 4 位遮罩值，且不显示任何完整值", () => {
    // 契约限制：DeliveryConflict 只有 name/owner/maskedValue，后端从不返回完整值，
    // 因此这里往对象上夹带一个自造的 value 字段（cast 绕过类型），
    // 验证组件只读取 maskedValue，即便数据里混入了完整值也不会渲染。
    const fullValue = "sk-full-secret-should-never-render";
    const prompt = makePrompt({
      conflicts: [
        {
          name: "ANTHROPIC_AUTH_TOKEN",
          owner: "foreign",
          maskedValue: "abcd",
          value: fullValue,
        },
        {
          name: "ANTHROPIC_BASE_URL",
          owner: "foreign",
          maskedValue: "wxyz",
          value: `https://${fullValue}.dev`,
        },
      ] as unknown as EnvConflictPrompt["conflicts"],
    });

    render(<EnvConflictDialog open prompt={prompt} />);

    expect(screen.getByText("ANTHROPIC_AUTH_TOKEN")).toBeInTheDocument();
    expect(screen.getByText("ANTHROPIC_BASE_URL")).toBeInTheDocument();
    expect(screen.getByText("abcd")).toBeInTheDocument();
    expect(screen.getByText("wxyz")).toBeInTheDocument();
    expect(screen.getByText("envConflict.overwriteNotice")).toBeInTheDocument();
    expect(document.body.textContent).not.toContain(fullValue);
  });

  it("点「接管并切换」以正确参数调用 envDeliveryAdopt，成功后 clear 并 retry", async () => {
    const prompt = makePrompt();
    render(<EnvConflictDialog open prompt={prompt} />);

    fireEvent.click(screen.getByRole("button", { name: "envConflict.adopt" }));

    await waitFor(() => {
      expect(envDeliveryAdoptMock).toHaveBeenCalledWith("claude", "p-1", [
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
      ]);
    });
    expect(prompt.onAdopted).toHaveBeenCalledTimes(1);
    expect(prompt.retry).toHaveBeenCalledTimes(1);
    expect(prompt.onCancel).not.toHaveBeenCalled();
  });

  it("点「取消」不调用 envDeliveryAdopt，只关闭对话框", () => {
    const prompt = makePrompt();
    render(<EnvConflictDialog open prompt={prompt} />);

    fireEvent.click(screen.getByRole("button", { name: "envConflict.cancel" }));

    expect(envDeliveryAdoptMock).not.toHaveBeenCalled();
    expect(prompt.retry).not.toHaveBeenCalled();
    expect(prompt.onAdopted).not.toHaveBeenCalled();
    expect(prompt.onCancel).toHaveBeenCalledTimes(1);
  });
});
