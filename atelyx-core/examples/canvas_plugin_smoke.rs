//! 画布插件无界面冒烟：经真实 `PluginRuntime` 加载、挂面、取帧，覆盖载入渲染、右键新建卡、
//! 卡片拖动与防抖落盘、撤销、视口（滚轮方向 / 世界坐标排版与整体倍乘 / 改尺寸保持视口）、
//! 面板头部脏标记与停用收尾。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use atelyx_core::plugin_runtime::{
    FilesResult, FrameInput, HostRequest, MeasureTextResult, Modifiers, MouseButton,
    PanelHeaderParams, PluginRuntime, PluginStatus, SurfaceKind, VaultFileEntry,
};
use atelyx_plugin_protocol::frame::SurfaceFrame;
use atelyx_plugin_protocol::method::{
    CollabMessageParams, FontSizes, MenuItem, MenuOpenParams, Radii, ThemeColors, ThemeResult,
};
use atelyx_plugin_protocol::paint::{DrawCommand, PaintColor, TextAlign, TextLayout};

const CANVAS_ID: &str = "atelyx.canvas";
const SURFACE: &str = "canvas";
const PANEL_W: f32 = 900.0;
const PANEL_H: f32 = 600.0;
/// 视图初始 pan = (40,40)、缩放 1；适配后单卡居中，卡片左上角落在 (170,80)。
const CARD_LEFT: f32 = 100.0;
const CARD_TOP: f32 = 100.0;
const CARD_W: f32 = 360.0;
const CARD_H: f32 = 240.0;
/// 卡头字号基础值（世界单位）：绘制字号 = 基础 × zoom。
const CARD_TITLE_SIZE: f32 = 12.0;
/// 主题正文字号基础值（世界单位）：绘制字号 = 基础 × zoom。
const BODY_SIZE: f32 = 14.0;
/// 滚动条槽宽（世界单位，SDK 与画布共同口径）：屏幕宽 = 槽宽 × zoom。
const SCROLLBAR_SLOT_W: f32 = 8.0;
/// 卡内容矩形右侧内缩（世界单位）：滚动条槽右缘距卡右缘的距离按 zoom 换算。
const CARD_CONTENT_INSET: f32 = 1.0;
/// 卡内容左右内边距（世界单位）。
const CARD_CONTENT_PAD: f32 = 8.0;
/// 卡片圆角半径（世界单位，随缩放变化）：内容下沿据此避开圆角。
const CARD_CORNER_RADIUS: f32 = 10.0;
/// 卡头世界高度与卡头到内容矩形的世界间距。
const CARD_HEAD_H: f32 = 30.0;
const CARD_BODY_GAP: f32 = 4.0;
/// 对话卡片尺寸（世界单位）。
const CHAT_W: f32 = 520.0;
const CHAT_H: f32 = 560.0;
/// 标题实际可用宽度 = 卡宽 - 左右内边距与图标位（见画布世界层排版）。
const TITLE_LIMIT: f32 = CARD_W - 36.0;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let source = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/debug/preset-plugins"));
    let root = atelyx_core::scratch::TempDir::new("canvas-smoke");
    copy_tree(&source, &root).expect("复制冒烟插件目录");
    let runtime = PluginRuntime::with_host_version(root.to_path_buf(), "0.6.5");
    runtime.scan();

    let vault = atelyx_core::scratch::TempDir::new("canvas-vault");
    std::fs::write(vault.join("demo.atlx"), seed_canvas()).expect("写初始画布");
    std::fs::write(vault.join("chat.atlx"), seed_chat_canvas()).expect("写对话画布");

    let state = Arc::new(HostState::default());
    serve(runtime.clone(), state.clone(), vault.to_path_buf());

    if !matches!(runtime.list().into_iter().find(|p| p.id == CANVAS_ID).map(|p| p.status), Some(PluginStatus::Running)) {
        enable(&runtime, CANVAS_ID).await;
    }
    wait_running(&runtime, CANVAS_ID).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    resize(&runtime);
    tokio::time::sleep(Duration::from_millis(50)).await;
    runtime.notify_surface_attached(CANVAS_ID, SURFACE, SurfaceKind::Panel);
    for _ in 0..20 {
        resize(&runtime);
        runtime.on_open_files_changed();
        tokio::time::sleep(Duration::from_millis(50)).await;
        if runtime
            .surface_frame(CANVAS_ID, SURFACE)
            .is_some_and(|frame| frame.size.is_some_and(|size| (size.w - PANEL_W).abs() < 0.5))
        {
            break;
        }
    }

    // 未打开画布：插件必须提交空态帧，而不是让宿主留着上一张帧
    let empty = wait_frame(&runtime, |frame| has_text(frame, "打开画布"))
        .await
        .expect("未打开画布时应提交空态帧");
    assert_empty_state_centered(&empty, "打开画布", "在文件面板或搜索面板单击一个 .atlx 画布开始编辑。");
    println!("空态：{} 条指令", empty.commands.len());

    // 宿主打开画布后才建会话
    *state.open_files.lock().unwrap() = vec!["demo.atlx".into()];
    runtime.on_open_files_changed();
    // 新会话从默认视口起算：再报一次尺寸，卡片浮层与命中才按面尺寸铺开
    resize(&runtime);

    // 首帧：网格底 + 卡片标题画进主帧（浮层随视口铺开后才有命中区）
    let frame = wait_frame(&runtime, |frame| has_text_prefix(frame, "现有卡") && !frame.hits.is_empty())
        .await
        .unwrap_or_else(|| panic!("载入后应出画布帧；当前无帧"));
    assert!(frame.hits.iter().any(|hit| !hit.id.is_empty()), "应有可命中区域");
    // 长标题按卡宽省略：世界层指令没有逐项裁剪，越界会盖住控件或溢出卡框
    let title = title_text(&frame).expect("应画出卡片标题");
    assert!(title.ends_with('…'), "长标题应省略：{title}");
    assert!(
        text_width(&title, CARD_TITLE_SIZE) <= TITLE_LIMIT + 0.5,
        "标题不应越出卡框：{} > {TITLE_LIMIT}",
        text_width(&title, CARD_TITLE_SIZE)
    );
    assert_eq!(
        title_size(&frame),
        Some(CARD_TITLE_SIZE),
        "载入时视图缩放为 1，卡头字号应等于世界基础值"
    );
    println!("载入：{} 条指令 / {} 个命中区", frame.commands.len(), frame.hits.len());

    // 协作在场：远端名签用宿主名册色（不看载荷自报色）；断链事件后立即摘除
    runtime.notify_collab_message(
        CANVAS_ID,
        &CollabMessageParams {
            channel: "canvas-presence".to_string(),
            payload: json!({
                "path": "demo.atlx",
                "present": true,
                "selected": ["n1"],
                "color": "#abcdef"
            }),
            peer_id: "p1".to_string(),
            peer_name: Some("林".to_string()),
            peer_color: Some("#123456".to_string()),
        },
    );
    let remote = wait_frame(&runtime, |frame| has_text_containing(frame, "林"))
        .await
        .expect("远端在场后应画出名签");
    assert!(
        remote.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Rect { stroke: Some(PaintColor::Rgba { rgba }), .. }
                if rgba == &[0x12, 0x34, 0x56, 255]
        )),
        "远端选区描边应取名册身份色"
    );
    runtime.notify_plugin_event(
        CANVAS_ID,
        atelyx_plugin_protocol::event::name::SPACE_PEERS_CHANGED,
        &json!({}),
    );
    wait_frame(&runtime, |frame| !has_text_containing(frame, "林"))
        .await
        .expect("断链后远端名签应立即消失");
    println!("协作在场身份色与断链清理：通过");

    // 长正文在卡内滚动：正文行全落在内容矩形内，滚动条槽右缘贴卡右缘
    assert_eq!(
        body_command(&frame).map(|body| body.3),
        Some(BODY_SIZE),
        "正文字号固定为屏幕像素"
    );
    assert_card_text_within_card(&frame, CARD_W, CARD_H);
    assert_scrollbar_hugs_card(&frame, CARD_W, CARD_H, true);
    println!("卡内正文裁剪与滚动条贴边：通过");

    // 右键空白 → 宿主弹菜单 → 选「新建文本卡」：画布内容变脏
    right_click_at(&runtime, 700.0, 200.0);
    wait_menu(&state, 1).await;
    runtime.notify_menu_closed(CANVAS_ID, Some("canvas-menu-new-text".to_string()));
    wait_written(&state, 1).await;
    let two_cards: Value = serde_json::from_str(&written_at(&state, 0)).expect("写盘内容是画布 JSON");
    assert_eq!(two_cards["nodes"].as_array().map(Vec::len), Some(2), "新建卡后应有 2 张卡");
    let dirty = wait_header(&state, |params| {
        params.header.as_ref().is_some_and(|header| header.dirty)
    })
    .await;
    assert!(dirty.is_some(), "新建卡后应声明为脏头部");

    // 拖动第一张卡：从卡头按到右下移 100×60
    let start = (320.0, 210.0);
    let end = (start.0 + 100.0, start.1 + 60.0);
    pointer(&runtime, FrameInput::PointerDown { x: start.0, y: start.1, button: MouseButton::Left, hit: None });
    pointer(&runtime, FrameInput::PointerMove { x: end.0, y: end.1, hit: None });
    pointer(&runtime, FrameInput::PointerUp { x: end.0, y: end.1, button: MouseButton::Left, hit: None });
    wait_written(&state, 2).await;
    let moved: Value = serde_json::from_str(&written_at(&state, 1)).expect("写盘内容是画布 JSON");
    let x = moved["nodes"][0]["x"].as_f64().unwrap_or(0.0);
    assert!(x > CARD_LEFT as f64 + 50.0, "拖动后 x 应移动：{x}");
    assert!(
        wait_header(&state, |params| !params.header.as_ref().is_some_and(|header| header.dirty))
            .await
            .is_some(),
        "落盘完成后应声明为干净头部"
    );
    println!("新建与拖动落盘：通过");

    // 撤销：宿主把画布作用域命令转发为事件，插件回到拖动前的位置并再次落盘
    runtime.notify_plugin_event(
        CANVAS_ID,
        atelyx_core::plugin_runtime::event_name::CANVAS_COMMAND,
        &atelyx_core::plugin_runtime::CanvasCommandParams { command: "undo".to_string() },
    );
    wait_written(&state, 3).await;
    let undone: Value = serde_json::from_str(&written_at(&state, 2)).expect("撤销写盘");
    assert!(
        undone["nodes"][0]["x"].as_f64().unwrap_or(999.0) <= CARD_LEFT as f64 + 0.5,
        "撤销后应回到拖动前的 x"
    );
    println!("撤销：通过");

    // 拖拽途中 Delete：快捷键被忽略，既不删卡也不打断拖拽；且拖动过程本身不触发保存
    let before_delete = state.written.lock().unwrap().len();
    let drag_start = (320.0, 210.0);
    pointer(&runtime, FrameInput::PointerDown { x: drag_start.0, y: drag_start.1, button: MouseButton::Left, hit: None });
    pointer(&runtime, FrameInput::PointerMove { x: drag_start.0 + 100.0, y: drag_start.1 + 60.0, hit: None });
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        state.written.lock().unwrap().len(),
        before_delete,
        "拖动在途不应触发保存"
    );
    runtime.notify_plugin_event(
        CANVAS_ID,
        atelyx_core::plugin_runtime::event_name::CANVAS_COMMAND,
        &atelyx_core::plugin_runtime::CanvasCommandParams { command: "delete".to_string() },
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        state.written.lock().unwrap().len(),
        before_delete,
        "拖拽中的 Delete 应被忽略且不落盘"
    );
    pointer(&runtime, FrameInput::PointerUp { x: drag_start.0 + 100.0, y: drag_start.1 + 60.0, button: MouseButton::Left, hit: None });
    wait_written(&state, before_delete + 1).await;
    let after_drag_delete: Value =
        serde_json::from_str(&written_at(&state, before_delete)).expect("拖拽后写盘");
    assert_eq!(
        after_drag_delete["nodes"].as_array().map(Vec::len),
        Some(2),
        "拖拽中的 Delete 不应删卡"
    );
    assert!(
        after_drag_delete["nodes"][0]["x"].as_f64().unwrap_or(0.0) > CARD_LEFT as f64 + 50.0,
        "拖拽应继续生效"
    );
    println!("拖拽中 Delete 忽略：通过");

    // 滚轮方向：协议增量正 = 向下；向上滚（负增量）应放大。字号按世界基础值 × zoom 绘制。
    let before_zoom = zoom_percent(&frame).expect("工具条应显示缩放百分比");
    wheel(&runtime, -40.0, false);
    let zoomed = wait_frame(&runtime, |frame| {
        zoom_percent(frame).is_some_and(|pct| pct > before_zoom)
    })
    .await
    .expect("向上滚应放大");
    let zoom_factor = card_zoom(&zoomed);
    assert!(
        title_size(&zoomed)
            .is_some_and(|size| (size / zoom_factor - CARD_TITLE_SIZE).abs() < 0.05),
        "放大后卡头字号应为基础字号 × zoom：当前 {:?} / {zoom_factor}",
        title_size(&zoomed)
    );
    println!("滚轮方向与字号倍乘：通过（{before_zoom}% → {}%）", zoom_percent(&zoomed).unwrap_or(0));

    // 改面板尺寸不再重置视口：既有缩放应原样保留
    let kept = zoom_percent(&zoomed).unwrap_or(0);
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Resize {
            width: 1000.0,
            height: 600.0,
        },
    );
    let resized = wait_frame_at_width(&runtime, 1000.0, |frame| {
        zoom_percent(frame).is_some_and(|pct| pct == kept)
    })
    .await
    .expect("改尺寸后应保持既有缩放");
    let resized_zoom = card_zoom(&resized);
    assert!(
        title_size(&resized).is_some_and(|size| (size / resized_zoom - CARD_TITLE_SIZE).abs() < 0.05),
        "改尺寸后字号仍应为基础字号 × zoom"
    );
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Resize {
            width: PANEL_W,
            height: PANEL_H,
        },
    );
    let restored = wait_frame(&runtime, |frame| zoom_percent(frame) == Some(kept))
        .await
        .expect("尺寸还原后缩放仍应保留");
    println!("改尺寸保持视口：通过（{kept}%）");

    // 世界坐标排版 + 输出整体倍乘：0.4 / 0.6 / 1.0 / 1.5 / 2.0 下正文逐行内容一致、
    // 字号与内容偏移都是基础值的同一倍率（换行位置不随缩放跳动）。
    let (anchor_x, anchor_y) = title_point(&restored).expect("应有卡头标题");
    let (anchor_x, anchor_y) = (anchor_x + 2.0, anchor_y + 2.0);
    let mut current = restored;
    let mut baseline: Option<Vec<String>> = None;
    let mut ratios: Vec<(f32, f32, f32)> = Vec::new();
    for target in [40, 60, 100, 150, 200] {
        current = zoom_to(&runtime, &current, target, anchor_x, anchor_y).await;
        let lines = card_text_lines(&current, CARD_W, CARD_H);
        assert!(!lines.is_empty(), "{target}% 时卡内应画出正文");
        match &baseline {
            Some(expected) => {
                assert_eq!(&lines, expected, "{target}% 下正文换行位置与逐行内容应完全一致")
            }
            None => baseline = Some(lines),
        }
        let (cx, cy, cw, _) = card_bounds(&current, CARD_W, CARD_H).expect("应找到卡片背景");
        let zoom = cw / CARD_W;
        let body = body_command(&current).expect("文本卡应画出正文");
        assert!(
            (body.3 / zoom - BODY_SIZE).abs() < 0.05,
            "{target}% 下正文字号应为 {BODY_SIZE} × {zoom}：{}",
            body.3
        );
        ratios.push(((body.1 - cx) / zoom, (body.2 - cy) / zoom, body.3 / zoom));
        assert_card_text_within_card(&current, CARD_W, CARD_H);
        assert_scrollbar_hugs_card(&current, CARD_W, CARD_H, true);
    }
    let reference = ratios[0];
    for (x, y, size) in &ratios {
        assert!((x - reference.0).abs() < 0.05, "正文横向偏移比例不一致：{x} vs {}", reference.0);
        assert!((y - reference.1).abs() < 0.05, "正文纵向偏移比例不一致：{y} vs {}", reference.1);
        assert!((size - reference.2).abs() < 0.05, "正文字号比例不一致：{size} vs {}", reference.2);
    }
    println!("世界坐标排版与等比倍乘：通过（基准 14 / 偏移 {:?}）", reference);

    // 底部多余空白：各档缩放下滚到底时最后一行下缘贴齐卡内容区下缘（仅让出圆角），且末行完整可见。
    for target in [50, 100, 200] {
        current = zoom_to(&runtime, &current, target, anchor_x, anchor_y).await;
        let (cx, cy, cw, ch) = card_bounds(&current, CARD_W, CARD_H).expect("应找到卡片背景");
        wheel_at(&runtime, cx + cw / 2.0, cy + ch / 2.0, 10000.0);
        let at_bottom = wait_frame(&runtime, |frame| has_text_containing(frame, "正文段落 59"))
            .await
            .expect("滚到底应看到最后一段正文");
        let zoom = cw / CARD_W;
        let body_top = cy + (CARD_HEAD_H + CARD_BODY_GAP) * zoom;
        let body_bottom = cy + ch - CARD_CORNER_RADIUS * zoom;
        let (last_bottom, last_line) =
            last_content_line_bottom(&at_bottom, CARD_W, CARD_H).expect("滚到底应画出末行");
        assert!(
            (last_bottom - body_bottom).abs() <= 1.5,
            "{target}% 末行下缘应与卡内容区下缘一致：{last_bottom} vs {body_bottom}"
        );
        assert!(
            last_bottom - last_line >= body_top - 0.5,
            "{target}% 末行应完整落在卡内容区内，不能被视口切掉一行"
        );
        println!("底部无多余空白：通过（{target}% / 末行下缘 {last_bottom} / 内容下缘 {body_bottom}）");
    }

    // 低缩放（30%）：内容不越出卡内容区、文字不被横向裁半，卡头仍按比例绘制而不是占位。
    current = zoom_to(&runtime, &current, 30, anchor_x, anchor_y).await;
    assert_card_text_within_card(&current, CARD_W, CARD_H);
    let low_zoom = card_zoom(&current);
    assert!(
        title_size(&current).is_some_and(|size| (size / low_zoom - CARD_TITLE_SIZE).abs() < 0.05),
        "低缩放仍应绘制卡头标题（不切占位形态）"
    );
    println!("低缩放不越界：通过（{}%）", zoom_percent(&current).unwrap_or(0));

    let restored = zoom_to(&runtime, &current, kept, anchor_x, anchor_y).await;

    // Ctrl+滚轮：正增量（向下）应把画面向上推（pan.1 减小，卡头屏幕上移）
    let title_y = text_y(&restored).expect("应有卡头标题");
    wheel(&runtime, 40.0, true);
    let panned = wait_frame(&runtime, |frame| {
        text_y(frame).is_some_and(|y| y < title_y - 20.0)
    })
    .await
    .expect("Ctrl+滚轮向下应向上平移画面");
    assert_eq!(zoom_percent(&panned), Some(kept), "Ctrl+滚轮不应改变缩放");
    println!("滚轮平移方向：通过");

    // 小地图矩形内的滚轮被消费：不应改变缩放
    let minimap_zoom = zoom_percent(&panned).unwrap_or(0);
    let minimap_mods = Modifiers::default();
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Wheel {
            x: 800.0,
            y: 550.0,
            delta_x: 0.0,
            delta_y: 1000.0,
            modifiers: minimap_mods,
            hit: None,
        },
    );
    // 再走一次 Ctrl+滚轮（只平移不缩放）：用平移产生的新帧证明小地图上的那次已被丢弃
    let minimap_title_y = text_y(&panned).expect("应有卡头标题");
    wheel(&runtime, 40.0, true);
    let after_minimap = wait_frame(&runtime, |frame| {
        zoom_percent(frame).is_some_and(|pct| pct == minimap_zoom)
            && text_y(frame).is_some_and(|y| y < minimap_title_y - 20.0)
    })
    .await
    .expect("小地图上的滚轮应被消费（缩放未变且随后平移生效）");
    assert_eq!(
        zoom_percent(&after_minimap),
        Some(minimap_zoom),
        "小地图上的滚轮应被消费"
    );
    println!("小地图滚轮消费：通过");

    // 切到对话画布：恢复 messages 的 steps / 附件 / 引用胶囊 / 压缩标记与「模型 · 推理等级」标签
    *state.open_files.lock().unwrap() = vec!["chat.atlx".into()];
    runtime.on_open_files_changed();
    resize(&runtime);
    let chat = match wait_frame(&runtime, |frame| {
        has_text(frame, "思考过程")
            && has_text_prefix(frame, "上下文已压缩")
            && has_text(frame, "gpt-x · 中")
    })
    .await
    {
        Some(frame) => frame,
        None => {
            if let Some(frame) = runtime.surface_frame(CANVAS_ID, SURFACE) {
                eprintln!("当前帧缩放：{:?}", zoom_percent(&frame));
                for command in &frame.commands {
                    if let DrawCommand::Text { content, .. } = command {
                        eprintln!("TEXT：{content}");
                    }
                }
            }
            panic!("对话卡应恢复步骤摘要、压缩标记与模型·推理等级");
        }
    };
    assert!(
        has_text_containing(&chat, "说明"),
        "引用胶囊应渲染引用标签（当前指令里没有含「说明」的文字）"
    );
    assert!(
        chat.commands.iter().any(|command| matches!(
            command,
            DrawCommand::Text { content, .. } if content.contains("先看看目录")
        )),
        "思考块折叠摘要应可见"
    );
    // 消息区在卡内滚动：继续往下滚，露出纯工具轮次（正文为空也要有工具块）
    let scrolled = {
        let mut found = None;
        for _ in 0..6 {
            wheel_at(&runtime, 300.0, 320.0, 120.0);
            tokio::time::sleep(Duration::from_millis(40)).await;
            if let Some(frame) = runtime.surface_frame(CANVAS_ID, SURFACE) {
                if has_text(&frame, "读取文件") {
                    found = Some((*frame).clone());
                    break;
                }
            }
        }
        found
    }
    .expect("纯工具轮次应在消息区滚动后可见");
    assert!(
        has_text(&scrolled, "最终结论"),
        "滚动到底应看到后续正文"
    );
    // 卡内全部文字（消息 / 输入区 / 引用）落在内容矩形内；缩略图宽度收敛到内容宽
    assert_card_text_within_card(&chat, CHAT_W, CHAT_H);
    assert_card_text_within_card(&scrolled, CHAT_W, CHAT_H);
    assert_scrollbar_hugs_card(&chat, CHAT_W, CHAT_H, false);
    assert_scrollbar_hugs_card(&scrolled, CHAT_W, CHAT_H, false);
    assert_thumb_within_content(&chat, CHAT_W, CHAT_H);
    println!("对话卡信息恢复：通过（{} 条指令）", chat.commands.len());

    // 表格 / 媒体卡：引用缺失也要出提示，不留空白卡
    assert!(
        has_text(&chat, "引用表格缺失"),
        "表格引用非表时应提示「引用表格缺失」"
    );
    assert!(
        has_text(&chat, "附件缺失"),
        "媒体附件缺失时应提示「附件缺失」"
    );
    println!("媒体 / 表格卡提示：通过");

    // 对话卡两处「在 AI 面板打开」入口：选中卡后工具条出现按钮，右键菜单同项，
    // 两条入口都发 ai.chat.openPanel 给宿主；标签策略（仅激活、不新建）由壳侧单测覆盖，
    // 冒烟只验证请求确实发出（面板会话真源与画布卡分离，不定位会话）
    let (title_x, title_y) = chat_title_point(&chat).expect("对话卡应有卡头标题");
    let (click_x, click_y) = (title_x + 2.0, title_y + 2.0);
    assert!(
        !chat.hits.iter().any(|hit| hit.id == "canvas-open-panel"),
        "未选中对话卡时不应出现该入口"
    );
    pointer(&runtime, FrameInput::PointerDown { x: click_x, y: click_y, button: MouseButton::Left, hit: None });
    pointer(&runtime, FrameInput::PointerUp { x: click_x, y: click_y, button: MouseButton::Left, hit: None });
    wait_frame(&runtime, |frame| {
        frame.hits.iter().any(|hit| hit.id == "canvas-open-panel")
    })
    .await
    .expect("选中对话卡后工具条应出现「在 AI 面板打开」");
    right_click_at(&runtime, click_x, click_y);
    let menu = wait_menu(&state, 2).await;
    assert!(
        menu.items.iter().any(|item| matches!(
            item,
            MenuItem::Action { id, label, .. } if id == "canvas-menu-open-panel" && label == "在 AI 面板打开"
        )),
        "对话卡右键菜单应含「在 AI 面板打开」：{:?}",
        menu.items
    );
    runtime.notify_menu_closed(CANVAS_ID, Some("canvas-menu-open-panel".to_string()));
    wait_open_panel(&state, 1).await;
    pointer(
        &runtime,
        FrameInput::PointerDown {
            x: 24.0,
            y: 18.0,
            button: MouseButton::Left,
            hit: Some("canvas-open-panel".to_string()),
        },
    );
    pointer(
        &runtime,
        FrameInput::PointerUp {
            x: 24.0,
            y: 18.0,
            button: MouseButton::Left,
            hit: Some("canvas-open-panel".to_string()),
        },
    );
    wait_open_panel(&state, 2).await;
    println!("对话卡在 AI 面板打开入口：通过");

    disable(&runtime, CANVAS_ID).await;
    let status = runtime
        .list()
        .into_iter()
        .find(|plugin| plugin.id == CANVAS_ID)
        .map(|plugin| plugin.status);
    assert!(matches!(status, Some(PluginStatus::Stopped) | None), "停用后应退出运行态");
    println!("停用：通过");
    runtime.shutdown();
}

