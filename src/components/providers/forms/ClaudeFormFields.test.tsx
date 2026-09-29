import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useForm } from "react-hook-form";
import { Form } from "@/components/ui/form";
import { ClaudeFormFields } from "./ClaudeFormFields";

// OPT-A（方案 §11.2-5/6）：Claude 表单的模型拉取请求 generation 隔离。
// endpoint/key/isFullUrl 变化或卸载后，旧请求的结果/toast 不得回灌表单。

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    info: vi.fn(),
    error: vi.fn(),
  },
}));

const fetchModelsForConfig = vi.fn();
const fetchModelsForProvider = vi.fn();
const showFetchModelsError = vi.fn();

vi.mock("@/lib/api/model-fetch", () => ({
  fetchModelsForConfig: (...args: unknown[]) => fetchModelsForConfig(...args),
  fetchModelsForProvider: (...args: unknown[]) => fetchModelsForProvider(...args),
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
function Harness(props: {
  baseUrl: string;
  apiKey?: string;
  apiKeyConfiguredStatus?: { present: boolean } | null;
  apiKeyRevealTarget?: { app: "claude"; providerId: string } | null;
}) {
  const form = useForm();
  return (
    <Form {...form}>
      <ClaudeFormFields
        shouldShowApiKey={false}
        apiKey={props.apiKey ?? "key-a"}
        onApiKeyChange={vi.fn()}
        shouldShowApiKeyLink={false}
        websiteUrl=""
        templateValueEntries={[]}
        templateValues={{}}
        templatePresetName=""
        onTemplateValueChange={vi.fn()}
        isNonOfficialCategory={false}
        baseUrl={props.baseUrl}
        onBaseUrlChange={vi.fn()}
        shouldShowModelSelector={true}
        claudeModel=""
        defaultHaikuModel=""
        defaultHaikuModelName=""
        defaultSonnetModel=""
        defaultSonnetModelName=""
        defaultOpusModel=""
        defaultOpusModelName=""
        defaultFableModel=""
        defaultFableModelName=""
        subagentModel=""
        onModelChange={vi.fn()}
        // 非 anthropic 格式让高级区默认展开，获取按钮可见
        apiFormat="openai_chat"
        onApiFormatChange={vi.fn()}
        apiKeyField="ANTHROPIC_AUTH_TOKEN"
        onApiKeyFieldChange={vi.fn()}
        isFullUrl={false}
        onFullUrlChange={vi.fn()}
        apiKeyConfiguredStatus={props.apiKeyConfiguredStatus ?? null}
        apiKeyRevealTarget={props.apiKeyRevealTarget ?? null}
      />
    </Form>
  );
}

const fetchButton = () =>
  screen.getByRole("button", { name: "providerForm.fetchModels" });

beforeEach(() => {
  vi.clearAllMocks();
});

describe("ClaudeFormFields 模型拉取 generation 隔离", () => {
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

describe("ClaudeFormFields 编辑态按供应商获取模型", () => {
  it("表单无明文 key 但后端已配置时，改走 fetchModelsForProvider", async () => {
    const user = userEvent.setup();
    fetchModelsForProvider.mockResolvedValueOnce([fetchedModel]);

    render(
      <Harness
        baseUrl="https://a.example.com"
        apiKey=""
        apiKeyConfiguredStatus={{ present: true }}
        apiKeyRevealTarget={{ app: "claude", providerId: "p1" }}
      />,
    );
    await user.click(fetchButton());

    await waitFor(() => {
      expect(toast.success).toHaveBeenCalledWith(
        "providerForm.fetchModelsSuccess",
      );
    });
    expect(fetchModelsForProvider).toHaveBeenCalledWith(
      "claude",
      "p1",
      "https://a.example.com",
      false,
      undefined,
    );
    expect(fetchModelsForConfig).not.toHaveBeenCalled();
  });

  it("表单填了明文 key 时仍走 fetchModelsForConfig（显式 set 意图优先）", async () => {
    const user = userEvent.setup();
    fetchModelsForConfig.mockResolvedValueOnce([fetchedModel]);

    render(
      <Harness
        baseUrl="https://a.example.com"
        apiKey="typed-key"
        apiKeyConfiguredStatus={{ present: true }}
        apiKeyRevealTarget={{ app: "claude", providerId: "p1" }}
      />,
    );
    await user.click(fetchButton());

    await waitFor(() => {
      expect(toast.success).toHaveBeenCalledWith(
        "providerForm.fetchModelsSuccess",
      );
    });
    expect(fetchModelsForConfig).toHaveBeenCalled();
    expect(fetchModelsForProvider).not.toHaveBeenCalled();
  });

  it("无明文 key 且后端未配置时，仍提示先填 API Key", async () => {
    const user = userEvent.setup();

    render(
      <Harness
        baseUrl="https://a.example.com"
        apiKey=""
        apiKeyConfiguredStatus={{ present: false }}
        apiKeyRevealTarget={null}
      />,
    );
    await user.click(fetchButton());

    expect(showFetchModelsError).toHaveBeenCalledWith(null, expect.anything(), {
      hasApiKey: false,
      hasBaseUrl: true,
    });
    expect(fetchModelsForProvider).not.toHaveBeenCalled();
    expect(fetchModelsForConfig).not.toHaveBeenCalled();
  });
});
