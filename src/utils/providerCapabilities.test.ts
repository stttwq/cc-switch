import { describe, it, expect } from "vitest";
import type { Provider } from "@/types";
import { resolveCodexOfficialIdentity } from "@/utils/providerCapabilities";

function mkProvider(overrides: Partial<Provider> = {}): Provider {
  return { id: "p1", name: "Test", settingsConfig: {}, ...overrides };
}

describe("resolveCodexOfficialIdentity", () => {
  it("recognizes the fixed Codex Official card", () => {
    const native = mkProvider({
      id: "codex-official",
      category: "official",
      settingsConfig: { auth: {}, config: "" },
    });
    const unbound = mkProvider({
      id: "follow-login",
      category: "official",
      settingsConfig: {
        auth: {
          auth_mode: "chatgpt",
          tokens: { refresh_token: "legacy-live-only-token" },
        },
        config: "",
      },
    });

    expect(resolveCodexOfficialIdentity("codex", native)).toBe("native_login");
    expect(resolveCodexOfficialIdentity("codex", unbound)).toBe("native_login");
    expect(resolveCodexOfficialIdentity("claude", native)).toBeNull();
  });

  it("does not infer Codex login identity from a stale Official category", () => {
    const storedAuthKey = mkProvider({
      id: "legacy-api-key",
      category: "official",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "sk-legacy" },
        config: "",
      },
    });
    const storedBearer = mkProvider({
      id: "legacy-bearer",
      category: "official",
      settingsConfig: {
        auth: {},
        config: 'experimental_bearer_token = "sk-legacy"',
      },
    });
    const explicitOpenAi = mkProvider({
      id: "official-openai",
      category: "official",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "sk-official" },
        config: 'model_provider = "openai"',
      },
    });
    const unmarkedCustom = mkProvider({
      id: "custom-upstream",
      category: "official",
      settingsConfig: {
        auth: {},
        config:
          'model_provider = "custom"\n[model_providers.custom]\nbase_url = "https://example.com/v1"',
      },
    });
    const implicitCustom = mkProvider({
      id: "implicit-custom-upstream",
      category: "official",
      settingsConfig: {
        auth: {},
        config: 'model_provider = "ollama"',
      },
    });
    expect(resolveCodexOfficialIdentity("codex", storedAuthKey)).toBe(
      "api_key",
    );
    expect(resolveCodexOfficialIdentity("codex", storedBearer)).toBeNull();
    expect(resolveCodexOfficialIdentity("codex", explicitOpenAi)).toBe(
      "api_key",
    );
    expect(resolveCodexOfficialIdentity("codex", unmarkedCustom)).toBeNull();
    expect(resolveCodexOfficialIdentity("codex", implicitCustom)).toBeNull();

    const unifiedSession = mkProvider({
      id: "unified-session",
      category: "official",
      settingsConfig: {
        auth: {},
        config:
          'model_provider = "custom"\n[model_providers.custom]\nname = "OpenAI"\nrequires_openai_auth = true\nsupports_websockets = true\nwire_api = "responses"',
      },
    });
    expect(resolveCodexOfficialIdentity("codex", unifiedSession)).toBe(
      "native_login",
    );
  });

  it("keeps category-less fixed legacy cards recognizable", () => {
    const fixed = mkProvider({
      id: "codex-official",
      settingsConfig: { auth: {}, config: "" },
    });
    const fixedApiKey = mkProvider({
      id: "codex-official",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "sk-legacy" },
        config: "",
      },
    });
    const fixedCustom = mkProvider({
      id: "codex-official",
      settingsConfig: {
        auth: {},
        config: 'model_provider = "custom"',
      },
    });

    expect(resolveCodexOfficialIdentity("codex", fixed)).toBe("native_login");
    expect(resolveCodexOfficialIdentity("codex", fixedApiKey)).toBeNull();
    expect(resolveCodexOfficialIdentity("codex", fixedCustom)).toBeNull();
  });
});
