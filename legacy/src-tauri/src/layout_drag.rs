//! 布局迷你窗口管理器的跨窗口拖拽层：拖拽会话（Rust 持有，源窗口只上报输入）、
//! DOM 命中收集、落点解析与释放看门狗。前端只上报输入并接收 `drag-session` 广播渲染 ghost。
//! 权威窗口 bounds 由 `layout_window` 的窗口事件驱动，前端不再维护 bounds 注册表。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::Instant;

use crate::layout::{apply_layout_op, LayoutInner, LayoutState, PrewarmWindow};
use crate::layout_model::{
    active_layout, set_active_tree, split_panel_op, tear_off_from_detached, tear_off_from_panel,
    LayoutOp, ViewKind, WindowBounds,
};
use crate::layout_persist::{broadcast_layout, schedule_persist, DRAG_SESSION_EVENT};
use crate::layout_window::{reconcile_panel_windows, seed_window_bounds, PANEL_LABEL_PREFIX};

/// 撕裂窗口默认尺寸（logical px，与前端一致）。
const PANEL_WINDOW_WIDTH: f64 = 420.0;
const PANEL_WINDOW_HEIGHT: f64 = 560.0;
/// 预热窗口标题占位（认领时按条目里的激活标签刷新，见 `layout_window::show_prewarmed_window`）。
const PREWARM_TITLE: &str = "面板";
/// 拖拽看门狗：**静默超时即收尾**（释放事件丢失的跨平台兜底）。
///
/// 为什么按「一段时间没有新的移动上报」而不是「光标移出所有应用窗口」：窗口内同样会丢事件
/// （指针捕获失效 / 事件被别的窗口吞掉），只判窗口外会让会话永久保留——ghost 常驻、落点永不提交。
/// 空闲上限按光标是否落在任一应用窗口内分两档：窗口内更长。真正避免「拖着停在目标上被误收尾」的是
/// 收尾前的左键探测（Windows 可探测）；无探测能力的平台只能靠这两档计时兜底，长时间停顿仍会收尾。
const DRAG_IDLE_OUTSIDE_MS: u64 = 1200;
const DRAG_IDLE_INSIDE_MS: u64 = 4000;
/// 拖拽结束命中回程等待上限（ms）：广播最终坐标后轮询光标所在窗口的命中变化，
/// 命中即提前结束；无目标窗口（桌面）时走完全部超时。超时而非固定延时——
/// 命中回程快时不再空等，行为上限与原固定延时一致。
const DRAG_FINAL_SETTLE_TIMEOUT_MS: u64 = 60;
/// 命中回程轮询步长（ms）。
const DRAG_SETTLE_POLL_MS: u64 = 10;

/// 跨窗口拖拽会话（由 Rust 持有，源窗口只上报输入）。
#[derive(Clone, Debug)]
pub struct DragSession {
    pub tab_id: String,
    pub view: ViewKind,
    /// 源窗口 label（"main" 或 panel-<id>）。
    pub source_window: String,
    /// 源宿主：主窗口面板 id 或撕裂窗口 id。
    pub source_host: String,
    /// 源面板尺寸（logical px；撕裂新窗默认取此值，0 = 回退固定默认）。
    pub source_width: f64,
    pub source_height: f64,
    /// 最后已知屏幕坐标（logical px；拖拽终点/看门狗兜底用）。
    pub screen_x: f64,
    pub screen_y: f64,
}

/// DOM 命中分区（wire 值 camelCase，与前端 hitTest* 一致；未知值反序列化即报错，
/// 属构造性防御——落点解析无需再为「未知 zone」写兜底分支）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum DropZone {
    #[default]
    Center,
    Left,
    Right,
    Top,
    Bottom,
    Tab,
}

/// 某窗口最近上报的 DOM 命中（zone 语义与前端 hitTest* 一致）。
/// 字段 camelCase 与前端 `DragHit` 对齐——缺 `panelId` 时解析为 None，落点解析会取消停靠。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DragHit {
    /// Center = 加标签；Left/Right/Top/Bottom = 分割（主窗口面板）；Tab = 标签条排序。
    pub zone: DropZone,
    /// 主窗口面板 id（撕裂窗口命中为 None）。
    pub panel_id: Option<String>,
    /// zone = Tab 时的插入位置。
    pub tab_index: Option<usize>,
}

/// 拖拽开始载荷（start=Some 时随 `drag_update` 创建会话；坐标走命令参数，不入载荷）。
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DragStartPayload {
    pub tab_id: String,
    pub view: ViewKind,
    pub source_window: String,
    pub source_host: String,
    /// 源面板尺寸（logical px；撕裂新窗默认取此值，0 = 未知回退固定默认）。
    #[serde(default)]
    pub source_width: f64,
    #[serde(default)]
    pub source_height: f64,
}

/// 拖拽广播载荷（ghost + 各窗口命中计算驱动；End 时 active=false 其余字段为 None）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DragBroadcast {
    pub active: bool,
    pub tab_id: Option<String>,
    pub view: Option<ViewKind>,
    pub screen_x: Option<f64>,
    pub screen_y: Option<f64>,
    pub source_window: Option<String>,
}

/// 点是否在窗口矩形内。
fn point_in_bounds(x: f64, y: f64, b: &WindowBounds) -> bool {
    x >= b.x && x <= b.x + b.width && y >= b.y && y <= b.y + b.height
}

/// 撕裂窗口 bounds：窗口创建在鼠标附近（左上角偏移）；尺寸 = 源面板尺寸（>0 用之，
/// 否则回退固定默认——源 DOM 缺失等未知场景）。
fn bounds_near(x: f64, y: f64, w: f64, h: f64) -> WindowBounds {
    let w = if w > 0.0 { w } else { PANEL_WINDOW_WIDTH };
    let h = if h > 0.0 { h } else { PANEL_WINDOW_HEIGHT };
    WindowBounds {
        x: (x - w / 2.0).round(),
        y: (y - 24.0).round(),
        width: w,
        height: h,
        scale: 0.0,
    }
}

/// 广播拖拽会话（ghost + 命中计算驱动）。
fn broadcast_drag(app: &AppHandle, b: &DragBroadcast) {
    let _ = app.emit(DRAG_SESSION_EVENT, b);
}

/// 拖拽会话广播（Begin/Move 同构）。
fn drag_broadcast_active(inner: &LayoutInner) -> Option<DragBroadcast> {
    inner.drag.as_ref().map(|d| DragBroadcast {
        active: true,
        tab_id: Some(d.tab_id.clone()),
        view: Some(d.view.clone()),
        screen_x: Some(d.screen_x),
        screen_y: Some(d.screen_y),
        source_window: Some(d.source_window.clone()),
    })
}

/// 新 begin 是否按新会话重建：同一次手势（同标签 + 同源窗口）保留原会话（先到先得，
/// 连续 begin / 解析中不覆盖进行中会话）；身份变了说明上一轮手势的释放事件丢了，
/// 旧会话不会自愈（ghost 显示旧标签、松手提交旧标签），必须按新 begin 重建。
/// 抽成纯函数便于直测——命令本身要 AppHandle，无 Tauri 运行时不可单测。
fn begin_replaces_session(existing: Option<&DragSession>, p: &DragStartPayload) -> bool {
    match existing {
        None => true,
        Some(d) => d.tab_id != p.tab_id || d.source_window != p.source_window,
    }
}

/// 无落点可参考时的预热窗口初始 bounds（默认面板尺寸 + 原点；认领时会按落点搬到实际位置）。
fn default_prewarm_bounds() -> WindowBounds {
    WindowBounds { x: 0.0, y: 0.0, width: PANEL_WINDOW_WIDTH, height: PANEL_WINDOW_HEIGHT, scale: 0.0 }
}

/// 备一个预热窗口（已备一个即不动）。先登记占位再建窗：建窗在途时它已「在册」，调和的幽灵回收
/// 不会把它当残留窗口关掉；建窗失败清占位，下一次拖拽起手重试。
/// 调用点须无手势在途（拖拽起手时的一次停顿可接受，落点是手势结束后的空档）。
/// 建窗在放锁后执行：建窗路径（`seed_window_bounds` → `write_bounds`）会自行取布局锁。
/// 建窗期间的预留不会被别人动：认领要求 `ready`（只在建窗落地后置真），故这份预留仍是自己的。
fn ensure_prewarm(app: &AppHandle, bounds: WindowBounds) {
    let state = app.state::<LayoutState>();
    let id = nanoid::nanoid!();
    {
        let Ok(mut inner) = state.inner.lock() else {
            eprintln!("[layout] 布局状态锁已损坏，放弃预热窗口");
            return;
        };
        // 复核：并发调用（拖拽起手与落点收尾）已在建的那一个就是备用窗口
        if inner.prewarm.is_some() {
            return;
        }
        inner.prewarm = Some(PrewarmWindow { id: id.clone(), ready: false });
    }
    let label = format!("{PANEL_LABEL_PREFIX}{id}");
    crate::commands::windows::create_panel_window_internal(app, &label, PREWARM_TITLE, &bounds, false);
    let created = app.get_webview_window(&label).is_some();
    let Ok(mut inner) = state.inner.lock() else { return };
    if created {
        if let Some(p) = inner.prewarm.as_mut() {
            p.ready = true;
        }
    } else {
        // 建窗失败：清占位，下一次拖拽起手重试
        inner.prewarm = None;
    }
}

