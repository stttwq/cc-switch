import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ProviderCard } from "@/components/providers/ProviderCard";
import type { Provider } from "@/types";
import { createTestQueryClient } from "../utils/testQueryClient";

vi.mock("@/components/providers/ProviderActions", () => ({
  ProviderActions: (props: { onDuplicate?: () => void }) => (
    <>
      {props.onDuplicate ? (
        <button onClick={props.onDuplicate}>duplicate-provider</button>
      ) : null}
    </>
  ),
}));

vi.mock("@/components/ProviderIcon", () => ({
  ProviderIcon: () => null,
}));

function renderCard(
  provider: Provider,
  options: {
    isCurrent?: boolean;
    onEdit?: (provider: Provider) => void;
  } = {},
) {
  const queryClient = createTestQueryClient();

  return render(
    <QueryClientProvider client={queryClient}>
      <ProviderCard
        provider={provider}
        appId="codex"
        isCurrent={options.isCurrent ?? false}
        onSwitch={vi.fn()}
        onEdit={options.onEdit ?? vi.fn()}
        onDelete={vi.fn()}
        onOpenWebsite={vi.fn()}
        onDuplicate={vi.fn()}
      />
    </QueryClientProvider>,
  );
}

describe("ProviderCard Codex Official account identity", () => {
  it("keeps the provider name on the native card and explains its login", () => {
    renderCard({
      id: "codex-official",
      name: "OpenAI Official",
      category: "official",
      settingsConfig: { auth: {}, config: "" },
    });

    expect(
      screen.getByRole("heading", { name: "OpenAI Official" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("账号会随 Codex CLI 当前登录变化"),
    ).toBeInTheDocument();
  });

  it("shows a manual note instead of generated account guidance", () => {
    renderCard({
      id: "codex-official",
      name: "OpenAI Official",
      notes: "Primary work provider",
      category: "official",
      settingsConfig: { auth: {}, config: "" },
    });

    expect(screen.getByText("Primary work provider")).toBeInTheDocument();
    expect(
      screen.queryByText("账号会随 Codex CLI 当前登录变化"),
    ).not.toBeInTheDocument();
  });

  it("treats a legacy unbound Official card as follow-login", () => {
    const provider: Provider = {
      id: "legacy-unbound",
      name: "Legacy Official",
      category: "official",
      settingsConfig: { auth: {}, config: "" },
      meta: {},
    };
    renderCard(provider, { isCurrent: true });

    expect(
      screen.getByText("账号会随 Codex CLI 当前登录变化"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "duplicate-provider" }),
    ).toBeInTheDocument();
  });

  it("does not label a stored API-key card as follow-login", () => {
    renderCard({
      id: "legacy-api-key",
      name: "Legacy API Key",
      category: "official",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "sk-stored" },
        config: "",
      },
    });

    expect(
      screen.queryByText("账号会随 Codex CLI 当前登录变化"),
    ).not.toBeInTheDocument();
  });
});
