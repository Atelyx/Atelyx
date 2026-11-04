//! 画布会话状态：磁盘真源的内存投影、视口与交互态、撤销快照。
//! 几何判定复用格式库 `atelyx_format::canvas::geometry`，本模块只管会话与显示态。

use std::collections::{HashMap, HashSet};

use atelyx_format::canvas::geometry::{
    self, Guide, View, display_size, fit_transform, node_size as card_size_of,
};
use atelyx_format::canvas::{CanvasFile, CanvasFileEdge, CanvasFileNode};
use serde_json::Value;

use crate::refs::RefEntry;

/// 画布保存动机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveReason {
    AutoSave,
    Flush,
}

/// 自动保存防抖（毫秒）。
pub const AUTOSAVE_DEBOUNCE_MS: u64 = 500;
/// 撤销栈深度。
pub const UNDO_DEPTH: usize = 50;
/// 工具条高度（屏幕像素）。
pub const TOOLBAR_H: f32 = 36.0;
/// 小地图尺寸与视口边距（屏幕像素）。
pub const MINIMAP_W: f32 = 180.0;
pub const MINIMAP_H: f32 = 120.0;
pub const MINIMAP_MARGIN: f32 = 12.0;
/// 网格步长（世界坐标）。
pub const GRID_STEP: f64 = 100.0;
/// 框选起手最小位移（屏幕像素）。
pub const MARQUEE_MIN: f32 = 4.0;
/// 卡片屏幕宽不超过它时只画卡头与居中图标（世界层文字不再缩小到不可读）。
pub const COMPACT_CARD_PX: f32 = 120.0;
/// 创建分组时成员包围盒外扩（世界坐标）。
pub const GROUP_PAD: f64 = 24.0;
/// 粘贴落位逐行避让步长（世界坐标）。
pub const PASTE_SPOT_STEP: f64 = 24.0;

/// 卡引用内容（正文浮层 / 图片贴片的数据源）。
#[derive(Debug, Clone)]
pub enum RefContent {
    Text(String),
    Image,
    Table { summary: String, body: String },
    Binary,
    Missing,
}

/// 拖拽会话。
#[derive(Debug, Clone)]
pub struct DragState {
    pub kind: DragKind,
    pub start: (f32, f32),
    /// 起始几何（id → (x, y, width, height)），width/height 已按显示尺寸展开。
    pub orig: HashMap<String, (f64, f64, f64, f64)>,
    /// 拖拽期是否实际改写过几何（松手判定「无操作回退快照」）。
    pub mutated: bool,
}

#[derive(Debug, Clone)]
pub enum DragKind {
    Pan,
    Marquee {
        base: Vec<String>,
    },
    Card,
    Resize(String),
    Link(String),
    /// 小地图拖动：按下即把视口中心对到指针处。
    Minimap,
}

/// 撤销快照。
#[derive(Debug, Clone)]
pub struct CanvasSnapshot {
    pub nodes: Vec<CanvasFileNode>,
    pub edges: Vec<CanvasFileEdge>,
}

/// 画布会话。
pub struct Session {
    /// 相对仓库根的 `.atlx` 路径。
    pub file: String,
    pub data: CanvasFile,
    /// 落盘基线（补丁 diff 源）。
    pub saved: CanvasFile,
    pub dirty: bool,
    pub save_error: Option<String>,
    /// 数据变更代（自动保存触发判据）。
    pub version: u64,
    /// 同一文件内的读盘 / 写盘守卫。
    pub epoch: u64,
    /// 下一笔落盘不记 edit 版本（回滚由宿主记 restore）。
    pub suppress_history: bool,
    pub view: ViewState,
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            file: self.file.clone(),
            data: self.data.clone(),
            saved: self.saved.clone(),
            dirty: self.dirty,
            save_error: self.save_error.clone(),
            version: self.version,
            epoch: self.epoch,
            suppress_history: self.suppress_history,
            view: self.view.clone(),
        }
    }
}

impl Session {
    pub fn new(file: String, data: CanvasFile, epoch: u64) -> Self {
        let saved = data.clone();
        Self {
            file,
            data,
            saved,
            dirty: false,
            save_error: None,
            version: 1,
            epoch,
            suppress_history: false,
            view: ViewState::default(),
        }
    }

    /// 数据已变更：置脏 + 版本自增。
    pub fn touch(&mut self) {
        self.dirty = true;
        self.save_error = None;
        self.version += 1;
    }

    pub fn request_fit(&mut self) {
        self.view.fit_pending = true;
    }

