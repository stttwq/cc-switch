//! 完全卸载时的托管数据清理。
//!
//! MSI 的 `RemoveLegacyUserData` 自定义动作只删 `%USERPROFILE%\.cc-switch`；
//! 但 CC Switch 还会往两处**应用外部**写数据，卸载时不清干净，重装后旧供应商
//! 就会复活或留下残骸：
//!
//! 1. `~/.pi/agent/models.json`：Pi 的**原生**配置。启动时
//!    `lib.rs` → `provider::import_pi_providers_from_live` 会把它当 live 源重新导入。
//! 2. Windows 凭据管理器 `cc-switch/v1/*` 与 `HKCU\Environment` 的 `CC_SWITCH_*`。
//! 3. `~/.codex/config.toml` 里的 `env_key = "CC_SWITCH_CODEX_API_KEY"`：它指向
//!    第 2 步刚删掉的环境变量，留着会让 Codex 在重装前彻底无法认证。
//!
//! 清理边界是「CC Switch 托管的」而非「全部」：Pi 的 `models.json` 只删除
//! `apiKey` 引用 `$CC_SWITCH_PI_*` 的节点（CC Switch 写入的形态），用户手工配置
//! 的供应商原样保留。凭据 target 形如 `cc-switch/v1/provider/<app>/<id>/<field>`，
//! 前缀 `cc-switch/` 只由 CC Switch 写，直接按前缀删。
//!
//! Claude 与 Pi 的 live 文件不需要这一步：Claude 的 sanitizer 只写非敏感字段、
//! 从不产生 `CC_SWITCH_*` 引用；Pi 的引用已由上面的 `models.json` 清理覆盖。

use std::path::{Path, PathBuf};

/// Pi 写入 `models.json` 时使用的环境变量引用前缀，用于判定节点归属。
const PI_ENV_PREFIX: &str = "$CC_SWITCH_PI_";
/// `models.json` 中供应商集合的顶层键。
const PI_PROVIDERS_KEY: &str = "providers";
/// 凭据管理器 target 前缀（`SecretTarget::to_target_string` 的固定开头）。
const SECRET_TARGET_PREFIX: &str = "cc-switch/";
/// `HKCU\Environment` 中由 CC Switch 投递的变量前缀。
const ENV_VAR_PREFIX: &str = "CC_SWITCH_";
/// Codex live `config.toml` 中被 CC Switch 注入的 `env_key` 值。
///
/// 见 `services/provider/codex_sanitizer.rs`：切换第三方 Codex 供应商时，密钥不落
/// live 文件，`config.toml` 只写这个环境变量名。
const CODEX_ENV_KEY_VALUE: &str = "CC_SWITCH_CODEX_API_KEY";

/// 清理结果统计，供 `--cleanup-user-data` 打印与测试断言。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// `models.json` 中删除的 CC Switch 托管供应商节点数。
    pub pi_providers_removed: usize,
    /// `models.json` 中保留下来的用户自有供应商节点数。
    pub pi_providers_kept: usize,
    /// 凭据管理器删除的条目数。
    pub secrets_deleted: usize,
    /// `HKCU\Environment` 删除的变量数。
    pub env_vars_deleted: usize,
    /// Codex `config.toml` 中删除的 `env_key` 引用数。
    pub codex_env_keys_removed: usize,
}

impl CleanupReport {
    /// 人类可读的单行摘要。
    pub fn summary(&self) -> String {
        format!(
            "pi_providers_removed={} pi_providers_kept={} secrets_deleted={} env_vars_deleted={} codex_env_keys_removed={}",
            self.pi_providers_removed,
            self.pi_providers_kept,
            self.secrets_deleted,
            self.env_vars_deleted,
            self.codex_env_keys_removed
        )
    }
}

