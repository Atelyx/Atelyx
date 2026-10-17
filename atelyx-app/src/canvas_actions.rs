//! 画布动作：打开 / 载入 / 保存链与消息处理（本地路径；对称表格模式；空间模式在 space_actions）。

use std::collections::HashMap;
use std::io::Cursor;

use iced::Task;
use serde_json::{json, Value};

use crate::canvas_view::{
    hit_card, hit_resize_handle, node_size, world_from_screen, CanvasMsg, CanvasSession, DragKind, RefContent,
    SaveReason, View, CARD_HEADER_H, CARD_MIN_H, CARD_MIN_W,
};
pub(crate) use crate::canvas_view::{LoadOut, RefRaw};
use crate::{recent_view, vault, App, Message, RepoKind};

/// 画布历史记版合并窗口（与表格 / 空间侧口径一致）。
const HISTORY_COALESCE_MS: i64 = 60_000;

/// 打开 .atlx（脏拦截已由 gate 处理）：置「上次打开」、清旧会话、走载入链。
pub fn open_canvas(app: &mut App, rel: &str) -> Task<Message> {
    app.ui.last_canvas_file = Some(rel.to_string());
    // 清掉上一画布会话：载入在途期间不留旧数据（防串画布写盘）
    app.canvas = None;
    app.history_kind = atelyx_core::history::HistoryKind::Canvas;
    app.schedule_persist();
    load_canvas(app, rel)
}