fn written_at(state: &HostState, index: usize) -> String {
    state.written.lock().unwrap()[index].clone()
}

fn seed_canvas() -> String {
    // 标题刻意超过卡宽：覆盖世界层单行省略（世界层指令没有逐项裁剪）
    let title = "现有卡 · 一个用于验证标题在卡头内省略而不是溢出卡框的很长很长的标题名称";
    json!({
        "schema": "atelyx-canvas/v1",
        "id": "cv-smoke",
        "title": "demo",
        "nodes": [{
            "id": "n1",
            "type": "text",
            "x": CARD_LEFT,
            "y": CARD_TOP,
            "width": CARD_W,
            "height": CARD_H,
            "data": { "title": title, "bodyMd": seed_body() }
        }],
        "edges": [],
        "createdAt": 0,
        "updatedAt": 0
    })
    .to_string()
}

/// 长正文：首行供字号断言定位，其后多段让正文超出卡内视口（滚动条与视口裁剪才有意义）。
fn seed_body() -> String {
    let mut body = String::from("正文");
    for index in 0..60 {
        body.push_str(&format!(
            "\n\n正文段落 {index}：用于验证长正文在卡内换行、滚动与裁剪。"
        ));
    }
    body
}

/// 对话画布：覆盖消息步骤 / 空正文工具轮次 / 附件缩略图 / 引用胶囊 / 压缩标记，
/// 以及引用缺失的表格与媒体卡提示。
fn seed_chat_canvas() -> String {
    json!({
        "schema": "atelyx-canvas/v1",
        "id": "cv-chat",
        "title": "chat",
        "nodes": [
            {
                "id": "c1",
                "type": "conversation",
                "x": 10,
                "y": 10,
                "width": 520,
                "height": 560,
                "data": {
                    "title": "对话",
                    "model": "gpt-x",
                    "reasoningEffort": "medium",
                    "compaction": {
                        "upToMessageId": "m1",
                        "messageCount": 1,
                        "summary": "压缩摘要"
                    },
                    "messages": [
                        {
                            "id": "m1",
                            "role": "user",
                            "content": "看看附件",
                            "displayContent": "看看附件 [笔记：说明]",
                            "refs": [{ "path": "note.md", "label": "说明", "refKind": "note" }],
                            "attachments": [
                                { "kind": "image", "file": "missing1.png" },
                                { "kind": "image", "file": "missing2.png" },
                                { "kind": "image", "file": "missing3.png" },
                                { "kind": "image", "file": "missing4.png" },
                                { "kind": "image", "file": "missing5.png" }
                            ]
                        },
                        {
                            "id": "m2",
                            "role": "assistant",
                            "content": "",
                            "steps": [
                                { "kind": "reasoning", "text": "先看看目录" },
                                {
                                    "kind": "tool",
                                    "run": {
                                        "name": "read_file",
                                        "status": "done",
                                        "argsSummary": "a.md",
                                        "resultSummary": "已完成",
                                        "args": { "path": "a.md" },
                                        "result": "ok"
                                    }
                                }
                            ]
                        },
                        { "id": "m3", "role": "assistant", "content": "最终结论" }
                    ]
                }
            },
            {
                "id": "t1",
                "type": "table",
                "x": 560,
                "y": 10,
                "width": 220,
                "height": 140,
                "data": { "title": "表格", "file": "missing.csv" }
            },
            {
                "id": "i1",
                "type": "media",
                "x": 560,
                "y": 180,
                "width": 220,
                "height": 160,
                "data": { "title": "图片", "file": "missing.png" }
            }
        ],
        "edges": [],
        "createdAt": 0,
        "updatedAt": 0
    })
    .to_string()
}

