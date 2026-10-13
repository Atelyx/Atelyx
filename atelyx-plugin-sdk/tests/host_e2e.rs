//! 宿主运行时端到端：以真实插件进程（`echo` / `crasher` 夹具）验证安装即启用、握手与界面提交、
//! 停用结束进程、崩溃退避与熔断。宿主侧只经 `atelyx_core::plugin_runtime` 访问插件。

use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{PluginRuntime, PluginStatus, VaultSwitchPayload};
use atelyx_core::vault::test_support::TempDir;
use atelyx_plugin_sdk::protocol::manifest::MANIFEST_FILE;

/// 当前平台的可执行入口名（与 manifest 的 entry 映射一致）。
fn entry_name() -> &'static str {
    if cfg!(windows) {
        "plugin.exe"
    } else {
        "plugin"
    }
}

/// 造一个可安装的插件目录：拷贝给定可执行文件为入口，并按 id 写 manifest。
/// `extra` 追加在 `permissions` 之后（形如 `,"keepMountedOnVaultSwitch":true`）。
fn make_plugin(dir: &std::path::Path, id: &str, exe: &str, extra: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            r#"{{"id":"{id}","name":"Fixture","version":"1.0.0","apiVersion":1,
                "entry":{{"windows":"plugin.exe","linux":"plugin"}},"permissions":["state"]{extra}}}"#
        ),
    )
    .unwrap();
    std::fs::copy(exe, dir.join(entry_name())).expect("拷贝入口可执行文件失败");
}