/// 可用的预热窗口 id（建窗已落地才可用；在途或未备 = None，落点回退现场建窗）。
fn ready_prewarm_id(inner: &LayoutInner) -> Option<String> {
    inner.prewarm.as_ref().filter(|p| p.ready).map(|p| p.id.clone())
}

/// 撕裂条目建成后的收尾：条目建起来了（`created`）才消费该备用窗口，并返回需显窗的（id, 落点）。
/// 标签已不在源面板/源窗口等未建成路径保留备用窗口给下一次；现场建窗（无预热窗口）返回 None，
/// 显隐由建窗路径负责。
fn adopt_prewarm(
    inner: &mut LayoutInner,
    prewarm: Option<String>,
    created: bool,
    bounds: WindowBounds,
) -> Option<(String, WindowBounds)> {
    let id = prewarm?;
    if !created {
        return None;
    }
    if inner.prewarm.as_ref().is_some_and(|p| p.id == id) {
        inner.prewarm = None;
    }
    Some((id, bounds))
}

/// 拖拽更新（start=Some 创建会话，None 仅更新坐标）：更新会话 + 广播 + 重置看门狗。
/// start=Some：刷新全部窗口 bounds（窗口可能在启动后移动过而事件/种子未覆盖，落点解析
/// 的「光标在哪个窗口」判定需要最新 bounds）+ 建会话（含 loaded 检查 + 清空旧命中）。
/// None：仅更新坐标（保留 drag_resolving 守卫——解析中忽略迟到 move，防覆盖 OS 光标权威坐标）。
#[tauri::command]
pub async fn drag_update(
    app: AppHandle,
    screen_x: f64,
    screen_y: f64,
    start: Option<DragStartPayload>,
) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    if start.is_some() {
        for label in app.webview_windows().keys().cloned().collect::<Vec<_>>() {
            seed_window_bounds(&app, &label);
        }
    }
    // 拖拽起手即备一个预热撕裂窗口：建窗停顿与前端装载都落在手势期间，真撕裂时直接显窗。
    // 已备一个不重复建（预热只在真正需要时付一次）；建窗在放锁后执行。
    let start_bounds = start
        .as_ref()
        .map(|p| bounds_near(screen_x, screen_y, p.source_width, p.source_height));
    let (gen, broadcast) = {
        let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
        match start {
            Some(p) => {
                // 会话创建是持久化消费路径的前置：bootstrap 完成前不接受（防默认态覆写磁盘状态）
                if !inner.loaded {
                    return Ok(());
                }
                // 解析中（settle 等待/已进入 drag_end）：直接建会话会覆写 OS 光标权威坐标，
                // 且会被随后的收尾一并解析掉。存为 pending，当前收尾完成后接续为新会话
                // （见 finish_drag 的 apply_pending_start）。
                if inner.drag_resolving {
                    inner.pending_start = Some((p, screen_x, screen_y));
                    return Ok(());
                }
                // 已有会话：同一次手势的重复 begin（先到先得）保留原会话；身份不同（标签/源窗口变了）
                // 说明上一轮的释放事件丢了（Wayland 吞 pointerup、Windows 轮询也失败），旧会话不会自愈——
                // 若继续沿用，松手时提交的会是旧标签。按新 begin 重建会话。
                if begin_replaces_session(inner.drag.as_ref(), &p) {
                    inner.drag = Some(DragSession {
                        tab_id: p.tab_id,
                        view: p.view,
                        source_window: p.source_window,
                        source_host: p.source_host,
                        source_width: p.source_width,
                        source_height: p.source_height,
                        screen_x,
                        screen_y,
                    });
                    inner.drag_hits.clear();
                } else if let Some(d) = inner.drag.as_mut() {
                    // 同一次手势的重复 begin：身份不变，但视图/宿主/尺寸以本次上报为准——
                    // 残留会话期间标签可能改过视图（ghost 图标与文字取自 `view`），面板也可能被移动过
                    d.view = p.view;
                    d.source_host = p.source_host;
                    d.source_width = p.source_width;
                    d.source_height = p.source_height;
                }
            }
            None => {
                // 解析中（settle 等待/已进入 drag_end）忽略迟到 move——否则会覆盖 OS 光标权威坐标
                if inner.drag_resolving {
                    return Ok(());
                }
                let Some(d) = inner.drag.as_mut() else {
                    return Ok(());
                };
                d.screen_x = screen_x;
                d.screen_y = screen_y;
            }
        }
        // 双开/会话已存在时也更新坐标（start 分支用最新坐标）
        if let Some(d) = inner.drag.as_mut() {
            d.screen_x = screen_x;
            d.screen_y = screen_y;
        }
        inner.drag_move_gen += 1;
        (inner.drag_move_gen, drag_broadcast_active(&inner))
    };
    if let Some(b) = broadcast {
        broadcast_drag(&app, &b);
    }
    if let Some(bounds) = start_bounds {
        ensure_prewarm(&app, bounds);
    }
    arm_watchdog(&app, gen, Instant::now(), DRAG_IDLE_OUTSIDE_MS);
    Ok(())
}

/// 窗口上报自身 DOM 命中（各窗口在光标经过时计算并上报，drag-end 落点解析用）。
#[tauri::command]
pub async fn drag_hit(app: AppHandle, window: String, hit: Option<DragHit>) -> Result<(), String> {
    let state = app.state::<LayoutState>();
    let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
    if inner.drag.is_none() {
        return Ok(());
    }
    match hit {
        Some(h) => {
            inner.drag_hits.insert(window, h);
        }
        None => {
            inner.drag_hits.remove(&window);
        }
    }
    Ok(())
}

/// 结束拖拽（源窗口 pointerup / Windows 轮询 / Rust 看门狗）：解析落点 + 广播结束。
#[tauri::command]
pub async fn drag_end(
    app: AppHandle,
    screen_x: Option<f64>,
    screen_y: Option<f64>,
    cancelled: bool,
) -> Result<(), String> {
    finish_drag(&app, screen_x, screen_y, cancelled).await;
    Ok(())
}

