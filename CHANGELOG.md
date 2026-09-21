# Changelog

All notable changes to CC Switch will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.2] - Unreleased

On-demand credential reveal, IPC input hardening, and a large Windows-only repo/security cleanup.

### Added

- **Reveal a provider's API key on demand.** Editing a provider now shows an eye button that reads a single field's key from Credential Manager once (`reveal_provider_secret`), unmasks it in place, and re-masks on blur or after 60 s. Batch reads (list/cards/tray) still never carry a key — the frontend is zero-secret by default, not zero-secret ever.
- **Base URL is back-filled and visible.** The Base URL edit box now defaults to the current value per app (Claude `env`, Codex TOML `base_url`, Pi top-level), and provider cards show the active endpoint's host so you can tell which endpoint you are on without opening the editor.

### Changed

- `get_providers` is now async and no longer reads Credential Manager per provider on the main thread.
- Credential principle updated from "frontend zero-secret" to "frontend zero-secret by default + explicit on-demand reveal"; documented in SECURITY.md.

### Fixed / Security

- **IPC input surface tightened (S-1/S-2/S-4):** `get_session_messages`/`delete_session` confine `sourcePath` to the provider root; config import/export go through a native dialog so the path never round-trips through the renderer; `open_external` parses the URL and allows only `http`/`https`.
- **HTTP stack (S-8):** reqwest no longer pulls `native-tls`/`schannel`; it uses rustls against the **OS certificate store** (`rustls-tls-native-roots`), which also fixes a real-machine `UnknownIssuer` failure downloading skills behind a TLS-intercepting proxy. A single reqwest version now appears in the shipped target.
- Bumped transitive deps (rustls, rustls-webpki, h2, anyhow, uds_windows) to clear five RustSec advisories.

### Removed / Cleanup

- **Windows-only code (C5):** deleted `linux_fix`, the macOS-only session-terminal subsystem and `launch_session_terminal`, non-Windows `cfg` branches in `misc.rs`/`lib.rs`/`tray.rs`/`lightweight.rs`/`auto_launch.rs`, the `webkit2gtk`/`libc`/`objc2` dependencies, and the iOS/Android/macOS icons and `Info.plist`.
- **Repo identity:** README, CHANGELOG, CONTRIBUTING/SUPPORT/CODE_OF_CONDUCT/CODEOWNERS, issue templates and docs now point at this fork instead of upstream `farion1231`/`ccswitch.io`; upstream 3.x history moved to `docs/changelog-upstream-3.x.md`; 92 upstream release notes and the en/ja manuals removed; in-app copy no longer describes removed apps (Gemini/Claude Desktop) as managed.

### Build / Release

- All GitHub Actions pinned to commit SHAs; added a weekly `cargo-deny` + `gitleaks` + `pnpm audit` workflow and a blocking `cargo deny check advisories` gate on the main CI backend job.
- Releases ship `SHA256SUMS` + a **minisign** signature (public key committed as `minisign.pub`); SECURITY.md rewritten for 2.x.

## [2.0.1] - 2026-09-20

Fixes from the first real-machine upgrade rehearsal (3.20.3 → 2.0.0), recorded in `docs/plans/secrets-slimdown-acceptance-zh.md`.

### Fixed

- **Editing the provider you are currently using now takes effect immediately.** Credentials were delivered to `HKCU\Environment` only when switching providers, so saving a new key or base URL on the active card left Claude Code, Codex and Pi reading the previous key. The save path re-delivers now — before anything is written to the database, so a failed delivery no longer leaves a card that says "save failed" while actually being half-saved.
- **Cloud sync is no longer refused by the export guard.** Base URLs were registered among "secrets seen this session", so any payload mentioning the same domain as a provider's website was rejected with 「导出护栏拒绝」. A URL that embeds `user:password@` is still treated as a secret.
- **Re-upgrading after a rollback no longer deadlocks.** With the pre-migration database restored, the environment variables left by the previous install were judged foreign and refused, so the live rewrite failed on every launch and the dialog blamed a running CLI. The rewrite step now adopts variables that follow our own naming before it delivers.
- **Outstanding live rewrites are visible after the migration dialog is dismissed.** That one-time dialog was the only place reporting them. Settings → Advanced → Credential Manager maintenance now lists the pending rewrites with their reasons and offers a retry that runs immediately instead of waiting for the next launch.

## [2.0.0] - 2026-09-19

**This is a breaking release.** It removes the local router, six of the nine supported apps, the auto-updater and every form of plaintext credential storage. Existing installations migrate automatically on first launch, but the shape of the product changes: cc-switch no longer proxies requests, no longer stores keys on disk, and no longer builds for macOS or Linux.

### Removed