/// 轮询等某插件的界面提交次数达到下限（超时即失败），返回实际次数。
fn wait_submits(runtime: &PluginRuntime, id: &str, min: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let submits = runtime
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.view_submits)
            .unwrap_or(0);
        if submits >= min {
            return submits;
        }
        assert!(Instant::now() < deadline, "等待 {id} 界面提交次数 ≥ {min} 超时，当前 {submits}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 轮询等插件自持数据落盘且含指定片段（超时即失败）。
fn wait_state_contains(path: &std::path::Path, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "等待 {path:?} 出现 {needle} 超时，实际：{text}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 装一个 echo、启用并等到首次界面提交。返回（临时目录守卫、运行时、插件 id、插件自持数据路径）。
fn install_ready_echo(keep_mounted: bool) -> (TempDir, std::sync::Arc<PluginRuntime>, String, std::path::PathBuf) {
    let tmp = TempDir::new("plugin-host-vault-switch");
    let root = tmp.join("plugins");
    let runtime = PluginRuntime::new(root.clone());
    let src = tmp.join("src");
    let extra = if keep_mounted { r#","keepMountedOnVaultSwitch":true"# } else { "" };
    make_plugin(&src, "com.atelyx.echo", env!("CARGO_BIN_EXE_echo"), extra);
    let id = runtime.install_dir(&src).expect("安装 echo 应成功");
    runtime.set_enabled(&id, true).expect("启用应成功");
    wait_status(&runtime, &id, |s| *s == PluginStatus::Running, Duration::from_secs(10));
    wait_submits(&runtime, &id, 1);
    let state = root.join(&id).join("data").join("state.json");
    (tmp, runtime, id, state)
}

/// 轮询等待某插件达到期望状态（超时即失败）。
fn wait_status(runtime: &PluginRuntime, id: &str, want: fn(&PluginStatus) -> bool, timeout: Duration) -> PluginStatus {
    let deadline = Instant::now() + timeout;
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.status)
            .unwrap_or(PluginStatus::Stopped);
        if want(&status) {
            return status;
        }
        assert!(Instant::now() < deadline, "等待插件 {id} 状态超时，最后状态：{status:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 安装即启用 → 握手完成并提交界面 → 停用结束进程 → 卸载移除。
#[test]
fn install_enable_view_submit_disable_uninstall() {
    let tmp = TempDir::new("plugin-host-echo");
    let runtime = PluginRuntime::new(tmp.join("plugins"));
    let src = tmp.join("src");
    make_plugin(&src, "com.atelyx.echo", env!("CARGO_BIN_EXE_echo"), "");

    let id = runtime.install_dir(&src).expect("安装 echo 应成功");
    assert_eq!(id, "com.atelyx.echo");
    // 安装只落位内容，不自动启用：未启用即不在运行
    assert!(!runtime.list().into_iter().find(|p| p.id == id).unwrap().enabled);
    assert_eq!(
        runtime.list().into_iter().find(|p| p.id == id).unwrap().status,
        PluginStatus::Stopped
    );

    // 启用：握手 + on_ready（订阅 + 提交界面）跑通，状态运行中且已收到界面树提交
    runtime.set_enabled(&id, true).expect("启用应成功");
    wait_status(&runtime, &id, |s| *s == PluginStatus::Running, Duration::from_secs(20));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let submits = runtime.list().into_iter().find(|p| p.id == id).unwrap().view_submits;
        if submits >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "等待 echo 首次界面提交超时");
        std::thread::sleep(Duration::from_millis(100));
    }

    // 停用：进程树结束，状态回到未运行
    runtime.set_enabled(&id, false).expect("停用应成功");
    assert_eq!(
        runtime.list().into_iter().find(|p| p.id == id).unwrap().status,
        PluginStatus::Stopped
    );

    // 卸载：目录消失、列表不再含该插件
    runtime.uninstall(&id).expect("卸载应成功");
    assert!(runtime.list().is_empty(), "卸载后不应再有插件条目");
}

/// 崩溃退避与熔断：入口启动即退出，连续崩溃达阈值后进入熔断（不再自动重启）。
#[test]
fn crash_restarts_then_circuit_breaks() {
    let tmp = TempDir::new("plugin-host-crash");
    let runtime = PluginRuntime::new(tmp.join("plugins"));
    let src = tmp.join("src");
    make_plugin(&src, "com.atelyx.crasher", env!("CARGO_BIN_EXE_crasher"), "");

    let id = runtime.install_dir(&src).expect("安装 crasher 应成功");
    runtime.set_enabled(&id, true).expect("启用应成功");
    // 退避序列 1+2+4+8 秒后第 5 次崩溃熔断；留足余量
    let status = wait_status(
        &runtime,
        &id,
        |s| *s == PluginStatus::CircuitBroken,
        Duration::from_secs(45),
    );
    assert_eq!(status, PluginStatus::CircuitBroken);

    // 熔断后可手动重试：状态离开熔断（随即又因崩溃进入重启 / 熔断路径）
    runtime.retry(&id).expect("重试应成功");
    assert_ne!(
        runtime.list().into_iter().find(|p| p.id == id).unwrap().status,
        PluginStatus::CircuitBroken
    );
}

/// 某插件的界面提交次数（重启即重新提交，是「被宿主重载」的可观测信号）。
fn submits_of(runtime: &PluginRuntime, id: &str) -> u64 {
    runtime.list().into_iter().find(|p| p.id == id).map(|p| p.view_submits).unwrap_or(0)
}

/// 切仓库容器：首次激活（启动自动进入）只广播不重载——插件刚由宿主启动、本就没有旧容器上下文；
/// 首次激活与「同一容器再次进入」都不重载，真正换容器才重载未声明保活的插件。
#[test]
fn vault_switch_reloads_plugin_without_keep_mounted() {
    let (_tmp, runtime, id, state_path) = install_ready_echo(false);

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/first".into() });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(submits_of(&runtime, &id), 1, "首次激活不该重载插件");
    assert!(
        std::fs::read_to_string(&state_path).is_err(),
        "未声明保活的插件不该收到 vault:switch"
    );

    // 同一容器再次进入：容器身份未变，同样不动作
    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/first".into() });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(submits_of(&runtime, &id), 1, "同一容器重复进入不该重载插件");

    // 换容器：未声明保活的插件由宿主重载（停进程再起 → 重新提交界面树）
    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/other".into() });
    assert_eq!(wait_submits(&runtime, &id, 2), 2, "换容器应重载插件一次");
    assert!(
        std::fs::read_to_string(&state_path).is_err(),
        "未声明保活的插件始终不该收到 vault:switch"
    );
}

/// 切仓库容器：声明保活的插件不重载，订阅者在每次容器更换时收到 vault:switch 并把容器身份写进
/// 自持数据（首次激活即送达——这是保活插件得知当前容器的唯一来源）。
#[test]
fn vault_switch_keeps_mounted_subscriber_informed() {
    let (_tmp, runtime, id, state_path) = install_ready_echo(true);

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/first".into() });
    wait_state_contains(&state_path, "/vaults/first");

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/other".into() });
    wait_state_contains(&state_path, "/vaults/other");

    // 保活即不重载：界面提交次数仍是 on_ready 那一次
    assert_eq!(submits_of(&runtime, &id), 1, "声明保活的插件不应被重载");
}
