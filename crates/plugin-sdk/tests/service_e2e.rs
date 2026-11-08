//! 能力总线端到端：以真实插件（`service-stub` 夹具）验证内置目录归宿主、插件新建服务、
//! `service.call` 与直接调用对拍、服务随提供者下线从目录消失，以及未知服务 / 未知方法 /
//! 内置服务名占用的可读拒绝。
//!
//! 探针由夹具自己发起（同时走直接调用与 `service.call`），结果写进它的自持数据；宿主侧用
//! `vault:file.changed` 广播触发新一轮探针。

mod support;
use support::stage;
use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::service::ServiceInfo;
use atelyx_core::plugin_runtime::{PluginRuntime, PluginStatus};
use atelyx_core::vault::test_support::TempDir;
use serde_json::Value;

const STUB_ID: &str = "com.atelyx.service-stub";

/// 装并启用 `service-stub`，等到它跑起来并落下第一次探针。
fn install_service_stub(tmp: &TempDir, runtime: &std::sync::Arc<PluginRuntime>) {
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    stage(&src, STUB_ID, "service_stub", &["state"], "");
    let id = runtime.install_dir(&src).expect("安装 service-stub 应成功").id;
    assert_eq!(id, STUB_ID);
    runtime.set_enabled(&id, true).expect("启用应成功");
    wait_status(runtime, &id, |status| *status == PluginStatus::Running);
    wait_state(runtime, &id, |state| state["probe"].as_u64().unwrap_or(0) >= 1);
}

fn wait_status(runtime: &PluginRuntime, id: &str, want: impl Fn(&PluginStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|plugin| plugin.id == id)
            .map(|plugin| plugin.status)
            .unwrap_or(PluginStatus::Stopped);
        if want(&status) {
            return;
        }
        assert!(Instant::now() < deadline, "等待 {id} 状态超时：{status:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn state_path(runtime: &PluginRuntime, id: &str) -> std::path::PathBuf {
    runtime
        .plugin_data_dir(id)
        .expect("插件应已安装")
        .join("state.json")
}

fn read_state(runtime: &PluginRuntime, id: &str) -> Value {
    let text = std::fs::read_to_string(state_path(runtime, id)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// 等自持数据满足条件（探针落盘是异步的）。
fn wait_state(runtime: &PluginRuntime, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state = read_state(runtime, id);
        if ready(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "等待探针落盘超时：{state}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn service<'a>(list: &'a atelyx_core::plugin_runtime::service::ListResult, name: &str) -> &'a ServiceInfo {
    list.services
        .iter()
        .find(|entry| entry.service == name)
        .unwrap_or_else(|| panic!("目录里应有服务 {name}"))
}

/// 探针里 `direct` / `via` 两条路都必须成功且结果一致。
fn assert_paths_agree(state: &Value) -> Value {
    let direct = &state["direct"]["ok"];
    let via = &state["via"]["ok"];
    assert!(!direct.is_null(), "直接调用应成功：{state}");
    assert_eq!(direct, via, "`service.call` 与直接调用必须落到同一实现：{state}");
    direct.clone()
}

#[test]
fn service_bus_registry_call_and_failover() {
    let tmp = TempDir::new("plugin-service-bus");
    let runtime = PluginRuntime::new(tmp.join("plugins"));

    // 1. 内置目录：全部由宿主实现，不落 provider
    let builtin = runtime.service_list();
    let vault = service(&builtin, "vault");
    assert_eq!(vault.provider, None, "内置实现不落 provider");
    assert!(vault.methods.iter().any(|name| name == "readText"));
    let markdown = service(&builtin, "markdown");
    assert_eq!(markdown.provider, None);
    assert_eq!(markdown.methods, vec!["parse".to_string()]);

    // 2. 装好夹具：它登记自己的 `demo` 服务，两条路都落到它
    install_service_stub(&tmp, &runtime);
    let first = wait_state(&runtime, STUB_ID, |state| state["probe"].as_u64() == Some(1));
    let served = assert_paths_agree(&first);
    assert_eq!(served["marker"], "stub-echo", "外部服务应应答：{served}");
    assert_eq!(served["service"], "demo");
    assert_eq!(served["method"], "echo");
    assert_eq!(served["params"]["text"], "hi");
    let listed = runtime.service_list();
    assert_eq!(
        service(&listed, "demo").provider.as_deref(),
        Some(STUB_ID),
        "插件自己的服务归它提供"
    );
    assert_eq!(service(&listed, "vault").provider, None, "内置服务不受影响");

    // 3. 提供者停用：它的服务从目录消失，内置服务照旧
    runtime.set_enabled(STUB_ID, false).expect("停用应成功");
    wait_status(&runtime, STUB_ID, |status| *status == PluginStatus::Stopped);
    assert!(!runtime.service_available("demo"), "提供者下线后服务不可用");
    assert!(runtime.service_available("vault"), "内置服务不随插件停用");

    // 4. 重新启用：重新登记后服务回到目录
    std::fs::remove_file(state_path(&runtime, STUB_ID)).expect("清掉上次会话的探针结果");
    runtime.set_enabled(STUB_ID, true).expect("重新启用应成功");
    wait_status(&runtime, STUB_ID, |status| *status == PluginStatus::Running);
    let restarted = wait_state(&runtime, STUB_ID, |state| state["probe"].as_u64() == Some(1));
    assert_eq!(
        assert_paths_agree(&restarted),
        served,
        "重新登记后结果与首次一致"
    );
    assert_eq!(
        service(&runtime.service_list(), "demo").provider.as_deref(),
        Some(STUB_ID)
    );

    // 5. 可读拒绝：占用内置服务名、未知服务、未知方法
    let errors = &restarted["errors"];
    let builtin_registration = errors["builtinRegistration"]
        .as_str()
        .expect("占用内置服务名的结果应落盘");
    assert!(
        builtin_registration.contains("已被占用"),
        "内置服务名应可读拒绝：{builtin_registration}"
    );
    let unknown_service = errors["unknownService"]["error"]
        .as_str()
        .expect("未知服务应回错误");
    assert!(
        unknown_service.contains(atelyx_core::plugin_runtime::service::error::UNKNOWN_SERVICE),
        "{unknown_service}"
    );
    let unknown_method = errors["unknownMethod"]["error"]
        .as_str()
        .expect("未知方法应回错误");
    assert!(
        unknown_method.contains(atelyx_core::plugin_runtime::service::error::UNKNOWN_METHOD),
        "{unknown_method}"
    );

    runtime.set_enabled(STUB_ID, false).expect("收尾停用应成功");
}