/// 冒烟宿主：记录写盘内容、头部声明与菜单请求，并应答插件的能力调用。
#[derive(Default)]
struct HostState {
    written: Mutex<Vec<String>>,
    headers: Mutex<Vec<PanelHeaderParams>>,
    menus: Mutex<Vec<MenuOpenParams>>,
    open_panels: Mutex<usize>,
    unhandled: Mutex<Vec<String>>,
    vault: Mutex<PathBuf>,
    /// 宿主当前打开的文件：先空后给 `demo.atlx`，用来覆盖「未打开画布」空态。
    open_files: Mutex<Vec<String>>,
}

fn serve(runtime: Arc<PluginRuntime>, state: Arc<HostState>, vault: PathBuf) {
    *state.vault.lock().unwrap() = vault;
    tokio::spawn(async move {
        while let Some(request) = runtime.recv_host_request().await {
            match request {
                HostRequest::MeasureBatch { request_id, params, .. } => {
                    let sizes: Vec<MeasureTextResult> =
                        params.items.iter().map(|item| measure(item)).collect();
                    reply(&runtime, request_id, json!({ "sizes": sizes }));
                }
                HostRequest::QueryTheme { request_id, .. } => {
                    reply(&runtime, request_id, serde_json::to_value(theme()).unwrap());
                }
                HostRequest::QueryFonts { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "families": ["Sans Serif"] }));
                }
                HostRequest::VaultReadText { request_id, path, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    match std::fs::read_to_string(&target) {
                        Ok(content) => reply(&runtime, request_id, json!({ "content": content })),
                        Err(error) => runtime.resolve_shell_call(request_id, Err(error.to_string())),
                    }
                }
                HostRequest::VaultReadBinary { request_id, .. } => {
                    // 冒烟里附件刻意缺失：立刻回错误，引用解析才能继续（不然载入会一直等）
                    runtime.resolve_shell_call(request_id, Err("附件不存在".to_string()));
                }
                HostRequest::VaultWriteText { request_id, path, content, .. } => {
                    let target = state.vault.lock().unwrap().join(path);
                    state.written.lock().unwrap().push(content.clone());
                    let result = std::fs::write(&target, content).map_err(|e| e.to_string());
                    match result {
                        Ok(()) => reply(&runtime, request_id, json!({})),
                        Err(error) => runtime.resolve_shell_call(request_id, Err(error)),
                    }
                }
                HostRequest::HistoryRecord { request_id, .. } => {
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::PanelHeader { request_id, params, .. } => {
                    state.headers.lock().unwrap().push(params);
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::EditorOpenFiles { request_id, .. } => {
                    let paths = state.open_files.lock().unwrap().clone();
                    reply(&runtime, request_id, json!({ "paths": paths }));
                }
                HostRequest::AiAgents { request_id, .. } => {
                    reply(
                        &runtime,
                        request_id,
                        json!({ "agents": [{ "id": "chat", "name": "对话", "builtin": true }] }),
                    );
                }
                HostRequest::AiModels { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "providers": [] }));
                }
                HostRequest::VaultListDirs { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "dirs": ["."] }));
                }
                HostRequest::VaultFiles { request_id, .. } => {
                    reply(
                        &runtime,
                        request_id,
                        serde_json::to_value(FilesResult {
                            files: vec![VaultFileEntry {
                                path: "demo.atlx".to_string(),
                                name: "demo.atlx".to_string(),
                                icon: "shapes".to_string(),
                                openable: true,
                            }],
                        })
                        .unwrap(),
                    );
                }
                HostRequest::MenuOpen { request_id, params, .. } => {
                    state.menus.lock().unwrap().push(params);
                    // 菜单走界面服务通道：用错回执通道会让 open_menu 永远等不到结果
                    runtime.resolve_ui_call(request_id, Ok(()));
                }
                HostRequest::ChatOpenPanel { request_id, .. } => {
                    *state.open_panels.lock().unwrap() += 1;
                    reply(&runtime, request_id, json!({}));
                }
                HostRequest::SpaceRoom { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "kind": "local", "connected": false }));
                }
                HostRequest::CollabPeers { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "connected": false }));
                }
                HostRequest::CollabSend { request_id, .. } => {
                    reply(&runtime, request_id, json!({ "queued": false }));
                }
                other => {
                    eprintln!("未处理的宿主请求：{other:?}");
                    state.unhandled.lock().unwrap().push(format!("{other:?}"));
                }
            }
        }
    });
}

