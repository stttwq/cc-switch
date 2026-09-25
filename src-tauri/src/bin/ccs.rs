//! 2.2 方案 P1：`ccs` 控制台 shim 子二进制。
//!
//! 只做：设 UTF-8 控制台输出码页 → 解析 `std::env::args` → 调
//! `cc_switch_lib::cli::run_env_command` → 按退出码 `std::process::exit`。
//! 不启动 tauri，不做退出码映射以外的任何逻辑（退出码由 lib 定，`Err` 按错误种类映射）。

use cc_switch_lib::cli::{
    self, classify_exit, error_message_en, EnvCommandOutput, Shell, EXIT_USAGE,
};

fn main() {
    // PS 5.1 管道按系统 ANSI 码页解码子进程 stdout；先把输出码页设为 UTF-8，
    // 避免含非 ASCII 字节的密钥经 `| iex` 管道被 GBK 解码损坏。
    set_console_utf8();

    let args: Vec<String> = std::env::args().collect();
    match run(args) {
        Ok(Some(output)) => emit(output),
        Ok(None) => std::process::exit(cli::EXIT_OK), // 仅 --help/--version
        Err((code, msg)) => {
            eprintln!("ccs: {msg}");
            std::process::exit(code);
        }
    }
}

/// 把 stdout 结果透传给用户：stdout 只含 shell 语句，warnings/hints 走 stderr。
fn emit(output: EnvCommandOutput) -> ! {
    print!("{}", output.stdout);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    for w in &output.warnings {
        eprintln!("ccs: {w}");
    }
    std::process::exit(output.exit_code)
}

fn run(args: Vec<String>) -> Result<Option<EnvCommandOutput>, (i32, String)> {
    let mut it = args.into_iter().skip(1); // 去掉程序名
    match it.next().as_deref() {
        Some("env") => {}
        // 卸载清理：MSI 的自定义动作调用 `ccs.exe --cleanup-user-data`。
        Some("--cleanup-user-data") => return Ok(Some(run_cleanup())),
        Some("--help") | Some("-h") | None => {
            eprintln!("{}", usage());
            return Ok(None);
        }
        Some(other) => {
            return Err((
                EXIT_USAGE,
                format!("unknown subcommand '{other}'. {}", usage()),
            ));
        }
    }

    let app = it
        .next()
        .ok_or_else(|| (EXIT_USAGE, format!("<app> is required. {}", usage())))?;

    let mut provider_id: Option<String> = None;
    let mut shell_arg: Option<String> = None;
    let mut clear = false;

    while let Some(a) = it.next() {
        match a.as_str() {
            "--clear" | "--unset" => clear = true,
            "--shell" => {
                let v = it
                    .next()
                    .ok_or_else(|| (EXIT_USAGE, "--shell requires a value".to_string()))?;
                shell_arg = Some(v);
            }
            other if other.starts_with("--") => {
                return Err((EXIT_USAGE, format!("unknown flag '{other}'")));
            }
            positional => {
                if provider_id.is_some() {
                    return Err((EXIT_USAGE, "too many positional arguments".to_string()));
                }
                provider_id = Some(positional.to_string());
            }
        }
    }

    // shell：显式 --shell 优先；否则探测，并在 stderr 提示可覆盖。
    let (shell, auto_note) = match shell_arg.as_deref() {
        Some(s) => (
            Shell::parse(s).ok_or_else(|| {
                (
                    EXIT_USAGE,
                    format!("unsupported --shell '{s}' (powershell|cmd|bash)"),
                )
            })?,
            None,
        ),
        None => {
            let detected = cli::detect_shell();
            (
                detected,
                Some(format!(
                    "auto-detected shell '{}' (override with --shell powershell|cmd|bash)",
                    detected.as_str()
                )),
            )
        }
    };

    if shell == Shell::Cmd {
        eprintln!(
            "ccs: cmd has no eval pipeline; run: \
             for /f \"delims=\" %i in ('ccs env {app}{} --shell cmd') do @%i",
            provider_id
                .as_deref()
                .map(|p| format!(" {p}"))
                .unwrap_or_default()
        );
    }

    let result = cli::run_env_command(&app, provider_id.as_deref(), shell, clear)
        .map_err(|e| (classify_exit(&e), error_message_en(&e)))?;

    // 探测提示排在业务告警前，避免用户误读默认 shell。
    let mut output = result;
    if let Some(note) = auto_note {
        output.warnings.insert(0, note);
    }
    Ok(Some(output))
}

fn usage() -> &'static str {
    "usage: ccs env <app> [provider_id] [--shell powershell|cmd|bash] [--clear]\n\
     \x20      ccs --cleanup-user-data\n\
     \x20 app: claude | codex | pi (pi requires an explicit provider_id)\n\
     \x20 exit codes: 0 ok, 2 usage, 3 missing key, 4 db version mismatch, 5 store/config unavailable"
}

/// 卸载时清理 CC Switch 写到应用外部的托管数据。
///
/// 结果写 stderr，stdout 保持为空：MSI 的 `ExeCommand` 不消费输出，
/// 但保持与 `env` 子命令一致的「stdout 只放给 shell 的语句」约定。
fn run_cleanup() -> EnvCommandOutput {
    let report = cc_switch_lib::uninstall_cleanup::run();
    eprintln!("ccs: user data cleanup done ({})", report.summary());
    EnvCommandOutput {
        stdout: String::new(),
        warnings: Vec::new(),
        exit_code: cli::EXIT_OK,
    }
}

#[cfg(windows)]
fn set_console_utf8() {
    use windows_sys::Win32::Globalization::CP_UTF8;
    use windows_sys::Win32::System::Console::SetConsoleOutputCP;
    unsafe {
        SetConsoleOutputCP(CP_UTF8);
    }
}

#[cfg(not(windows))]
fn set_console_utf8() {}