/// OS 全局光标位置（物理 px；跨窗口释放点权威）。仅 Windows 支持（GetCursorPos），
/// 其他平台返回 None（前端 pointerup/会话坐标兜底）。
#[cfg(target_os = "windows")]
fn global_cursor_pos() -> Option<(f64, f64)> {
    use winapi::shared::windef::POINT;
    use winapi::um::winuser::GetCursorPos;
    let mut pt = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut pt) } != 0 {
        Some((pt.x as f64, pt.y as f64))
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
fn global_cursor_pos() -> Option<(f64, f64)> {
    None
}

/// 物理 px 光标 → 逻辑 px（取光标所在窗口的 scale；不在任何窗口内时取源窗口 scale）。
/// 混合 DPI 下各窗口 scale 不同，按所在窗口换算后与同窗口 bounds 比较自洽；
/// 光标在桌面（窗外）时退回源窗口 scale 近似；源窗口 bounds 缺失（该窗口 seed 未成功：
/// 取窗口/scale 失败时 `seed_window_bounds` 不写条目）按 1.0 近似——本函数在持锁块内被调用，
/// lookup 缺失不得 panic（一次 panic 会毒化布局锁，使布局与多窗口能力在本进程剩余生命周期永久失效）。
fn cursor_to_logical(
    window_bounds: &HashMap<String, WindowBounds>,
    fallback_label: &str,
    px: f64,
    py: f64,
) -> (f64, f64) {
    let mut sf = window_bounds.get(fallback_label).map(|b| b.scale).unwrap_or(1.0);
    for b in window_bounds.values() {
        let lx = px / b.scale;
        let ly = py / b.scale;
        if point_in_bounds(lx, ly, b) {
            sf = b.scale;
            break;
        }
    }
    (px / sf, py / sf)
}

/// 收尾完成后接续解析窗口内到达的新 begin：建新会话 + 清旧命中 + 升世代号，
/// 返回（新世代号, 会话广播）。无 pending 返回 None——调用方按原路径广播 active=false。
/// 抽成纯函数便于直测（收尾路径要 AppHandle，无 Tauri 运行时不可单测）。
fn apply_pending_start(inner: &mut LayoutInner) -> Option<(u64, Option<DragBroadcast>)> {
    let (p, x, y) = inner.pending_start.take()?;
    inner.drag = Some(DragSession {
        tab_id: p.tab_id,
        view: p.view,
        source_window: p.source_window,
        source_host: p.source_host,
        source_width: p.source_width,
        source_height: p.source_height,
        screen_x: x,
        screen_y: y,
    });
    // 新会话从干净命中开始（旧手势的命中会误导本次落点解析）
    inner.drag_hits.clear();
    inner.drag_move_gen += 1;
    let gen = inner.drag_move_gen;
    Some((gen, drag_broadcast_active(inner)))
}

/// 拖拽结束共享实现（命令与看门狗合流）。
async fn finish_drag(app: &AppHandle, screen_x: Option<f64>, screen_y: Option<f64>, cancelled: bool) {
    let state = app.state::<LayoutState>();
    let need_settle = {
        let Ok(mut inner) = state.inner.lock() else {
            eprintln!("[layout] 布局状态锁已损坏，放弃本次拖拽收尾");
            return;
        };
        if !inner.loaded || inner.drag_resolving || inner.drag.is_none() {
            return;
        }
        // 释放点坐标权威（Windows）：以 OS 全局光标为准——跨窗口/轮询路径的会话坐标
        // 会滞后甚至停在源窗口边界（指针捕获不跨 OS 窗口），是「停靠回主窗口失败」主因。
        // 无 OS 光标（Linux/Wayland）时退回调用方坐标；都没有则保留会话最后坐标。
        if let Some((px, py)) = global_cursor_pos() {
            let src = inner.drag.as_ref().map(|d| d.source_window.clone()).unwrap_or_default();
            let (lx, ly) = cursor_to_logical(&inner.window_bounds, &src, px, py);
            if let Some(d) = inner.drag.as_mut() {
                d.screen_x = lx;
                d.screen_y = ly;
            }
        } else if let (Some(x), Some(y)) = (screen_x, screen_y) {
            if let Some(d) = inner.drag.as_mut() {
                d.screen_x = x;
                d.screen_y = y;
            }
        }
        // 解析中标记：pointerup/轮询/看门狗并发进入时直接返回，只解析一次
        inner.drag_resolving = true;
        // 广播前快照光标所在窗口的命中；广播后轮询该条目的变化（新上报/移除）即提前结束，
        // 消除节流窗口内最后移动导致的命中滞后（拖回/停靠误判）
        let (x, y) = inner.drag.as_ref().map(|d| (d.screen_x, d.screen_y)).unwrap_or((0.0, 0.0));
        let settle_target = if cancelled { None } else { cursor_window_label(&inner.window_bounds, x, y) };
        let settle_snapshot = settle_target.as_ref().and_then(|l| inner.drag_hits.get(l).cloned());
        let broadcast = drag_broadcast_active(&inner);
        drop(inner);
        if !cancelled {
            // 广播最终位置：让光标所在窗口上报精确命中（覆盖节流窗口内的最后移动）
            if let Some(b) = broadcast {
                broadcast_drag(app, &b);
            }
            Some((settle_target, settle_snapshot))
        } else {
            None
        }
    };
    if let Some((target, snapshot)) = need_settle {
        wait_for_settle(&state, target.as_deref(), snapshot).await;
    }

    let Ok(mut inner) = state.inner.lock() else {
        eprintln!("[layout] 布局状态锁已损坏，放弃本次拖拽落点提交");
        return;
    };
    inner.drag_resolving = false;
    let adopted = resolve_drag(&mut inner, cancelled);
    // 取消路径不改布局，不置 dirty（避免无谓落盘）
    if !cancelled {
        inner.dirty = true;
    }
    // 解析窗口内到达的新 begin 在此接续：直接开启新会话（跳过中间的 active=false 广播——
    // 前端据广播清拖拽态，跳过后正在进行的下一手势的 dragActive 不被误清），并重建看门狗
    let resumed = apply_pending_start(&mut inner);
    let ui = inner.ui.clone();
    drop(inner);
    reconcile_panel_windows(app);
    schedule_persist(app, &state);
    broadcast_layout(app, &ui);
    // 认领的预热窗口：条目与广播都已就位，再搬位显窗——窗口出现时已带数据（先写条目再显窗）
    if let Some((id, bounds)) = &adopted {
        crate::layout_window::show_prewarmed_window(app, id, bounds);
    }
    match resumed {
        Some((gen, Some(b))) => {
            broadcast_drag(app, &b);
            arm_watchdog(app, gen, Instant::now(), DRAG_IDLE_OUTSIDE_MS);
        }
        _ => {
            broadcast_drag(app, &DragBroadcast {
                active: false,
                tab_id: None,
                view: None,
                screen_x: None,
                screen_y: None,
                source_window: None,
            });
        }
    }
    // 备用窗口已消费（或本就缺）时补建下一个：此刻无手势在途，建窗停顿落在这里
    ensure_prewarm(app, adopted.map(|(_, bounds)| bounds).unwrap_or_else(default_prewarm_bounds));
}

/// 事件驱动 settle：轮询光标所在窗口的命中条目相对广播前快照的变化（新上报/移除），
/// 命中即提前结束等待；无目标窗口（桌面）时条件恒不满足，走完全部超时。
async fn wait_for_settle(state: &LayoutState, target: Option<&str>, snapshot: Option<DragHit>) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(DRAG_FINAL_SETTLE_TIMEOUT_MS);
    loop {
        let changed = match target {
            Some(label) => {
                let Ok(inner) = state.inner.lock() else {
                    eprintln!("[layout] 布局状态锁已损坏，放弃等待拖拽落点");
                    return;
                };
                inner.drag_hits.get(label).cloned() != snapshot
            }
            None => false,
        };
        if changed || tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(DRAG_SETTLE_POLL_MS)).await;
    }
}

/// 落点决策（`decide_drop_ops` 的输出；`resolve_drag` 据此应用布局操作）。
///
/// 为什么返回枚举而非 Vec<LayoutOp>：边缘分割的「新面板 id」由 `split_panel_op` 运行时
/// 生成，纯函数无法预知——故 `SplitPanelThenDock` 分支只携带分割目标与停靠来源，
/// 由 `resolve_drag` 执行分割后回填新面板 id 再应用停靠。
#[derive(Clone, Debug, PartialEq)]
enum DropDecision {
    /// 无操作（主窗口 chrome / 原面板 center / 未知 zone）。
    None,
    /// 主窗口标签组内排序。
    MoveTabWithin { panel_id: String, to_index: usize },
    /// 主窗口跨面板移动（含 tab 插入位）。
    MoveTabBetween { from_panel_id: String, to_panel_id: String, index: Option<usize> },
    /// 撕裂窗口标签停靠进主窗口面板（含 tab 插入位）。
    DockIntoPanel { panel_id: String, index: Option<usize> },
    /// 主窗口面板边缘分割：先 SplitPanel 生成新面板，再按 `dock` 把标签移入。
    SplitPanelThenDock { panel_id: String, direction: String, position: String, dock: DropDock },
    /// 停靠进撕裂窗口（含 tab 插入位）。
    DockIntoDetached { window_id: String, index: Option<usize> },
    /// 窗外：主窗口面板撕裂建新窗。
    TearOff { panel_id: String },
    /// 窗外：撕裂窗口再撕裂建新窗。
    TearOffFromDetached { window_id: String },
}

/// 边缘分割后标签的去向（来源 = 主窗口面板 or 撕裂窗口）。
#[derive(Clone, Debug, PartialEq)]
enum DropDock {
    MoveTabBetween { from_panel_id: String },
    DockIntoPanel,
}

/// 光标所在窗口 label（与落点解析的窗口判定一致）：主窗口优先；撕裂窗口重叠时取
/// 面积最小 = 最具体的落点（HashMap 遍历顺序不定，避免重叠窗口命中歧义）。
fn cursor_window_label(
    window_bounds: &HashMap<String, WindowBounds>,
    x: f64,
    y: f64,
) -> Option<String> {
    if let Some(b) = window_bounds.get("main") {
        if point_in_bounds(x, y, b) {
            return Some("main".to_string());
        }
    }
    window_bounds
        .iter()
        .filter(|(label, b)| label.as_str() != "main" && point_in_bounds(x, y, b))
        .min_by(|(la, a), (lb, b)| {
            (a.width * a.height)
                .total_cmp(&(b.width * b.height))
                .then_with(|| la.cmp(lb))
        })
        .map(|(label, _)| label.clone())
}

