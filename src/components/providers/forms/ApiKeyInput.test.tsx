import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import ApiKeyInput from "./ApiKeyInput";
import { providersApi } from "@/lib/api";

// P3（安全方案 §7.1-2 / §9.1「API Key」行）：reveal 只影响展示——
// 读到的明文不得进入受控表单值（onChange），否则「显示后不改就保存」
// 会被当成显式 set 同值提交，后端被迫多取一次整包判等。

vi.mock("@/lib/api", () => ({
  providersApi: {
    revealSecret: vi.fn(),
  },
}));

const renderInput = (onChange = vi.fn()) => {
  render(
    <ApiKeyInput
      value=""
      onChange={onChange}
      configuredStatus={{ present: true }}
      revealTarget={{ app: "claude", providerId: "p1" }}
    />,
  );
  return onChange;
};

const eyeButton = () =>
  screen.getByRole("button", { name: "apiKeyInput.reveal" });

describe("ApiKeyInput 展示与修改意图分离", () => {
  it("点击 reveal 后显示明文，但不触发 onChange（不改就保存 = keep）", async () => {
    const user = userEvent.setup();
    vi.mocked(providersApi.revealSecret).mockResolvedValue("sk-revealed");
    const onChange = renderInput();

    await user.click(eyeButton());

    await screen.findByDisplayValue("sk-revealed");
    expect(providersApi.revealSecret).toHaveBeenCalledWith(
      "claude",
      "p1",
      "api_key",
    );
    expect(onChange).not.toHaveBeenCalled();
  });

  it("reveal 后重新遮罩显示回空占位，受控值仍为空", async () => {
    const user = userEvent.setup();
    vi.mocked(providersApi.revealSecret).mockResolvedValue("sk-revealed");
    const onChange = renderInput();

    await user.click(eyeButton());
    await screen.findByDisplayValue("sk-revealed");
    await user.click(screen.getByRole("button", { name: "apiKeyInput.hide" }));

    expect(screen.getByDisplayValue("")).toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();
  });

  it("reveal 后继续编辑：编辑后的值作为显式 set 意图进入 onChange", async () => {
    const user = userEvent.setup();
    vi.mocked(providersApi.revealSecret).mockResolvedValue("sk-revealed");
    const onChange = renderInput();

    await user.click(eyeButton());
    await screen.findByDisplayValue("sk-revealed");
    const input = screen.getByDisplayValue("sk-revealed");
    await user.type(input, "x");

    expect(onChange).toHaveBeenLastCalledWith("sk-revealedx");
  });

  it("未配置（新建表单）时眼睛退化为纯遮罩开关，不请求回显", async () => {
    const user = userEvent.setup();
    render(
      <ApiKeyInput value="" onChange={() => {}} configuredStatus={null} />,
    );

    await user.click(screen.getByRole("button", { name: "apiKeyInput.show" }));

    expect(providersApi.revealSecret).not.toHaveBeenCalled();
  });
});
