//! 资源管理器右键层叠菜单专用启动器（无控制台，避免闪窗）。
//!
//! 与 `ccs.exe` 的区别只有一个：`windows_subsystem = "windows"`，从资源管理器启动时
//! Windows 不会为它分配控制台窗口，故不会出现「先闪一下黑框再开终端」。它只解析
//! `<app> [--cwd <dir>]`，调 [`cc_switch_lib::cli::run_open_command`]（默认直接跑 CLI），
//! 出错时弹一个消息框（无控制台可写 stderr）。凭据注入边界与 GUI「运行 X」完全一致。
#![cfg_attr(windows, windows_subsystem = "windows")]

use cc_switch_lib::cli;

fn main() {
    let mut it = std::env::args().skip(1); // 去掉程序名

    let app = match it.next() {
        Some(a) => a,
        None => {
            report_error("用法: ccs-open <app> [--cwd <dir>]");
            std::process::exit(cli::EXIT_USAGE);
        }
    };

    let mut cwd: Option<String> = None;
    while let Some(a) = it.next() {
        // 右键菜单只会传 --cwd，其余参数忽略（不因未知参数打断打开）。
        if a == "--cwd" {
            cwd = it.next();
        }
    }

    if let Err(err) = cli::run_open_command(&app, cwd) {
        report_error(&cli::error_message_zh(&err));
        std::process::exit(cli::classify_exit(&err));
    }
}

/// 无控制台环境下的错误提示：Windows 弹消息框；其他平台回退到 stderr。
#[cfg(windows)]
fn report_error(msg: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let title: Vec<u16> = "CC Switch"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let text: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 传入均为以 NUL 结尾的宽字符串；hwnd 传 0（无父窗口）。
    unsafe {
        MessageBoxW(0 as _, text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn report_error(msg: &str) {
    eprintln!("ccs-open: {msg}");
}
