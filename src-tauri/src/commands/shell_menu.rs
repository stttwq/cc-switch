//! Windows 资源管理器右键「在此打开终端」层叠子菜单的装/卸载。
//!
//! 只写 `HKCU\Software\Classes`（当前用户，无需管理员），落点两处：
//! - `Directory\shell\CCSwitchTerminal`      —— 右键点在文件夹上
//! - `Directory\Background\shell\CCSwitchTerminal` —— 右键点在文件夹空白处
//!
//! 层叠菜单用「空 `SubCommands` + 子 `shell` 键」法（无需 CommandStore/管理员）：
//! 父键设 `MUIVerb` 与空字符串 `SubCommands`，子命令挂在 `<父>\shell\<verb>\command`，
//! 命令行指向同目录 `ccs.exe open <app> --cwd "%V"`，凭据注入与 GUI「打开终端」同边界。

/// 前端传入的多语言菜单标签（随应用语言而变，注册表只存一次）。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellMenuLabels {
    /// 顶层项，如「在此打开终端」
    pub root: String,
    pub claude: String,
    pub codex: String,
    pub pi: String,
}

/// 注册表键名（英文固定，避免随语言漂移导致装卸不匹配）。
const MENU_KEY: &str = "CCSwitchTerminal";

/// 注册资源管理器右键层叠菜单。已存在则覆盖（幂等）。
#[tauri::command]
pub async fn register_shell_menu(labels: ShellMenuLabels) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_impl::register(&labels).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = labels;
        Err("右键菜单仅支持 Windows".to_string())
    }
}

/// 卸载右键菜单。不存在也返回成功（幂等）。
#[tauri::command]
pub async fn unregister_shell_menu() -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_impl::unregister().map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        Err("右键菜单仅支持 Windows".to_string())
    }
}

/// 查询右键菜单是否已注册（供设置页开关回显）。
#[tauri::command]
pub async fn is_shell_menu_registered() -> Result<bool, String> {
    #[cfg(windows)]
    {
        Ok(windows_impl::is_registered())
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{ShellMenuLabels, MENU_KEY};
    use std::io;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    /// 两个落点的父键相对路径（相对 HKCU）。
    fn parent_paths() -> [String; 2] {
        [
            format!(r"Software\Classes\Directory\shell\{MENU_KEY}"),
            format!(r"Software\Classes\Directory\Background\shell\{MENU_KEY}"),
        ]
    }

    /// 定位同安装目录下的 `ccs-open.exe`（无控制台启动器，避免闪窗）绝对路径。
    fn launcher_path() -> io::Result<String> {
        let exe = std::env::current_exe()?;
        let dir = exe
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法定位安装目录"))?;
        Ok(dir.join("ccs-open.exe").to_string_lossy().to_string())
    }

    pub fn register(labels: &ShellMenuLabels) -> io::Result<()> {
        let launcher = launcher_path()?;
        let icon = std::env::current_exe()?.to_string_lossy().to_string();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);

        // (排序前缀, app 参数, 标签)——前缀保证 Claude→Codex→Pi 顺序稳定。
        let items = [
            ("01claude", "claude", labels.claude.as_str()),
            ("02codex", "codex", labels.codex.as_str()),
            ("03pi", "pi", labels.pi.as_str()),
        ];

        for parent in parent_paths() {
            // 覆盖前先清掉旧子树，避免残留改名项。
            let _ = hkcu.delete_subkey_all(&parent);

            let (root, _) = hkcu.create_subkey(&parent)?;
            root.set_value("MUIVerb", &labels.root)?;
            // 空字符串 SubCommands 触发「子 shell 键」层叠法。
            root.set_value("SubCommands", &"")?;
            root.set_value("Icon", &icon)?;

            let (shell, _) = root.create_subkey("shell")?;
            for (verb, app, label) in items {
                let (verb_key, _) = shell.create_subkey(verb)?;
                verb_key.set_value("MUIVerb", &label)?;
                verb_key.set_value("Icon", &icon)?;
                let (cmd, _) = verb_key.create_subkey("command")?;
                let line = format!("\"{launcher}\" {app} --cwd \"%V\"");
                cmd.set_value("", &line)?;
            }
        }
        Ok(())
    }

    pub fn unregister() -> io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for parent in parent_paths() {
            match hkcu.delete_subkey_all(&parent) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        // 只要任一落点存在即视为已注册。
        parent_paths()
            .iter()
            .any(|p| hkcu.open_subkey_with_flags(p, KEY_READ).is_ok())
    }
}
