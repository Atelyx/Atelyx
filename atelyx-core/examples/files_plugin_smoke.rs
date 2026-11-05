//! 文件面板插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、尺寸下发、取帧，覆盖展开 / 折叠、
//! 排序与目录颜色写回、文件行点击回发宿主、容器行菜单与移除、拖拽会话收尾、删除确认与根菜单。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    ContainerEntry, CreateEntryParams, CreateEntryResult, DirListResult, DocTypeEntry,
    DragStateParams, FrameInput, HostRequest,
    MeasureTextResult, MouseButton, PickColorResult, PluginRuntime, PluginStatus, SurfaceKind,
    UiDirEntry, VaultUiPatch, VaultUiResult, VaultUiState,
};
use atelyx_plugin_protocol::event;
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{FontSizes, Radii, ThemeColors, ThemeResult};
use atelyx_plugin_protocol::paint::{ColorToken, DrawCommand, PaintColor, TextAlign};

const FILES_ID: &str = "atelyx.files";
const FILES_SURFACE: &str = "files";
/// 冒烟下发的面板尺寸（与 SDK 首帧占位尺寸不同，才能证明按尺寸重排生效）。
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;
/// 窄面宽度：主名必须被省略，后缀徽标仍贴行尾。
const NARROW_W: f32 = 160.0;
/// 行右内衬（与插件 `ROW_PAD_X` 同值，判徽标右缘用）。
const ROW_PAD_X: f32 = 8.0;
/// 行侧缝（与插件 `GUTTER` 同值）：行体本身从面板右缘退让这么宽。
const GUTTER: f32 = 8.0;
/// 徽标右缘 = 面板右缘 - 侧缝 - 行内衬。
const ROW_RIGHT: f32 = NARROW_W - GUTTER - ROW_PAD_X;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    assert_eq!(
        format!("plugin:{FILES_ID}:{FILES_SURFACE}"),
        atelyx_core::layout::FILES_VIEW,
        "布局常量与插件 id + 贡献 id 不得漂移"
    );
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = atelyx_core::scratch::TempDir::new("files-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();

    let state = HostState::new();
    state.entries.lock().unwrap().insert(
        String::new(),
        vec![
            entry("docs", "docs", true, 300),
            entry("media", "media", true, 250),
            entry("notes", "notes", true, 200),
            entry("plan.atb", "plan.atb", false, 100),
        ],
    );
    state.entries.lock().unwrap().insert(
        "docs".to_string(),
        vec![
            entry("docs/方案.md", "方案.md", false, 100),
            entry(&format!("docs/{LONG_NAME}"), LONG_NAME, false, 90),
        ],
    );
    // 第二个可展开目录：证明多目录并发取回的结果各自归位，不被互相覆盖
    state.entries.lock().unwrap().insert(
        "media".to_string(),
        vec![entry("media/图.png", "图.png", false, 80)],
    );
    serve(runtime.clone(), state.clone());

    enable(&runtime, FILES_ID).await;
    wait_running(&runtime, FILES_ID).await;
    runtime.notify_surface_attached(FILES_ID, FILES_SURFACE, SurfaceKind::Panel);
    resize(&runtime, FILES_ID, FILES_SURFACE);

    // 首帧：容器行 + 根目录条目（文件夹在前，按 mtime 新→旧）
    let frame = wait_frame(&runtime, |frame| has_text(frame, "plan"))
        .await
        .expect("首帧应列出根目录条目");
    assert!(has_text(&frame, "本地仓库"), "容器行应显示当前仓库名");
    let container_hit = format!("switch:{LOCAL_CONTAINER_ID}");
    assert!(
        frame.hits.iter().any(|hit| hit.id == container_hit),
        "容器行应挂命中区：{:?}",
        frame.hits.iter().map(|hit| hit.id.clone()).collect::<Vec<_>>()
    );
    assert!(
        frame.hits.iter().any(|hit| hit.id == "row:d:docs"),
        "根目录行应挂命中区：{:?}",
        frame.hits.iter().map(|hit| hit.id.clone()).collect::<Vec<_>>()
    );
    println!("挂面：{} 条指令 / {} 个命中区", frame.commands.len(), frame.hits.len());

    // 展开目录：点目录行 → 取下一层并出带子项的帧
    click_hit(&runtime, &frame, "row:d:docs");
    let expanded = wait_frame(&runtime, |frame| has_text(frame, "方案"))
        .await
        .expect("展开后应出现子项");
    assert!(
        expanded.hits.iter().any(|hit| hit.id.contains("方案.md")),
        "子项行应挂命中区：{:?}",
        expanded.hits.iter().map(|hit| hit.id.clone()).collect::<Vec<_>>()
    );
    assert!(has_text(&expanded, "docs"));
    assert!(
        has_text(&expanded, LONG_STEM),
        "宽度够时名字不应被省略：{:?}",
        expanded
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(state.requested_dirs(), vec![String::new(), "docs".to_string()]);
    println!("展开目录：通过");

    // 窄面：主名按可用宽尾部省略并加「…」，后缀徽标恒显，右缘与层级无关地对齐
    resize_to(&runtime, NARROW_W, PANEL_H);
    let narrow = wait_frame(&runtime, |frame| has_text(frame, "…"))
        .await
        .expect("窄面应出现省略号");
    assert!(!has_text(&narrow, LONG_STEM), "窄面不得完整画出主名");
    let texts = narrow
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { content, x, size, .. } => Some((content.as_str(), *x, *size)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let ellipsized = texts
        .iter()
        .find(|(content, ..)| content.ends_with('…'))
        .expect("应有被省略的主名");
    let name_right = ellipsized.1 + ellipsized.0.chars().count() as f32 * ellipsized.2 * 0.55;
    assert!(name_right <= ROW_RIGHT + 0.01, "主名不得越出面板：{name_right}");
    let badges = texts
        .iter()
        .filter(|(content, ..)| matches!(*content, "ATB" | "MD"))
        .collect::<Vec<_>>();
    assert!(badges.len() >= 2, "根层与子层都应保留后缀徽标：{badges:?}");
    for (content, x, size) in badges {
        let right = x + content.chars().count() as f32 * size * 0.55;
        assert!(
            (right - ROW_RIGHT).abs() < 0.6,
            "{content} 徽标应贴行右缘：{right} vs {ROW_RIGHT}"
        );
    }
    resize_to(&runtime, PANEL_W, PANEL_H);
    let wide = wait_frame(&runtime, |frame| has_text(frame, LONG_STEM))
        .await
        .expect("恢复宽面后应完整显示主名");
    assert!(!has_text(&wide, "…"), "宽面不应省略");
    println!("窄面省略与徽标对齐：通过");

    // 折叠同一目录：子项消失，且不再请求该目录
    let before = state.requested_dirs().len();
    click_hit(&runtime, &expanded, "row:d:docs");
    let collapsed = wait_frame(&runtime, |frame| !has_text(frame, "方案"))
        .await
        .expect("折叠后子项应消失");
    assert_eq!(state.requested_dirs().len(), before, "折叠不应再取目录");
    assert!(
        wait_patch(&state, |patch| patch.expanded.as_deref() == Some(&[String::new()][..])).await,
        "折叠应把展开集写回宿主（展开集的真源在宿主）"
    );
    println!("折叠目录：通过");
    let _ = collapsed;

    // 全部展开 / 收起：只作用于仓库内的目录，容器行（仓库根）保持原状
    let frame = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "toggle-all")
    })
    .await
    .expect("工具条应挂全部展开钮");
    click_hit(&runtime, &frame, "toggle-all");
    // 两个目录同批并发读：两份子项都要落到各自的父级下（并发不得互相覆盖或丢结果）
    let all = wait_frame(&runtime, |frame| has_text(frame, "方案") && has_text(frame, "图"))
        .await
        .expect("全部展开应出现两个目录的子项");
    assert!(has_text(&all, "plan"), "根条目应仍在");
    assert!(
        !has_text(&all, "打开目录"),
        "全部成功时不应留下失败提示：{:?}",
        all.commands
    );
    // 单目录读失败：提示可见、其余目录照常出树（失败不得只落日志）
    state.fail_dirs.lock().unwrap().insert("media".to_string());
    runtime.on_files_changed();
    let failed = wait_frame(&runtime, |frame| has_text(frame, "打开目录 media失败"))
        .await
        .expect("目录读失败应在面板顶部提示");
    assert!(has_text(&failed, "方案"), "一个目录失败不得拖垮整棵树");
    state.fail_dirs.lock().unwrap().remove("media");
    runtime.on_files_changed();
    wait_frame(&runtime, |frame| has_text(frame, "图"))
        .await
        .expect("失败恢复后应重新取到该目录");
    println!("目录并发与失败提示：通过");
    // 收起全部（上一次操作清掉失败提示）
    let all = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "toggle-all")
    })
    .await
    .expect("应能再取一帧");
    click_hit(&runtime, &all, "toggle-all");
    let none = wait_frame(&runtime, |frame| !has_text(frame, "方案") && !has_text(frame, "图"))
        .await
        .expect("收起全部应收起子目录");
    assert!(
        has_text(&none, "plan") && none.hits.iter().any(|hit| hit.id == "row:d:docs"),
        "容器行与根条目不参与收起全部"
    );
    println!("全部展开 / 收起：通过");

    // 排序菜单：点工具条排序钮 → 宿主弹菜单 → 选中键写回
    let frame = wait_frame(&runtime, |frame| !has_text(frame, "方案"))
        .await
        .expect("折叠后应能再取一帧");
    click_hit(&runtime, &frame, "sort");

    let sort_items = wait_menu(&state, |items| items.iter().any(|item| item["id"] == "sort:name-asc"))
        .await
        .expect("排序菜单应声明四档");
    let checked = sort_items
        .iter()
        .find(|item| item["kind"] == "checked")
        .and_then(|item| item["id"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(!checked.is_empty(), "当前档位应打勾");
    select_menu(&state, &runtime, "sort:name-asc");
    assert!(
        wait_patch(&state, |patch| patch.sort.as_deref() == Some("name-asc")).await,
        "选中排序档位应写回宿主"
    );
    println!("排序菜单：通过");

    // 文件行点击 → 宿主按扩展名打开
    click_hit(&runtime, &frame, "row:f:plan.atb");
    wait_opens(&state, 1).await;
    {
        let opens = state.opens.lock().unwrap();
        assert_eq!(opens[0].0, "plan.atb");
        assert!(!opens[0].1, "单击只打开文件，不在布局里新建/切换标签");
    }
    // 打开集合广播后，当前文件行应以强调色高亮（与旧面板同款）
    wait_frame(&runtime, |frame| {
        frame.commands.iter().any(|command| match command {
            DrawCommand::Text { content, color, .. } => {
                content == "plan"
                    && matches!(color, PaintColor::Token { name: ColorToken::Accent })
            }
            _ => false,
        })
    })
    .await
    .expect("打开后的文件行应以强调色高亮");
    println!("文件打开：通过");

    // 容器菜单：右键激活的容器行 → 「在文件管理器中打开」+ 禁用的「从列表移除」
    state.containers.lock().unwrap().push(ContainerEntry {
        id: "space:srv#room".into(),
        label: "团队空间".into(),
        root: None,
        server_url: Some("srv".into()),
        space_id: Some("room".into()),
        active: false,
    });
    runtime.on_files_changed();
    let frame = wait_frame(&runtime, |frame| has_text(frame, "plan"))
        .await
        .expect("事件后应重取");
    click_right(&runtime, &frame, &format!("switch:{LOCAL_CONTAINER_ID}"));
    let items = wait_menu(&state, |items| {
        items.iter().any(|item| item["id"] == format!("remove-container:{LOCAL_CONTAINER_ID}"))
    })
    .await
    .expect("容器菜单应给出打开与移除入口");
    assert!(
        items.iter().any(|item| item["id"] == "reveal-vault:/tmp/vault"),
        "本机仓库应给在文件管理器中打开：{items:?}"
    );
    println!("容器菜单：通过");

    // 目录颜色：右键目录行 → 「图标颜色」 → 宿主取色器 → 选中色写回仓库界面状态
    click_right(&runtime, &frame, "row:d:docs");
    wait_menu(&state, |items| items.iter().any(|item| item["id"] == "color:docs"))
        .await
        .expect("目录菜单应给出图标颜色入口");
    select_menu(&state, &runtime, "color:docs");
    wait_picks(&state, 1).await;
    assert!(
        wait_patch(&state, |patch| patch
            .folder_colors
            .as_ref()
            .is_some_and(|colors| colors.get("docs").map(String::as_str) == Some(PICKED_COLOR)))
            .await,
        "取色器选中的颜色应写回仓库界面状态"
    );
    println!("目录颜色：通过");

    // 新建：右键根 → 「新建笔记」→ 就地输入 → Enter → 宿主 vault.create
    click_right(&runtime, &frame, "row:d:docs");
    wait_menu(&state, |items| items.iter().any(|item| item["id"] == "draft:note:docs"))
        .await
        .expect("目录菜单应给出新建入口");
    select_menu(&state, &runtime, "draft:note:docs");
    wait_frame(&runtime, |frame| frame.hits.iter().any(|hit| hit.id == "inline-input"))
        .await
        .expect("新建草稿应出现就地输入行");
    type_text(&runtime, "会议记录");
    press_enter(&runtime);
    let created = wait_creates(&state, 1).await;
    assert_eq!((created.dir.as_str(), created.kind.as_str()), ("docs", "note"));
    assert_eq!(created.name, "会议记录");
    println!("新建条目：通过");

    // 切换容器：点非激活容器行 → 宿主收到切换请求，并恢复目标容器自己的展开集
    let frame = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "switch:space:srv#room")
    })
    .await
    .expect("协作空间行应挂命中区");
    click_hit(&runtime, &frame, "switch:space:srv#room");
    wait_switches(&state, 1).await;
    assert_eq!(state.switches.lock().unwrap()[0], "space:srv#room");
    let space_frame = wait_frame(&runtime, |frame| has_text(frame, "方案"))
        .await
        .expect("空间自己的展开集应恢复 docs 展开");
    assert!(
        space_frame.hits.iter().any(|hit| hit.id == format!("switch:{LOCAL_CONTAINER_ID}")),
        "切到空间后仍应列出本机容器"
    );
    click_hit(&runtime, &space_frame, &format!("switch:{LOCAL_CONTAINER_ID}"));
    wait_switches(&state, 2).await;
    assert_eq!(state.switches.lock().unwrap()[1], LOCAL_CONTAINER_ID);
    let local_frame = wait_frame(&runtime, |frame| !has_text(frame, "方案"))
        .await
        .expect("切回本机应恢复本机的收起态");
    assert!(has_text(&local_frame, "plan"), "本机条目应仍在");
    println!("切换容器与展开集隔离：通过");

    // 移除容器：激活行只给禁用项，非激活行进二段确认 → 宿主收到移除请求
    let frame = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "switch:space:srv#room")
    })
    .await
    .expect("容器行应同时列出两个容器");
    click_right(&runtime, &frame, &format!("switch:{LOCAL_CONTAINER_ID}"));
    let items = wait_menu(&state, |items| {
        items.iter().any(|item| item["id"] == format!("remove-container:{LOCAL_CONTAINER_ID}"))
    })
    .await
    .expect("激活容器菜单应列出移除项");
    let active_item = items
        .iter()
        .find(|item| item["id"] == format!("remove-container:{LOCAL_CONTAINER_ID}"))
        .expect("激活容器应给出移除项");
    assert_eq!(active_item["disabled"], json!(true), "激活容器不可移除");
    assert_eq!(active_item["note"], json!("当前容器"), "激活行应标出当前容器");
    click_right(&runtime, &frame, "switch:space:srv#room");
    let items = wait_menu(&state, |items| {
        items.iter().any(|item| item["id"] == "remove-container:space:srv#room")
    })
    .await
    .expect("非激活容器菜单应给出确认移除");
    assert!(
        items.iter().any(|item| item["kind"] == "submenu" && item["pane"] == json!(0)),
        "移除入口应在根面板作为子面板入口：{items:?}"
    );
    assert!(
        items.iter().any(|item| item["kind"] == "text" && item["pane"] == json!(1)),
        "确认面板应说明只移出列表：{items:?}"
    );
    select_menu(&state, &runtime, "remove-container:space:srv#room");
    wait_removes(&state, 1).await;
    assert_eq!(state.removes.lock().unwrap()[0], "space:srv#room");
    println!("移除容器：通过");

    // 拖拽：按住文件行移动超过阈值起拖，落点离开行（宿主推「无落点」）后松手仍要收尾
    let frame = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "row:f:plan.atb")
    })
    .await
    .expect("文件行应挂命中区");
    let hit = frame
        .hits
        .iter()
        .find(|hit| hit.id == "row:f:plan.atb")
        .expect("文件行命中区");
    let (x, y) = (hit.x + hit.w / 2.0, hit.y + hit.h / 2.0);
    let drag = Some("row:f:plan.atb".to_string());
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: drag.clone() },
    );
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerMove { x: x + 20.0, y, hit: drag },
    );
    wait_drags(&state, 1).await;
    assert_eq!(state.drags.lock().unwrap()[0], "plan.atb");
    // 起拖即显示列表下方根落点（落点区跟着拖拽会话，不依赖指针是否已落上去）
    wait_frame(&runtime, |frame| has_text(frame, "移到根目录"))
        .await
        .expect("起拖后应出现列表下方根落点");
    assert!(
        state
            .drag_hovers
            .lock()
            .unwrap()
            .iter()
            .any(|hover| hover.as_deref() == Some("plan.atb")),
        "起拖后应上报所在行落点"
    );
    // 列表下方空白 = 根落点：内容不满一屏时整块树视口挂 drop-root:bottom
    let bottom = frame
        .hits
        .iter()
        .find(|hit| hit.id == "drop-root:bottom")
        .expect("树视口空白区应挂根落点")
        .clone();
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerMove {
            x: bottom.x + bottom.w / 2.0,
            y: bottom.y + bottom.h - 4.0,
            hit: Some(bottom.id.clone()),
        },
    );
    wait_for(
        || state.drag_hovers.lock().unwrap().last().cloned() == Some(Some(String::new())),
        "空白区应上报仓库根落点",
    )
    .await;
    wait_frame(&runtime, |frame| has_text(frame, "松手移到根目录"))
        .await
        .expect("悬停空白区应显示松手落位");
    // 行侧缝 = 根落点：指针移到缝上要上报空路径（拖回仓库根）。
    // 整块树视口也挂根落点（drop-root:bottom），按带序号的侧缝 id 取
    let seam = frame
        .hits
        .iter()
        .find(|hit| hit.id.starts_with("drop-root:") && hit.id != "drop-root:bottom")
        .expect("行侧缝应挂命中区")
        .clone();
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerMove {
            x: seam.x + seam.w / 2.0,
            y: seam.y + seam.h / 2.0,
            hit: Some(seam.id.clone()),
        },
    );
    wait_for(
        || state.drag_hovers.lock().unwrap().last().cloned() == Some(Some(String::new())),
        "侧缝应上报仓库根落点",
    )
    .await;
    // 宿主悬停展开折叠目录（springing 转 false）→ 本面跟着展开，落点才够得到下一层
    runtime.notify_plugin_event(
        FILES_ID,
        event::name::DRAG_STATE,
        &DragStateParams { target: Some("docs".into()), springing: false, ..Default::default() },
    );
    wait_frame(&runtime, |frame| has_text(frame, "方案"))
        .await
        .expect("宿主展开的目录应在本面展开");
    // 指针离开所有行时宿主推「无落点」：那不是会话结束，松手仍要发 drag.end
    runtime.notify_plugin_event(FILES_ID, event::name::DRAG_STATE, &DragStateParams::default());
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerUp { x: x + 40.0, y, button: MouseButton::Left, hit: None },
    );
    wait_drag_ends(&state, 1).await;
    assert_eq!(state.drag_ends.lock().unwrap()[0], false, "松手应落位收尾");
    // 收尾丢失（宿主已取消会话）：新的一次按下按未落位收掉，不让下个手势被吞
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some("row:f:plan.atb".into()) },
    );
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerMove { x: x + 20.0, y, hit: Some("row:f:plan.atb".into()) },
    );
    wait_drags(&state, 2).await;
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some("row:f:plan.atb".into()) },
    );
    wait_drag_ends(&state, 2).await;
    assert!(state.drag_ends.lock().unwrap()[1], "丢失的会话应按未落位收掉");
    println!("拖拽会话：通过");

    // 重命名：输入行只给主名；手打同名后缀先剥掉，避免拼成 plan2.atb.atb
    let frame = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "row:f:plan.atb")
    })
    .await
    .expect("文件行应挂命中区");
    click_right(&runtime, &frame, "row:f:plan.atb");
    wait_menu(&state, |items| items.iter().any(|item| item["id"] == "rename:plan.atb"))
        .await
        .expect("文件菜单应给出重命名入口");
    select_menu(&state, &runtime, "rename:plan.atb");
    let editing = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "inline-input")
    })
    .await
    .expect("重命名应出现就地输入行");
    assert!(!has_text(&editing, "plan.atb"), "输入行只画主名，不画后缀");
    type_text(&runtime, "2.atb");
    press_enter(&runtime);
    let renames = wait_renames(&state, 1).await;
    assert_eq!(
        renames[0],
        ("plan.atb".to_string(), "plan2.atb".to_string()),
        "手打的同名后缀应剥掉再拼回"
    );
    println!("重命名后缀：通过");

    // 删除确认：右键文件 → 删除 → 二段确认才真的删（危险动作不直落）
    let frame = wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "row:f:plan.atb")
    })
    .await
    .expect("文件行应挂命中区");
    click_right(&runtime, &frame, "row:f:plan.atb");
    wait_menu(&state, |items| items.iter().any(|item| item["id"] == "delete:plan.atb|f"))
        .await
        .expect("文件菜单应给出删除入口");
    select_menu(&state, &runtime, "delete:plan.atb|f");
    wait_menu(&state, |items| {
        items.iter().any(|item| item["id"] == "delete-confirm:plan.atb|f")
    })
    .await
    .expect("删除应先给确认");
    assert!(state.deletes.lock().unwrap().is_empty(), "确认前不得真的删");
    select_menu(&state, &runtime, "delete-confirm:plan.atb|f");
    wait_deletes(&state, 1).await;
    assert_eq!(state.deletes.lock().unwrap()[0], "plan.atb");
    println!("删除确认：通过");

    // 空白处右键 = 仓库根菜单：新建入口齐全（旧面板根菜单同款）
    right_click_empty(&runtime);
    wait_menu(&state, |items| {
        items.iter().any(|item| item["id"] == "draft:table:")
            && items.iter().any(|item| item["id"] == "draft:folder:")
            && items.iter().any(|item| item["id"] == "draft:canvas:")
    })
    .await
    .expect("根菜单应给齐新建入口");
    println!("根菜单：通过");

    // 工具条「打开文件夹」→ 宿主选择器入口
    let frame = wait_frame(&runtime, |frame| frame.hits.iter().any(|hit| hit.id == "open-vault"))
        .await
        .expect("工具条应挂打开文件夹钮");
    click_hit(&runtime, &frame, "open-vault");
    wait_pick_vaults(&state, 1).await;
    println!("打开仓库入口：通过");

    // 空仓库：面板级空态在工具条下方居中
    state.entries.lock().unwrap().insert(String::new(), Vec::new());
    runtime.on_files_changed();
    let empty = wait_frame(&runtime, |frame| has_text(frame, "暂无文件"))
        .await
        .expect("空仓库应出面板级空态");
    assert_empty_state_centered(&empty, "暂无文件", "右键新建笔记或文件夹。");
    println!("空仓库空态：通过");

    runtime.set_enabled(FILES_ID, false).expect("停用");
    let status = runtime
        .list()
        .into_iter()
        .find(|plugin| plugin.id == FILES_ID)
        .map(|plugin| plugin.status);
    assert_eq!(status, Some(PluginStatus::Stopped), "停用后应回到 Stopped");
    println!("停用：通过");

    runtime.shutdown();
}