/// 执行全部托管数据清理。任何一步失败只记日志、不中断其余步骤——
/// 卸载流程里「清多少算多少」比「第一个错误就放弃」对用户更有利。
pub fn run() -> CleanupReport {
    let mut report = CleanupReport::default();

    match resolve_pi_models_path() {
        Some(path) => match cleanup_pi_models(&path) {
            Ok((removed, kept)) => {
                report.pi_providers_removed = removed;
                report.pi_providers_kept = kept;
            }
            Err(e) => log::warn!("卸载清理：处理 Pi models.json 失败: {e}"),
        },
        None => log::warn!("卸载清理：无法定位用户主目录，跳过 models.json"),
    }

    report.secrets_deleted = cleanup_secrets();
    report.env_vars_deleted = cleanup_env_vars();

    // 必须在 cleanup_env_vars 之后：先删变量再清引用，中途失败也不会留下
    // 「变量已删、config.toml 仍指向它」的空窗。
    if let Some(path) = resolve_codex_config_path() {
        match cleanup_codex_live_config(&path) {
            Ok(removed) => report.codex_env_keys_removed = removed,
            Err(e) => log::warn!("卸载清理：处理 Codex config.toml 失败: {e}"),
        }
    }

    report
}

/// 定位 Pi 的 `models.json`。
///
/// 不读 `settings.json` 里的自定义 Pi 目录：卸载时该文件可能已被
/// `RemoveInstallData` 删掉，且载入它需要 AppState。只认环境变量与默认位置，
/// 避免误删到别处。
fn resolve_pi_models_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("models.json"));
        }
    }
    dirs::home_dir().map(|home| home.join(".pi").join("agent").join("models.json"))
}

/// 删除 `models.json` 里由 CC Switch 托管的供应商节点，保留用户自有的。
///
/// 返回 `(删除数, 保留数)`。文件不存在或 `providers` 不是对象时返回 `(0, 0)`。
fn cleanup_pi_models(path: &Path) -> Result<(usize, usize), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
    };

    // 用 serde_json::Value 而非结构体：models.json 允许 Pi 的任意原生字段，
    // 结构体化会在重写时丢掉未知字段。
    let mut document: serde_json::Value =
        json5::from_str(&text).map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;

    let Some(providers) = document
        .get_mut(PI_PROVIDERS_KEY)
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok((0, 0));
    };

    let before = providers.len();
    providers.retain(|_, node| !is_cc_switch_managed_pi_provider(node));
    let removed = before - providers.len();
    let kept = providers.len();

    if removed == 0 {
        return Ok((0, kept));
    }

    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|e| format!("序列化 {} 失败: {e}", path.display()))?;
    bytes.push(b'\n');
    crate::config::atomic_write_private(path, &bytes)
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;

    Ok((removed, kept))
}

/// 判定一个 Pi 供应商节点是否由 CC Switch 托管。
///
/// 只认「`apiKey` 字符串以 `$CC_SWITCH_PI_` 开头」这一形态——CC Switch 写 live
/// 节点时一律把密钥替换成该引用（见 `pi_sanitizer`）。用明文密钥或其它写法的
/// 节点视为用户自有，不予删除。
fn is_cc_switch_managed_pi_provider(node: &serde_json::Value) -> bool {
    node.get("apiKey")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|key| key.starts_with(PI_ENV_PREFIX))
}

/// 删除凭据管理器里全部 `cc-switch/` 前缀的条目。返回删除条数。
fn cleanup_secrets() -> usize {
    #[cfg(target_os = "windows")]
    {
        match crate::secrets::windows_enumerate_targets(SECRET_TARGET_PREFIX) {
            Ok(targets) => {
                let mut deleted = 0;
                for target in targets {
                    match crate::secrets::windows_delete_credential(&target) {
                        Ok(()) => deleted += 1,
                        Err(e) => log::warn!("卸载清理：删除凭据 {target} 失败: {e}"),
                    }
                }
                deleted
            }
            Err(e) => {
                log::warn!("卸载清理：枚举凭据失败，跳过凭据清理: {e}");
                0
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// 删除 `HKCU\Environment` 中全部 `CC_SWITCH_` 前缀的变量。返回删除条数。
fn cleanup_env_vars() -> usize {
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::*;
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let env = match hkcu.open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE) {
            Ok(env) => env,
            Err(e) => {
                log::warn!("卸载清理：打开 HKCU\\Environment 失败，跳过环境变量清理: {e}");
                return 0;
            }
        };

        let names: Vec<String> = env
            .enum_values()
            .filter_map(Result::ok)
            .map(|(name, _)| name)
            .filter(|name| name.starts_with(ENV_VAR_PREFIX))
            .collect();

        let mut deleted = 0;
        for name in names {
            // 直接删注册表值，不走 EnvSink：EnvSink 的白名单与
            // `CC_SWITCH_TEST_HOME` 内存后端都不适合卸载场景。
            match env.delete_value(&name) {
                Ok(()) => deleted += 1,
                Err(e) => log::warn!("卸载清理：删除环境变量 {name} 失败: {e}"),
            }
        }
        if deleted > 0 {
            broadcast_environment_change();
        }
        deleted
    }

    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// 定位 Codex 的 `config.toml`。
///
/// 与 `resolve_pi_models_path` 同理：不读 `settings.json` 的自定义目录设置，
/// 卸载时该文件可能已随 `data` 目录被删。只认环境变量与默认位置。
fn resolve_codex_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir).join("config.toml"));
        }
    }
    dirs::home_dir().map(|home| home.join(".codex").join("config.toml"))
}