- **Local proxy and everything built on it**: `src-tauri/src/proxy/**` with request forwarding, failover queues, the usage dashboard, stream checking, model pricing and the routing takeover. No listening port is opened any more and no request is forwarded.
- **Seven applications**: Gemini, OpenCode (and OMO), OpenClaw (and Workspace), Hermes, GrokBuild (and xAI OAuth), Claude Desktop and Copilot are no longer supported. Only Claude Code, Codex and Pi remain. Providers, MCP servers and skills belonging to the removed apps are dropped from the database by the v19 migration.
- **Auto-update**: `tauri-plugin-updater` and the release-chain signing and `latest.json` artifacts are gone; the app no longer checks for or installs updates. `latest.json` / `.sig` assets are no longer published.
- **macOS and Linux builds**: releases ship a single Windows x86_64 MSI. The macOS/Linux build jobs, their packaging steps and the corresponding release notes are removed.
- **The "Gemini Native" upstream format for Claude providers**: protocol conversion left with the local router, so the option could only ever produce a card that does not work. The preset and the selector are gone; the v19 migration normalizes existing cards to OpenAI Chat Completions and the migration report asks you to verify the endpoint yourself.
- **The Codex advanced knobs that only fed format conversion**: prompt-cache routing mode, the Chat-Completions reasoning capability map (thinking / effort switches) and the Claude-emulation and `max_output_tokens` overrides are removed, along with their 16 locale strings. Nothing in the app read them any more, so flipping them silently did nothing; the model catalog and model-mapping settings are untouched.

### Changed

- **Credentials now live in Windows Credential Manager.** API keys, base URLs, Pi request headers, and the WebDAV / S3 sync credentials are stored there and nowhere else. `providers.settings_config`, `settings.json` and the live CLI config files no longer contain values.
- **Environment variables are the only delivery path.** Claude Code, Codex and Pi receive their credentials as user-level environment variables (`HKCU\Environment`); live config files contain only the *names* of those variables. The helper-command delivery modes (`apiKeyHelper`, Pi's `"!command"`) are not implemented by design.
- **Database schema v19.** Ten tables tied to the removed features are dropped, `providers.in_failover_queue` is dropped as a column, and `meta.usage_script` is stripped on the Rust side.
- **Existing installations migrate automatically and idempotently on first launch.** Provider credentials are extracted, written to Credential Manager, and the stored configs are rewritten with the secrets removed. No interaction is required; a report lists what was migrated and offers a retry for any live-file rewrite that failed.
- **Codex official cards no longer carry a ChatGPT login.** OAuth tokens found in stored configs are discarded rather than migrated — run `codex login` in the Codex CLI instead. Codex third-party cards that would silently fall back to `auth.json` are refused at switch time.
- **Listing providers never reads Credential Manager for keys.** The last-4-character hint is gone: showing it meant one credential read per provider on every list refresh. Cards and forms now say only whether a key is configured, which is derived from the credential registry instead.

### Fixed

- **WebDAV and S3 credentials are actually persisted.** The settings dialog used to place the password inside the settings object while the backend struct no longer had that field, so the value was dropped by serde and the command's `password` argument was always `None`; new sync configurations could never authenticate. Credentials are now sent as a dedicated three-state argument (absent = unchanged, empty = delete, value = store), and "Test connection" can use a password that has not been saved yet.
- **Keyless Codex official cards no longer leave OAuth tokens in SQLite.** The migration skipped rows with nothing to migrate, so a card holding only `auth.tokens` kept its refresh token in `providers.settings_config` forever — and the pending flag was already cleared, so it was never retried.
- **The database file no longer retains plaintext in free pages.** Overwritten `settings_config` values stayed readable in SQLite free pages after the migration; the migration now enables `secure_delete` before rewriting and runs `VACUUM` afterwards.
- **Sync credentials can be cleared.** Emptying the password or S3 key field now deletes the stored entry instead of being ignored.
- **The legacy official-proxy route is cleaned up.** Older versions pointed Codex at `http://127.0.0.1:<port>`; that dead table and its `model_provider` selector are now stripped on every live write instead of being written back.
- **`~/.claude/settings.local.json` conflicts are reported.** Its `env.ANTHROPIC_*` entries override the process environment, so the conflict scan lists them as read-only warnings. cc-switch does not modify that file.
- **Missing required credentials are rejected before saving.** Claude providers need an API key and Pi providers need a base URL (official and Bedrock/cloud-provider cards, which authenticate elsewhere, are exempt).

### Security

- Plaintext residue from older versions is deleted automatically at startup (`config.json`, its `.bak`/`.migrated` siblings, `codex_oauth_auth.json`, `backups/env-backup-*.json`, and legacy `%TEMP%\claude_*.json` launchers). Database backups are never auto-deleted.
- Exports, backups and sync payloads are scanned for credential patterns and refused if a plaintext secret is found; imported/restored data is scrubbed through the same extraction path.
- Known secret values are redacted from logs and from every text that leaves the backend; only a last-4 hint crosses IPC.
- `~/.codex/auth.json` is deleted only when it holds nothing but an API key that has already been migrated to Credential Manager.

## 更早版本

3.x 及更早版本为 fork 之前的上游历史，完整记录见 [`docs/changelog-upstream-3.x.md`](docs/changelog-upstream-3.x.md)。