fn measure(item: &atelyx_plugin_protocol::method::MeasureItem) -> MeasureTextResult {
    let layout = item.layout.unwrap_or(TextLayout { line_height: None, wrap: None, shaping: None });
    let factor = layout.line_height.unwrap_or(1.2);
    let line_height = item.size * factor;
    let wrap_w = item.max_width.unwrap_or(f32::MAX);
    let mut segments = Vec::new();
    for segment in item.text.split('\n') {
        let mut current = String::new();
        let mut width = 0.0f32;
        for ch in segment.chars() {
            let advance = item.size * if ch.is_ascii() { 0.55 } else { 1.0 };
            if !current.is_empty() && width + advance > wrap_w {
                segments.push(atelyx_plugin_protocol::method::MeasureSegment {
                    text: std::mem::take(&mut current),
                    width,
                });
                width = 0.0;
            }
            current.push(ch);
            width += advance;
        }
        segments.push(atelyx_plugin_protocol::method::MeasureSegment { text: current, width });
    }
    let width = segments.iter().map(|seg| seg.width).fold(0.0, f32::max);
    let lines = segments.len().max(1) as u32;
    MeasureTextResult { width, height: line_height * lines as f32, lines, segments: Some(segments) }
}

fn theme() -> ThemeResult {
    ThemeResult {
        colors: ThemeColors {
            text: "#1f2328".into(),
            text_secondary: "#57606a".into(),
            text_muted: "#8c959f".into(),
            accent: "#e0a94e".into(),
            accent_soft: "#e0a94e24".into(),
            danger: "#e8776c".into(),
            danger_soft: "#e8776c24".into(),
            success: "#5fbf8b".into(),
            success_soft: "#5fbf8b24".into(),
            warning: "#efc75e".into(),
            warning_soft: "#efc75e24".into(),
            surface: "#1b1e23".into(),
            surface_muted: "#131418".into(),
            surface_sunken: "#0c0d10".into(),
            hover: "#23272e".into(),
            border: "#2e333a".into(),
            border_strong: "#383d44".into(),
            background: "#131418".into(),
            border_subtle: "#252a31".into(),
            selection: "#e0a94e40".into(),
            scrim: "#00000099".into(),
            transparent: "#00000000".into(),
        },
        font_sizes: FontSizes { micro: 11.0, caption: 12.0, ui: 13.0, body: 14.0, h2: 16.0, h1: 20.0 },
        radii: Radii { xs: 4.0, sm: 6.0, md: 10.0 },
        event_colors: Vec::new(),
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

async fn disable(runtime: &Arc<PluginRuntime>, id: &str) {
    let runtime = runtime.clone();
    let id = id.to_string();
    tokio::task::spawn_blocking(move || runtime.set_enabled(&id, false))
        .await
        .expect("停用任务不应 panic")
        .expect("停用预置插件");
}

fn resize(runtime: &PluginRuntime) {
    runtime.notify_frame_input(CANVAS_ID, SURFACE, &FrameInput::Resize { width: PANEL_W, height: PANEL_H });
}

fn pointer(runtime: &PluginRuntime, input: FrameInput) {
    runtime.notify_frame_input(CANVAS_ID, SURFACE, &input);
}

fn right_click_at(runtime: &PluginRuntime, x: f32, y: f32) {
    for input in [
        FrameInput::PointerDown { x, y, button: MouseButton::Right, hit: None },
        FrameInput::PointerUp { x, y, button: MouseButton::Right, hit: None },
    ] {
        runtime.notify_frame_input(CANVAS_ID, SURFACE, &input);
    }
}

fn reply(runtime: &PluginRuntime, request_id: u64, result: Value) {
    runtime.resolve_shell_call(request_id, Ok(result));
}

fn has_text(frame: &SurfaceFrame, wanted: &str) -> bool {
    frame
        .commands
        .iter()
        .any(|command| matches!(command, DrawCommand::Text { content, .. } if content == wanted))
}

fn has_text_prefix(frame: &SurfaceFrame, prefix: &str) -> bool {
    frame.commands.iter().any(
        |command| matches!(command, DrawCommand::Text { content, .. } if content.starts_with(prefix))
    )
}

/// 任意文字片段包含目标字面量（引用胶囊 / 折叠摘要这类会被分段绘制）。
fn has_text_containing(frame: &SurfaceFrame, wanted: &str) -> bool {
    frame.commands.iter().any(
        |command| matches!(command, DrawCommand::Text { content, .. } if content.contains(wanted))
    )
}

/// 卡头标题（世界层，内容是省略后的文本）。
fn title_command(frame: &SurfaceFrame) -> Option<(&str, f32, f32)> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, x: _, y, size, .. } if content.starts_with("现有卡") => {
            Some((content.as_str(), *size, *y))
        }
        _ => None,
    })
}

