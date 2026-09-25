import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ProviderCard } from "@/components/providers/ProviderCard";
import type { Provider } from "@/types";
import { createTestQueryClient } from "../utils/testQueryClient";

vi.mock("@/components/providers/ProviderActions", () => ({
  ProviderActions: () => null,
}));

vi.mock("@/components/ProviderIcon", () => ({
  ProviderIcon: () => null,
}));

function renderCard(provider: Provider) {
  const queryClient = createTestQueryClient();

  return render(
    <QueryClientProvider client={queryClient}>
      <ProviderCard
        provider={provider}
        appId="claude"
        isCurrent
        onSwitch={vi.fn()}
        onEdit={vi.fn()}
        onDelete={vi.fn()}
        onOpenWebsite={vi.fn()}
        onDuplicate={vi.fn()}
      />
    </QueryClientProvider>,
  );
}

/**
 * 启动时自动导入的 live 配置在库里名为 "default"（id 与 name 都是）。
 * 直接渲染这个字面量会让供应商列表里出现一个叫 "default" 的条目，
 * 所以卡片对「名字仍是 default 字面量」的情况改用可读的展示名。
 */
describe("ProviderCard imported live provider naming", () => {
  it("shows a readable name instead of the raw default literal", () => {
    renderCard({
      id: "default",
      name: "default",
      category: "custom",
      settingsConfig: { env: {} },
    });

    expect(screen.queryByText("default")).not.toBeInTheDocument();
    expect(screen.getByText("Current Config")).toBeInTheDocument();
  });

  it("keeps a user-renamed imported provider as-is", () => {
    renderCard({
      id: "default",
      name: "我的中转站",
      category: "custom",
      settingsConfig: { env: {} },
    });

    expect(screen.getByText("我的中转站")).toBeInTheDocument();
    expect(screen.queryByText("Current Config")).not.toBeInTheDocument();
  });

  it("leaves other providers untouched", () => {
    renderCard({
      id: "some-other-provider",
      name: "其他供应商",
      category: "custom",
      settingsConfig: { env: {} },
    });

    expect(screen.getByText("其他供应商")).toBeInTheDocument();
  });
});
