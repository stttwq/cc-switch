# Can't See Custom Models in the Codex Desktop App? (FAQ)

> Applies to CC Switch v3.16.1 and later. This article explains "why the Codex desktop app can't see custom models" and the available mitigation; for the detailed step-by-step setup, see [Keep Codex Remote Control and Official Plugins While Using Third-Party APIs](./codex-official-auth-preservation-guide-en.md).

## Symptom

After you switch Codex to a third-party / custom model in CC Switch (DeepSeek, Kimi, GLM, MiniMax, an aggregator, etc.):

- The model picker in the **Codex desktop app** doesn't show these custom models — often only the official default model remains, and the reasoning level falls back to the official default;
- but everything works fine in the **command-line `codex`** `/model` menu.

Many users have run into this. Here's why, and what you can do about it.

## Why this happens

This is **not a CC Switch local-config problem and not a CC Switch bug** — it is the **Codex desktop app's (the upstream closed-source client's) own model-gating behavior**.

The Codex desktop app's model picker decides which models to allow based on your **current login identity**: when it can't detect an official ChatGPT / Codex login state, it forces the picker back to the official default model and hides the custom models you configured through `config.toml` (the reasoning level falls back to the official default too). The upstream has marked "exposing custom-provider models in the desktop GUI" as not planned, so CC Switch cannot fully fix this at the desktop-GUI level.

The command-line `codex` `/model` menu and request routing both recognize the custom providers in `config.toml` correctly — **only the desktop GUI picker is constrained by this gating layer**.

## Mitigation: keep the official login

The workaround is to **keep the official login state** so the desktop app's gating allows your custom models through. The key points are below (the full step-by-step setup is in the linked guide):

1. Log in once with an official ChatGPT / Codex account in Codex (a Free subscription is enough) to keep the official login state.
2. In CC Switch, switch to that third-party Codex provider. Switching only rewrites `~/.codex/config.toml` and never touches `~/.codex/auth.json`, so the official login state stays exactly as it was — no switch is needed, and no local routing or takeover is needed either (that whole capability has been removed).
3. Reopen your terminal (the third-party key is delivered through a user-level environment variable, which only new processes can read), then fully quit and restart Codex.

This way the desktop app still recognizes the official login identity, the gating lets your models through, and the custom models you configured reappear in the picker. Third-party model requests only use the key you configured for that provider and connect straight to that provider's endpoint — **the preserved official token is never sent to the third party**.

> 📖 Detailed step-by-step setup: [Keep Codex Remote Control and Official Plugins While Using Third-Party APIs](./codex-official-auth-preservation-guide-en.md)

## Still can't see them?

- **Confirm you weren't overwritten by an older version**: earlier versions wrote the third-party key into `auth.json` when switching to a third-party provider, which overwrote the official login state — that is exactly why the models disappeared. On the current version, logging in to the official account once more restores it.
- **The official login state expires**: if you haven't used the official login for several days, the picker may go empty again once the token expires — log in to the official account once more to restore it.
- **Command-line fallback diagnosis**: run `codex debug models` to list the models actually available on the CLI side and confirm the model itself is configured correctly (the CLI is unaffected by this gating).
- Individual Codex desktop versions may behave slightly differently; this is in the upstream client's domain, and no CC Switch version can fully fix it at the desktop-GUI level.

## References

- [Keep Codex Remote Control and Official Plugins While Using Third-Party APIs](./codex-official-auth-preservation-guide-en.md)
- [Switch provider (including credential delivery and why you need a new terminal)](../user-manual/en/2-providers/2.2-switch.md)