fn title_text(frame: &SurfaceFrame) -> Option<String> {
    title_command(frame).map(|(content, _, _)| content.to_string())
}

fn title_size(frame: &SurfaceFrame) -> Option<f32> {
    title_command(frame).map(|(_, size, _)| size)
}

fn text_y(frame: &SurfaceFrame) -> Option<f32> {
    title_command(frame).map(|(_, _, y)| y)
}

/// 卡头标题位置：缩放锚点必须落在正文浮层之外，滚轮才交给视图缩放。
fn title_point(frame: &SurfaceFrame) -> Option<(f32, f32)> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, x, y, .. } if content.starts_with("现有卡") => {
            Some((*x, *y))
        }
        _ => None,
    })
}

/// 对话卡卡头标题位置（选中卡与右键都落在卡头，正文浮层不参与）。
fn chat_title_point(frame: &SurfaceFrame) -> Option<(f32, f32)> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, x, y, .. } if content == "对话" => Some((*x, *y)),
        _ => None,
    })
}

/// 工具条上的缩放百分比。
fn zoom_percent(frame: &SurfaceFrame) -> Option<i32> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, .. } if content.ends_with('%') => {
            content.trim_end_matches('%').parse().ok()
        }
        _ => None,
    })
}

/// 卡片背景矩形：圆角矩形且宽高与卡片世界尺寸同比例（缩放由宽高反推）。
fn card_bounds(frame: &SurfaceFrame, world_w: f32, world_h: f32) -> Option<(f32, f32, f32, f32)> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Rect { x, y, w, h, corner_radius: Some(_), .. } => {
            let zoom = *w / world_w;
            (zoom >= 0.1 && zoom <= 3.0 && (*h - world_h * zoom).abs() < 1.0)
                .then_some((*x, *y, *w, *h))
        }
        _ => None,
    })
}