fn entry(path: &str, name: &str, is_dir: bool, mtime: i64) -> UiDirEntry {
    UiDirEntry { name: name.into(), path: path.into(), is_dir, mtime }
}

/// 取色器回填给插件的颜色（冒烟宿主固定选这一个）。
const PICKED_COLOR: &str = "#e0a94e";
/// 冒烟宿主的本地仓库容器 id（与 [`ContainerEntry::id`] 同一形态）。
const LOCAL_CONTAINER_ID: &str = "local:/tmp/vault";
/// 足够长、但在面板宽度内放得下的中文文件名：验证行内名字按内容定宽而不是被均分挤省略。
const LONG_NAME: &str = "这是一个特别长的中文文件名用来验证不会被过早省略.md";
/// 上面那条的主名（行内只画主名，后缀另作徽标）。
const LONG_STEM: &str = "这是一个特别长的中文文件名用来验证不会被过早省略";

/// 宿主侧回执：主题 / 量算 / 界面状态 / 菜单 / 打开与写回记录。
#[derive(Clone)]
struct HostState {
    entries: Arc<Mutex<std::collections::HashMap<String, Vec<UiDirEntry>>>>,
    containers: Arc<Mutex<Vec<ContainerEntry>>>,
    /// 插件请求过的目录（按请求顺序）。
    dirs: Arc<Mutex<Vec<String>>>,
    patches: Arc<Mutex<Vec<VaultUiPatch>>>,
    opens: Arc<Mutex<Vec<(String, bool)>>>,
    /// 当前打开的宿主菜单条目（插件声明，宿主渲染）。
    menu: Arc<Mutex<Vec<Value>>>,
    /// 插件发起过的拖拽（被拖路径）。
    drags: Arc<Mutex<Vec<String>>>,
    /// 插件回推的落点（None = 指针不在任何行上）。
    drag_hovers: Arc<Mutex<Vec<Option<String>>>>,
    /// 插件结束拖拽时的「取消」标志。
    drag_ends: Arc<Mutex<Vec<bool>>>,
    /// 插件请求新建的条目（目录 / 名字 / 类型），按请求顺序。
    creates: Arc<Mutex<Vec<CreateEntryParams>>>,
    /// 插件请求切换的容器 id。
    switches: Arc<Mutex<Vec<String>>>,
    /// 插件请求移出最近列表的容器 id。
    removes: Arc<Mutex<Vec<String>>>,
    /// 插件请求删除的仓库相对路径。
    deletes: Arc<Mutex<Vec<String>>>,
    /// 插件请求重命名的（原路径，目标路径）。
    renames: Arc<Mutex<Vec<(String, String)>>>,
    /// 按容器分开保存的展开集（切容器时各取各的）。
    expanded_by_container: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>>,
    /// 读取时故意回绝的目录（验证失败提示与部分失败）。
    fail_dirs: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 插件请求过的容器切换计数。
    pick_vaults: Arc<Mutex<usize>>,
    /// 插件请求过的取色次数。
    picks: Arc<Mutex<Vec<Option<String>>>>,
    /// 宿主注册的插件文档类型（「新建」菜单条目来源）。
    doc_types: Arc<Mutex<Vec<DocTypeEntry>>>,
    /// 宿主持有的展开集（仓库界面状态的真源）。
    expanded: Arc<Mutex<Vec<String>>>,
    /// 当前打开的文件（宿主 `editor.openFiles` 的回执来源）。
    active: Arc<Mutex<Vec<String>>>,
}