/// 载入 .atlx（本地 spawn_blocking + 缓存；空间走服务端读取）。回执 gen 守卫。
pub fn load_canvas(app: &App, rel: &str) -> Task<Message> {
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        return crate::space_actions::space_load_canvas(app, rel);
    }
    let gen = app.epoch();
    let state = app.vault.clone();
    let rel_owned = rel.to_string();
    Task::perform(
        async move {
            let r = {
                let rel = rel_owned.clone();
                tokio::task::spawn_blocking(move || {
                    let (root, _, _) = state.session_snapshot()?;
                    let (_, canvas) = vault::read_canvas_file_cached(&state, &root, &rel)?;
                    let docs = prepare_docs(&canvas);
                    let refs = load_refs_local(&root, &canvas);
                    let text_docs = prepare_text_docs(&canvas, &refs);
                    Ok::<LoadOut, String>(LoadOut { canvas, docs, text_docs, refs })
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
            };
            Message::Canvas(CanvasMsg::Loaded(gen, rel_owned, r))
        },
        |m| m,
    )
}

/// 载入回执：构建会话 + 图片字节转句柄 + 最近打开记录 + 在场跟随 + 历史刷新。
fn on_loaded(app: &mut App, gen: u64, file: String, result: Result<LoadOut, String>) -> Task<Message> {
    if gen != app.epoch() || app.ui.last_canvas_file.as_deref() != Some(file.as_str()) {
        return Task::none();
    }
    match result {
        Ok(out) => {
            let mut session = CanvasSession::new(file.clone(), out.canvas);
            session.view.docs = out.docs;
            session.view.text_docs = out.text_docs;
            for (path, raw) in out.refs {
                let content = match raw {
                    RefRaw::Text(t) => RefContent::Text(t),
                    RefRaw::Image { bytes, width, height } => {
                        let handle = iced::widget::image::Handle::from_bytes(bytes);
                        // 消息流附件缩略图与笔记图片共用句柄缓存（键 = 仓库相对路径）
                        app.note_images.insert(path.clone(), handle.clone());
                        RefContent::Image { handle, width, height }
                    }
                    RefRaw::Table { summary } => RefContent::Table { summary },
                    RefRaw::Binary => RefContent::Binary,
                    RefRaw::Missing => RefContent::Missing,
                };
                session.view.refs.insert(path, content);
            }
            // 打开即呈现全貌（覆盖变换渲染；首个指针事件收口进会话状态）
            session.request_fit();
            app.canvas = Some(session);
            // 内容载入成功即登记最近打开（本地与空间模式同链回执）
            recent_view::record(app, &file, recent_view::RecentKind::Canvas);
            // 空间模式在场跟随：名册与协作者据此看到本端正在编辑该画布
            if matches!(app.repo_kind, RepoKind::Space(_)) {
                crate::space_actions::send_presence(app);
            }
            app.reload_history()
        }
        Err(e) => {
            app.notify(crate::NoticeLevel::Error, format!("打开画布失败：{e}"));
            Task::none()
        }
    }
}

// ===== 引用内容载入 =====

/// 卡引用类别（按卡类型与 data.kind 推导）。
pub(crate) enum RefKind {
    Text,
    /// 非图片媒体（读取结果非 UTF-8 时按二进制占位归类）。
    Media,
    Image,
    Table,
}

/// 收集画布内的仓库文件引用（笔记 / 附件 / 表格 / 对话卡消息图片附件；去重保序）。
pub(crate) fn collect_ref_paths(canvas: &atelyx_core::vault::CanvasFile) -> Vec<(String, RefKind)> {
    let mut out: Vec<(String, RefKind)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |path: Option<&str>, kind: RefKind| {
        if let Some(p) = path {
            if !p.is_empty() && seen.insert(p.to_string()) {
                out.push((p.to_string(), kind));
            }
        }
    };
    for n in &canvas.nodes {
        match n.node_type.as_str() {
            "text" => push(n.data.get("file").and_then(Value::as_str), RefKind::Text),
            "table" => push(n.data.get("file").and_then(Value::as_str), RefKind::Table),
            "media" => {
                let kind = if n.data.get("kind").and_then(Value::as_str) == Some("image") {
                    RefKind::Image
                } else {
                    RefKind::Media
                };
                push(n.data.get("file").and_then(Value::as_str), kind);
            }
            "conversation" => {
                // 消息图片附件：字节经同一装载链回填句柄缓存（气泡缩略图渲染源）
                if let Some(attachments) = n.data.get("messages").and_then(|m| m.as_array()) {
                    for m in attachments {
                        if let Some(list) = m.get("attachments").and_then(Value::as_array) {
                            for a in list {
                                if a.get("kind").and_then(Value::as_str) == Some("image") {
                                    push(a.get("file").and_then(Value::as_str), RefKind::Image);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// 图片字节 → 像素尺寸（解码失败 = (0, 0)，绘制侧按占位处理）。
pub(crate) fn image_dimensions(bytes: &[u8]) -> (u32, u32) {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((0, 0))
}

/// 本地读引用内容（与画布同 root 的 safe_join 读取；单条失败不阻断，按缺失 / 二进制归类）。
fn load_refs_local(root: &std::path::Path, canvas: &atelyx_core::vault::CanvasFile) -> HashMap<String, RefRaw> {
    let mut out = HashMap::new();
    for (path, kind) in collect_ref_paths(canvas) {
        let raw = read_ref_local(root, &path, kind);
        out.insert(path, raw);
    }
    out
}

fn read_ref_local(root: &std::path::Path, path: &str, kind: RefKind) -> RefRaw {
    let Ok(abs) = vault::safe_join(root, path, false) else {
        return RefRaw::Missing;
    };
    if !abs.exists() {
        return RefRaw::Missing;
    }
    match kind {
        // 文件存在而读不出文本（非 UTF-8 / 读取失败）= 二进制占位，不冒充缺失
        RefKind::Text | RefKind::Media => std::fs::read_to_string(abs).map(RefRaw::Text).unwrap_or(RefRaw::Binary),
        RefKind::Image => match std::fs::read(abs) {
            Ok(bytes) => {
                let (width, height) = image_dimensions(&bytes);
                RefRaw::Image { bytes, width, height }
            }
            Err(_) => RefRaw::Missing,
        },
        RefKind::Table => match vault::read_table_file(abs.as_path()) {
            Ok(tb) => RefRaw::Table {
                summary: format!("{} 字段 · {} 行", tb.fields.len(), tb.rows.len()),
            },
            Err(_) => RefRaw::Missing,
        },
    }
}

/// 对话卡消息流 markdown 预解析（载入时一次性；只读流消息不变）。
pub(crate) fn prepare_docs(
    canvas: &atelyx_core::vault::CanvasFile,
) -> HashMap<u64, atelyx_core::markdown::MarkdownDocument> {
    let mut docs = HashMap::new();
    for n in &canvas.nodes {
        if n.node_type != "conversation" {
            continue;
        }
        let Some(messages) = n.data.get("messages").and_then(Value::as_array) else {
            continue;
        };
        for m in messages {
            // 引用签名并入缓存键（磁盘引用无 path 字段时签名空，走普通键）
            let sig = crate::chat_actions::mention_sig(m);
            let sources = [
                m.get("displayContent").and_then(Value::as_str),
                m.get("content").and_then(Value::as_str),
            ];
            for src in sources.into_iter().flatten() {
                if src.is_empty() {
                    continue;
                }
                let key = if m["role"] == json!("user") && !sig.is_empty() {
                    crate::chat_actions::doc_key(src, &sig)
                } else {
                    crate::chat_actions::source_hash(src)
                };
                docs.entry(key).or_insert_with(|| crate::markdown_view::parse_document(src));
            }
            if let Some(steps) = m.get("steps").and_then(Value::as_array) {
                for st in steps {
                    if st.get("kind").and_then(Value::as_str) == Some("text") {
                        if let Some(t) = st.get("text").and_then(Value::as_str) {
                            if !t.is_empty() {
                                let key = crate::chat_actions::source_hash(t);
                                docs.entry(key).or_insert_with(|| crate::markdown_view::parse_document(t));
                            }
                        }
                    }
                }
            }
        }
    }
    docs
}

/// 文本卡正文预解析：笔记引用剥 frontmatter（属性归笔记面板，卡内不重复 YAML）、
/// 画布内文本按 `bodyMd` 原样；正文为空（含引用缺失 / 二进制）的卡不入表。
pub(crate) fn prepare_text_docs(
    canvas: &atelyx_core::vault::CanvasFile,
    refs: &HashMap<String, RefRaw>,
) -> HashMap<String, atelyx_core::markdown::MarkdownDocument> {
    let mut docs = HashMap::new();
    for n in &canvas.nodes {
        if n.node_type != "text" {
            continue;
        }
        let source = match n.data.get("file").and_then(Value::as_str) {
            Some(f) => match refs.get(f) {
                Some(RefRaw::Text(body)) => atelyx_core::frontmatter::parse(body).body,
                _ => continue,
            },
            None => n.data.get("bodyMd").and_then(Value::as_str).unwrap_or("").to_string(),
        };
        if source.trim().is_empty() {
            continue;
        }
        docs.insert(n.id.clone(), crate::markdown_view::parse_document(&source));
    }
    docs
}

// ===== 保存链（本地） =====

/// 提交一次画布落盘：与磁盘真源按补丁合并（基线 diff → id 合并 → 队列原子写）。
/// 文件缺失 / 外部替换为其他实体 → 全量重建；冻结快照随回执带回供「落盘期间无新编辑」判定。
pub fn submit_canvas_save(app: &mut App, reason: SaveReason) -> Task<Message> {
    let Some(s) = app.canvas.as_ref() else { return Task::none() };
    if !s.dirty {
        return Task::none();
    }
    // 空间模式：基线 diff → 服务端补丁仲裁（id 不符回退全量），回执走 Space 消息
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        return crate::space_actions::space_submit_canvas_save(app, reason);
    }
    let Some(root) = app.vault.root().ok() else { return Task::none() };
    let gen = app.epoch();
    let file = s.file.clone();
    let baseline = s.saved.clone();
    let frozen = s.data.clone();
    let queue = app.vault.writes.clone();
    let state = app.vault.clone();
    Task::perform(
        async move {
            let r = async {
                let path = vault::safe_join(&root, &file.clone(), false)?;
                let mut disk = match vault::read_canvas_file_cached(&state, &root, &file.clone()) {
                    // 磁盘被外部替换为其他实体：本端全量重建（等同文件缺失）
                    Ok((_, c)) if c.id == frozen.id => c,
                    _ => frozen.clone(),
                };
                if let Some(patch) = vault::compute_canvas_patch(&baseline, &frozen) {
                    vault::apply_canvas_patch(&mut disk, &patch);
                }
                disk.updated_at = crate::now_secs() as i64;
                let json = serde_json::to_string_pretty(&disk).map_err(|e| e.to_string())?;
                queue.write(path.clone(), json).await?;
                vault::cache_put_canvas(&state, &path, &file, &disk);
                Ok(())
            }
            .await;
            Message::Canvas(CanvasMsg::Saved(gen, file, reason, r, frozen))
        },
        |m| m,
    )
}

/// 「落盘期间无新编辑」判定：标题 + 卡 + 连线与冻结快照一致（时间戳不参与）。
fn canvas_matches(s: &CanvasSession, frozen: &atelyx_core::vault::CanvasFile) -> bool {
    s.data.title == frozen.title && s.data.nodes == frozen.nodes && s.data.edges == frozen.edges
}

fn on_saved(
    app: &mut App,
    gen: u64,
    file: String,
    reason: SaveReason,
    result: Result<(), String>,
    frozen: atelyx_core::vault::CanvasFile,
) -> Task<Message> {
    // 身份守卫：gen + 文件路径双校验（防串画布回执）
    if gen != app.epoch() || !app.canvas.as_ref().is_some_and(|s| s.file == file) {
        return Task::none();
    }
    match result {
        Err(e) => {
            app.settle_history_restore(false);
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            s.save_error = Some(e.clone());
            if matches!(reason, SaveReason::Flush) {
                app.notify(crate::NoticeLevel::Error, format!("保存失败，操作未执行：{e}"));
            } else {
                app.notify(crate::NoticeLevel::Error, format!("保存失败：{e}"));
            }
            Task::none()
        }
        Ok(()) => {
            // 只认磁盘事实：冻结快照与当前数据一致才算落盘；期间又有编辑则保持脏，下一轮补写
            let stable = app.canvas.as_ref().is_some_and(|s| canvas_matches(s, &frozen));
            let mut task = Task::none();
            {
                let Some(s) = app.canvas.as_mut() else { return Task::none() };
                s.save_error = None;
                if stable {
                    s.dirty = false;
                    s.saved = frozen.clone();
                }
            }
            if matches!(reason, SaveReason::Flush) {
                // 落盘期间有新编辑（unstable）：重收一轮 Flush——脏拦截已在放行前 flush，
                // 此轮丢失会让关闭 / 切换动作静默挂起
                if stable {
                    task = task.chain(app.after_any_flush());
                } else {
                    task = task.chain(submit_canvas_save(app, SaveReason::Flush));
                }
            }
            if stable {
                app.settle_history_restore(true);
                match serde_json::to_string(&frozen) {
                    Ok(content) => task = task.chain(record_canvas_history(app, &content)),
                    // 序列化失败：宁缺版本也不把空串写入历史（结构体序列化失败为极端异常）
                    Err(e) => app.notify(crate::NoticeLevel::Error, format!("历史版本记录失败：{e}")),
                }
            }
            task
        }
    }
}

/// 画布历史记版（尽力而为；同步侧车写入，走线程池防阻塞）。
fn record_canvas_history(app: &App, content: &str) -> Task<Message> {
    // 空间模式：历史已在保存链内经服务端记录，此处不再本地记版
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        return Task::none();
    }
    let Some(s) = app.canvas.as_ref() else { return Task::none() };
    let rel = s.file.clone();
    let gen = app.epoch();
    let state = app.vault.clone();
    let author = crate::history_author(app);
    let opts = atelyx_core::history::RecordOpts {
        content: content.to_string(),
        action: atelyx_core::history::HistoryAction::Edit,
        author,
        coalesce_edit_ms: Some(HISTORY_COALESCE_MS),
        max_versions: 0,
        note: None,
    };
    Task::perform(
        async move {
            let r = tokio::task::spawn_blocking(move || {
                let (root, _, _) = state.session_snapshot()?;
                atelyx_core::history::record_version(&root, atelyx_core::history::HistoryKind::Canvas, &rel, &opts)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            Message::HistoryRecorded(gen, r.err())
        },
        |m| m,
    )
}

// ===== 消息处理 =====

/// 画布消息入口（对称表格 on_msg；数据版本变化由主循环判脏排自动保存）。
pub fn on_msg(app: &mut App, msg: CanvasMsg) -> Task<Message> {
    match msg {
        CanvasMsg::Loaded(gen, file, result) => on_loaded(app, gen, file, result),
        CanvasMsg::Saved(gen, file, reason, result, frozen) => on_saved(app, gen, file, reason, result, frozen),
        CanvasMsg::Press { pos, size } => on_press(app, pos, size),
        CanvasMsg::PanPress { pos, size } => on_pan_press(app, pos, size),
        CanvasMsg::Move { pos, size } => on_move(app, pos, size),
        CanvasMsg::Release => on_release(app),
        CanvasMsg::Wheel { delta_y, delta_x, pos, size } => on_wheel(app, delta_y, delta_x, pos, size),
        CanvasMsg::Zoom { factor, size } => {
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            commit_fit(s, size);
            let center = iced::Point::new(size.width / 2.0, size.height / 2.0);
            zoom_at(&mut s.view, center, factor);
            s.view.grid_cache.clear();
            s.redraw();
            Task::none()
        }
        CanvasMsg::FitView => {
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            s.request_fit();
            Task::none()
        }
        CanvasMsg::StreamToggle(key) => {
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            if !s.view.expanded.remove(&key) {
                s.view.expanded.insert(key);
            }
            s.redraw();
            Task::none()
        }
    }
}

/// 提交待收口的适应视图（首个携带视口尺寸的指针事件；与视图层覆盖渲染同一纯函数）。
fn commit_fit(s: &mut CanvasSession, size: iced::Size) {
    if s.view.fit_pending {
        s.apply_fit(size.width, size.height);
    }
}

/// 中键平移起手：不改变选中，直接进入平移会话。
fn on_pan_press(app: &mut App, pos: iced::Point, size: iced::Size) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    commit_fit(s, size);
    s.view.drag = Some(crate::canvas_view::DragState {
        kind: DragKind::Pan,
        start: (pos.x, pos.y),
        orig: HashMap::new(),
    });
    s.redraw();
    Task::none()
}

fn on_press(app: &mut App, pos: iced::Point, size: iced::Size) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    commit_fit(s, size);
    let view = View::of(&s.view);
    let world = world_from_screen(&view, pos.x, pos.y);
    // resize 把手优先（仅当前选中卡有把手）
    if let Some(id) = hit_resize_handle(&s.data, &s.view.selection, view.zoom, world) {
        let orig = geometry_of(&s.data, &id);
        s.view.selection = vec![id.clone()];
        s.view.drag = Some(crate::canvas_view::DragState {
            kind: DragKind::Resize(id),
            start: (pos.x, pos.y),
            orig,
        });
        s.redraw();
        return Task::none();
    }
    match hit_card(&s.data, world) {
        Some(i) => {
            let (id, node_type) = (s.data.nodes[i].id.clone(), s.data.nodes[i].node_type.clone());
            s.view.selection = vec![id.clone()];
            // 分组为静态容器不参与拖动；正文走原生浮层的卡（对话 / 文本）卡体按下只选中，拖动从卡头起手
            let overlay_body = matches!(node_type.as_str(), "conversation" | "text")
                && world.1 > s.data.nodes[i].y + CARD_HEADER_H as f64;
            if node_type != "group" && !overlay_body {
                let orig = geometry_of(&s.data, &id);
                s.view.drag = Some(crate::canvas_view::DragState {
                    kind: DragKind::Card,
                    start: (pos.x, pos.y),
                    orig,
                });
            }
            s.redraw();
        }
        None => {
            s.view.selection.clear();
            s.view.drag = Some(crate::canvas_view::DragState {
                kind: DragKind::Pan,
                start: (pos.x, pos.y),
                orig: HashMap::new(),
            });
            s.redraw();
        }
    }
    Task::none()
}

fn on_move(app: &mut App, pos: iced::Point, size: iced::Size) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    commit_fit(s, size);
    // 先取出拖拽会话（克隆轻量字段），再变更会话状态
    let Some(drag) = s.view.drag.as_ref().map(|d| (d.kind.clone(), d.start, d.orig.clone())) else {
        return Task::none();
    };
    let (kind, start, orig) = drag;
    match kind {
        DragKind::Pan => {
            s.view.pan.0 += pos.x - start.0;
            s.view.pan.1 += pos.y - start.1;
            if let Some(d) = s.view.drag.as_mut() {
                d.start = (pos.x, pos.y);
            }
            s.view.grid_cache.clear();
            s.redraw();
        }
        DragKind::Card => {
            let (dx, dy) = (
                ((pos.x - start.0) / s.view.zoom) as f64,
                ((pos.y - start.1) / s.view.zoom) as f64,
            );
            for (id, o) in &orig {
                if let Some(n) = s.data.nodes.iter_mut().find(|n| &n.id == id) {
                    n.x = o.0 + dx;
                    n.y = o.1 + dy;
                }
            }
            // 拖动中只改内存 + 重绘：置脏但不排保存（松手收口才触发落盘期限）
            s.dirty = true;
            s.save_error = None;
            s.view.content_cache.clear();
        }
        DragKind::Resize(id) => {
            let (dx, dy) = (
                ((pos.x - start.0) / s.view.zoom) as f64,
                ((pos.y - start.1) / s.view.zoom) as f64,
            );
            if let (Some(n), Some(o)) = (s.data.nodes.iter_mut().find(|n| n.id == id), orig.get(&id)) {
                n.width = Some((o.2 + dx).max(CARD_MIN_W));
                n.height = Some((o.3 + dy).max(CARD_MIN_H));
            }
            s.dirty = true;
            s.save_error = None;
            s.view.content_cache.clear();
        }
    }
    Task::none()
}

fn on_release(app: &mut App) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    // 卡 / resize 松手 = 拖拽期内容变化正式收口：版本自增触发主循环排自动保存；
    // 平移不产生数据变更
    let data_drag = matches!(
        s.view.drag.as_ref().map(|d| &d.kind),
        Some(DragKind::Card) | Some(DragKind::Resize(_))
    );
    s.view.drag = None;
    if data_drag {
        s.version += 1;
        s.redraw();
    }
    Task::none()
}

fn on_wheel(app: &mut App, delta_y: f32, delta_x: f32, pos: iced::Point, size: iced::Size) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    commit_fit(s, size);
    if app.mods.control() || app.mods.logo() {
        // Ctrl/Cmd+滚轮 = 纵向平移：滚上看见上方内容（画面随之下移）
        // 普通鼠标只有纵向滚轮，横向分量缺席时以纵向分量为准
        s.view.pan.1 += if delta_y != 0.0 { delta_y } else { delta_x };
    } else if app.mods.alt() {
        // Alt+滚轮 = 横向平移：滚上看见左侧内容（同上，仅纵向滚轮的鼠标用纵向分量表达左右）
        s.view.pan.0 += if delta_x != 0.0 { delta_x } else { delta_y };
    } else {
        // 滚轮 = 以光标为锚缩放（winit 滚上为正 → 放大），光标下的世界点保持不动
        let factor = (delta_y * crate::canvas_view::WHEEL_ZOOM_K).exp();
        zoom_at(&mut s.view, pos, factor);
    }
    s.view.grid_cache.clear();
    s.redraw();
    Task::none()
}

/// 光标锚点缩放（钳制上下限）。
fn zoom_at(view: &mut crate::canvas_view::CanvasViewState, cursor: iced::Point, factor: f32) {
    let next = (view.zoom * factor).clamp(crate::canvas_view::MIN_ZOOM, crate::canvas_view::MAX_ZOOM);
    if (next - view.zoom).abs() < f32::EPSILON {
        return;
    }
    let k = next / view.zoom;
    view.pan.0 = cursor.x - (cursor.x - view.pan.0) * k;
    view.pan.1 = cursor.y - (cursor.y - view.pan.1) * k;
    view.zoom = next;
}

/// 卡起始几何快照（拖拽会话用；width/height 按显示尺寸展开，resize 以显示尺寸为基准）。
fn geometry_of(canvas: &atelyx_core::vault::CanvasFile, id: &str) -> HashMap<String, (f64, f64, f64, f64)> {
    canvas
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| {
            let (w, h) = node_size(n);
            HashMap::from([(n.id.clone(), (n.x, n.y, w, h))])
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 引用读取口径：文件存在但读不出文本 = 二进制占位，路径不存在 = 缺失降级。
    #[test]
    fn 引用读取区分二进制占位与缺失() {
        let dir = std::env::temp_dir().join(format!(
            "atelyx-canvas-ref-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // 0xff 在 UTF-8 中不是合法起始字节，read_to_string 必然失败
        std::fs::write(dir.join("bin.dat"), [0xffu8, 0xfe, 0x00, 0x01]).unwrap();
        assert!(matches!(read_ref_local(&dir, "bin.dat", RefKind::Text), RefRaw::Binary));
        assert!(matches!(read_ref_local(&dir, "missing.md", RefKind::Text), RefRaw::Missing));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
