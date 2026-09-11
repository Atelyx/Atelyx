//! 布局迷你窗口管理器的跨窗口拖拽层：拖拽会话（Rust 持有，源窗口只上报输入）、
//! 命中调和、落点解析、释放检测（看门狗）与全局光标换算。前端只上报输入与接收
//! `drag-session` 广播渲染 ghost + 各自 DOM 命中。权威窗口 bounds 由
//! `layout_window` 的窗口事件驱动，前端不再维护 bounds 注册表。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::layout::{apply_layout_op, LayoutInner, LayoutState};
use crate::layout_model::{
    active_layout, set_active_tree, split_panel_op, LayoutOp, ViewKind, WindowBounds,
};
use crate::layout_persist::{broadcast_layout, schedule_persist, DRAG_SESSION_EVENT};
use crate::layout_window::{reconcile_panel_windows, seed_window_bounds, PANEL_LABEL_PREFIX};

/// 撕裂窗口默认尺寸（logical px，与前端一致）。
const PANEL_WINDOW_WIDTH: f64 = 420.0;
const PANEL_WINDOW_HEIGHT: f64 = 560.0;
/// 拖拽看门狗（ms）：光标移出所有应用窗口后长时间无输入 → 按最后坐标自动结束
/// （释放事件丢失的跨平台兜底；Windows 上前端左键轮询 + JS pointerup 先于本兜底生效）。
const DRAG_WATCHDOG_MS: u64 = 1200;
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
    let (gen, broadcast) = {
        let mut inner = state.inner.lock().map_err(|e| e.to_string())?;
        match start {
            Some(p) => {
                // 会话创建是持久化消费路径的前置：bootstrap 完成前不接受（防默认态覆写磁盘状态）
                if !inner.loaded {
                    return Ok(());
                }
                // 解析中（settle 等待/已进入 drag_end）忽略新 begin——否则会覆写 OS 光标权威坐标
                if inner.drag_resolving {
                    return Ok(());
                }
                // 已有会话则忽略（先到先得）——解析中/连续 begin 不覆盖进行中会话
                if inner.drag.is_none() {
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
    arm_watchdog(&app, gen);
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
    resolve_drag(&mut inner, cancelled);
    // 取消路径不改布局，不置 dirty（避免无谓落盘）
    if !cancelled {
        inner.dirty = true;
    }
    let ui = inner.ui.clone();
    drop(inner);
    reconcile_panel_windows(app);
    schedule_persist(app, &state);
    broadcast_layout(app, &ui);
    broadcast_drag(app, &DragBroadcast {
        active: false,
        tab_id: None,
        view: None,
        screen_x: None,
        screen_y: None,
        source_window: None,
    });
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
fn resolve_drag(inner: &mut LayoutInner, cancelled: bool) {
    // 仅 finish_drag 调用且入口已守卫 drag 恒非 None（drag_resolving 保证并发唯一），
    // 内部信任：take 后直接解包
    let drag = inner.drag.take().unwrap();
    // 命中保留给本次落点解析（decide_drop_ops 读取）；此处清空会让解析恒拿空表而取消全部停靠。
    // 下次拖拽开始时由 drag_update(start=Some) 清空。
    if cancelled {
        return;
    }
    let (x, y) = (drag.screen_x, drag.screen_y);
    let detached_ids: HashSet<String> = inner.ui.detached_windows.iter().map(|w| w.id.clone()).collect();
    match decide_drop_ops(&drag, &inner.drag_hits, &inner.window_bounds, &detached_ids) {
        DropDecision::None => {}
        DropDecision::MoveTabWithin { panel_id, to_index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::MoveTabWithin { panel_id, tab_id: drag.tab_id.clone(), to_index });
        }
        DropDecision::MoveTabBetween { from_panel_id, to_panel_id, index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::MoveTabBetween { from_panel_id, to_panel_id, tab_id: drag.tab_id.clone(), index });
        }
        DropDecision::DockIntoPanel { panel_id, index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::DockIntoPanel { panel_id, tab_id: drag.tab_id.clone(), index });
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
        }
        DropDecision::DockIntoDetached { window_id, index } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::DockIntoDetached { window_id, tab_id: drag.tab_id.clone(), index });
        }
        DropDecision::TearOff { panel_id } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::TearOff {
                panel_id,
                tab_id: drag.tab_id.clone(),
                bounds: bounds_near(x, y, drag.source_width, drag.source_height),
            });
        }
        DropDecision::TearOffFromDetached { window_id } => {
            apply_layout_op(&mut inner.ui, &LayoutOp::TearOffFromDetached {
                window_id,
                tab_id: drag.tab_id.clone(),
                bounds: bounds_near(x, y, drag.source_width, drag.source_height),
            });
        }
    }
}

/// 拖拽看门狗：仅当光标移出所有应用窗口且长时间无输入时按最后坐标自动结束
/// （释放事件丢失的跨平台兜底；世代号合并——仅最新一次 move 的到期任务真正执行）。
fn arm_watchdog(app: &AppHandle, gen: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(DRAG_WATCHDOG_MS)).await;
        let state = app.state::<LayoutState>();
        let should_finish = {
            let Ok(mut inner) = state.inner.lock() else {
                eprintln!("[layout] 布局状态锁已损坏，看门狗放弃收尾");
                return;
            };
            if inner.drag_move_gen != gen || inner.drag.is_none() {
                return;
            }
            let drag = inner.drag.clone().unwrap();
            let outside = inner.window_bounds.values().all(|b| !point_in_bounds(drag.screen_x, drag.screen_y, b));
            if !outside {
                return;
            }
            // 本看门狗接管：后续 move 会再触发新看门狗（用户在途拖动不受影响）
            inner.drag_move_gen += 1;
            true
        };
        if should_finish {
            finish_drag(&app, None, None, false).await;
        }
    });
}

// ===== 单元测试 =====

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutInner;
    use crate::layout_model::{find_panel, AppUiState, TabItem, UI_STATE_SCHEMA, WorkspaceLayout};
    use crate::layout_model::LayoutNode;

    fn ui_with(tree: LayoutNode) -> AppUiState {
        AppUiState {
            schema: UI_STATE_SCHEMA.into(),
            workspace_layouts: vec![WorkspaceLayout { id: "l1".into(), name: "L".into(), tree }],
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
        };
        resolve_drag(&mut inner, false);
        let p1 = find_panel(&active_layout(&inner.ui).tree, "p1").unwrap();
        assert!(p1.iter().any(|t| t.id == "t-canvas"), "上报命中应被消费并移动标签");
        let p2 = find_panel(&active_layout(&inner.ui).tree, "p2").unwrap();
        assert!(!p2.iter().any(|t| t.id == "t-canvas"));
        assert!(inner.drag.is_none(), "解析后拖拽会话应被取走");
    }

    /// 撕裂新窗 bounds 尺寸 = 源面板尺寸（>0 用之）；未知（0）回退固定默认。
    #[test]
    fn bounds_near_uses_source_size() {
        // 显式源尺寸生效，位置按源尺寸居中于光标
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
}