/// 落点决策（纯逻辑，供 `resolve_drag` 与单测共用）：根据拖拽会话 + 各窗口上报的 DOM
/// 命中 + 权威窗口 bounds + 撕裂窗口条目集合，返回唯一要执行的布局操作决策
/// （None = 取消/无操作）。不碰 LayoutInner——新面板 id 等运行时事实由调用方回填。
///
/// 只信「光标所在窗口」的命中：拖拽中光标移出某窗口后其命中即陈旧，误用会把撕裂误判成
/// 停靠/取消（如掠过主窗口面板后释放到窗外）。drop 落在光标所在窗口，由该窗口的
/// DOM 命中决定操作（settle 已按真实释放点刷新该窗口命中）。
fn decide_drop_ops(
    drag: &DragSession,
    hits: &HashMap<String, DragHit>,
    bounds: &HashMap<String, WindowBounds>,
    detached_ids: &HashSet<String>,
) -> DropDecision {
    let (x, y) = (drag.screen_x, drag.screen_y);

    // 1. 光标在主窗口内：用主窗口命中决定（无面板命中 = chrome → 取消）
    if let Some(mb) = bounds.get("main") {
        if point_in_bounds(x, y, mb) {
            let Some(hit) = hits.get("main").cloned() else {
                return DropDecision::None;
            };
            let Some(panel_id) = hit.panel_id else {
                return DropDecision::None;
            };
            let decision = match hit.zone {
                DropZone::Center => {
                    if drag.source_window == "main" {
                        if drag.source_host == panel_id {
                            DropDecision::None // 原面板：无操作
                        } else {
                            DropDecision::MoveTabBetween {
                                from_panel_id: drag.source_host.clone(),
                                to_panel_id: panel_id,
                                index: None,
                            }
                        }
                    } else {
                        DropDecision::DockIntoPanel { panel_id, index: None }
                    }
                }
                DropZone::Tab => {
                    if drag.source_window == "main" {
                        if drag.source_host == panel_id {
                            DropDecision::MoveTabWithin { panel_id, to_index: hit.tab_index.unwrap_or(0) }
                        } else {
                            DropDecision::MoveTabBetween {
                                from_panel_id: drag.source_host.clone(),
                                to_panel_id: panel_id,
                                index: hit.tab_index,
                            }
                        }
                    } else {
                        DropDecision::DockIntoPanel { panel_id, index: hit.tab_index }
                    }
                }
                DropZone::Left | DropZone::Right | DropZone::Top | DropZone::Bottom => {
                    // 边缘 = 分割出独立面板承载该标签（同级插入：左/上 = 前，右/下 = 后）
                    let direction = if hit.zone == DropZone::Left || hit.zone == DropZone::Right { "horizontal" } else { "vertical" };
                    let position = if hit.zone == DropZone::Left || hit.zone == DropZone::Top { "before" } else { "after" };
                    let dock = if drag.source_window == "main" {
                        DropDock::MoveTabBetween { from_panel_id: drag.source_host.clone() }
                    } else {
                        DropDock::DockIntoPanel
                    };
                    DropDecision::SplitPanelThenDock { panel_id, direction: direction.to_string(), position: position.to_string(), dock }
                }
            };
            return decision;
        }
    }

    // 2. 光标在撕裂窗口内（重叠取面积最小 = 最具体落点；条目存在 = 停靠，
    //    幽灵窗口（条目已移除）= 按窗外处理 → 落到步骤 3 撕裂建新窗）。
    if let Some(label) = cursor_window_label(bounds, x, y) {
        let window_id = label.trim_start_matches(PANEL_LABEL_PREFIX).to_string();
        if detached_ids.contains(&window_id) {
            let hit = hits.get(&label).cloned().unwrap_or_default();
            return DropDecision::DockIntoDetached { window_id, index: hit.tab_index };
        }
    }

    // 3. 未命中应用窗口：窗外 = 撕裂建新窗（Rust 建 OS 窗口由 finish_drag 的 reconcile 完成）
    if drag.source_window == "main" {
        DropDecision::TearOff { panel_id: drag.source_host.clone() }
    } else {
        DropDecision::TearOffFromDetached { window_id: drag.source_host.clone() }
    }
}

/// 落点解析（语义与前端原 resolveDrop 一致；坐标 = 会话最后已知屏幕坐标）：
/// 主窗口面板命中（center/tab/边缘分割）/ 撕裂窗口命中（加标签或排序）/
/// 窗外 = 撕裂建新窗。主窗口 chrome（无面板命中）= 取消。
/// 决策由纯函数 `decide_drop_ops` 给出（可单测），本函数只负责执行。
/// 返回本次认领的预热窗口（id + 落点 bounds）：撕裂落点复用了预建窗口时非空，
/// 调用方据此在条目落地后把它搬到落点并显窗（现场建窗的显隐由建窗路径负责）。
fn resolve_drag(inner: &mut LayoutInner, cancelled: bool) -> Option<(String, WindowBounds)> {
    // 仅 finish_drag 调用且入口已守卫 drag 恒非 None（drag_resolving 保证并发唯一），
    // 内部信任：take 后直接解包
    let drag = inner.drag.take().unwrap();
    // 命中保留给本次落点解析（decide_drop_ops 读取）；此处清空会让解析恒拿空表而取消全部停靠。
    // 下次拖拽开始时由 drag_update(start=Some) 清空。
    if cancelled {
        return None;
    }
    let (x, y) = (drag.screen_x, drag.screen_y);
    let detached_ids: HashSet<String> = inner.ui.detached_windows.iter().map(|w| w.id.clone()).collect();
    match decide_drop_ops(&drag, &inner.drag_hits, &inner.window_bounds, &detached_ids) {
        DropDecision::None => None,
        DropDecision::MoveTabWithin { panel_id, to_index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::MoveTabWithin { panel_id, tab_id: drag.tab_id.clone(), to_index });
            None
        }
        DropDecision::MoveTabBetween { from_panel_id, to_panel_id, index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::MoveTabBetween { from_panel_id, to_panel_id, tab_id: drag.tab_id.clone(), index });
            None
        }
        DropDecision::DockIntoPanel { panel_id, index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::DockIntoPanel { panel_id, tab_id: drag.tab_id.clone(), index });
            None
        }
        DropDecision::SplitPanelThenDock { panel_id, direction, position, dock } => {
            // 新面板 id 运行时生成：先分割出空面板，再按来源把标签移入
            let tree = active_layout(&inner.ui).tree;
            let (new_tree, new_panel_id) = split_panel_op(&tree, &panel_id, &direction, &position);
            set_active_tree(&mut inner.ui, new_tree);
            match dock {
                DropDock::MoveTabBetween { from_panel_id } => {
                    apply_layout_op(&mut inner.ui, &LayoutOp::MoveTabBetween {
                        from_panel_id,
                        to_panel_id: new_panel_id,
                        tab_id: drag.tab_id.clone(),
                        index: None,
                    });
                }
                DropDock::DockIntoPanel => {
                    apply_layout_op(&mut inner.ui, &LayoutOp::DockIntoPanel {
                        panel_id: new_panel_id,
                        tab_id: drag.tab_id.clone(),
                        index: None,
                    });
                }
            }
            None
        }
        DropDecision::DockIntoDetached { window_id, index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::DockIntoDetached { window_id, tab_id: drag.tab_id.clone(), index });
            None
        }
        DropDecision::TearOff { panel_id } => {
            let bounds = bounds_near(x, y, drag.source_width, drag.source_height);
            // 有可用预热窗口就把它的 id 交给 op（条目建成后该窗口即被认领进模型）
            let prewarm = ready_prewarm_id(inner);
            let created = tear_off_from_panel(&mut inner.ui, &panel_id, &drag.tab_id, &bounds, prewarm.clone());
            adopt_prewarm(inner, prewarm, created.is_some(), bounds)
        }
        DropDecision::TearOffFromDetached { window_id: source } => {
            let bounds = bounds_near(x, y, drag.source_width, drag.source_height);
            let prewarm = ready_prewarm_id(inner);
            let created = tear_off_from_detached(&mut inner.ui, &source, &drag.tab_id, &bounds, prewarm.clone());
            adopt_prewarm(inner, prewarm, created.is_some(), bounds)
        }
    }
}

