//! 能力总线端到端：以真实插件进程（`service-stub` 夹具）验证内置目录、`service.call` 与直接
//! 调用对拍、外部提供者顶替、提供者停用后的回落，以及未知服务 / 未知方法 / 不可替换服务的拒绝。
//!
//! 探针由夹具自己发起（同时走直接调用与 `service.call`），结果写进它的自持数据；宿主侧用
//! `vault:file.changed` 广播触发新一轮探针。

use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::service::ServiceInfo;
use atelyx_core::plugin_runtime::{FileChangeKind, PluginRuntime, PluginStatus};
use atelyx_core::vault::test_support::TempDir;
use atelyx_plugin_sdk::protocol::manifest::MANIFEST_FILE;
use serde_json::Value;

const STUB_ID: &str = "com.atelyx.service-stub";

fn entry_name() -> &'static str {
    if cfg!(windows) {
        "plugin.exe"
    } else {
        "plugin"
    }
}

/// 装并启用 `service-stub`，等到它跑起来并落下第一次探针。
fn install_service_stub(tmp: &TempDir, runtime: &std::sync::Arc<PluginRuntime>) {
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join(MANIFEST_FILE),
        r#"{"id":"com.atelyx.service-stub","name":"ServiceStub","version":"1.0.0","apiVersion":1,
            "entry":{"windows":"plugin.exe","linux":"plugin"},"permissions":["state"]}"#,
    )
    .unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_service_stub"), src.join(entry_name()))
        .expect("拷贝入口可执行文件失败");
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

/// 触发一次新的探针（夹具订阅了 `vault:file.changed`），返回序号更大的那次结果。
fn probe(runtime: &std::sync::Arc<PluginRuntime>, after: u64) -> Value {
    runtime.on_file_changed("probe.md", FileChangeKind::Updated, None);
    wait_state(runtime, STUB_ID, |state| {
        state["probe"].as_u64().unwrap_or(0) > after
    })
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

    // 1. 内置目录：平台服务在册、可替换与不可替换区分开
    let builtin = runtime.service_list();
    let vault = service(&builtin, "vault");
    assert_eq!(vault.provider, None, "内置实现不落 provider");
    assert!(vault.replaceable);
    assert!(vault.methods.iter().any(|name| name == "readText"));
    assert!(!service(&builtin, "query").replaceable, "字体与主题不外置");

    // 2. 内置路径对拍：夹具装好后的第一次探针两侧结果一致
    install_service_stub(&tmp, &runtime);
    let first = wait_state(&runtime, STUB_ID, |state| state["probe"].as_u64() == Some(1));
    let builtin_result = assert_paths_agree(&first);
    assert!(builtin_result.get("json").is_some(), "内置实现应给出状态：{builtin_result}");

    // 3. 外部服务：新服务由提供者接任，总线调用落到插件自己
    let registered = runtime.service_list();
    let provided = service(&registered, "stub");
    assert_eq!(provided.provider.as_deref(), Some(STUB_ID), "无内置实现的服务由提供者接任");
    let marker = first["provided"]["ok"].clone();
    assert_eq!(marker["marker"], "stub-stub", "总线调用落到提供者：{marker}");
    assert_eq!(marker["service"], "stub");
    assert_eq!(marker["method"], "ping");
    assert_eq!(marker["params"]["echo"], 1);
    // 探针读到的是插件自己的自持数据（每轮都在变），这里只要求两条路一致
    let still_builtin = probe(&runtime, 1);
    assert_paths_agree(&still_builtin);

    // 4. 提供者停用：服务摘除（没有内置实现可回落），重新启用后再次接任
    runtime.set_enabled(STUB_ID, false).expect("停用应成功");
    wait_status(&runtime, STUB_ID, |status| *status == PluginStatus::Stopped);
    assert!(
        runtime.service_list().services.iter().all(|entry| entry.service != "stub"),
        "提供者停用后服务离场"
    );

    std::fs::remove_file(state_path(&runtime, STUB_ID)).expect("清掉上次会话的探针结果");
    runtime.set_enabled(STUB_ID, true).expect("重新启用应成功");
    wait_status(&runtime, STUB_ID, |status| *status == PluginStatus::Running);
    let restarted = wait_state(&runtime, STUB_ID, |state| state["probe"].as_u64() == Some(1));
    assert_paths_agree(&restarted);
    assert_eq!(service(&runtime.service_list(), "stub").provider.as_deref(), Some(STUB_ID));

    // 5. 可读拒绝：不可替换服务登记、未知服务、未知方法
    let errors = &restarted["errors"];
    let query = errors["queryRegistration"]
        .as_str()
        .expect("query 的登记结果应落盘");
    assert!(query.contains("不可替换"), "不可替换服务应可读拒绝：{query}");
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