/// 清除 Codex `config.toml` 中指向 `CC_SWITCH_CODEX_API_KEY` 的 `env_key`。
///
/// 返回删除的字段数。只处理这一个值：用户自己写的 `env_key`（如 `OPENAI_API_KEY`）
/// 与第三方供应商自带的其它键一律不动。
///
/// 删除 `env_key` 会去掉该 provider 的凭据短路，因此必须同时把
/// `requires_openai_auth` 压回 `false`：否则 `requires_openai_auth = true` 且无
/// 短路时会回退到 `auth.json` 里的官方 OAuth 登录，把官方凭据发到第三方端点
/// （`codex_config.rs` 的写入门控正是为了拦住这个形态）。删空后的无名表一并删除，
/// 否则 Codex 0.149+ 会拒绝整份配置。
fn cleanup_codex_live_config(path: &Path) -> Result<usize, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("读取 {} 失败: {e}", path.display())),
    };

    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;

    let mut removed = 0usize;

    // 顶层 env_key 与逐表 env_key 都可能存在（sanitizer 两条分支各写一处）。
    if doc.get("env_key").and_then(|item| item.as_str()) == Some(CODEX_ENV_KEY_VALUE) {
        doc.remove("env_key");
        // 顶层 env_key 是「无 provider 表」形态的短路；同层的 openai_base_url
        // 会让 Codex 把请求发往第三方却无凭据短路，一并清掉。
        doc.remove("openai_base_url");
        removed += 1;
    }

    // 收集需要删除的表名，不能在遍历中改集合。
    let mut emptied_tables: Vec<String> = Vec::new();
    if let Some(providers) = doc
        .get_mut("model_providers")
        .and_then(|item| item.as_table_like_mut())
    {
        let ids: Vec<String> = providers.iter().map(|(key, _)| key.to_string()).collect();
        for id in ids {
            let Some(table) = providers
                .get_mut(&id)
                .and_then(|item| item.as_table_like_mut())
            else {
                continue;
            };
            if table.get("env_key").and_then(|item| item.as_str()) != Some(CODEX_ENV_KEY_VALUE) {
                continue;
            }
            table.remove("env_key");
            // 必须同事务去掉官方登录回退，否则凭据短路消失后会漏官方 OAuth。
            if table
                .get("requires_openai_auth")
                .and_then(|item| item.as_bool())
                == Some(true)
            {
                table.insert("requires_openai_auth", toml_edit::value(false));
            }
            removed += 1;
            if table.is_empty() {
                emptied_tables.push(id);
            }
        }
    }
    if !emptied_tables.is_empty() {
        if let Some(providers) = doc
            .get_mut("model_providers")
            .and_then(|item| item.as_table_like_mut())
        {
            for id in &emptied_tables {
                providers.remove(id);
            }
        }
    }

    if removed == 0 {
        return Ok(0);
    }

    crate::config::atomic_write_private(path, doc.to_string().as_bytes())
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;

    Ok(removed)
}

