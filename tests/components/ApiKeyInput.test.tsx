import { act, fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import i18n from "i18next";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import ApiKeyInput from "@/components/providers/forms/ApiKeyInput";
import { providersApi } from "@/lib/api";

// setupTests 以空资源初始化 i18n；这里补上生产里真实存在的键（zh），
// 让「已配置」提示与眼睛按钮的可访问名在断言中可验证。
beforeAll(() => {
  i18n.addResourceBundle(
    "zh",
    "translation",
    {
      providerForm: {
        apiKeyConfigured: "已配置，留空保持不变",
        apiKeyConfiguredHint: "已配置，留空保持不变，点击右侧眼睛查看",
      },
      apiKeyInput: {
        placeholder: "请输入API Key",
        show: "显示API Key",
        hide: "隐藏API Key",
        reveal: "查看已保存的API Key",
        revealFailed: "读取已保存的API Key失败，请稍后重试",
      },
    },
    true,
    true,
  );
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

const REVEALED = "sk-revealed-key-1234";
const REVEAL_TARGET = { app: "claude" as const, providerId: "p1" };

function getInput(): HTMLInputElement {
  return screen.getByLabelText("API Key") as HTMLInputElement;
}

function getEye(): HTMLButtonElement {
  return screen.getByRole("button");
}

/** 受控外壳：模拟上层把 onChange 的值写回 value 的真实行为。 */
function Harness({
  initialValue = "",
  present = true,
}: {
  initialValue?: string;
  present?: boolean;
}) {
  const [value, setValue] = useState(initialValue);
  return (
    <ApiKeyInput
      value={value}
      onChange={setValue}
      configuredStatus={{ present }}
      revealTarget={REVEAL_TARGET}
    />
  );
}

describe("ApiKeyInput（§5.2.2 前端零密钥 + §1.4.4 按需回显）", () => {
  it("present=true 时不回显存量值：input value 为空串、显示圆点占位、眼睛可见", () => {
    const stored = "sk-stored-key-abcd";
    render(
      <ApiKeyInput
        value={stored}
        onChange={vi.fn()}
        configuredStatus={{ present: true }}
        revealTarget={REVEAL_TARGET}
      />,
    );

    const input = getInput();
    // 核心验收：任何已存密钥值都不得进入 input 的 value
    expect(input.value).toBe("");
    expect(input.type).toBe("password");
    // 圆点是 placeholder，不是 value
    expect(input.placeholder).toBe("••••••••");
    // 眼睛按钮必须存在（旧实现只在 displayValue 非空时渲染，导致编辑时无从回显）
    expect(getEye()).toHaveAccessibleName("查看已保存的API Key");
    // 页面文本只说「已配置」，不含任何密钥片段
    expect(
      screen.getByText("已配置，留空保持不变，点击右侧眼睛查看"),
    ).toBeInTheDocument();
    expect(document.body.textContent).not.toContain(stored);
  });

  it("点眼睛按需回显一次：reveal 被调用、输入框切到明文、再点恢复遮罩", async () => {
    const spy = vi
      .spyOn(providersApi, "revealSecret")
      .mockResolvedValue(REVEALED);

    render(<Harness />);

    await act(async () => {
      fireEvent.click(getEye());
    });

    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith("claude", "p1", "api_key");
    expect(getInput().type).toBe("text");
    expect(getInput().value).toBe(REVEALED);

    // 再点一次只是切回遮罩，不会重新读一次
    fireEvent.click(getEye());
    expect(getInput().type).toBe("password");
    expect(spy).toHaveBeenCalledTimes(1);
  });

  it("回显失败时显示错误文案，且不清空已配置状态", async () => {
    vi.spyOn(providersApi, "revealSecret").mockRejectedValue(
      new Error("credential backend error"),
    );

    render(<Harness />);

    await act(async () => {
      fireEvent.click(getEye());
    });

    expect(
      screen.getByText("读取已保存的API Key失败，请稍后重试"),
    ).toBeInTheDocument();
    // 仍然是「已配置」：失败不改变 present 语义
    expect(
      screen.getByText("已配置，留空保持不变，点击右侧眼睛查看"),
    ).toBeInTheDocument();
    expect(getInput().value).toBe("");
  });

  it("凭据条目丢失（reveal 返回 null）按失败处理", async () => {
    vi.spyOn(providersApi, "revealSecret").mockResolvedValue(null);

    render(<Harness />);

    await act(async () => {
      fireEvent.click(getEye());
    });

    expect(
      screen.getByText("读取已保存的API Key失败，请稍后重试"),
    ).toBeInTheDocument();
  });

  it("显示态 60 秒后自动重新遮罩（决策 A4）", async () => {
    vi.useFakeTimers();
    render(<Harness initialValue="sk-typed-by-user" present={false} />);

    expect(getInput().type).toBe("password");
    fireEvent.click(getEye());
    expect(getInput().type).toBe("text");

    await act(async () => {
      vi.advanceTimersByTime(60_000);
    });

    expect(getInput().type).toBe("password");
  });

  it("失焦立即重新遮罩（决策 A4）", () => {
    render(<Harness initialValue="sk-typed-by-user" present={false} />);

    fireEvent.click(getEye());
    expect(getInput().type).toBe("text");

    fireEvent.blur(getInput());
    expect(getInput().type).toBe("password");
  });

  it("present=false 时正常回显 value 且不显示「已配置」", () => {
    const { rerender } = render(
      <ApiKeyInput
        value="sk-visible-wxyz"
        onChange={vi.fn()}
        configuredStatus={{ present: false }}
      />,
    );

    expect(getInput().value).toBe("sk-visible-wxyz");
    expect(screen.queryByText(/已配置/)).not.toBeInTheDocument();

    // 未传 configuredStatus（新增供应商场景）同样正常回显
    rerender(<ApiKeyInput value="sk-visible-wxyz" onChange={vi.fn()} />);
    expect(getInput().value).toBe("sk-visible-wxyz");
    expect(screen.queryByText(/已配置/)).not.toBeInTheDocument();
  });

  it("已配置时用户键入的新值正常回显并上报", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <ApiKeyInput
        value=""
        onChange={onChange}
        configuredStatus={{ present: true }}
      />,
    );

    fireEvent.change(getInput(), { target: { value: "sk-new-1234" } });
    expect(onChange).toHaveBeenCalledWith("sk-new-1234");

    // 上层受控更新后，用户自己输入的新值正常回显（不回显的只是存量密钥）
    rerender(
      <ApiKeyInput
        value="sk-new-1234"
        onChange={onChange}
        configuredStatus={{ present: true }}
      />,
    );
    expect(getInput().value).toBe("sk-new-1234");
  });
});
