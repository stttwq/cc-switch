//! 真实凭据管理器的便携包端到端测试（不改动本机凭据）。
//!
//! 跑法：`cargo test --test portable_live -- --ignored --nocapture`
//!
//! 只做「导出 → 校验密文无明文 → 导入到内存库」这一条只读链路，
//! 不向真实凭据管理器写入任何条目。

use cc_switch_lib::secrets::{portable, SecretStore, WindowsSecretStore};

const PASSPHRASE: &str = "cc-switch-test-passphrase-0001";

#[test]
#[ignore]
fn export_real_credentials_and_verify_roundtrip() {
    let store = WindowsSecretStore::new().expect("windows store");

    let (bytes, report) = futures::executor::block_on(portable::export(&store, PASSPHRASE, "test"))
        .expect("export real credentials");

    println!("导出 {report:?}");
    println!("便携包大小: {} 字节", bytes.len());
    assert!(report.exported > 0, "本机应有可导出的凭据");

    // 密文里不得出现任何明文 target 值。
    let text = String::from_utf8_lossy(&bytes);
    for probe in ["cc-switch/v1", "sk-"] {
        assert!(!text.contains(probe), "便携包不得含明文片段: {probe}");
    }

    // 导入到内存库，逐条比对与真实凭据一致。
    let dest = cc_switch_lib::secrets::InMemorySecretStore::new();
    let imported = futures::executor::block_on(portable::import(&dest, &bytes, PASSPHRASE))
        .expect("import into memory store");
    println!("导入 {imported:?}");
    assert_eq!(
        imported.imported + imported.overwritten + imported.unchanged,
        report.exported,
        "导入条目总数应与导出一致"
    );

    // 抽检：真实凭据管理器里的值应原样恢复。
    let targets = futures::executor::block_on(store.list_targets("cc-switch/")).expect("list");
    let mut checked = 0;
    for target in targets.iter().filter(|t| t != &"cc-switch/v1/probe") {
        let Some(real) =
            futures::executor::block_on(store.get_target_raw(target)).expect("read real")
        else {
            continue;
        };
        let restored = futures::executor::block_on(dest.get_target_raw(target))
            .expect("read dest")
            .unwrap_or_else(|| panic!("{target} 未恢复"));
        assert_eq!(real.as_str(), restored.as_str(), "{target} 值不一致");
        checked += 1;
    }
    println!("逐条比对通过: {checked} 条");
    assert!(checked > 0);
}