/// 当前视图的缩放倍率（由卡宽反推，避免工具条百分比取整误差）。
fn card_zoom(frame: &SurfaceFrame) -> f32 {
    card_bounds(frame, CARD_W, CARD_H).map(|(_, _, w, _)| w / CARD_W).unwrap_or(1.0)
}

/// 卡内容区内的正文行（按绘制顺序，排除卡头与世界层 chrome）：换行一致性与末行断言都读它。
fn card_text_lines(frame: &SurfaceFrame, world_w: f32, world_h: f32) -> Vec<String> {
    let (cx, cy, cw, ch) = card_bounds(frame, world_w, world_h).expect("应找到卡片背景");
    let zoom = cw / world_w;
    let body_top = cy + (CARD_HEAD_H + CARD_BODY_GAP) * zoom;
    let body_bottom = cy + ch - CARD_CORNER_RADIUS * zoom;
    let content_left = cx + (CARD_CONTENT_INSET + CARD_CONTENT_PAD) * zoom;
    let content_right =
        cx + cw - (CARD_CONTENT_INSET + SCROLLBAR_SLOT_W + CARD_CONTENT_PAD) * zoom;
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            DrawCommand::Text { content, x, y, size, .. }
                if *y >= body_top - 0.5
                    && *y <= body_bottom
                    && *x >= content_left - 1.0
                    && *x + text_width(content, *size) <= content_right + 1.5 =>
            {
                Some(content.clone())
            }
            _ => None,
        })
        .collect()
}