/// 通知已运行进程环境变量已变（卸载场景下尽力而为，失败不影响清理结果）。
#[cfg(target_os = "windows")]
fn broadcast_environment_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };

    let env_str: Vec<u16> = "Environment\0".encode_utf16().collect();
    // SAFETY: env_str 是以 NUL 结尾的宽字符串；lpParam 传其指针，其余为文档约定的常量。
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            env_str.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_models(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join("models.json");
        std::fs::write(&path, contents).expect("seed models.json");
        path
    }

    #[test]
    fn managed_node_is_recognized_by_api_key_reference() {
        assert!(is_cc_switch_managed_pi_provider(&json!({
            "name": "鸡毛",
            "baseUrl": "https://api.example.com/v1",
            "api": "openai-completions",
            "apiKey": "$CC_SWITCH_PI_JM_API_KEY",
            "models": [{"id": "kimi-k3"}]
        })));
    }

    #[test]
    fn user_owned_node_is_not_managed() {
        // 明文密钥、缺失 apiKey、非字符串 apiKey 都算用户自有。
        assert!(!is_cc_switch_managed_pi_provider(&json!({
            "name": "mine",
            "apiKey": "sk-literal-user-key"
        })));
        assert!(!is_cc_switch_managed_pi_provider(&json!({
            "name": "mine",
            "baseUrl": "https://api.example.com/v1"
        })));
        assert!(!is_cc_switch_managed_pi_provider(&json!({
            "name": "mine",
            "apiKey": 42
        })));
        // 其它应用的前缀不认（避免误伤用户自己的 CC_SWITCH 风格变量）。
        assert!(!is_cc_switch_managed_pi_provider(&json!({
            "name": "mine",
            "apiKey": "$CC_SWITCH_CODEX_API_KEY"
        })));
    }

    #[test]
    fn cleanup_removes_managed_and_keeps_user_nodes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_models(
            dir.path(),
            r#"{
                "providers": {
                    "jm": {"name": "鸡毛", "apiKey": "$CC_SWITCH_PI_JM_API_KEY"},
                    "hb": {"name": "黑与白", "apiKey": "$CC_SWITCH_PI_HB_API_KEY"},
                    "mine": {"name": "user", "apiKey": "sk-literal"}
                },
                "otherTopLevel": {"kept": true}
            }"#,
        );

        let (removed, kept) = cleanup_pi_models(&path).expect("cleanup");

        assert_eq!(removed, 2);
        assert_eq!(kept, 1);
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let providers = after["providers"].as_object().expect("providers object");
        assert_eq!(providers.len(), 1);
        assert!(providers.contains_key("mine"), "user node must survive");
        assert_eq!(
            after["otherTopLevel"]["kept"], true,
            "unrelated top-level fields must survive"
        );
    }

    #[test]
    fn cleanup_is_noop_when_no_managed_nodes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original = r#"{"providers":{"mine":{"apiKey":"sk-literal"}}}"#;
        let path = write_models(dir.path(), original);

        let (removed, kept) = cleanup_pi_models(&path).expect("cleanup");

        assert_eq!(removed, 0);
        assert_eq!(kept, 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "file must be untouched when nothing is managed"
        );
    }

    #[test]
    fn cleanup_tolerates_missing_file_and_missing_providers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.json");
        assert_eq!(cleanup_pi_models(&missing).expect("missing file"), (0, 0));

        let no_providers = write_models(dir.path(), r#"{"someOtherKey":1}"#);
        assert_eq!(
            cleanup_pi_models(&no_providers).expect("no providers"),
            (0, 0)
        );
    }

    #[test]
    fn cleanup_accepts_json5_with_comments() {
        // Pi 原生文件允许 JSONC/JSON5；解析器必须与 pi_config 保持一致。
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_models(
            dir.path(),
            r#"{
                // managed
                "providers": {
                    "jm": {"apiKey": "$CC_SWITCH_PI_JM_API_KEY",},
                },
            }"#,
        );

        let (removed, kept) = cleanup_pi_models(&path).expect("json5 cleanup");

        assert_eq!(removed, 1);
        assert_eq!(kept, 0);
    }

    #[test]
    fn empty_providers_object_reports_zero_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_models(
            dir.path(),
            r#"{"providers":{"a":{"apiKey":"$CC_SWITCH_PI_A_API_KEY"}}}"#,
        );

        let (removed, kept) = cleanup_pi_models(&path).expect("cleanup");

        assert_eq!(removed, 1);
        assert_eq!(kept, 0);
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(after["providers"].as_object().unwrap().is_empty());
    }

    fn write_codex_config(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, contents).expect("seed config.toml");
        path
    }

    #[test]
    fn codex_cleanup_removes_injected_env_key() {
        // 真实形态：切换第三方 Codex 供应商后 config.toml 里的 env_key。
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_codex_config(
            dir.path(),
            r#"model_provider = "custom"
model = "gpt-6-astra"

[model_providers.custom]
name = "custom"
wire_api = "responses"
requires_openai_auth = true
env_key = "CC_SWITCH_CODEX_API_KEY"

[desktop]
followUpQueueMode = "queue"
"#,
        );

        let removed = cleanup_codex_live_config(&path).expect("cleanup");

        assert_eq!(removed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("CC_SWITCH_CODEX_API_KEY"));
        // 其余字段全部保留。
        assert!(after.contains(r#"model_provider = "custom""#));
        assert!(after.contains(r#"model = "gpt-6-astra""#));
        assert!(after.contains(r#"name = "custom""#));
        assert!(after.contains("[desktop]"));
        assert!(after.contains(r#"followUpQueueMode = "queue""#));
        // 凭据短路被移除后，官方登录回退必须一并关掉，否则会漏官方 OAuth。
        assert!(
            after.contains("requires_openai_auth = false"),
            "dropping env_key must also disable the official-auth fallback"
        );
    }

    #[test]
    fn codex_cleanup_keeps_table_when_it_has_other_fields() {
        // 去掉 env_key 后表里还有 base_url 等字段时，不能整表删除。
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_codex_config(
            dir.path(),
            r#"model_provider = "custom"

[model_providers.custom]
name = "custom"
base_url = "https://api.vendor.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
env_key = "CC_SWITCH_CODEX_API_KEY"
"#,
        );

        let removed = cleanup_codex_live_config(&path).expect("cleanup");

        assert_eq!(removed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("[model_providers.custom]"));
        assert!(after.contains("https://api.vendor.example.com/v1"));
        assert!(!after.contains("CC_SWITCH_CODEX_API_KEY"));
        assert!(after.contains("requires_openai_auth = false"));
    }

    #[test]
    fn codex_cleanup_keeps_user_owned_env_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let original = r#"model_provider = "my-vendor"

[model_providers.my-vendor]
name = "My Vendor"
base_url = "https://api.my-vendor.example.com/v1"
env_key = "MY_OWN_VENDOR_KEY"
"#;
        let path = write_codex_config(dir.path(), original);

        let removed = cleanup_codex_live_config(&path).expect("cleanup");

        assert_eq!(removed, 0);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "file must be untouched when the env_key is user-authored"
        );
    }

    #[test]
    fn codex_cleanup_drops_table_emptied_by_the_removal() {
        // Codex 0.149+ 会因「无名空表」拒绝整份配置，删空后必须连表一起删。
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_codex_config(
            dir.path(),
            r#"model_provider = "custom"

[model_providers.custom]
env_key = "CC_SWITCH_CODEX_API_KEY"

[model_providers.other]
name = "Other"
"#,
        );

        let removed = cleanup_codex_live_config(&path).expect("cleanup");

        assert_eq!(removed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("[model_providers.custom]"));
        assert!(after.contains("[model_providers.other]"));
    }

    #[test]
    fn codex_cleanup_removes_top_level_env_key_too() {
        // sanitizer 在无 provider 表时会回落到顶层 env_key。
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_codex_config(
            dir.path(),
            "env_key = \"CC_SWITCH_CODEX_API_KEY\"\nmodel = \"gpt-6\"\n",
        );

        let removed = cleanup_codex_live_config(&path).expect("cleanup");

        assert_eq!(removed, 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(!after.contains("CC_SWITCH_CODEX_API_KEY"));
        assert!(after.contains("model = \"gpt-6\""));
    }

    #[test]
    fn codex_cleanup_tolerates_missing_file_and_unrelated_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.toml");
        assert_eq!(cleanup_codex_live_config(&missing).expect("missing"), 0);

        let original = "model = \"gpt-6\"\n";
        let path = write_codex_config(dir.path(), original);
        assert_eq!(cleanup_codex_live_config(&path).expect("no env_key"), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}