    pub fn apply_fit(&mut self, vw: f32, vh: f32) {
        // 世界区在工具条下方：居中按扣除工具条后的高度算
        let usable = (vh - TOOLBAR_H).max(1.0);
        let (zoom, pan) = fit_transform(&self.data.nodes, display_size, vw, usable);
        self.view.zoom = zoom;
        self.view.pan = pan;
        self.view.fit_pending = false;
        self.view.fitted = true;
    }

    pub fn acquire_lock(&mut self, node_id: &str, now_ms: i64) {
        self.view.locks.entry(node_id.to_string()).or_insert(now_ms);
    }

    /// 会话内节点的显示尺寸（收起分组 = 折叠小卡）。
    pub fn node(&self, id: &str) -> Option<&CanvasFileNode> {
        self.data.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut CanvasFileNode> {
        self.data.nodes.iter_mut().find(|n| n.id == id)
    }

    pub fn snapshot(&self) -> CanvasSnapshot {
        CanvasSnapshot {
            nodes: self.data.nodes.clone(),
            edges: self.data.edges.clone(),
        }
    }

    pub fn push_undo(&mut self) {
        self.view.redo.clear();
        self.push_snapshot(self.snapshot());
    }

    fn push_snapshot(&mut self, snapshot: CanvasSnapshot) {
        self.view.undo.push(snapshot);
        if self.view.undo.len() > UNDO_DEPTH {
            self.view.undo.remove(0);
        }
    }

    pub fn restore_snapshot(&mut self, snapshot: CanvasSnapshot) {
        self.data.nodes = snapshot.nodes;
        self.data.edges = snapshot.edges;
        self.view
            .selection
            .retain(|id| self.data.nodes.iter().any(|n| &n.id == id));
        self.view.hover = None;
        self.touch();
    }

    /// 是否有对话卡在流式生成（生成中不接撤销 / 重做）。
    pub fn any_streaming(&self) -> bool {
        self.view.card_chat.values().any(|chat| chat.streaming)
    }

    pub fn undo(&mut self) -> bool {
        if self.any_streaming() {
            return false;
        }
        let Some(prev) = self.view.undo.pop() else {
            return false;
        };
        let current = self.snapshot();
        self.view.redo.push(current);
        self.restore_snapshot(prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.any_streaming() {
            return false;
        }
        let Some(next) = self.view.redo.pop() else {
            return false;
        };
        self.push_snapshot(self.snapshot());
        self.restore_snapshot(next);
        true
    }

    /// 拖动集合的起始几何；只有被选中的分组会带上自身成员。
    pub fn drag_geometry(&self) -> HashMap<String, (f64, f64, f64, f64)> {
        let mut ids: HashSet<String> = self.view.selection.iter().cloned().collect();
        for id in self.view.selection.clone() {
            if self.node(&id).is_some_and(|n| n.node_type == "group") {
                for member in geometry::group_members(&self.data.nodes, &id) {
                    ids.insert(member);
                }
            }
        }
        ids.into_iter()
            .filter_map(|id| {
                let n = self.node(&id)?;
                let (w, h) = display_size(n);
                Some((id, (n.x, n.y, w, h)))
            })
            .collect()
    }
}

/// 节点是否被收起的所属分组隐藏。
pub fn hidden_of(nodes: &[CanvasFileNode], id: &str) -> bool {
    geometry::hidden_by_collapsed(nodes, id)
}

/// 远端协作者在场。
#[derive(Debug, Clone)]
pub struct RemotePeer {
    pub peer_id: String,
    /// 对端所在画布（仓库相对路径）：同一空间里对端可能开的是另一张画布。
    pub path: String,
    pub name: String,
    pub color: (u8, u8, u8),
    pub selected: Vec<String>,
    /// 流式中的对话卡 id。
    pub streaming: Vec<String>,
    /// 本端看到的编辑锁声明（nodeId → since）。
    pub locks: HashMap<String, i64>,
    /// 最近一次收到该对端状态的时刻（过期剔除）。
    pub at: std::time::Instant,
}

impl Default for RemotePeer {
    fn default() -> Self {
        Self {
            peer_id: String::new(),
            path: String::new(),
            name: String::new(),
            color: (0x61, 0xaf, 0xef),
            selected: Vec::new(),
            streaming: Vec::new(),
            locks: HashMap::new(),
            at: std::time::Instant::now(),
        }
    }
}

/// 对话卡运行态（输入 / 流式 / 错误 / 滚动跟随）。
#[derive(Debug, Clone, Default)]
pub struct CardChat {
    pub draft: String,
    /// 草稿内的光标字节位置（`#` 候选按它取查询串）。
    pub caret: usize,
    /// 草稿里已登记的 `#` 引用（按插入顺序）。
    pub mention_refs: Vec<RefEntry>,
    pub mention_picker: Option<MentionPicker>,
    /// 托盘里已选、尚未落盘的图片。
    pub pending_images: Vec<PendingImage>,
    pub streaming: bool,
    pub error: Option<String>,
    pub follow_bottom: bool,
    /// 待二次确认的回滚消息 id。
    pub rollback_confirm: Option<String>,
    pub compacting: bool,
}

/// 待发送的图片（字节在内存，落盘后只留路径）。
#[derive(Debug, Clone)]
pub struct PendingImage {
    pub file_name: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// `#` 候选目标：仓库文件 / 目录，或画布内的卡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionTarget {
    Vault { rel: String, is_dir: bool },
    Card { node_id: String },
}

/// `#` 候选浮层状态。
#[derive(Debug, Clone)]
pub struct MentionPicker {
    /// 触发 `#` 的字节位置。
    pub at: usize,
    pub selected: usize,
    pub candidates: Vec<MentionTarget>,
}

/// 一次发送的冻结参数（图片落盘、提示词预取期间保持不变）。
#[derive(Debug, Clone)]
pub struct SendPlan {
    pub node: String,
    pub display: String,
    /// 发送前的草稿原文（失败时原样回填，不去空白）。
    pub draft: String,
    pub refs: Vec<RefEntry>,
    pub images: Vec<PendingImage>,
    pub agent_id: Option<String>,
    pub model_pick: Option<(String, String)>,
    pub effort: Option<String>,
}

/// 画布视图态。
#[derive(Clone)]
pub struct ViewState {
    pub zoom: f32,
    pub pan: (f32, f32),
    pub fit_pending: bool,
    /// 本会话已按真实尺寸定过视口：之后的尺寸变化保留用户缩放与平移。
    pub fitted: bool,
    pub selection: Vec<String>,
    pub hover: Option<String>,
    pub drag: Option<DragState>,
    pub link_to: Option<(f32, f32)>,
    /// 文本卡引用内容（键 = 仓库相对路径）。
    pub refs: HashMap<String, RefContent>,
    /// 对话卡运行态（键 = 卡节点 id）。
    pub card_chat: HashMap<String, CardChat>,
    /// 本端持有的对话卡编辑锁（nodeId → 占锁时刻毫秒）。
    pub locks: HashMap<String, i64>,
    pub undo: Vec<CanvasSnapshot>,
    pub redo: Vec<CanvasSnapshot>,
    pub guides: Vec<Guide>,
    pub marquee_to: Option<(f32, f32)>,
    /// 分组改名草稿（节点 id，草稿）。
    pub group_rename: Option<(String, String)>,
    /// 最近一次上报的视口尺寸。
    pub viewport: (f32, f32),
    /// 远端在场（同空间同画布）。
    pub remote: Vec<RemotePeer>,
    /// 工具条按钮悬停（`fit` / `zoom-in` / `zoom-out`）。
    pub toolbar_hover: Option<String>,
    /// 右键菜单落点（世界坐标；新建卡按它定位）。
    pub menu_at: Option<(f64, f64)>,
    /// 本端协作身份；锁主判定在同 since 时需要它比较 peer id。
    pub self_id: Option<String>,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: (40.0, 40.0),
            fit_pending: false,
            fitted: false,
            selection: Vec::new(),
            hover: None,
            drag: None,
            link_to: None,
            refs: HashMap::new(),
            card_chat: HashMap::new(),
            locks: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            guides: Vec::new(),
            marquee_to: None,
            group_rename: None,
            viewport: (1.0, 1.0),
            remote: Vec::new(),
            toolbar_hover: None,
            menu_at: None,
            self_id: None,
        }
    }
}