impl HostState {
    fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Default::default())),
            containers: Arc::new(Mutex::new(vec![ContainerEntry {
                id: "local:/tmp/vault".into(),
                label: "本地仓库".into(),
                root: Some("/tmp/vault".into()),
                server_url: None,
                space_id: None,
                active: true,
            }])),
            dirs: Arc::new(Mutex::new(Vec::new())),
            patches: Arc::new(Mutex::new(Vec::new())),
            opens: Arc::new(Mutex::new(Vec::new())),
            menu: Arc::new(Mutex::new(Vec::new())),
            drags: Arc::new(Mutex::new(Vec::new())),
            drag_hovers: Arc::new(Mutex::new(Vec::new())),
            drag_ends: Arc::new(Mutex::new(Vec::new())),
            creates: Arc::new(Mutex::new(Vec::new())),
            switches: Arc::new(Mutex::new(Vec::new())),
            removes: Arc::new(Mutex::new(Vec::new())),
            deletes: Arc::new(Mutex::new(Vec::new())),
            renames: Arc::new(Mutex::new(Vec::new())),
            expanded_by_container: Arc::new(Mutex::new(std::collections::HashMap::from([
                ("local:/tmp/vault".to_string(), vec![String::new()]),
                ("space:srv#room".to_string(), vec![String::new(), "docs".to_string()]),
            ]))),
            fail_dirs: Arc::new(Mutex::new(std::collections::HashSet::new())),
            pick_vaults: Arc::new(Mutex::new(0)),
            picks: Arc::new(Mutex::new(Vec::new())),
            doc_types: Arc::new(Mutex::new(vec![DocTypeEntry {
                extension: "board".into(),
                label: "画板".into(),
                icon: "layout-dashboard".into(),
            }])),
            expanded: Arc::new(Mutex::new(vec![String::new()])),
            active: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requested_dirs(&self) -> Vec<String> {
        self.dirs.lock().unwrap().clone()
    }

    /// 当前激活的容器 id（展开集按它归属）。
    fn active_container(&self) -> String {
        self.containers
            .lock()
            .unwrap()
            .iter()
            .find(|container| container.active)
            .map(|container| container.id.clone())
            .unwrap_or_default()
    }
}

