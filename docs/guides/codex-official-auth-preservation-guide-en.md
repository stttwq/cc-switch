# Keep Codex Remote Control and Official Plugins While Using Third-Party APIs: CC Switch Setup Guide

> Applies to CC Switch v3.16.1 and later. This guide is organized from the current code and the user manual, and contains no real Access Tokens or API keys.

## What this guide solves

Many Codex users want both of these at the same time:

1. Use models from DeepSeek, Kimi, GLM, MiniMax, SiliconFlow, or other third-party APIs, or use GPT models through an aggregator.
2. Keep Codex official-app capabilities such as mobile remote control and official plugins.

Previously, when switching to a third-party provider, the old behavior wrote the third-party API key into Codex `auth.json`, which could overwrite the original official ChatGPT / Codex login cache. The third-party model worked, but features that depend on the official login state disappeared.

The current behavior removes this conflict: **switching to a third-party provider writes only `config.toml` and never touches `~/.codex/auth.json`**. Codex App therefore still sees you as logged in with the official account, while model requests go to the third-party provider currently selected in CC Switch. The third-party provider's API key is no longer written into `config.toml` either — it is delivered through a user-level environment variable.

## Quick answer

Recommended order:

1. In the CC Switch Codex panel, switch to `OpenAI Official`.
2. Start Codex and log in once with an official ChatGPT / Codex account. A Free subscription is enough.
3. Return to CC Switch and add or switch to a third-party Codex provider.
4. Reopen your terminal (only new processes can read the new environment variable), then start Codex again so `config.toml` and the model catalog are reloaded.

> ℹ️ No "Codex App Enhancements" switch is needed anymore, and no local routing or takeover either — the entire local routing capability has been removed.

## Prerequisites

Prepare the following:

- CC Switch v3.16.1 or later.
- Codex installed and able to start. Installing both the app and CLI is recommended.
- An official ChatGPT / Codex account that can log in to Codex. A Free subscription is enough.
- A third-party API key, such as DeepSeek, Kimi, GLM, MiniMax, OpenRouter, SiliconFlow, or similar.

Do not manually copy or share the contents of `~/.codex/auth.json`. It stores official login cache and Access Tokens, so it is sensitive. When CC Switch reads it, the content is used only to recognize the login state; it is never synced or exported.

## Step 1: Switch back to OpenAI Official and complete official login

Open CC Switch and switch to the top-level `Codex` tab. First select the `OpenAI Official` provider, or add it from the preset providers if it is missing, and make it the current provider.

Then start Codex, preferably the CLI, and follow the official login flow to sign in with your ChatGPT / Codex account. This account can be on the Free plan. In this setup, it mainly preserves the official identity required by Codex App, and does not pay for third-party model usage.

After login, Codex stores the official login cache in `~/.codex/auth.json`. The key point going forward is: switching to a third-party provider does not overwrite this file.

## Step 2: Add a third-party Codex provider

Return to the Codex panel and click the plus button in the upper-right corner to add a provider. Prefer built-in presets such as DeepSeek, Kimi, MiniMax, GLM, or SiliconFlow.

Using DeepSeek as an example, after selecting the preset, you only need to enter the API key. The preset automatically configures the base URL and the default model list.

> ⚠️ **Important change**: CC Switch no longer converts request protocols. A third-party provider must itself offer an endpoint that Codex can talk to directly (`wire_api = "responses"`). A provider that only exposes an OpenAI Chat Completions endpoint needs a Responses-compatible entry point on the provider side, otherwise it cannot be used through Codex — earlier versions relied on local routing for protocol conversion, and that capability has been removed.

## Step 3: Switch to the third-party provider and reopen the terminal

Return to the Codex provider list and enable the third-party provider you just added. After switching:

1. **Close and reopen your terminal** — the third-party provider's key is written into the user-level environment variable `CC_SWITCH_CODEX_API_KEY`, and a running terminal cannot read the new value
2. Restart Codex — Codex reads `config.toml` at startup, and the `/model` menu usually needs a restart before it reloads the model catalog

After reopening, you can run a quick verification:

- In Codex App, the account information still shows the official account. This is expected.
- In CC Switch, the current Codex provider is the third-party provider.
- The third-party provider dashboard or balance records show actual model requests.

## How it works

Codex mainly uses two configuration files:

```text
~/.codex/auth.json
~/.codex/config.toml
```

They have different responsibilities:

- `auth.json` stores the official ChatGPT / Codex login cache, which Codex App needs to identify the official account and enable remote control and official plugins.
- `config.toml` stores runtime configuration such as the current model provider, base URL, model, and model catalog.

When switching to a third-party provider, CC Switch writes only `config.toml`:

```toml
model_provider = "custom"

[model_providers.custom]
name = "DeepSeek"
base_url = "https://api.deepseek.com"
wire_api = "responses"
env_key = "CC_SWITCH_CODEX_API_KEY"
```

`env_key` is a **variable name**, not the key value; the real API key lives in the Windows Credential Manager and is written into a user-level environment variable at switch time. At the same time, `auth.json` keeps the official login cache unchanged. Codex App can still identify the official account, while model requests go straight to the third-party API according to the current provider and base URL in `config.toml`.

> 💡 `base_url` is the one exception in this setup: the Codex CLI does not support a variable reference in that field, so the currently active provider's address is written in plain text into `config.toml`, while other providers' addresses stay only in the credential manager.

There is one more safety gate: if a `[model_providers.*]` table carries `requires_openai_auth = true` but has no credentials of its own (neither an `env_key` nor a token), CC Switch **refuses to write** — because that would make Codex fall back to the official login credentials in `auth.json` when accessing a third-party address and leak the official credentials. Add an API key for that provider, or remove the fallback instruction.

## Side effects to understand

### Codex still shows the official account

This is the easiest part to misunderstand. Codex App reads the official login state from `auth.json`, so it continues to display the official account.

That does not mean model requests are still going to official OpenAI. Actual traffic is determined by the current Codex provider in CC Switch and `config.toml`.

### Do not use the Codex account display to judge billing

If you switch to DeepSeek, Codex can still display the official account, while model requests go to the DeepSeek API. Billing, quota, error codes, and data policy should all be understood according to the third-party provider.

### Restart Codex after changing model mappings

Codex reads the model catalog at startup. Even if CC Switch has generated a new model catalog, a running Codex process may not hot-load it, so restart Codex after editing model mappings.

## FAQ

**I switched to a third-party API. Why does Codex still show the official account?**

This is expected. Official account information comes from `auth.json`; the actual model provider comes from `config.toml` and the current provider in CC Switch.

**Is a Free subscription really enough?**

Yes. The official account is mainly used to obtain and preserve the official login state required by Codex App. Third-party model requests use the third-party API key configured in CC Switch.

**What should I do if official plugins or mobile remote control still do not work?**

Switch back to `OpenAI Official`, restart Codex, and complete official login once, then switch back to the third-party provider. The switch itself does not overwrite `auth.json`, so the login state usually does not need to be redone.

**What if third-party requests return 404, the model list is wrong, or streaming responses are broken?**

First confirm that the provider itself offers a Responses-compatible endpoint (CC Switch no longer converts protocols); then check that `base_url` and `model_provider` in `config.toml` point to the same table; finally confirm that you reopened your terminal (`CC_SWITCH_CODEX_API_KEY` is only visible to new processes). If it is still broken, see [5.4 Environment Variable Conflicts](../user-manual/en/5-faq/5.4-env-conflict.md).

**Can I switch back to OpenAI Official while using a third-party provider?**

Yes. Switching is an ordinary operation, and there is no old "takeover blocks switching to official" restriction — that local routing capability has been removed. After switching back to official, reopen your terminal as well.

## References

- [Can't see custom models in the Codex desktop app? (FAQ)](./codex-desktop-custom-model-visibility-en.md)
- [Add a provider](../user-manual/en/2-providers/2.1-add.md)
- [Switch provider](../user-manual/en/2-providers/2.2-switch.md)
- [Configuration files](../user-manual/en/5-faq/5.1-config-files.md)
- [CC Switch v3.16.1 Release Note](../release-notes/v3.16.1-en.md)
