//! `editor` 能力端到端：以真实插件（`editor-stub` 夹具）验证插件登记的自有服务可被直接调用与
//! `service.call` 两条路命中同一实现，并在提供者停用后从能力总线消失。

mod support;

use support::stage;
use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{PluginRuntime, PluginStatus};
use atelyx_core::vault::test_support::TempDir;
use serde_json::Value;

const STUB_ID: &str = "com.atelyx.editor-stub";
/// 夹具自有的服务名（与内置服务不重名）。
const SERVICE: &str = "demoeditor";

fn install_editor_stub(tmp: &TempDir, runtime: &std::sync::Arc<PluginRuntime>) {
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    stage(&src, STUB_ID, "editor_stub", &["state", "files"], "");

    let id = runtime.install_dir(&src).expect("安装 editor-stub 应成功").id;
    assert_eq!(id, STUB_ID);
    runtime.set_enabled(&id, true).expect("启用应成功");
    wait_status(runtime, &id, |status| *status == PluginStatus::Running);
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

/// 等探针落盘（夹具登记完服务后立即发起探针）。
fn wait_probe(runtime: &PluginRuntime) -> Value {
    let path = state_path(runtime, STUB_ID);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state: Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        if state["probe"].as_u64().unwrap_or(0) >= 1 {
            return state;
        }
        assert!(Instant::now() < deadline, "等待探针落盘超时");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn service_entry(runtime: &PluginRuntime) -> Option<atelyx_core::plugin_runtime::service::ServiceInfo> {
    runtime
        .service_list()
        .services
        .into_iter()
        .find(|entry| entry.service == SERVICE)
}

#[test]
fn editor_service_scope_and_owner_shutdown() {
    let tmp = TempDir::new("plugin-editor");
    let runtime = PluginRuntime::new(tmp.join("plugins"));

    // 1. 出厂：内置 `editor` 服务归宿主，两个方法都在册
    let editor = runtime
        .service_list()
        .services
        .into_iter()
        .find(|entry| entry.service == "editor")
        .expect("目录里应有 editor 服务");
    assert_eq!(editor.provider, None, "内置实现不落 provider");
    for method in ["selection", "replaceSelection"] {
        assert!(
            editor.methods.iter().any(|name| name == method),
            "{method} 应在册：{:?}",
            editor.methods
        );
    }

    // 2. 装好夹具：它登记自有服务，两条路径都落到它
    install_editor_stub(&tmp, &runtime);
    let state = wait_probe(&runtime);
    let selection = &state["selection"]["ok"];
    assert_eq!(selection["marker"], "stub-editor", "{state}");
    assert_eq!(selection["method"], "selection", "{state}");
    let replace = &state["replace"]["ok"];
    assert_eq!(replace["method"], "replaceSelection", "{state}");
    assert_eq!(replace["params"]["text"], "*", "调用入参应原样到达提供者：{state}");
    let direct = &state["direct"]["ok"];
    assert_eq!(direct, selection, "直接调用与 `service.call` 必须落到同一提供者：{state}");

    // 3. 自有服务在册，内置服务不受影响
    assert_eq!(
        service_entry(&runtime).and_then(|entry| entry.provider).as_deref(),
        Some(STUB_ID)
    );
    assert!(runtime.service_available("editor"), "内置服务照旧在册");

    // 4. 提供者停用：自有服务离开能力总线
    runtime.set_enabled(STUB_ID, false).expect("停用应成功");
    wait_status(&runtime, STUB_ID, |status| *status == PluginStatus::Stopped);
    assert!(!runtime.service_available(SERVICE), "无人提供时服务不可用");
    assert!(service_entry(&runtime).is_none(), "无人提供时服务离开目录");
    assert!(runtime.service_available("editor"), "内置服务不随插件停用");
}
