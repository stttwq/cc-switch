import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useForm } from "react-hook-form";
import { Form } from "@/components/ui/form";
import { CodexFormFields } from "./CodexFormFields";

// OPT-A（方案 §11.2-5/6）：Codex 表单的模型拉取请求 generation 隔离。
// endpoint/key/isFullUrl 变化或卸载后，旧请求的结果/toast 不得回灌表单；
// 作废在途请求须重置 loading；旧请求晚到不得清掉新在途请求的 loading。

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    info: vi.fn(),
    error: vi.fn(),
  },
}));

const fetchModelsForConfig = vi.fn();
const showFetchModelsError = vi.fn();

vi.mock("@/lib/api/model-fetch", () => ({
  fetchModelsForConfig: (...args: unknown[]) => fetchModelsForConfig(...args),
  showFetchModelsError: (...args: unknown[]) => showFetchModelsError(...args),
}));

import { toast } from "sonner";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const fetchedModel = { id: "m1", ownedBy: "test" };

/** 提供真实 usage 同款的 react-hook-form 上下文（FormLabel 依赖它） */
function Harness(props: { baseUrl: string; apiKey?: string }) {
  const form = useForm();
  return (
    <Form {...form}>
      <CodexFormFields
        codexApiKey={props.apiKey ?? "key-a"}
        onApiKeyChange={vi.fn()}
        shouldShowApiKeyLink={false}
        websiteUrl=""
        isNonOfficialCategory={false}
        codexBaseUrl={props.baseUrl}
        onBaseUrlChange={vi.fn()}
        isFullUrl={false}
        onFullUrlChange={vi.fn()}
        // Responses 格式让高级区默认展开，获取按钮可见
        apiFormat="openai_responses"
        onApiFormatChange={vi.fn()}
        anthropicAuthField="ANTHROPIC_AUTH_TOKEN"
        onAnthropicAuthFieldChange={vi.fn()}
        // 模型映射区（含获取按钮）仅在可编辑时渲染
        catalogModels={[]}
        onCatalogModelsChange={vi.fn()}
      />
    </Form>
  );
}

const fetchButton = () =>
  screen.getByRole("button", { name: "providerForm.fetchModels" });

beforeEach(() => {
  vi.clearAllMocks();
});

describe("CodexFormFields 模型拉取 generation 隔离", () => {
  it("请求发出后切换 endpoint，旧请求晚到不更新 UI、不弹 toast", async () => {
    const user = userEvent.setup();
    const first = deferred<{ id: string; ownedBy: string }[]>();
    fetchModelsForConfig.mockReturnValueOnce(first.promise);

    const { rerender } = render(<Harness baseUrl="https://a.example.com" />);
    await user.click(fetchButton());

    // 切换 endpoint → generation 自增，旧请求作废
    rerender(<Harness baseUrl="https://b.example.com" />);

    await act(async () => {
      first.resolve([fetchedModel]);
    });

    expect(toast.success).not.toHaveBeenCalled();
    expect(toast.info).not.toHaveBeenCalled();
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("作废在途请求后 loading 被重置，获取按钮重新可用", async () => {
    const user = userEvent.setup();
    const pending = deferred<{ id: string; ownedBy: string }[]>();
    fetchModelsForConfig.mockReturnValueOnce(pending.promise);

    const { rerender } = render(<Harness baseUrl="https://a.example.com" />);
    await user.click(fetchButton());
    expect(fetchButton()).toBeDisabled();

    // 切换 endpoint：在途请求作废，loading 立即重置
    rerender(<Harness baseUrl="https://b.example.com" />);
    expect(fetchButton()).toBeEnabled();
  });

  it("旧请求晚到的 finally 不清掉新在途请求的 loading", async () => {
    const user = userEvent.setup();
    const first = deferred<{ id: string; ownedBy: string }[]>();
    const second = deferred<{ id: string; ownedBy: string }[]>();
    fetchModelsForConfig
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    const { rerender } = render(<Harness baseUrl="https://a.example.com" />);
    await user.click(fetchButton());

    // 切换 endpoint（旧请求作废、loading 重置）后再次发起新请求
    rerender(<Harness baseUrl="https://b.example.com" />);
    await user.click(fetchButton());
    expect(fetchButton()).toBeDisabled();

    // 旧请求晚到：不得清掉新请求的 loading
    await act(async () => {
      first.resolve([fetchedModel]);
    });
    expect(fetchButton()).toBeDisabled();

    // 新请求返回：loading 正常解除
    await act(async () => {
      second.resolve([fetchedModel]);
    });
    await waitFor(() => {
      expect(fetchButton()).toBeEnabled();
    });
  });

  it("卸载后在途请求返回，不弹任何 toast", async () => {
    const user = userEvent.setup();
    const pending = deferred<{ id: string; ownedBy: string }[]>();
    fetchModelsForConfig.mockReturnValueOnce(pending.promise);

    const { unmount } = render(<Harness baseUrl="https://a.example.com" />);
    await user.click(fetchButton());
    unmount();

    await act(async () => {
      pending.resolve([fetchedModel]);
    });

    expect(toast.success).not.toHaveBeenCalled();
    expect(toast.info).not.toHaveBeenCalled();
    expect(toast.error).not.toHaveBeenCalled();
  });

  it("generation 未失效的正常请求结果照常应用", async () => {
    const user = userEvent.setup();
    fetchModelsForConfig.mockResolvedValueOnce([fetchedModel]);

    render(<Harness baseUrl="https://a.example.com" />);
    await user.click(fetchButton());

    await waitFor(() => {
      // 测试环境 i18n 未初始化，t 返回键名（count 选项被丢弃）
      expect(toast.success).toHaveBeenCalledWith(
        "providerForm.fetchModelsSuccess",
      );
    });
  });
});
