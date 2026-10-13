//! 宿主运行时端到端：以真实插件进程（`echo` / `crasher` 夹具）验证安装即启用、握手与界面提交、
//! 停用结束进程、崩溃退避与熔断。宿主侧只经 `atelyx_core::plugin_runtime` 访问插件。

use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{PluginRuntime, PluginStatus};
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
fn make_plugin(dir: &std::path::Path, id: &str, exe: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        format!(
            r#"{{"id":"{id}","name":"Fixture","version":"1.0.0","apiVersion":1,
                "entry":{{"windows":"plugin.exe","linux":"plugin"}},"permissions":["state"]}}"#
        ),
    )
    .unwrap();
    std::fs::copy(exe, dir.join(entry_name())).expect("拷贝入口可执行文件失败");
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
    make_plugin(&src, "com.atelyx.echo", env!("CARGO_BIN_EXE_echo"));

    let id = runtime.install_dir(&src).expect("安装 echo 应成功");
    assert_eq!(id, "com.atelyx.echo");

    // 握手 + on_ready（订阅 + 提交界面）跑通：状态运行中且已收到界面树提交
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
    make_plugin(&src, "com.atelyx.crasher", env!("CARGO_BIN_EXE_crasher"));

    let id = runtime.install_dir(&src).expect("安装 crasher 应成功");
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