/// 看门狗每轮裁决（纯逻辑，可单测）：无事可做 / 已空闲够久该收尾 / 尚未超时空闲上限（重新武装等待）。
#[derive(Clone, Copy, Debug, PartialEq)]
enum WatchdogDecision {
    /// 会话已结束、已被接管或正在解析落点：本轮不介入。
    Idle,
    /// 空闲已超上限：按最后坐标收尾。
    Finish,
    /// 尚未超上限：以该毫秒数重新武装。
    Rearm(u64),
}

/// 看门狗裁决：`gen` = 本任务武装时的移动世代号，`started` = 本轮空闲计时起点（跨轮重新武装时不变，
/// 故窗口内外两档的时间线共用同一个起点）。
/// 世代号不同 = 期间又有移动上报（新一代看门狗负责），本轮直接退出——空闲计时的起点由此天然等于
/// 「最后一次移动上报」，无需另存时间戳。窗口内用更长的空闲上限（光标可能停在目标上观察）。
///
/// `left_button_down`：左键是否仍按下（`None` = 平台无探测能力）。看门狗本意是兜「释放事件丢失」，
/// 不该把「用户仍按着但没动」当成释放——那会在用户还在拖的时候自动提交落点。作为参数传入是为了
/// 让判定可直测（真实取值由调用方探测，测试注入受控值）。
fn watchdog_decision(
    inner: &LayoutInner,
    gen: u64,
    now: Instant,
    started: Instant,
    left_button_down: Option<bool>,
) -> WatchdogDecision {
    if inner.drag_resolving || inner.drag_move_gen != gen || inner.drag.is_none() {
        return WatchdogDecision::Idle;
    }
    let Some(drag) = inner.drag.as_ref() else {
        return WatchdogDecision::Idle;
    };
    // 光标落在任一应用窗口内：用户可能拖着停在目标上观察，给更长的空闲上限
    let inside = inner
        .window_bounds
        .values()
        .any(|b| point_in_bounds(drag.screen_x, drag.screen_y, b));
    let limit = if inside { DRAG_IDLE_INSIDE_MS } else { DRAG_IDLE_OUTSIDE_MS };
    let elapsed_ms = now.duration_since(started).as_millis() as u64;
    if elapsed_ms < limit {
        // 传「距上限还剩多久」：计时起点跨轮不变，传上限会让窗口内档变成两段相加
        return WatchdogDecision::Rearm(limit - elapsed_ms);
    }
    if left_button_down == Some(true) {
        // 左键仍按着（用户在途拖动只是停住）：等「窗口内上限」再复查一次，不按剩余量空转忙等
        return WatchdogDecision::Rearm(DRAG_IDLE_INSIDE_MS);
    }
    WatchdogDecision::Finish
}

