//! 宿主运行时端到端：以真实插件进程（`stub` / `crasher` 夹具）验证安装即启用、握手与界面提交、
//! 停用结束进程、崩溃退避与熔断。宿主侧只经 `atelyx_core::plugin_runtime` 访问插件。

use std::time::{Duration, Instant};

use atelyx_core::plugin_runtime::{
    CanvasPointer, ChatChangeKind, ContributionSlot, DrawCommand, EventValue, MouseButton, NodeBody,
    PluginRuntime, PluginStatus, SurfaceKind, VaultSwitchPayload, ViewEvent, ViewNode,
};
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

/// 装一个 stub、启用并等到首次界面提交。返回（临时目录守卫、运行时、插件 id、插件自持数据路径）。
fn install_ready_stub(keep_mounted: bool) -> (TempDir, std::sync::Arc<PluginRuntime>, String, std::path::PathBuf) {
    let tmp = TempDir::new("plugin-host-vault-switch");
    let root = tmp.join("plugins");
    let runtime = PluginRuntime::new(root.clone());
    let src = tmp.join("src");
    let extra = if keep_mounted { r#","keepMountedOnVaultSwitch":true"# } else { "" };
    make_plugin(&src, "com.atelyx.stub", env!("CARGO_BIN_EXE_stub"), extra);
    let id = runtime.install_dir(&src).expect("安装 stub 应成功").id;
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
    let tmp = TempDir::new("plugin-host-stub");
    let runtime = PluginRuntime::new(tmp.join("plugins"));
    let src = tmp.join("src");
    make_plugin(&src, "com.atelyx.stub", env!("CARGO_BIN_EXE_stub"), "");

    let id = runtime.install_dir(&src).expect("安装 stub 应成功").id;
    assert_eq!(id, "com.atelyx.stub");
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
        assert!(Instant::now() < deadline, "等待 stub 首次界面提交超时");
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

    let id = runtime.install_dir(&src).expect("安装 crasher 应成功").id;
    runtime.set_enabled(&id, true).expect("启用应成功");
    // 退避序列 1+2+4+8 秒后第 5 次崩溃熔断；留足余量
    let status = wait_status(
        &runtime,
        &id,
        |s| *s == PluginStatus::CircuitBroken,
        Duration::from_secs(45),
    );
    assert_eq!(status, PluginStatus::CircuitBroken);

    // 崩溃过程落进插件日志（正式构建没有控制台，日志是唯一可查处）
    let log = runtime.log_tail(&id);
    assert!(log.contains("[进程] 意外退出"), "日志应含退出记录：{log}");
    assert!(log.contains("[熔断]"), "日志应含熔断记录：{log}");

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

/// 界面树里的文本节点内容。
fn text_of(tree: &ViewNode, node_id: &str) -> String {
    match &tree.find(node_id).unwrap_or_else(|| panic!("树里应有 {node_id} 节点")).body {
        NodeBody::Text { content, .. } => content.clone(),
        other => panic!("{node_id} 应为 text 节点，实际：{other:?}"),
    }
}

/// 轮询等画布清单满足条件（超时即失败），返回满足条件时的清单快照。
fn wait_canvas(
    runtime: &PluginRuntime,
    id: &str,
    want: impl Fn(&[DrawCommand]) -> bool,
) -> std::sync::Arc<Vec<DrawCommand>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(list) = runtime.surface_canvas(id, "panel", "board") {
            if want(&list) {
                return list;
            }
        }
        assert!(Instant::now() < deadline, "等待画布清单满足条件超时");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// 清单里的文字指令内容（stub 用提示文字回报最近一次指针事件）。
fn canvas_text(commands: &[DrawCommand]) -> String {
    for command in commands {
        if let DrawCommand::Text { content, .. } = command {
            return content.clone();
        }
    }
    String::new()
}

/// 清单里折线指令的点数（无折线 = `None`）。
fn canvas_path_points(commands: &[DrawCommand]) -> Option<usize> {
    commands.iter().find_map(|command| match command {
        DrawCommand::Path { points, .. } => Some(points.len()),
        _ => None,
    })
}

/// 画布闭环：挂载后插件提交清单 → 指针事件回发驱动清单更新 → 停用清单清空。
#[test]
fn canvas_submit_and_pointer_event_roundtrip() {
    let (_tmp, runtime, id, _state) = install_ready_stub(false);

    // 树里有画布节点，指针事件按 mouseEvents 声明
    let tree = runtime.surface_tree(&id, "panel").expect("应已存档首棵树");
    let NodeBody::Canvas { mouse_events, .. } = &tree.find("board").expect("树里应有画布节点").body
    else {
        panic!("board 应为 canvas 节点");
    };
    assert!(mouse_events.contains(&atelyx_core::plugin_runtime::CanvasEventKind::PointerDown));

    // 面挂载：插件重发界面并提交画布清单（网格 + 提示文字）
    runtime.notify_surface_attached(&id, "panel", SurfaceKind::Panel);
    wait_submits(&runtime, &id, 2);
    wait_canvas(&runtime, &id, |list| !list.is_empty());

    // 指针按下：插件采点并重发清单（提示文字变化）
    runtime.send_view_event(
        &id,
        "panel",
        "board",
        ViewEvent::Canvas {
            pointer: CanvasPointer::Down { x: 10.0, y: 10.0, button: MouseButton::Left },
        },
    );
    wait_canvas(&runtime, &id, |list| canvas_text(list).contains("拖拽画线"));

    // 移动采点：折线指令出现（两点）
    runtime.send_view_event(
        &id,
        "panel",
        "board",
        ViewEvent::Canvas { pointer: CanvasPointer::Move { x: 20.0, y: 20.0 } },
    );
    wait_canvas(&runtime, &id, |list| canvas_path_points(list) == Some(2));

    // 双击清空：折线消失
    runtime.send_view_event(
        &id,
        "panel",
        "board",
        ViewEvent::Canvas {
            pointer: CanvasPointer::DoubleClick { x: 20.0, y: 20.0, button: MouseButton::Left },
        },
    );
    wait_canvas(&runtime, &id, |list| canvas_path_points(list).is_none());

    // 停用：画布清单随会话收尾清空
    runtime.set_enabled(&id, false).expect("停用应成功");
    assert!(runtime.surface_canvas(&id, "panel", "board").is_none(), "停用后画布清单应清空");
}

/// 贡献注册 → 面挂载通知 → 节点事件回发 → 界面树更新 → 停用撤贡献：
/// 界面贡献的完整闭环（宿主侧只经 `plugin_runtime` 的公开面观察）。
#[test]
fn contribution_surface_event_loop_and_teardown() {
    let (_tmp, runtime, id, _state) = install_ready_stub(false);

    // 握手后注册贡献：id 即该面挂载后的 surfaceId
    let contributions = runtime.contributions();
    assert_eq!(contributions.len(), 1, "示例插件应注册一个面板贡献");
    assert_eq!(contributions[0].plugin_id, id);
    assert_eq!(contributions[0].slot, ContributionSlot::Panel);
    assert_eq!(contributions[0].id, "panel");
    assert!(!contributions[0].label.is_empty());

    // 界面树按面存档：停用前可读到插件提交的最新树
    let first = runtime.surface_tree(&id, "panel").expect("应已存档首棵树");
    assert_eq!(text_of(&first, "status"), "就绪");

    // 面挂载：插件重发界面（宿主在布局变化 / 插件重启时都会发这一条）
    runtime.notify_surface_attached(&id, "panel", SurfaceKind::Panel);
    let submits = wait_submits(&runtime, &id, 2);
    assert!(submits >= 2, "面挂载应触发一次重发，实际提交 {submits} 次");

    // 节点事件回发：输入变更 → 插件改状态并整树重发
    runtime.send_view_event(&id, "panel", "query", ViewEvent::Change { value: EventValue::Text("模型".into()) });
    wait_submits(&runtime, &id, 3);
    let updated = runtime.surface_tree(&id, "panel").expect("事件后应读到新树");
    assert_eq!(text_of(&updated, "status"), "输入：模型");

    // 停用：贡献注册与界面树随会话收尾一并撤销，不再对外可见
    runtime.set_enabled(&id, false).expect("停用应成功");
    assert!(runtime.contributions().is_empty(), "停用后贡献注册应撤销");
    assert!(runtime.surface_tree(&id, "panel").is_none(), "停用后界面树应清空");
}

/// 切仓库容器：首次激活（启动自动进入）只广播不重载——插件刚由宿主启动、本就没有旧容器上下文；
/// 首次激活与「同一容器再次进入」都不重载，真正换容器才重载未声明保活的插件。
#[test]
fn vault_switch_reloads_plugin_without_keep_mounted() {
    let (_tmp, runtime, id, state_path) = install_ready_stub(false);

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
    let (_tmp, runtime, id, state_path) = install_ready_stub(true);

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/first".into() });
    wait_state_contains(&state_path, "/vaults/first");

    runtime.on_vault_switch(VaultSwitchPayload::Local { root: "/vaults/other".into() });
    wait_state_contains(&state_path, "/vaults/other");

    // 保活即不重载：界面提交次数仍是 on_ready 那一次
    assert_eq!(submits_of(&runtime, &id), 1, "声明保活的插件不应被重载");
}

/// 会话变更广播：订阅了 ai:chat.changed 的运行中插件收到载荷并写进自持数据。
#[test]
fn chat_changed_reaches_subscribers() {
    let (_tmp, runtime, _id, state_path) = install_ready_stub(true);

    runtime.on_chat_changed("s-42", ChatChangeKind::Messages);
    wait_state_contains(&state_path, "s-42");
    wait_state_contains(&state_path, "messages");
}