impl ViewState {
    pub fn view(&self) -> View {
        View {
            zoom: self.zoom,
            pan: self.pan,
        }
    }

    pub fn screen(&self, x: f64, y: f64) -> (f32, f32) {
        self.view().screen(x, y)
    }

    pub fn world(&self, x: f32, y: f32) -> (f64, f64) {
        self.view().world(x, y)
    }

    /// 编辑锁是否被对端持有：只看同一条画布上的对端，主不是本端即只读。
    pub fn locked_by_peer(&self, path: &str, node_id: &str) -> Option<&RemotePeer> {
        lock_owner(
            self.locks.get(node_id).copied(),
            &self.remote,
            path,
            self.self_id.as_deref(),
            node_id,
        )
    }
}

/// 编辑锁的远端持有者：锁主按 since 最小、同 since 比 peer id 判定（与合入侧同一套规则）。
/// 本端未拿到自身 peer id 时退化为「对端 since 更早即锁主」，供只读呈现使用。
pub fn lock_owner<'a>(
    mine: Option<i64>,
    remote: &'a [RemotePeer],
    path: &str,
    self_id: Option<&str>,
    node_id: &str,
) -> Option<&'a RemotePeer> {
    let peers: Vec<&RemotePeer> = remote.iter().filter(|peer| peer.path == path).collect();
    let claims = |self_id: &str| {
        let mut claims = vec![atelyx_format::canvas::collab::LockClaim {
            peer_id: self_id.to_string(),
            since: mine.unwrap_or(i64::MAX),
        }];
        claims.extend(peers.iter().filter_map(|peer| {
            peer.locks
                .get(node_id)
                .map(|since| atelyx_format::canvas::collab::LockClaim {
                    peer_id: peer.peer_id.clone(),
                    since: *since,
                })
        }));
        claims
    };
    let owner = self_id.and_then(|self_id| {
        atelyx_format::canvas::collab::compute_lock_owner(&claims(self_id))
    });
    match owner {
        Some(owner) if Some(owner.as_str()) == self_id => None,
        Some(owner) => peers
            .iter()
            .copied()
            .find(|peer| peer.peer_id == owner && peer.locks.contains_key(node_id)),
        None => peers.into_iter().find(|peer| {
            peer.locks
                .get(node_id)
                .is_some_and(|since| mine.is_none_or(|mine| *since < mine))
        }),
    }
}