fn serve(runtime: Arc<PluginRuntime>, state: HostState) {
    tokio::spawn(async move {
        while let Some(request) = runtime.recv_host_request().await {
            match request {
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&runtime, request_id, serde_json::to_value(theme()).unwrap());
                }
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    let sizes: Vec<MeasureTextResult> = params
                        .items
                        .iter()
                        .map(|item| MeasureTextResult { width: item.text.chars().count() as f32 * item.size * 0.55, height: item.size * 1.2, lines: 1, segments: None })
                        .collect();
                    reply(&runtime, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::VaultUi { request_id, params, .. } => {
                    let dir = params.dir.clone().unwrap_or_default();
                    state.dirs.lock().unwrap().push(dir.clone());
                    if state.fail_dirs.lock().unwrap().contains(&dir) {
                        runtime.resolve_shell_call(request_id, Err(format!("读取 {dir} 失败")));
                        continue;
                    }
                    let entries = state
                        .entries
                        .lock()
                        .unwrap()
                        .get(&dir)
                        .cloned()
                        .unwrap_or_default();
                    let result = VaultUiResult {
                        state: VaultUiState {
                            expanded: state.expanded.lock().unwrap().clone(),
                            sort: "mtime-desc".into(),
                            ..VaultUiState::default()
                        },
                        entries,
                        containers: state.containers.lock().unwrap().clone(),
                        doc_types: state.doc_types.lock().unwrap().clone(),
                    };
                    reply(&runtime, request_id, serde_json::to_value(result).unwrap());
                }
                HostRequest::VaultUiWrite { request_id, patch, .. } => {
                    state.patches.lock().unwrap().push(patch.clone());
                    if let Some(expanded) = patch.expanded.clone() {
                        let container = state.active_container();
                        state
                            .expanded_by_container
                            .lock()
                            .unwrap()
                            .insert(container, expanded.clone());
                        *state.expanded.lock().unwrap() = expanded;
                    }
                    let result = VaultUiResult {
                        state: VaultUiState {
                            sort: patch.sort.clone().unwrap_or_else(|| "mtime-desc".into()),
                            expanded: state.expanded.lock().unwrap().clone(),
                            ..VaultUiState::default()
                        },
                        entries: Vec::new(),
                        containers: state.containers.lock().unwrap().clone(),
                        doc_types: state.doc_types.lock().unwrap().clone(),
                    };
                    reply(&runtime, request_id, serde_json::to_value(result).unwrap());
                }
                HostRequest::MenuOpen { request_id, params, .. } => {
                    let items = serde_json::to_value(&params.items)
                        .ok()
                        .and_then(|value| value.as_array().cloned())
                        .unwrap_or_default();
                    *state.menu.lock().unwrap() = items;
                    // 菜单走界面服务通道（不是壳侧服务）：用错回执通道会让插件的 open_menu 永远等不到结果
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::DragStart { request_id, params, .. } => {
                    state.drags.lock().unwrap().push(params.item.path.clone());
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::DragHover { request_id, params, .. } => {
                    state.drag_hovers.lock().unwrap().push(params.target.clone());
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::DragEnd { request_id, params, .. } => {
                    state.drag_ends.lock().unwrap().push(params.cancelled);
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::PickColor { request_id, params, .. } => {
                    state.picks.lock().unwrap().push(params.initial.clone());
                    // 取色器回执带选中色，走壳侧服务的值通道
                    let result = serde_json::to_value(PickColorResult {
                        color: Some(PICKED_COLOR.to_string()),
                    })
                    .expect("取色结果可序列化");
                    resolve(&runtime, request_id, result);
                }
                HostRequest::PickVault { request_id, .. } => {
                    *state.pick_vaults.lock().unwrap() += 1;
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::VaultListDirs { request_id, .. } => {
                    let dirs = state
                        .entries
                        .lock()
                        .unwrap()
                        .keys()
                        .filter(|dir| !dir.is_empty())
                        .cloned()
                        .collect::<Vec<_>>();
                    let result = serde_json::to_value(DirListResult { dirs }).unwrap();
                    reply(&runtime, request_id, result);
                }
                HostRequest::VaultCreate { request_id, params, .. } => {
                    state.creates.lock().unwrap().push(params.clone());
                    let result = serde_json::to_value(CreateEntryResult {
                        path: format!("{}/{}", params.dir, params.name).trim_start_matches('/').into(),
                        is_dir: params.kind == "folder",
                    })
                    .expect("新建结果可序列化");
                    resolve(&runtime, request_id, result);
                }
                HostRequest::VaultSwitchContainer { request_id, params, .. } => {
                    state.switches.lock().unwrap().push(params.id.clone());
                    let from = state.active_container();
                    let current = state.expanded.lock().unwrap().clone();
                    state.expanded_by_container.lock().unwrap().insert(from, current);
                    let next = state
                        .expanded_by_container
                        .lock()
                        .unwrap()
                        .get(&params.id)
                        .cloned()
                        .unwrap_or_else(|| vec![String::new()]);
                    *state.expanded.lock().unwrap() = next;
                    for container in state.containers.lock().unwrap().iter_mut() {
                        container.active = container.id == params.id;
                    }
                    resolve(&runtime, request_id, json!({}));
                    // 真宿主切换后广播载体：订阅方据此重取该容器的界面状态（含展开集）
                    runtime.notify_plugin_event(FILES_ID, event::name::VAULT_SWITCH, &json!({}));
                }
                HostRequest::VaultRemoveContainer { request_id, params, .. } => {
                    state.removes.lock().unwrap().push(params.id.clone());
                    resolve(&runtime, request_id, json!({}));
                }
                HostRequest::VaultRemove { request_id, path, .. } => {
                    state.deletes.lock().unwrap().push(path);
                    resolve(&runtime, request_id, json!({}));
                }
                HostRequest::VaultRename { request_id, from, to, .. } => {
                    state.renames.lock().unwrap().push((from, to));
                    resolve(&runtime, request_id, json!({}));
                }
                HostRequest::EditorOpenFile { request_id, path, reveal, .. } => {
                    state.opens.lock().unwrap().push((path.clone(), reveal));
                    {
                        let mut active = state.active.lock().unwrap();
                        active.retain(|open| open != &path);
                        active.push(path);
                    }
                    reply(&runtime, request_id, json!({}));
                    // 真宿主在 update 收口广播该事件：订阅方据此重取打开集合
                    runtime.notify_plugin_event(
                        FILES_ID,
                        "editor:open.changed",
                        &json!({}),
                    );
                }
                HostRequest::EditorOpenFiles { request_id, .. } => {
                    let result = serde_json::to_value(atelyx_core::plugin_runtime::EditorOpenFiles {
                        paths: state.active.lock().unwrap().clone(),
                    })
                    .expect("打开文件清单可序列化");
                    reply(&runtime, request_id, result);
                }
                other => eprintln!("未处理的宿主请求：{other:?}"),
            }
        }
    });
}

/// 模拟宿主菜单选中：把选中项回发给插件（与壳侧同一条通道）。
fn select_menu(state: &HostState, runtime: &Arc<PluginRuntime>, selected: &str) {
    state.menu.lock().unwrap().clear();
    runtime.notify_menu_closed(FILES_ID, Some(selected.to_string()));
}

fn theme() -> ThemeResult {
    ThemeResult {
        colors: ThemeColors { accent: "#e0a94e".into(), ..ThemeColors::default() },
        font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
        radii: Radii { xs: 4.0, sm: 6.0, md: 10.0 },
        event_colors: vec!["#e06c75".into()],
    }
}

fn reply(runtime: &PluginRuntime, request_id: u64, value: Value) {
    runtime.resolve_shell_call(request_id, Ok(value));
}

/// 壳侧服务的值通道回执（与 [`reply`] 同一条，名字用于区分语义）。
fn resolve(runtime: &PluginRuntime, request_id: u64, value: Value) {
    runtime.resolve_shell_call(request_id, Ok(value));
}

/// 右键一个命中区（菜单类入口）。
fn click_right(runtime: &PluginRuntime, frame: &SurfaceFrame, id: &str) {
    let hit = frame
        .hits
        .iter()
        .find(|hit| hit.id == id)
        .unwrap_or_else(|| panic!("帧里没有命中区 {id}"));
    let x = hit.x + hit.w / 2.0;
    let y = hit.y + hit.h / 2.0;
    for button in [MouseButton::Right] {
        runtime.notify_frame_input(
            FILES_ID,
            FILES_SURFACE,
            &FrameInput::PointerDown { x, y, button, hit: Some(id.into()) },
        );
        runtime.notify_frame_input(
            FILES_ID,
            FILES_SURFACE,
            &FrameInput::PointerUp { x, y, button, hit: Some(id.into()) },
        );
    }
}

/// 右键列表空白处（无命中区）：= 仓库根菜单。
fn right_click_empty(runtime: &PluginRuntime) {
    let (x, y) = (PANEL_W / 2.0, PANEL_H - 12.0);
    for button in [MouseButton::Right] {
        runtime.notify_frame_input(
            FILES_ID,
            FILES_SURFACE,
            &FrameInput::PointerDown { x, y, button, hit: None },
        );
        runtime.notify_frame_input(
            FILES_ID,
            FILES_SURFACE,
            &FrameInput::PointerUp { x, y, button, hit: None },
        );
    }
}

/// 向已聚焦的就地输入框提交一段文本。
fn type_text(runtime: &PluginRuntime, text: &str) {
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::Text { text: text.into(), composing: false, caret: None, candidates: None },
    );
}

/// 就地输入按 Enter（提交）。
fn press_enter(runtime: &PluginRuntime) {
    use atelyx_plugin_protocol::frame::{Key, KeyAction, Modifiers, NamedKey};
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::Key {
            key: Key::Named { name: NamedKey::Enter },
            action: KeyAction::Press,
            modifiers: Modifiers::default(),
            text: None,
        },
    );
}

async fn wait_picks(state: &HostState, count: usize) {
    wait_for(|| state.picks.lock().unwrap().len() >= count, "等取色请求超时").await;
}

async fn wait_creates(state: &HostState, count: usize) -> CreateEntryParams {
    wait_for(|| state.creates.lock().unwrap().len() >= count, "等新建请求超时").await;
    state.creates.lock().unwrap()[0].clone()
}

async fn wait_switches(state: &HostState, count: usize) {
    wait_for(|| state.switches.lock().unwrap().len() >= count, "等切换容器请求超时").await;
}

async fn wait_removes(state: &HostState, count: usize) {
    wait_for(|| state.removes.lock().unwrap().len() >= count, "等移除容器请求超时").await;
}

async fn wait_drags(state: &HostState, count: usize) {
    wait_for(|| state.drags.lock().unwrap().len() >= count, "等拖拽发起超时").await;
}

async fn wait_deletes(state: &HostState, count: usize) {
    wait_for(|| state.deletes.lock().unwrap().len() >= count, "等删除请求超时").await;
}

async fn wait_drag_ends(state: &HostState, count: usize) {
    wait_for(|| state.drag_ends.lock().unwrap().len() >= count, "等拖拽收尾超时").await;
}

async fn wait_pick_vaults(state: &HostState, count: usize) {
    wait_for(|| *state.pick_vaults.lock().unwrap() >= count, "等打开仓库请求超时").await;
}

async fn wait_for(mut ready: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn enable(runtime: &Arc<PluginRuntime>, id: &str) {
    let runtime = runtime.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || runtime.set_enabled(&id, true))
        .await
        .expect("启用任务不应 panic")
        .expect("启用预置插件");
}

async fn wait_running(runtime: &Arc<PluginRuntime>, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = runtime
            .list()
            .into_iter()
            .find(|plugin| plugin.id == id)
            .map(|plugin| plugin.status);
        if status == Some(PluginStatus::Running) {
            return;
        }
        assert!(Instant::now() < deadline, "插件未进入运行态：{status:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn resize(runtime: &PluginRuntime, plugin_id: &str, surface_id: &str) {
    runtime.notify_frame_input(
        plugin_id,
        surface_id,
        &FrameInput::Resize { width: PANEL_W, height: PANEL_H },
    );
}

/// 指定尺寸重排（窄面验证省略与徽标对齐）。
fn resize_to(runtime: &PluginRuntime, width: f32, height: f32) {
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::Resize { width, height },
    );
}

fn click_hit(runtime: &PluginRuntime, frame: &SurfaceFrame, id: &str) {
    let hit = frame
        .hits
        .iter()
        .find(|hit| hit.id == id)
        .unwrap_or_else(|| panic!("帧里没有命中区 {id}"));
    let x = hit.x + hit.w / 2.0;
    let y = hit.y + hit.h / 2.0;
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerDown { x, y, button: MouseButton::Left, hit: Some(id.into()) },
    );
    runtime.notify_frame_input(
        FILES_ID,
        FILES_SURFACE,
        &FrameInput::PointerUp { x, y, button: MouseButton::Left, hit: Some(id.into()) },
    );
}

async fn wait_frame(
    runtime: &Arc<PluginRuntime>,
    want: impl Fn(&SurfaceFrame) -> bool,
) -> Option<Arc<SurfaceFrame>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = runtime.surface_frame(FILES_ID, FILES_SURFACE) {
            if want(&frame) {
                return Some(frame);
            }
        }
        if Instant::now() >= deadline {
            return runtime.surface_frame(FILES_ID, FILES_SURFACE);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_menu(
    state: &HostState,
    want: impl Fn(&[Value]) -> bool,
) -> Option<Vec<Value>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let items = state.menu.lock().unwrap().clone();
        if want(&items) {
            return Some(items);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_patch(state: &HostState, want: impl Fn(&VaultUiPatch) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if state.patches.lock().unwrap().iter().any(|patch| want(patch)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_opens(state: &HostState, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if state.opens.lock().unwrap().len() >= count {
            return;
        }
        assert!(Instant::now() < deadline, "等打开请求超时");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_renames(state: &HostState, count: usize) -> Vec<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let renames = state.renames.lock().unwrap().clone();
        if renames.len() >= count {
            return renames;
        }
        assert!(Instant::now() < deadline, "等重命名请求超时");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 帧内是否含指定文本（指令里的文字内容）。
fn has_text(frame: &SurfaceFrame, needle: &str) -> bool {
    frame.commands.iter().any(|command| match command {
        DrawCommand::Text { content, .. } => content.contains(needle),
        _ => false,
    })
}

/// 空态：标题 / 说明按面宽居中，内容块（64 图标 + 两侧 16 间距 + 两行文字）在工具条下方的剩余区居中。
fn assert_empty_state_centered(frame: &SurfaceFrame, title: &str, desc: &str) {
    let size = frame.size.expect("整面帧必须带尺寸");
    // 空态排在工具条 + 1px 分隔线 + 容器行之下：容器行底边（无容器时取树顶部留白）
    let rule_bottom = frame
        .commands
        .iter()
        .find_map(|command| match command {
            DrawCommand::Rect { x, y, w, h, .. } if *x == 0.0 && *w >= size.w - 0.5 && *h <= 1.5 => {
                Some(*y + *h)
            }
            _ => None,
        })
        .unwrap_or(0.0);
    let tree_top = frame
        .hits
        .iter()
        .filter(|hit| hit.id.starts_with("switch:"))
        .map(|hit| hit.y + hit.h)
        .fold(rule_bottom + 6.0, f32::max);
    let content_top = tree_top + 12.0;
    let text = |wanted: &str| {
        frame.commands.iter().find_map(|command| match command {
            DrawCommand::Text { content, x, y, size, max_width, align, .. } if content == wanted => {
                Some((*x, *y, *size, *max_width, *align))
            }
            _ => None,
        }).unwrap_or_else(|| panic!("空态缺少文字：{wanted}"))
    };
    let title = text(title);
    let desc = text(desc);
    for (label, (x, _, _, max_width, align)) in [("标题", title), ("说明", desc)] {
        assert_eq!(max_width, Some(size.w), "空态{label}应按面宽对齐");
        assert_eq!(align, Some(TextAlign::Center), "空态{label}应居中");
        assert!(x.abs() < 0.5, "空态{label}对齐盒的左缘应在面左缘");
    }
    let block = 64.0 + 16.0 + title.2 * 1.2 + 16.0 + desc.2 * 1.2;
    let avail = size.h - content_top;
    let top = content_top + (avail - block) / 2.0;
    assert!((title.1 - (top + 64.0 + 16.0)).abs() < 0.5, "空态标题应垂直居中");
    let desc_y = top + 64.0 + 16.0 + title.2 * 1.2 + 16.0;
    assert!((desc.1 - desc_y).abs() < 0.5, "空态说明应垂直居中");
    assert!(((top + desc_y + desc.2 * 1.2) / 2.0 - (size.h + content_top) / 2.0).abs() < 0.5, "空态内容块应垂直居中");
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