/// 拖拽看门狗（静默兜底）：空闲达上限即收尾（释放事件丢失的跨平台兜底）。
///
/// `started` 是**本次空闲计时的起点**，跨轮保持不变（每轮 `Rearm` 都只等剩余时间），
/// 否则「窗口内上限」会被拆成多段相加而实际变长。
/// `wait_ms` 只决定本轮先睡多久（首轮按窗口外上限起等：窗口外场景的兜底速度不被窗口内长上限拖慢）。
fn arm_watchdog(app: &AppHandle, gen: u64, started: Instant, wait_ms: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        let state = app.state::<LayoutState>();
        let decision = {
            let Ok(inner) = state.inner.lock() else {
                eprintln!("[layout] 布局状态锁已损坏，看门狗放弃收尾");
                return;
            };
            watchdog_decision(
                &inner,
                gen,
                Instant::now(),
                started,
                crate::commands::windows::is_mouse_left_down(),
            )
        };
        match decision {
            WatchdogDecision::Idle => {}
            WatchdogDecision::Rearm(ms) => arm_watchdog(&app, gen, started, ms),
            WatchdogDecision::Finish => {
                // 标记本世代已由看门狗接管：后续 move 会上报新世代并重新武装
                match state.inner.lock() {
                    Ok(mut inner) => {
                        if inner.drag_move_gen == gen {
                            inner.drag_move_gen += 1;
                        }
                    }
                    Err(_) => return,
                }
                finish_drag(&app, None, None, false).await;
            }
        }
    });
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutInner;
    use crate::layout_model::{
        find_panel, AppUiState, DetachedWindow, TabItem, UI_STATE_SCHEMA, WorkspaceLayout,
    };
    use crate::layout_model::LayoutNode;

    fn ui_with(tree: LayoutNode) -> AppUiState {
        AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            scenes: vec![crate::layout_model::Scene {
                id: crate::layout_model::DEFAULT_SCENE_ID.into(),
                name: "默认".into(),
                home_layout: crate::layout_model::create_home_layout(),
                active_layout_id: Some("l1".into()),
                layouts: vec![WorkspaceLayout { id: "l1".into(), name: "L".into(), tree }],
            }],
            active_scene_id: Some(crate::layout_model::DEFAULT_SCENE_ID.into()),
            active_layout_id: Some("l1".into()),
            ..Default::default()
        }
    }

    fn panel(id: &str, views: &[&str]) -> LayoutNode {
        let tabs: Vec<TabItem> = views
            .iter()
            .map(|v| TabItem { id: format!("t-{v}"), view: v.to_string(), locked: false })
            .collect();
        let active = tabs.first().map(|t| t.id.clone());
        LayoutNode::Panel { id: id.to_string(), tabs, active_tab_id: active }
    }

    fn two_panels_horizontal() -> LayoutNode {
        LayoutNode::Split {
            id: "s1".into(),
            direction: "horizontal".into(),
            children: vec![panel("p1", &["files"]), panel("p2", &["canvas"])],
            sizes: vec![20.0, 80.0],
        }
    }

    // ---- 落点决策纯函数（decide_drop_ops）----

    fn drag(tab: &str, view: &str, source_window: &str, source_host: &str, x: f64, y: f64) -> DragSession {
        DragSession { tab_id: tab.into(), view: view.into(), source_window: source_window.into(), source_host: source_host.into(), source_width: 0.0, source_height: 0.0, screen_x: x, screen_y: y }
    }

    fn hit(zone: DropZone, panel_id: Option<&str>, tab_index: Option<usize>) -> DragHit {
        DragHit { zone, panel_id: panel_id.map(|s| s.into()), tab_index }
    }

    fn main_bounds() -> WindowBounds {
        WindowBounds { x: 0.0, y: 0.0, width: 1000.0, height: 700.0, scale: 1.0 }
    }

    fn win_bounds(x: f64, y: f64) -> WindowBounds {
        WindowBounds { x, y, width: 400.0, height: 300.0, scale: 1.0 }
    }

    /// 构造 decide_drop_ops 输入并求值：hits/bounds 键为窗口 label（"main" / "panel-<id>"），
    /// detached_ids 为撕裂窗口条目 id（不带前缀）。
    fn decide(
        d: &DragSession,
        hits: &[(&str, DragHit)],
        bounds: &[(&str, WindowBounds)],
        detached_ids: &[&str],
    ) -> DropDecision {
        let hits: HashMap<String, DragHit> =
            hits.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let bounds: HashMap<String, WindowBounds> =
            bounds.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let detached_ids: HashSet<String> = detached_ids.iter().map(|s| s.to_string()).collect();
        decide_drop_ops(d, &hits, &bounds, &detached_ids)
    }

    #[test]
    fn drop_main_center_decisions() {
        // 原面板 center = 无操作
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Center, Some("p1"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::None);
        // 主窗口来源跨面板 = 移动
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Center, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::MoveTabBetween { from_panel_id: "p1".into(), to_panel_id: "p2".into(), index: None });
        // 撕裂窗口来源 = 停靠回主窗口面板
        let d = drag("t-files", "files", "panel-w1", "w1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Center, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::DockIntoPanel { panel_id: "p2".into(), index: None });
    }

    #[test]
    fn drop_main_tab_decisions() {
        // 同面板 tab = 组内排序（带插入位；缺省 0）
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Tab, Some("p1"), Some(3)))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::MoveTabWithin { panel_id: "p1".into(), to_index: 3 });
        let res = decide(&d, &[("main", hit(DropZone::Tab, Some("p1"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::MoveTabWithin { panel_id: "p1".into(), to_index: 0 });
        // 跨面板 = 带插入位移动
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Tab, Some("p2"), Some(1)))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::MoveTabBetween { from_panel_id: "p1".into(), to_panel_id: "p2".into(), index: Some(1) });
        // 撕裂窗口来源 = 停靠回主窗口面板（带插入位）
        let d = drag("t-files", "files", "panel-w1", "w1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Tab, Some("p2"), Some(2)))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::DockIntoPanel { panel_id: "p2".into(), index: Some(2) });
    }

    #[test]
    fn drop_main_edge_decisions() {
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        // left = 水平分割 + 前插 + 主窗口来源跨面板移入
        let res = decide(&d, &[("main", hit(DropZone::Left, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::SplitPanelThenDock {
            panel_id: "p2".into(),
            direction: "horizontal".into(),
            position: "before".into(),
            dock: DropDock::MoveTabBetween { from_panel_id: "p1".into() },
        });
        // right = 水平分割 + 后插
        let res = decide(&d, &[("main", hit(DropZone::Right, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::SplitPanelThenDock {
            panel_id: "p2".into(),
            direction: "horizontal".into(),
            position: "after".into(),
            dock: DropDock::MoveTabBetween { from_panel_id: "p1".into() },
        });
        // top/bottom = 垂直分割
        let res = decide(&d, &[("main", hit(DropZone::Top, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::SplitPanelThenDock {
            panel_id: "p2".into(),
            direction: "vertical".into(),
            position: "before".into(),
            dock: DropDock::MoveTabBetween { from_panel_id: "p1".into() },
        });
        let res = decide(&d, &[("main", hit(DropZone::Bottom, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::SplitPanelThenDock {
            panel_id: "p2".into(),
            direction: "vertical".into(),
            position: "after".into(),
            dock: DropDock::MoveTabBetween { from_panel_id: "p1".into() },
        });
        // 撕裂窗口来源 = 停靠进分割出的新面板
        let d = drag("t-files", "files", "panel-w1", "w1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Right, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::SplitPanelThenDock {
            panel_id: "p2".into(),
            direction: "horizontal".into(),
            position: "after".into(),
            dock: DropDock::DockIntoPanel,
        });
    }

    #[test]
    fn drop_chrome_cancels() {
        // 未知 zone 在 serde 层即被拒绝（DropZone 枚举 + 构造性防御），落点解析无需兜底分支
        // 主窗口命中无 panel_id = 视为 chrome → 取消
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Center, None, None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::None);
        // 光标在主窗口内但无任何命中 = chrome → 取消（即使同时落在撕裂窗口 bounds 内也取消）
        let d = drag("t-files", "files", "main", "p1", 500.0, 300.0);
        let res = decide(&d, &[], &[("main", main_bounds()), ("panel-w1", win_bounds(100.0, 100.0))], &["w1"]);
        assert_eq!(res, DropDecision::None);
    }

    #[test]
    fn drop_detached_window_and_tear_off() {
        // 撕裂窗口命中（条目存在）= 停靠，tab_index 来自该窗口上报命中
        let d = drag("t-files", "files", "main", "p1", 1200.0, 200.0);
        let res = decide(&d, &[("panel-w1", hit(DropZone::Tab, None, Some(2)))], &[("main", main_bounds()), ("panel-w1", win_bounds(1100.0, 100.0))], &["w1"]);
        assert_eq!(res, DropDecision::DockIntoDetached { window_id: "w1".into(), index: Some(2) });
        // 无命中上报 = 默认插尾
        let d = drag("t-files", "files", "main", "p1", 1200.0, 200.0);
        let res = decide(&d, &[], &[("main", main_bounds()), ("panel-w1", win_bounds(1100.0, 100.0))], &["w1"]);
        assert_eq!(res, DropDecision::DockIntoDetached { window_id: "w1".into(), index: None });
        // 幽灵窗口（bounds 在但条目已移除）= 按窗外撕裂建新窗
        let d = drag("t-files", "files", "main", "p1", 1200.0, 200.0);
        let res = decide(&d, &[], &[("main", main_bounds()), ("panel-w1", win_bounds(1100.0, 100.0))], &[]);
        assert_eq!(res, DropDecision::TearOff { panel_id: "p1".into() });
        // 窗外（主窗口来源）= 撕裂
        let d = drag("t-files", "files", "main", "p1", 3000.0, 2000.0);
        let res = decide(&d, &[], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::TearOff { panel_id: "p1".into() });
        // 窗外（撕裂窗口来源）= 再撕裂
        let d = drag("t-files", "files", "panel-w1", "w1", 3000.0, 2000.0);
        let res = decide(&d, &[], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::TearOffFromDetached { window_id: "w1".into() });
    }

    /// 回归：只信光标所在窗口的命中——拖拽掠过 main 后留下的陈旧命中不得把
    /// 窗外撕裂误判成停靠，也不得覆盖撕裂窗口自身的停靠目标。
    #[test]
    fn drop_ignores_stale_main_hit_outside_main() {
        // 光标在窗外，main 有陈旧命中（拖拽中掠过主窗口面板留下）→ 仍按窗外撕裂
        let d = drag("t-files", "files", "main", "p1", 3000.0, 2000.0);
        let res = decide(&d, &[("main", hit(DropZone::Center, Some("p2"), None))], &[("main", main_bounds())], &[]);
        assert_eq!(res, DropDecision::TearOff { panel_id: "p1".into() });
        // 光标在撕裂窗口内，main 有陈旧命中 → 停靠撕裂窗口（而非误停回 main）
        let d = drag("t-files", "files", "main", "p1", 1200.0, 200.0);
        let res = decide(
            &d,
            &[("main", hit(DropZone::Center, Some("p2"), None)), ("panel-w1", hit(DropZone::Center, None, None))],
            &[("main", main_bounds()), ("panel-w1", win_bounds(1100.0, 100.0))],
            &["w1"],
        );
        assert_eq!(res, DropDecision::DockIntoDetached { window_id: "w1".into(), index: None });
        // 撕裂窗口来源拖回主窗口：光标在主窗口 → 用 main 命中停靠回面板
        let d = drag("t-files", "files", "panel-w1", "w1", 500.0, 300.0);
        let res = decide(&d, &[("main", hit(DropZone::Center, Some("p2"), None))], &[("main", main_bounds())], &["w1"]);
        assert_eq!(res, DropDecision::DockIntoPanel { panel_id: "p2".into(), index: None });
    }

    /// 回归：`resolve_drag` 集成路径（拖拽会话 + 上报命中 → 落点解析应用）。
    /// 曾在解析前清空 `drag_hits` 导致命中恒为空 → 全部停靠/排序被取消（主窗口内重排与
    /// 拖回停靠均表现为「释放后无反应」）。纯函数单测直接传入 hits 覆盖不到该消费路径。
    #[test]
    fn resolve_drag_applies_reported_hits() {
        let mut inner = LayoutInner {
            ui: ui_with(two_panels_horizontal()),
            persist_gen: 0,
            dirty: false,
            loaded: true,
            window_bounds: HashMap::from([(
                "main".into(),
                WindowBounds { x: 0.0, y: 0.0, width: 1000.0, height: 800.0, scale: 1.0 },
            )]),
            drag: Some(DragSession {
                tab_id: "t-canvas".into(),
                view: "canvas".into(),
                source_window: "main".into(),
                source_host: "p2".into(),
                source_width: 0.0,
                source_height: 0.0,
                screen_x: 500.0,
                screen_y: 400.0,
            }),
            drag_hits: HashMap::from([(
                "main".into(),
                DragHit { zone: DropZone::Center, panel_id: Some("p1".into()), tab_index: None },
            )]),
            drag_move_gen: 0,
            drag_resolving: false,
            pending_start: None,
            residence_backup: None,
            prewarm: None,
        };
        resolve_drag(&mut inner, false);
        let p1 = find_panel(&active_layout(&inner.ui).tree, "p1").unwrap();
        assert!(p1.iter().any(|t| t.id == "t-canvas"), "上报命中应被消费并移动标签");
        let p2 = find_panel(&active_layout(&inner.ui).tree, "p2").unwrap();
        assert!(!p2.iter().any(|t| t.id == "t-canvas"));
        assert!(inner.drag.is_none(), "解析后拖拽会话应被取走");
    }

    // ---- 预热备用窗口（拖拽起手预建 → 落点认领）----

    /// 认领只在建窗落地后发生：在途（ready = false）不可用且预留保留（留给后续落点复用）。
    #[test]
    fn ready_prewarm_requires_ready() {
        let mut inner = inner_with_drag(1, 500.0, 300.0);
        inner.prewarm = Some(PrewarmWindow { id: "w-pre".into(), ready: false });
        assert!(ready_prewarm_id(&inner).is_none());
        assert_eq!(inner.prewarm.as_ref().map(|p| p.id.as_str()), Some("w-pre"), "在途预留不得被消费");
        if let Some(p) = inner.prewarm.as_mut() {
            p.ready = true;
        }
        assert_eq!(ready_prewarm_id(&inner).as_deref(), Some("w-pre"));
        // 未建成条目（created = false）不消费：备用窗口留给下一次
        assert!(adopt_prewarm(&mut inner, Some("w-pre".into()), false, default_prewarm_bounds()).is_none());
        assert!(inner.prewarm.is_some(), "条目未建不得消费备用窗口");
        // 建成后才消费，并返回需显窗的（id, 落点）
        let bounds = default_prewarm_bounds();
        let adopted = adopt_prewarm(&mut inner, Some("w-pre".into()), true, bounds.clone());
        assert_eq!(adopted, Some(("w-pre".to_string(), bounds)));
        assert!(inner.prewarm.is_none(), "认领后预留被消费");
        // 无预热窗口的现场建窗路径：不返回显窗目标
        assert!(adopt_prewarm(&mut inner, None, true, default_prewarm_bounds()).is_none());
    }

    /// 窗外撕裂认领预热窗口：新条目 id = 预分配 id（窗口随即被认领进模型），
    /// 并返回落点供调用方显窗（现场建窗的显隐由建窗路径负责）。
    #[test]
    fn resolve_drag_adopts_ready_prewarm() {
        let mut inner = inner_with_drag(1, 3000.0, 2000.0);
        inner.prewarm = Some(PrewarmWindow { id: "w-pre".into(), ready: true });
        let adopted = resolve_drag(&mut inner, false);
        assert_eq!(inner.ui.detached_windows.len(), 1);
        assert_eq!(inner.ui.detached_windows[0].id, "w-pre");
        let (id, bounds) = adopted.expect("认领预建窗口应返回落点用于显窗");
        assert_eq!(id, "w-pre");
        assert_eq!(inner.ui.detached_windows[0].bounds, bounds, "返回值应与条目 bounds 一致");
        assert!(inner.prewarm.is_none(), "认领后备位置空（由收尾补建下一个）");
    }

    /// 预热在途（建窗未落地）时落点回退现场建窗：条目 id 现场生成，预留保留给下一次。
    #[test]
    fn resolve_drag_keeps_unready_prewarm_and_generates_id() {
        let mut inner = inner_with_drag(1, 3000.0, 2000.0);
        inner.prewarm = Some(PrewarmWindow { id: "w-pre".into(), ready: false });
        let adopted = resolve_drag(&mut inner, false);
        assert_eq!(inner.ui.detached_windows.len(), 1);
        assert_ne!(inner.ui.detached_windows[0].id, "w-pre");
        assert!(adopted.is_none(), "未认领时无需显窗");
        assert_eq!(inner.prewarm.as_ref().map(|p| p.id.as_str()), Some("w-pre"), "在途预留不得被消费");
    }

    /// 撕裂窗口再撕裂同样认领预热窗口（来源 = 撕裂窗口的那条落点路径）。
    #[test]
    fn resolve_drag_adopts_prewarm_from_detached_source() {
        let mut inner = inner_with_drag(1, 3000.0, 2000.0);
        inner.drag = Some(drag("t-note", "note", "panel-w1", "w1", 3000.0, 2000.0));
        inner.ui.detached_windows = vec![DetachedWindow {
            id: "w1".into(),
            tabs: vec![TabItem { id: "t-note".into(), view: "note".into(), locked: false }],
            active_tab_id: Some("t-note".into()),
            bounds: WindowBounds { x: 1000.0, y: 0.0, width: 300.0, height: 400.0, scale: 1.0 },
            hidden: false,
            restore_on_launch: true,
            options: crate::layout_model::WindowOptions::default(),
            pinned: false,
        }];
        inner.prewarm = Some(PrewarmWindow { id: "w-pre".into(), ready: true });
        let adopted = resolve_drag(&mut inner, false);
        // 源窗口拖空被移除，只剩认领出的新窗口
        assert_eq!(inner.ui.detached_windows.len(), 1);
        assert_eq!(inner.ui.detached_windows[0].id, "w-pre");
        assert_eq!(inner.ui.detached_windows[0].tabs[0].id, "t-note");
        assert_eq!(adopted.expect("认领预建窗口应返回落点").0, "w-pre");
    }

    /// 撕裂新窗 bounds 尺寸 = 源面板尺寸（>0 用之）；未知（0）回退固定默认。
    #[test]
    fn bounds_near_uses_source_size() {        // 显式源尺寸生效，位置按源尺寸居中于光标
        let b = bounds_near(1000.0, 500.0, 360.0, 480.0);
        assert_eq!((b.width, b.height), (360.0, 480.0));
        assert_eq!(b.x, (1000.0_f64 - 180.0).round());
        assert_eq!(b.y, (500.0_f64 - 24.0).round());
        // 0（源 DOM 缺失）回退固定默认
        let b = bounds_near(1000.0, 500.0, 0.0, 0.0);
        assert_eq!((b.width, b.height), (PANEL_WINDOW_WIDTH, PANEL_WINDOW_HEIGHT));
    }

    /// 回归：resolve_drag 窗外撕裂路径把拖拽会话携带的源面板尺寸写进新窗口 bounds
    /// （源尺寸未知（0）时回退固定默认）。
    #[test]
    fn resolve_drag_tear_off_uses_source_size() {
        let mut inner = LayoutInner {
            ui: ui_with(two_panels_horizontal()),
            persist_gen: 0,
            dirty: false,
            loaded: true,
            window_bounds: HashMap::from([(
                "main".into(),
                WindowBounds { x: 0.0, y: 0.0, width: 1000.0, height: 800.0, scale: 1.0 },
            )]),
            drag: Some(DragSession {
                tab_id: "t-canvas".into(),
                view: "canvas".into(),
                source_window: "main".into(),
                source_host: "p2".into(),
                source_width: 360.0,
                source_height: 480.0,
                screen_x: 3000.0,
                screen_y: 2000.0,
            }),
            drag_hits: HashMap::new(),
            drag_move_gen: 0,
            drag_resolving: false,
            pending_start: None,
            residence_backup: None,
            prewarm: None,
        };
        resolve_drag(&mut inner, false);
        assert_eq!(inner.ui.detached_windows.len(), 1);
        let w = &inner.ui.detached_windows[0];
        assert_eq!((w.bounds.width, w.bounds.height), (360.0, 480.0));
        assert_eq!(w.bounds.x, (3000.0_f64 - 180.0).round());
        assert_eq!(w.bounds.y, (2000.0_f64 - 24.0).round());
    }

    /// DropZone wire 值与前端 hitTest* 一致；未知值反序列化报错（构造性防御）。
    #[test]
    fn drop_zone_serde_roundtrip() {
        for (zone, wire) in [
            (DropZone::Center, "center"),
            (DropZone::Left, "left"),
            (DropZone::Right, "right"),
            (DropZone::Top, "top"),
            (DropZone::Bottom, "bottom"),
            (DropZone::Tab, "tab"),
        ] {
            assert_eq!(serde_json::to_string(&zone).unwrap(), format!("\"{wire}\""));
            assert_eq!(serde_json::from_str::<DropZone>(&format!("\"{wire}\"")).unwrap(), zone);
        }
        assert!(serde_json::from_str::<DropZone>("\"weird\"").is_err());
    }

    /// 回归：重叠撕裂窗口取面积最小的窗口（最具体落点）——HashMap 遍历顺序不定，
    /// 同点重叠时旧实现可能命中任意一个，落点不确定。
    #[test]
    fn drop_overlapping_detached_takes_smallest_area() {
        let b1 = WindowBounds { x: 1100.0, y: 100.0, width: 600.0, height: 400.0, scale: 1.0 };
        let b2 = WindowBounds { x: 1150.0, y: 150.0, width: 400.0, height: 300.0, scale: 1.0 };
        // 光标在重叠区 → 取面积最小的 w2
        let d = drag("t-files", "files", "main", "p1", 1200.0, 200.0);
        let res = decide(
            &d,
            &[("panel-w2", hit(DropZone::Tab, None, Some(1)))],
            &[("main", main_bounds()), ("panel-w1", b1.clone()), ("panel-w2", b2.clone())],
            &["w1", "w2"],
        );
        assert_eq!(res, DropDecision::DockIntoDetached { window_id: "w2".into(), index: Some(1) });
        // 面积最小窗口是幽灵（条目已移除）→ 按窗外撕裂建新窗
        let d = drag("t-files", "files", "main", "p1", 1200.0, 200.0);
        let res = decide(
            &d,
            &[],
            &[("main", main_bounds()), ("panel-w1", b1), ("panel-w2", b2)],
            &["w1"],
        );
        assert_eq!(res, DropDecision::TearOff { panel_id: "p1".into() });
    }

    /// 光标换算按所在窗口 scale；窗外回退源窗口 scale（scale 已并入 WindowBounds 注册表）。
    #[test]
    fn cursor_to_logical_uses_window_scale_and_fallback() {
        let bounds = HashMap::from([
            ("main".into(), WindowBounds { x: 0.0, y: 0.0, width: 1000.0, height: 700.0, scale: 1.0 }),
            ("panel-w1".into(), WindowBounds { x: 2000.0, y: 0.0, width: 400.0, height: 300.0, scale: 2.0 }),
        ]);
        // 物理 (4400, 300)：落在 w1（逻辑 2000-2400 × 0-300，scale 2.0）内 → 按 2.0 换算 (2200, 150)
        assert_eq!(cursor_to_logical(&bounds, "main", 4400.0, 300.0), (2200.0, 150.0));
        // 光标在桌面（窗外）→ 回退源窗口 scale（main = 1.0）
        assert_eq!(cursor_to_logical(&bounds, "main", 5000.0, 5000.0), (5000.0, 5000.0));
        // 源窗口 bounds 缺失（seed 未成功）→ 按 1.0 近似，不得 panic（本函数在持锁块内调用）；
        // 坐标须落在所有窗口之外，否则 for 循环会先命中窗口分支、覆盖回退值
        assert_eq!(cursor_to_logical(&bounds, "panel-gone", 9000.0, 9000.0), (9000.0, 9000.0));
    }

    // ---- 看门狗裁决（watchdog_decision；丢事件后的自动收尾）----

    /// 构造带活跃拖拽会话的 LayoutInner：`gen` 为移动世代号，坐标决定光标是否落在窗口内。
    fn inner_with_drag(gen: u64, x: f64, y: f64) -> LayoutInner {
        LayoutInner {
            ui: ui_with(two_panels_horizontal()),
            persist_gen: 0,
            dirty: false,
            loaded: true,
            window_bounds: HashMap::from([("main".to_string(), main_bounds())]),
            drag: Some(drag("t-files", "files", "main", "p1", x, y)),
            drag_hits: HashMap::new(),
            drag_move_gen: gen,
            drag_resolving: false,
            pending_start: None,
            residence_backup: None,
            prewarm: None,
        }
    }

    fn start_payload(tab: &str, window: &str) -> DragStartPayload {
        DragStartPayload {
            tab_id: tab.into(),
            view: "files".into(),
            source_window: window.into(),
            source_host: "p1".into(),
            source_width: 0.0,
            source_height: 0.0,
        }
    }

    #[test]
    fn begin_replaces_only_foreign_session() {
        let existing = drag("t-files", "files", "main", "p1", 0.0, 0.0);
        // 无会话：建新会话
        assert!(begin_replaces_session(None, &start_payload("t-files", "main")));
        // 同一次手势的重复 begin（同标签同源窗口）：保留原会话（先到先得）
        assert!(!begin_replaces_session(Some(&existing), &start_payload("t-files", "main")));
        // 换了标签或换了源窗口：上一轮释放丢了、旧会话不会自愈 → 按新 begin 重建
        assert!(begin_replaces_session(Some(&existing), &start_payload("t-note", "main")));
        assert!(begin_replaces_session(Some(&existing), &start_payload("t-files", "panel-w1")));
    }

    #[test]
    fn apply_pending_start_resumes_queued_begin() {
        let mut inner = inner_with_drag(3, 500.0, 300.0);
        inner.pending_start = Some((start_payload("t-note", "panel-w1"), 700.0, 400.0));
        inner.drag_hits.insert(
            "main".into(),
            DragHit { zone: DropZone::Center, panel_id: Some("p1".into()), tab_index: None },
        );
        let (gen, broadcast) = apply_pending_start(&mut inner).unwrap();
        // 新会话按 pending 载荷 + 首帧坐标建立
        let d = inner.drag.as_ref().unwrap();
        assert_eq!(d.tab_id, "t-note");
        assert_eq!(d.source_window, "panel-w1");
        assert_eq!((d.screen_x, d.screen_y), (700.0, 400.0));
        // 旧命中清空（防误导新会话的落点解析）；pending 被消费
        assert!(inner.drag_hits.is_empty());
        assert!(inner.pending_start.is_none());
        // 世代号 +1，广播为活跃会话（新会话坐标）
        assert_eq!(gen, 4);
        let b = broadcast.unwrap();
        assert!(b.active);
        assert_eq!(b.tab_id.as_deref(), Some("t-note"));
        assert_eq!((b.screen_x, b.screen_y), (Some(700.0), Some(400.0)));
    }

    #[test]
    fn apply_pending_start_noops_without_pending() {
        let mut inner = inner_with_drag(3, 500.0, 300.0);
        assert!(apply_pending_start(&mut inner).is_none());
        // 无 pending 时不动会话与世代号（原收尾路径继续广播 active=false）
        assert!(inner.drag.is_some());
        assert_eq!(inner.drag_move_gen, 3);
    }

    #[test]
    fn watchdog_ignores_stale_generation() {
        // 期间又有移动上报（世代号变了）：本轮退出，由新一代看门狗负责
        let inner = inner_with_drag(7, 500.0, 300.0);
        let t0 = Instant::now();
        assert_eq!(
            watchdog_decision(&inner, 6, t0 + Duration::from_secs(60), t0, Some(false)),
            WatchdogDecision::Idle
        );
    }

    #[test]
    fn watchdog_ignores_missing_session_and_resolving() {
        let t0 = Instant::now();
        let mut inner = inner_with_drag(1, 500.0, 300.0);
        inner.drag = None;
        assert_eq!(
            watchdog_decision(&inner, 1, t0 + Duration::from_secs(60), t0, Some(false)),
            WatchdogDecision::Idle
        );
        let mut inner = inner_with_drag(1, 500.0, 300.0);
        inner.drag_resolving = true;
        assert_eq!(
            watchdog_decision(&inner, 1, t0 + Duration::from_secs(60), t0, Some(false)),
            WatchdogDecision::Idle
        );
    }

    #[test]
    fn watchdog_finishes_when_cursor_outside_after_outside_idle() {
        let inner = inner_with_drag(3, 5000.0, 5000.0);
        let t0 = Instant::now();
        assert_eq!(
            watchdog_decision(
                &inner,
                3,
                t0 + Duration::from_millis(DRAG_IDLE_OUTSIDE_MS),
                t0,
                None
            ),
            WatchdogDecision::Finish
        );
    }

    #[test]
    fn watchdog_rearm_reports_remaining_not_limit() {
        // 调用方每轮重置计时起点：Rearm 必须给「还剩多久」，给上限会让窗口内档实际变成两段相加
        let inner = inner_with_drag(3, 500.0, 300.0);
        let t0 = Instant::now();
        let elapsed = Duration::from_millis(DRAG_IDLE_OUTSIDE_MS + 100);
        assert_eq!(
            watchdog_decision(&inner, 3, t0 + elapsed, t0, None),
            WatchdogDecision::Rearm(DRAG_IDLE_INSIDE_MS - DRAG_IDLE_OUTSIDE_MS - 100)
        );
    }

    #[test]
    fn watchdog_keeps_waiting_inside_window_up_to_longer_limit() {
        // 光标在窗口内：到窗口内的长上限才收尾（窗口外上限早就过了）
        let inner = inner_with_drag(3, 500.0, 300.0);
        let t0 = Instant::now();
        assert_eq!(
            watchdog_decision(
                &inner,
                3,
                t0 + Duration::from_millis(DRAG_IDLE_INSIDE_MS),
                t0,
                Some(false)
            ),
            WatchdogDecision::Finish
        );
    }

    #[test]
    fn watchdog_does_not_finish_while_left_button_still_down() {
        // 用户在途拖动、只是停住没动（左键仍按着）：不得当释放丢失收尾，继续等
        let inner = inner_with_drag(3, 500.0, 300.0);
        let t0 = Instant::now();
        assert_eq!(
            watchdog_decision(
                &inner,
                3,
                t0 + Duration::from_millis(DRAG_IDLE_INSIDE_MS * 3),
                t0,
                Some(true)
            ),
            WatchdogDecision::Rearm(DRAG_IDLE_INSIDE_MS)
        );
    }

    #[test]
    fn watchdog_inside_limit_is_longer_than_outside() {
        // 两档上限的相对关系即「窗口内不误收尾」的全部保证，回归时须一并检查
        assert!(DRAG_IDLE_INSIDE_MS > DRAG_IDLE_OUTSIDE_MS);
    }
}
