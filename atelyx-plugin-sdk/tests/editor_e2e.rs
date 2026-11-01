//! `editor` 能力端到端：以真实插件进程（`editor-stub` 夹具）验证选区服务可被外部提供者
//! 顶替、两条调用路径都落到活动提供者，以及提供者停用后回落内置实现。

use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{PluginRuntime, PluginStatus};
use atelyx_core::vault::test_support::TempDir;
use atelyx_plugin_sdk::protocol::manifest::MANIFEST_FILE;
use serde_json::Value;

const STUB_ID: &str = "com.atelyx.editor-stub";

fn entry_name() -> &'static str {
    if cfg!(windows) {
        "plugin.exe"
    } else {
        "plugin"
    }
}

fn install_editor_stub(tmp: &TempDir, runtime: &std::sync::Arc<PluginRuntime>) {
    let src = tmp.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join(MANIFEST_FILE),
        r#"{"id":"com.atelyx.editor-stub","name":"EditorStub","version":"1.0.0","apiVersion":1,
            "entry":{"windows":"plugin.exe","linux":"plugin"},"permissions":["state","vault"]}"#,
    )
    .unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_editor_stub"), src.join(entry_name()))
        .expect("拷贝入口可执行文件失败");
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

/// 等探针落盘（夹具只在成为活动提供者后才发起探针）。
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

/// 等夹具完成登记（握手先于 `on_ready` 完成，`Running` 不代表服务已在册）。
fn wait_candidate(runtime: &PluginRuntime) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let registered = editor_entry(runtime)
            .is_some_and(|entry| entry.candidates.iter().any(|id| id == STUB_ID));
        if registered {
            return;
        }
        assert!(Instant::now() < deadline, "等待 editor 服务登记超时");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn editor_entry(runtime: &PluginRuntime) -> Option<atelyx_core::plugin_runtime::service::ServiceInfo> {
    runtime
        .service_list()
        .services
        .into_iter()
        .find(|entry| entry.service == "editor")
}

#[test]
fn editor_service_takeover_and_fallback() {
    let tmp = TempDir::new("plugin-editor");
    let runtime = PluginRuntime::new(tmp.join("plugins"));

    // 1. 出厂：editor 由平台内置实现承载，公开方法都在册
    let editor = editor_entry(&runtime).expect("目录里应有 editor 服务");
    assert_eq!(editor.provider, None, "内置实现不落 provider");
    assert!(editor.replaceable);
    for method in ["selection", "replaceSelection"] {
        assert!(
            editor.methods.iter().any(|name| name == method),
            "{method} 应在册：{:?}",
            editor.methods
        );
    }

    // 2. 外部登记只是候选，内置实现仍活动（夹具此时不发探针）
    install_editor_stub(&tmp, &runtime);
    wait_candidate(&runtime);
    let editor = editor_entry(&runtime).expect("目录里应有 editor 服务");
    assert_eq!(editor.provider, None, "登记不等于生效");
    assert_eq!(editor.candidates, vec![STUB_ID.to_string()]);
    assert!(!state_path(&runtime, STUB_ID).exists(), "候选身份下不应发起探针");

    // 3. 切换后两条路径都落到外部提供者
    runtime
        .set_service_provider("editor", Some(STUB_ID))
        .expect("切换到候选提供者应成功");
    assert_eq!(
        editor_entry(&runtime).and_then(|entry| entry.provider).as_deref(),
        Some(STUB_ID)
    );
    let state = wait_probe(&runtime);
    let selection = &state["selection"]["ok"];
    assert_eq!(selection["marker"], "stub-editor", "{state}");
    assert_eq!(selection["method"], "selection", "{state}");
    let replace = &state["replace"]["ok"];
    assert_eq!(replace["method"], "replaceSelection", "{state}");
    assert_eq!(replace["params"]["text"], "*", "调用入参应原样到达提供者：{state}");
    let direct = &state["direct"]["ok"];
    assert_eq!(direct, selection, "直接调用与 `service.call` 必须落到同一提供者：{state}");

    // 4. 切回内置实现：外部提供者停用后编辑器服务仍可用
    runtime
        .set_service_provider("editor", None)
        .expect("内置实现常驻，应允许回落");
    assert_eq!(
        editor_entry(&runtime).and_then(|entry| entry.provider),
        None,
        "回落的是内置实现"
    );
    runtime.set_enabled(STUB_ID, false).expect("停用应成功");
    wait_status(&runtime, STUB_ID, |status| *status == PluginStatus::Stopped);
    assert!(runtime.service_available("editor"), "内置实现不受外部提供者停用影响");
}
