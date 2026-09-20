import { describe, expect, it } from "vitest";
import { applyBaseUrlForApp } from "@/utils/providerConfigUtils";

const BASE_URL = "https://api.example.com/v1";

/**
 * §1.4.3 验收：回显的 Base URL 必须落在各应用真正读取的位置，
 * 且 Claude / Codex 的 settingsConfig **不得**出现顶层 `baseUrl`。
 */
describe("applyBaseUrlForApp（§1.4.3 Base URL 回填位置）", () => {
  it("Claude 写进 env.ANTHROPIC_BASE_URL，且不留顶层 baseUrl", () => {
    const result = applyBaseUrlForApp(
      "claude",
      { env: { ANTHROPIC_MODEL: "claude-opus-4" } },
      BASE_URL,
    );

    expect(result.env).toEqual({
      ANTHROPIC_MODEL: "claude-opus-4",
      ANTHROPIC_BASE_URL: BASE_URL,
    });
    expect(result).not.toHaveProperty("baseUrl");
  });

  it("Claude 原本没有 env 时也能建出 env", () => {
    const result = applyBaseUrlForApp("claude", {}, BASE_URL);
    expect(result.env).toEqual({ ANTHROPIC_BASE_URL: BASE_URL });
    expect(result).not.toHaveProperty("baseUrl");
  });

  it("Codex 写进当前激活的 model_providers 段，且不留顶层 baseUrl", () => {
    const config = [
      'model_provider = "custom"',
      "",
      "[model_providers.custom]",
      'name = "Custom"',
      "",
    ].join("\n");

    const result = applyBaseUrlForApp("codex", { config }, BASE_URL);
    const text = String(result.config);

    expect(text).toContain(`[model_providers.custom]`);
    expect(text).toContain(`base_url = "${BASE_URL}"`);
    expect(result).not.toHaveProperty("baseUrl");
  });

  it("Codex 的 config 为空时不注入（避免写出 Codex 不认的顶层键）", () => {
    const result = applyBaseUrlForApp("codex", { config: "" }, BASE_URL);
    expect(result.config).toBe("");
    expect(result).not.toHaveProperty("baseUrl");
  });

  it("Pi 用顶层 baseUrl", () => {
    const result = applyBaseUrlForApp(
      "pi",
      { api: "openai-completions" },
      BASE_URL,
    );
    expect(result).toEqual({
      api: "openai-completions",
      baseUrl: BASE_URL,
    });
  });

  it("没有回显值时原样返回配置", () => {
    for (const empty of [null, undefined, "", "   "]) {
      const config = { env: { ANTHROPIC_MODEL: "claude-opus-4" } };
      expect(applyBaseUrlForApp("claude", config, empty)).toEqual(config);
    }
  });
});
