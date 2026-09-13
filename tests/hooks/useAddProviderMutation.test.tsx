import type { ReactNode } from "react";
import { act, renderHook } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAddProviderMutation } from "@/lib/query/mutations";

const apiMocks = vi.hoisted(() => ({
  add: vi.fn(),
  updateTrayMenu: vi.fn(),
}));

const uuidMocks = vi.hoisted(() => ({
  generateUUID: vi.fn(),
}));

const toastMocks = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  warning: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  providersApi: {
    add: (...args: unknown[]) => apiMocks.add(...args),
    updateTrayMenu: (...args: unknown[]) => apiMocks.updateTrayMenu(...args),
  },
  sessionsApi: {},
  settingsApi: {},
}));

vi.mock("@/utils/uuid", () => ({
  generateUUID: () => uuidMocks.generateUUID(),
}));

vi.mock("sonner", () => ({
  toast: toastMocks,
}));

function createWrapper() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: { retry: false },
    },
  });

  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
  );

  return { wrapper };
}

beforeEach(() => {
  apiMocks.add.mockReset().mockResolvedValue(true);
  apiMocks.updateTrayMenu.mockReset().mockResolvedValue(true);
  uuidMocks.generateUUID.mockReset().mockReturnValue("generated-uuid");
  toastMocks.success.mockReset();
  toastMocks.error.mockReset();
  toastMocks.warning.mockReset();
});

describe("useAddProviderMutation", () => {
  it("adds a Pi provider without a separate default-model command", async () => {
    const { wrapper } = createWrapper();
    const { result } = renderHook(() => useAddProviderMutation("pi"), {
      wrapper,
    });

    const provider = await act(async () =>
      result.current.mutateAsync({
        name: "Pi Provider",
        providerKey: "pi-provider",
        settingsConfig: {
          api: "openai-responses",
          baseUrl: "https://example.com/v1",
          apiKey: "secret",
          models: [{ id: "model-a" }],
        },
      }),
    );

    expect(apiMocks.add).toHaveBeenCalledWith(
      expect.objectContaining({ id: "pi-provider" }),
      "pi",
      undefined,
    );
    expect(provider.id).toBe("pi-provider");
  });

  it("reports a Pi provider add failure", async () => {
    apiMocks.add.mockRejectedValueOnce(new Error("provider add failed"));
    const { wrapper } = createWrapper();
    const { result } = renderHook(() => useAddProviderMutation("pi"), {
      wrapper,
    });

    await act(async () => {
      await expect(
        result.current.mutateAsync({
          name: "Pi Provider",
          providerKey: "pi-provider",
          settingsConfig: { models: [{ id: "model-a" }] },
        }),
      ).rejects.toThrow("provider add failed");
    });

    expect(apiMocks.add).toHaveBeenCalledWith(
      expect.objectContaining({ id: "pi-provider" }),
      "pi",
      undefined,
    );
    expect(toastMocks.error).toHaveBeenCalled();
    expect(toastMocks.warning).not.toHaveBeenCalled();
  });
});
