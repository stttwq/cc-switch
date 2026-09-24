# Changelog

All notable changes to CC Switch will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.2.4] - 2026-09-24

### Fixed

- **Right-click "Open here" now works with non-ASCII (e.g. Chinese) folder paths (Windows).** The launcher batch file is written as UTF-8, but `cmd.exe` parses it in the console code page (GBK/936 on Chinese Windows), so a `cd /d "<Chinese path>"` line was misread and the terminal failed to switch into the clicked folder (surfacing as errors such as `'/d' is not recognized` / "path not found"). The target directory is now passed through an environment variable (`%CC_SWITCH_CWD%`, delivered to the child process as UTF-16), leaving the batch file pure ASCII so any path resolves correctly.
- **Pi right-click launch resolves a provider even without a native default.** Pi runs in additive mode with no cc-switch "current provider", so the context-menu launcher relied solely on Pi's native `defaultProvider` in `settings.json` and failed with "no default provider" when it was unset. Launching a Pi provider from the GUI ("Run Pi" / "Open Terminal") now writes it back to Pi's `defaultProvider` (preserving the file's other fields, idempotent), so the right-click menu and Pi itself both follow your most recent choice; when exactly one Pi provider exists it is used automatically.

### Changed

- **Strict credential delivery now defaults to global-strict.** Fresh installs (and configs missing the field) no longer write provider secrets into `HKCU\Environment`; credentials are injected only into terminals launched from cc-switch or activated via `ccs env`. Existing settings that already recorded the switch are left untouched.

## [2.2.3] - 2026-09-24

### Fixed

- **Explorer right-click menu self-heals on startup (Windows).** After an app update, an already-registered context menu could keep pointing at a stale command line (old launcher path / old command format), leaving the menu broken until re-toggled. Startup now silently rewrites any expired command line (and refreshes the icon) for existing registry entries only — unregistered users and fresh installs are untouched. Failure is logged and never blocks launch.

## [2.2.2] - 2026-09-24

### Added

- **Explorer right-click "Open terminal here" (Windows).** An opt-in Settings toggle registers a cascading folder context menu (Claude / Codex / Pi) under the current user's registry — no admin required. Clicking an entry runs that CLI in the clicked folder with the current provider's credentials injected (same boundary as "Open Terminal": env-only, never `HKCU\Environment` or a file), skipping the open-main-window-then-pick-folder flow. A dedicated windowless launcher binary (`ccs-open.exe`, built with `windows_subsystem = "windows"`) avoids the console-window flash, and the entries run the CLI directly instead of just opening a shell. The MSI removes the menu keys on a true uninstall (not on version upgrades) via a conditioned custom action.

## [2.2.1] - Unreleased

### Added

- **Custom terminal for "Open Terminal" (Windows).** The preferred-terminal dropdown gains a "Custom terminal" option backed by two new settings — an executable path and an argument template. The template's `{bat}` placeholder expands to the launcher batch script; quoting is honored when splitting (`-e cmd /K "{bat}"` passes four arguments, which Pebrel/WezTerm/Alacritty-style variadic `-e` requires). Leaving the template empty defaults to `-e cmd /K "{bat}"`. Launch failure falls back to cmd, and credentials still enter the child process only via the environment.

## [2.2.0] - Unreleased

Strict-mode ergonomics: activate credentials in your own shell, a tiered strict-delivery switch, and a fixed "Open Terminal" that lands Codex/Pi in a real shell.

### Added

- **`ccs env <app>` shell shim (P1).** A new console sub-binary (`ccs.exe`, built from a dedicated `[[bin]]`, no tauri/single-instance) lets you activate the current provider's credentials inside *your own* PowerShell / cmd / Git Bash: `ccs env claude | iex`, `eval "$(ccs env claude --shell bash)"`. Keys go only into that shell process (same security boundary as the terminal injection — never `HKCU\Environment`, never a file). `--clear` emits only unsets and synchronously deregisters from `managed_env_vars` so re-activation isn't falsely blocked. Credentials reuse `provider_env_pairs`; the shim refuses to migrate a schema that is newer/older than itself and resolves the custom config-dir override from `app_paths.json`. A "Copy activation command" button in Settings emits a one-time absolute-path snippet (does not touch PATH). Stable exit codes: 0 ok, 2 usage, 3 missing key, 4 DB version, 5 store/config unavailable.
- **Tiered strict-delivery mode (P2).** The global bool becomes three states — Off / Per app / Global — via an additive `env_delivery_strict_apps` list (old `env_delivery_strict_mode` still honored). Delivery/preflight now decide per app (`strict_for`), enabling a mode only reclaims the variables of apps that just turned strict, and the tray hint / diagnostics / provider-card badge now aggregate instead of reading the raw bool. The mutual-exclusion invariant is enforced at the single always-on save path.
- **"Run X" entry + real interactive shell (P3).** The CLI name is now chosen by app (shared `cli_command_for`), so Codex/Pi launch the correct CLI. "Open Terminal" lands an environment-loaded interactive shell without auto-running a CLI; a new "Run X" action starts the app's CLI directly. The terminal button is no longer Claude-only.

### Changed

- Codex/Pi provider cards now show the "Open Terminal" / "Run X" actions (previously only Claude).

### Notes

- Shipping `ccs.exe` inside the MSI is a packaging step that must be verified against a real per-user installer build (see the plan's open point ①).

## [2.1.0] - 2026-09-21

End-to-end encrypted cloud sync, a strict credential-delivery mode, one-click diagnostics, and a large Windows-only repo/security hardening pass (on-demand key reveal, IPC input tightening).

### Added

- **Reveal a provider's API key on demand.** Editing a provider now shows an eye button that reads a single field's key from Credential Manager once (`reveal_provider_secret`), unmasks it in place, and re-masks on blur or after 60 s. Batch reads (list/cards/tray) still never carry a key — the frontend is zero-secret by default, not zero-secret ever.
- **Base URL is back-filled and visible.** The Base URL edit box now defaults to the current value per app (Claude `env`, Codex TOML `base_url`, Pi top-level), and provider cards show the active endpoint's host so you can tell which endpoint you are on without opening the editor.
- **End-to-end encrypted sync (E2E).** Opt-in per transport (WebDAV / S3). A user passphrase (Argon2id → KEK) seals a per-snapshot data key (XChaCha20-Poly1305) that encrypts `db.sql` and `skills.zip`; the AAD binds `snapshotId + seq`, the inner manifest (device name / time / plaintext hash) is itself encrypted, and a monotonic `seq` blocks rollback. The server only ever sees ciphertext and an opaque `snapshot_id`. The passphrase is stored in Credential Manager and **never uploaded**; losing it means the remote is unrecoverable. Keys are not part of the synced payload, so a restored device shows "key required". v3 lives in a separate `{root}/v3/{profile}` layout — 2.0 clients ignore it, and enabling E2E refuses to downgrade to v2 plaintext.
- **Conditional writes for concurrency.** WebDAV manifest uploads use `If-Match`/`If-None-Match` (412 → "remote changed"); S3 does a best-effort HEAD compare. A rollback conflict returns a structured payload so the UI can offer an explicit "apply anyway".
- **Strict credential-delivery mode (B5).** A global switch (off by default) that stops writing secrets into `HKCU\Environment` on provider switch — keys are injected only into terminals launched from cc-switch (`Command::env`). Enabling it immediately reclaims any already-delivered keys. Off-machine CLIs then fail closed (Codex missing `env_key`, Pi unresolved vars); provider cards show a "strict delivery" badge.
- **One-click diagnostics.** The About page copies a redacted bundle — version, DB schema, migration markers, key-target count, strict-mode state, `crash.log` presence, and up to the last 50 log lines run through known-secret redaction with all `http(s)` hosts/paths masked — with no keys, provider names, base URLs, or WebDAV/S3 endpoints.

### Changed

- `get_providers` is now async and no longer reads Credential Manager per provider on the main thread.
- Credential principle updated from "frontend zero-secret" to "frontend zero-secret by default + explicit on-demand reveal"; documented in SECURITY.md.

### Fixed / Security

- **IPC input surface tightened (S-1/S-2/S-4):** `get_session_messages`/`delete_session` confine `sourcePath` to the provider root; config import/export go through a native dialog so the path never round-trips through the renderer; `open_external` parses the URL and allows only `http`/`https`.
- **HTTP stack (S-8):** reqwest no longer pulls `native-tls`/`schannel`; it uses rustls against the **OS certificate store** (`rustls-tls-native-roots`), which also fixes a real-machine `UnknownIssuer` failure downloading skills behind a TLS-intercepting proxy. A single reqwest version now appears in the shipped target.
- Bumped transitive deps (rustls, rustls-webpki, h2, anyhow, uds_windows) to clear five RustSec advisories.
- **Sync transport security.** `http` remote endpoints are refused by default — only loopback / private-network URLs pass, and only when "allow insecure" is checked; existing http remotes configured before the upgrade get a one-time grandfather so they aren't silently cut off. Wrong passphrase and tampered ciphertext are deliberately indistinguishable (fail closed, no oracle).
- **Provider-switch delivery is now transactional.** The old value of each environment variable is snapshotted (`Zeroizing`) before the delete-then-write sequence; if writing any new variable fails, the switch rolls back — previously written vars are removed and the old values restored — instead of leaving a half-applied "old deleted, new not written" state.
- **DB robustness (T-4/T-5).** `busy_timeout = 5000` and a startup `PRAGMA quick_check` with an offline "restore from latest backup" path (WAL intentionally not enabled — it corrupts on cloud/NAS/WSL2-UNC sync dirs); a `pre-sync-restore-<ts>.db` file backup is taken before applying a synced snapshot.
- **Tighter local ACL (S-10).** On first run, `~/.cc-switch` is locked down to the current user + SYSTEM + Administrators (idempotent `icacls`).

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