pub fn text_data() -> Value {
    serde_json::json!({ "title": "文本", "bodyMd": "" })
}

/// 卡头标题：对话卡取节点 title → 首条用户消息首行 → 「对话」。
pub fn card_title(node: &CanvasFileNode) -> String {
    if let Some(title) = node
        .data
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return title.to_string();
    }
    if node.node_type == "conversation" {
        if let Some(messages) = node.data.get("messages").and_then(Value::as_array) {
            for m in messages {
                if m.get("role").and_then(Value::as_str) != Some("user") {
                    continue;
                }
                let text = m
                    .get("displayContent")
                    .and_then(Value::as_str)
                    .or_else(|| m.get("content").and_then(Value::as_str))
                    .unwrap_or("");
                let first = text.lines().next().unwrap_or("").trim();
                if !first.is_empty() {
                    return first.chars().take(16).collect();
                }
            }
        }
        return "对话".to_string();
    }
    String::new()
}

/// 文件名去扩展名。
pub fn file_title(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(name)
        .to_string()
}

/// 卡头图标键（lucide 名）与标题。
pub fn card_head(node: &CanvasFileNode) -> (&'static str, String) {
    match node.node_type.as_str() {
        "conversation" => ("message-square", card_title(node)),
        "text" => {
            if let Some(file) = node.data.get("file").and_then(Value::as_str) {
                ("sticky-note", file_title(file))
            } else {
                let title = node
                    .data
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("文本");
                ("align-left", title.to_string())
            }
        }
        "media" => {
            let name = node
                .data
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| node.data.get("file").and_then(Value::as_str))
                .map(file_title)
                .unwrap_or_else(|| "附件".to_string());
            ("image", name)
        }
        "table" => {
            let title = node
                .data
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    node.data
                        .get("file")
                        .and_then(Value::as_str)
                        .map(file_title)
                })
                .unwrap_or_default();
            ("table", title)
        }
        "link" => {
            let url = node
                .data
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("链接");
            ("link", url.to_string())
        }
        other => ("box", other.to_string()),
    }
}

/// 由原生节点尺寸取缺省落点尺寸（新建卡用）。
pub fn new_card_size(node_type: &str) -> (f64, f64) {
    card_size_of(&CanvasFileNode {
        id: String::new(),
        node_type: node_type.to_string(),
        x: 0.0,
        y: 0.0,
        width: None,
        height: None,
        data: Value::Null,
    })
}

