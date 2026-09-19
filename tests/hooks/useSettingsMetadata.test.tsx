import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useSettingsMetadata } from "@/hooks/useSettingsMetadata";

describe("useSettingsMetadata", () => {
  it("通过 setter 更新需要重启标志", async () => {
    const { result } = renderHook(() => useSettingsMetadata());

    await act(async () => {
      result.current.setRequiresRestart(true);
    });
    expect(result.current.requiresRestart).toBe(true);

    await act(async () => {
      result.current.acknowledgeRestart();
    });
    expect(result.current.requiresRestart).toBe(false);
  });
});
