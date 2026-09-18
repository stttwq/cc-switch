import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { EnvWarningBanner } from "@/components/env/EnvWarningBanner";
import type { EnvConflict } from "@/types/env";

/**
 * §5.3.3 收尾 + §10 单元（前端）：旧的环境变量警告横幅同样只允许显示
 * 遮罩后的末 4 位（maskedValue），永不显示完整值。
 *
 * 契约限制：EnvConflict 类型本身只有 maskedValue 字段、没有完整值可传，
 * 因此这里往数据里夹带一个自造的 varValue 完整值字段（cast 绕过类型），
 * 断言组件只读取 maskedValue——即便输入混入完整值也不得出现在 DOM。
 */
describe("EnvWarningBanner 遮罩值显示", () => {
  it("展开后只显示 maskedValue，不显示混入的完整值", () => {
    const fullValue = "sk-full-env-value-should-not-render";
    const conflicts = [
      {
        varName: "ANTHROPIC_AUTH_TOKEN",
        maskedValue: "wxyz",
        sourceType: "system",
        sourcePath: "HKEY_CURRENT_USER\\Environment",
        varValue: fullValue,
      },
    ] as unknown as EnvConflict[];

    render(
      <EnvWarningBanner
        conflicts={conflicts}
        onDismiss={vi.fn()}
        onDeleted={vi.fn()}
      />,
    );

    // 展开明细列表
    fireEvent.click(screen.getByRole("button", { name: "env.actions.expand" }));

    expect(screen.getByText("ANTHROPIC_AUTH_TOKEN")).toBeInTheDocument();
    expect(screen.getByText(/wxyz/)).toBeInTheDocument();
    expect(document.body.textContent).not.toContain(fullValue);
  });
});