/// 工具条缩放控件：自右向左为放大、缩小、适应，百分比文字画在它们左侧。
/// 绘制与命中共用本函数，返回 (控件 id, 矩形 x/y/w/h)。
pub fn zoom_buttons(w: f32) -> [(&'static str, (f32, f32, f32, f32)); 3] {
    const SIZE: f32 = 24.0;
    const GAP: f32 = 4.0;
    const RIGHT: f32 = 12.0;
    let y = (TOOLBAR_H - SIZE) / 2.0;
    let mut rects = [("", (0.0, 0.0, 0.0, 0.0)); 3];
    let mut x = w - RIGHT;
    for (slot, id) in rects.iter_mut().zip(["zoom-in", "zoom-out", "fit"]) {
        x -= SIZE;
        *slot = (id, (x, y, SIZE, SIZE));
        x -= GAP;
    }
    rects
}

/// 小地图映射：内容包围盒等比缩进小地图框并居中。
/// 返回 `(scale, offset_x, offset_y)`，屏幕坐标 = `(world - offset) * scale` 再加小地图框左上角。
pub fn minimap_transform(nodes: &[CanvasFileNode]) -> Option<(f32, f64, f64)> {
    let rects: Vec<(f64, f64, f64, f64)> = nodes
        .iter()
        .filter(|node| !geometry::hidden_by_collapsed(nodes, &node.id))
        .map(|node| {
            let (w, h) = display_size(node);
            (node.x, node.y, w, h)
        })
        .collect();
    if rects.is_empty() {
        return None;
    }
    let (x, y, w, h) = geometry::bbox_of_rects(&rects);
    let (w, h) = (w.max(1.0), h.max(1.0));
    let scale = ((MINIMAP_W - 16.0) / w as f32)
        .min((MINIMAP_H - 16.0) / h as f32)
        .min(0.5);
    let scale64 = scale as f64;
    let pad_x = (MINIMAP_W as f64 - w * scale64) / 2.0;
    let pad_y = (MINIMAP_H as f64 - h * scale64) / 2.0;
    Some((scale, x - pad_x / scale64, y - pad_y / scale64))
}

/// 世界点是否落在卡正文浮层区域（正文区只选中不拖动）。
pub fn over_card_body(node: &CanvasFileNode, world: (f64, f64)) -> bool {
    if !matches!(node.node_type.as_str(), "conversation" | "text") {
        return false;
    }
    let (_, sh) = display_size(node);
    world.1 > node.y + 30.0 && world.1 <= node.y + sh
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn card(id: &str, node_type: &str, x: f64, y: f64, w: f64, h: f64) -> CanvasFileNode {
        CanvasFileNode {
            id: id.into(),
            node_type: node_type.into(),
            x,
            y,
            width: Some(w),
            height: Some(h),
            data: json!({}),
        }
    }

    fn session_with(nodes: Vec<CanvasFileNode>) -> Session {
        Session::new(
            "a.atlx".into(),
            CanvasFile {
                schema: atelyx_format::canvas::CANVAS_SCHEMA.into(),
                id: "c".into(),
                title: "a".into(),
                nodes,
                edges: Vec::new(),
                created_at: 0,
                updated_at: 0,
            },
            1,
        )
    }

    /// 拖动分组带成员；拖动成员只动成员自己（组是空间关系，不反向拉整组）。
    #[test]
    fn drag_geometry_covers_group_only_when_group_selected() {
        let group = card("g", "group", 0.0, 0.0, 1000.0, 1000.0);
        let first = card("a", "text", 100.0, 100.0, 200.0, 100.0);
        let second = card("b", "text", 500.0, 500.0, 200.0, 100.0);
        let mut session = session_with(vec![group, first, second]);

        session.view.selection = vec!["g".into()];
        let mut keys: Vec<String> = session.drag_geometry().into_keys().collect();
        keys.sort();
        assert_eq!(keys, ["a", "b", "g"]);

        session.view.selection = vec!["a".into()];
        let keys: Vec<String> = session.drag_geometry().into_keys().collect();
        assert_eq!(keys, ["a"]);
    }

    /// 工具条绘制与命中共用同一组矩形：自右向左放大 / 缩小 / 适应。
    #[test]
    fn zoom_buttons_are_shared_by_draw_and_hit() {
        let buttons = zoom_buttons(400.0);
        assert_eq!(buttons[0].0, "zoom-in");
        assert_eq!(buttons[1].0, "zoom-out");
        assert_eq!(buttons[2].0, "fit");
        let (right, (x, y, w, h)) = buttons[2];
        assert_eq!(right, "fit");
        assert_eq!((w, h), (24.0, 24.0));
        assert!((y - (TOOLBAR_H - 24.0) / 2.0).abs() < f32::EPSILON);
        assert!(((x + w) - (400.0 - 12.0 - 2.0 * 28.0)).abs() < f32::EPSILON);
        assert!(buttons[0].1.0 > buttons[1].1.0 && buttons[1].1.0 > buttons[2].1.0);
    }
}