/// 卡内容区里最靠下的一行：返回（行下缘，行高）。滚动到底时用它核对内容下缘是否贴齐。
fn last_content_line_bottom(
    frame: &SurfaceFrame,
    world_w: f32,
    world_h: f32,
) -> Option<(f32, f32)> {
    let (cx, cy, cw, _) = card_bounds(frame, world_w, world_h).expect("应找到卡片背景");
    let zoom = cw / world_w;
    let body_top = cy + (CARD_HEAD_H + CARD_BODY_GAP) * zoom;
    let content_left = cx + (CARD_CONTENT_INSET + CARD_CONTENT_PAD) * zoom;
    let content_right =
        cx + cw - (CARD_CONTENT_INSET + SCROLLBAR_SLOT_W + CARD_CONTENT_PAD) * zoom;
    let mut bottommost: Option<(f32, f32)> = None;
    for command in &frame.commands {
        let DrawCommand::Text { content, x, y, size, .. } = command else {
            continue;
        };
        if *x < content_left - 1.0 || *x + text_width(content, *size) > content_right + 1.5 {
            continue;
        }
        // 行盒高度与字号同一比例（宿主量算结果按字号线性缩放），不能按字号分档估
        let line = *size * 1.625;
        if *y + line < body_top {
            continue;
        }
        let bottom = *y + line;
        if bottommost.is_none_or(|(last, _)| bottom > last) {
            bottommost = Some((bottom, line));
        }
    }
    bottommost
}

/// 文本卡正文首行（字号断言与缩放锚点都按它定位）。
fn body_command(frame: &SurfaceFrame) -> Option<(&str, f32, f32, f32)> {
    frame.commands.iter().find_map(|command| match command {
        DrawCommand::Text { content, x, y, size, .. } if content == "正文" => {
            Some((content.as_str(), *x, *y, *size))
        }
        _ => None,
    })
}

/// 卡内每条滚动条槽右缘距卡右缘恒定（距离按 zoom 等比），且槽落在卡内。
/// `require` 为 false 时不要求滚动条可见：滚动停止后控件层会隐藏槽。
fn assert_scrollbar_hugs_card(
    frame: &SurfaceFrame,
    world_w: f32,
    world_h: f32,
    require: bool,
) {
    let (cx, cy, cw, ch) = card_bounds(frame, world_w, world_h).expect("应找到卡片背景");
    let zoom = cw / world_w;
    let slot_w = SCROLLBAR_SLOT_W * zoom;
    let mut bars = 0;
    for command in &frame.commands {
        let DrawCommand::Rect { x, y, w, h, corner_radius: None, .. } = command else {
            continue;
        };
        if (*w - slot_w).abs() > 0.5 || *h < 24.0 * zoom {
            continue;
        }
        // 与卡片无交集：属于别的卡或小地图，跳过。
        if *x + *w <= cx || *x >= cx + cw || *y + *h <= cy || *y >= cy + ch {
            continue;
        }
        // 与卡片相交的槽不得伸出卡片矩形；输入框内部的滚动槽在内容区，不贴卡缘。
        assert!(
            *x >= cx - 0.5 && *x + *w <= cx + cw + 0.5,
            "滚动条横向越出卡片：{}..{} 不在 {cx}..{}",
            x,
            *x + *w,
            cx + cw
        );
        assert!(
            *y >= cy - 0.5 && *y + *h <= cy + ch + 0.5,
            "滚动条纵向越出卡片：{}..{} 不在 {cy}..{}",
            y,
            *y + *h,
            cy + ch
        );
        let distance = cx + cw - (x + w);
        if (distance - CARD_CONTENT_INSET * zoom).abs() < 1.5 {
            bars += 1;
        }
    }
    if require {
        assert!(bars > 0, "长内容应画出卡内滚动条");
    }
}

/// 卡内全部文字对外可见的部分必须落在卡片矩形内（圆角以内），未裁剪的文字右缘不压滚动条槽。
/// 跨视口边的 Markdown 行会整行画出、由滚动裁剪截断，所以纵向按下缘用「可见矩形」判定。
fn assert_card_text_within_card(frame: &SurfaceFrame, world_w: f32, world_h: f32) {
    let (cx, cy, cw, ch) = card_bounds(frame, world_w, world_h).expect("应找到卡片背景");
    let zoom = cw / world_w;
    let content_left = cx + (CARD_CONTENT_INSET + CARD_CONTENT_PAD) * zoom;
    let content_right =
        cx + cw - (CARD_CONTENT_INSET + SCROLLBAR_SLOT_W + CARD_CONTENT_PAD) * zoom;
    let body_top = cy + (CARD_HEAD_H + CARD_BODY_GAP) * zoom;
    let card_bottom = cy + ch - CARD_CORNER_RADIUS * zoom;
    let mut clips: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut lines = 0;
    for command in &frame.commands {
        match command {
            DrawCommand::PushClip { x, y, w, h } => clips.push((*x, *y, *w, *h)),
            DrawCommand::PopClip => {
                clips.pop();
            }
            DrawCommand::Text { content, x, y, size, .. } => {
                // 卡头文字按卡宽省略（另行断言），不参与内容区约束。
                if *y < body_top - 0.5 || *y > card_bottom || *x < cx - 0.5 || *x > cx + cw {
                    continue;
                }
                lines += 1;
                assert!(
                    *x >= content_left - 1.0,
                    "卡内文字左缘越出内容矩形：{content} @ {x} < {content_left}"
                );
                let right = *x + text_width(content, *size);
                assert!(
                    right <= content_right + 1.5,
                    "卡内文字右缘越出内容矩形：{content} → {right} > {content_right}"
                );
                let mut visible = (*x, *y, text_width(content, *size),
                    if *size >= 16.0 { *size * 1.3 } else { *size * 1.625 });
                for clip in &clips {
                    visible = intersect(visible, *clip);
                }
                if visible.2 <= 0.0 || visible.3 <= 0.0 {
                    continue;
                }
                assert!(
                    visible.1 + visible.3 <= card_bottom + 1.0,
                    "卡内可见文字下缘越出卡片圆角：{content} → {} > {card_bottom}",
                    visible.1 + visible.3
                );
            }
            _ => {}
        }
    }
    assert!(lines > 0, "卡内容区应画出文字");
}

