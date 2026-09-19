import { fireEvent, render, screen } from "@testing-library/react";
import i18n from "i18next";
import { beforeAll, describe, expect, it, vi } from "vitest";
import ApiKeyInput from "@/components/providers/forms/ApiKeyInput";

// setupTests 以空资源初始化 i18n；这里补上生产里真实存在的键（zh），
// 让「已配置」提示在断言中可验证。
beforeAll(() => {
  i18n.addResourceBundle(
    "zh",
    "translation",
    {
      providerForm: {
        apiKeyConfigured: "已配置，留空保持不变",
      },
    },
    true,
    true,
  );
});

function getInput(): HTMLInputElement {
  return screen.getByLabelText("API Key") as HTMLInputElement;
}

describe("ApiKeyInput（§5.2.2 前端零密钥）", () => {
  it("present=true 时不回显存量值：input value 为空串，且只提示已配置", () => {
    const stored = "sk-stored-key-abcd";
    render(
      <ApiKeyInput
        value={stored}
        onChange={vi.fn()}
        configuredStatus={{ present: true }}
      />,
    );

    const input = getInput();
    // 核心验收：任何已存密钥值都不得进入 input 的 value
    expect(input.value).toBe("");
    expect(input.type).toBe("password");
    // 页面文本只说"已配置"，不含任何密钥片段（§5.2.1 批量读取不碰凭据管理器）
    expect(screen.getByText("已配置，留空保持不变")).toBeInTheDocument();
    expect(document.body.textContent).not.toContain(stored);
  });

  it("present=true 时用户键入的新值正常回显并上报", () => {
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
});