/// 附件缩略图对外可见的绘制区（命令矩形 ∩ 外层裁剪）必须落在卡内容矩形内。
fn assert_thumb_within_content(frame: &SurfaceFrame, world_w: f32, world_h: f32) {
    let (cx, cy, cw, ch) = card_bounds(frame, world_w, world_h).expect("应找到卡片背景");
    let zoom = cw / world_w;
    let body_top = cy + (CARD_HEAD_H + CARD_BODY_GAP) * zoom;
    let body_bottom = cy + ch - CARD_CORNER_RADIUS * zoom;
    let content_left = cx + (CARD_CONTENT_INSET + CARD_CONTENT_PAD) * zoom;
    let content_right =
        cx + cw - (CARD_CONTENT_INSET + SCROLLBAR_SLOT_W + CARD_CONTENT_PAD) * zoom;
    let content_width = content_right - content_left;
    let mut clips: Vec<(f32, f32, f32, f32)> = Vec::new();
    let mut thumbs = 0;
    let mut rows: Vec<(f32, f32, f32)> = Vec::new();
    for command in &frame.commands {
        match command {
            DrawCommand::PushClip { x, y, w, h } => clips.push((*x, *y, *w, *h)),
            DrawCommand::PopClip => {
                clips.pop();
            }
            DrawCommand::Image { x, y, w, h, .. } => {
                let mut rect = (*x, *y, *w, *h);
                for clip in &clips {
                    rect = intersect(rect, *clip);
                }
                if rect.2 <= 0.0 || rect.3 <= 0.0 || rect.0 < cx - 0.5 || rect.0 > cx + cw {
                    continue;
                }
                thumbs += 1;
                assert!(
                    rect.0 >= content_left - 1.0,
                    "缩略图左缘越出内容矩形：{} < {content_left}",
                    rect.0
                );
                assert!(
                    rect.1 >= body_top - 1.0,
                    "缩略图上缘越出内容矩形：{} < {body_top}",
                    rect.1
                );
                assert!(
                    rect.2 <= content_width + 1.0,
                    "缩略图宽度应收敛到内容宽：{} > {content_width}",
                    rect.2
                );
                assert!(
                    rect.0 + rect.2 <= content_right + 1.5,
                    "缩略图右缘越出内容矩形：{} > {content_right}",
                    rect.0 + rect.2
                );
                assert!(
                    rect.1 + rect.3 <= body_bottom + 1.0,
                    "缩略图下缘越出内容矩形：{} > {body_bottom}",
                    rect.1 + rect.3
                );
                match rows.iter_mut().find(|row| (row.0 - rect.1).abs() < 2.0) {
                    Some(row) => {
                        row.1 = row.1.min(rect.0);
                        row.2 = row.2.max(rect.0 + rect.2);
                    }
                    None => rows.push((rect.1, rect.0, rect.0 + rect.2)),
                }
            }
            _ => {}
        }
    }
    assert!(thumbs >= 5, "对话卡应画出全部附件缩略图：{thumbs} < 5");
    // 同一行的缩略图总宽必须收敛到内容宽：多图 / 窄卡都不铺到卡外。
    for (y, left, right) in rows {
        let span = right - left;
        assert!(
            span <= content_width + 1.0,
            "缩略图行总宽应收敛到内容宽 @ {y}：{span} > {content_width}"
        );
    }
}

fn intersect(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    (x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// 与宿主量算同一口径的宽度估算（ASCII 0.55 字宽，其余 1 字宽）。
fn text_width(text: &str, size: f32) -> f32 {
    text.chars()
        .map(|ch| size * if ch.is_ascii() { 0.55 } else { 1.0 })
        .sum()
}

fn wheel(runtime: &PluginRuntime, delta_y: f32, control: bool) {
    wheel_with(runtime, 600.0, 500.0, delta_y, control);
}

/// 指定位置的滚轮（用于滚动卡片浮层内的消息区）。
fn wheel_at(runtime: &PluginRuntime, x: f32, y: f32, delta_y: f32) {
    wheel_with(runtime, x, y, delta_y, false);
}

fn wheel_with(runtime: &PluginRuntime, x: f32, y: f32, delta_y: f32, control: bool) {
    let mut modifiers = Modifiers::default();
    modifiers.control = control;
    runtime.notify_frame_input(
        CANVAS_ID,
        SURFACE,
        &FrameInput::Wheel {
            x,
            y,
            delta_x: 0.0,
            delta_y,
            modifiers,
            hit: None,
        },
    );
}

/// 空态：标题 / 说明按面宽居中，内容块（64 图标 + 两侧 16 间距 + 两行文字）垂直中心落在面中心。
fn assert_empty_state_centered(frame: &SurfaceFrame, title: &str, desc: &str) {
    let size = frame.size.expect("整面帧必须带尺寸");
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
    let top = (size.h - block) / 2.0;
    assert!((title.1 - (top + 64.0 + 16.0)).abs() < 0.5, "空态标题应垂直居中");
    let desc_y = top + 64.0 + 16.0 + title.2 * 1.2 + 16.0;
    assert!((desc.1 - desc_y).abs() < 0.5, "空态说明应垂直居中");
    assert!(((top + desc_y + desc.2 * 1.2) / 2.0 - size.h / 2.0).abs() < 0.5, "空态内容块应垂直居中");
}

async fn wait_menu(state: &HostState, count: usize) -> MenuOpenParams {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(params) = state.menus.lock().unwrap().get(count.saturating_sub(1)) {
            return params.clone();
        }
        assert!(Instant::now() < deadline, "右键应请求宿主弹菜单");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_open_panel(state: &HostState, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while *state.open_panels.lock().unwrap() < count {
        assert!(Instant::now() < deadline, "应请求宿主打开 AI 面板（期望 {count} 次）");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_written(state: &HostState, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while state.written.lock().unwrap().len() < count {
        assert!(
            Instant::now() < deadline,
            "宿主未收到写盘回执（累计 {}）；未接线的宿主请求：{:?}",
            state.written.lock().unwrap().len(),
            state.unhandled.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_header(
    state: &HostState,
    accept: impl Fn(&PanelHeaderParams) -> bool,
) -> Option<PanelHeaderParams> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(params) = state.headers.lock().unwrap().iter().rev().find(|params| accept(params)) {
            return Some(params.clone());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_frame(
    runtime: &PluginRuntime,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    wait_frame_at_width(runtime, PANEL_W, accept).await
}

async fn wait_frame_at_width(
    runtime: &PluginRuntime,
    width: f32,
    accept: impl Fn(&SurfaceFrame) -> bool,
) -> Option<SurfaceFrame> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(frame) = runtime.surface_frame(CANVAS_ID, SURFACE) {
            let sized = frame.size.is_some_and(|size| (size.w - width).abs() < 0.5);
            if sized && accept(&frame) {
                return Some((*frame).clone());
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 把视图缩放到目标百分比：锚点落在卡头（正文浮层之外，滚轮才交给视图），
/// 按缩放步进的指数口径（`exp(-delta * 0.0025)`）反解增量。
async fn zoom_to(
    runtime: &PluginRuntime,
    frame: &SurfaceFrame,
    target: i32,
    anchor_x: f32,
    anchor_y: f32,
) -> SurfaceFrame {
    // 从卡宽反推实际倍率，避免工具条百分比取整带来的累积偏差。
    let current = card_zoom(frame);
    let target_zoom = target as f32 / 100.0;
    let delta = -(target_zoom / current).ln() / 0.0025;
    wheel_at(runtime, anchor_x, anchor_y, delta);
    wait_frame(runtime, |frame| {
        zoom_percent(frame).is_some_and(|pct| (pct - target).abs() <= 1)
    })
    .await
    .unwrap_or_else(|| {
        let now = runtime.surface_frame(CANVAS_ID, SURFACE);
        panic!("未缩放到 {target}%（当前 {:?}）", now.as_deref().and_then(zoom_percent))
    })
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

async fn wait_running(runtime: &PluginRuntime, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = runtime.list().into_iter().find(|plugin| plugin.id == id).map(|p| p.status);
        if matches!(status, Some(PluginStatus::Running)) {
            return;
        }
        assert!(Instant::now() < deadline, "{id} 未进入运行态：{status:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
