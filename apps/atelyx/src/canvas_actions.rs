//! 画布动作：打开 / 载入 / 保存链 / 白板交互与消息处理（本地路径；对称表格模式；空间模式在 space_actions）。

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use iced::Task;
use serde_json::{json, Value};

use atelyx_core::vault::CanvasFileNode;

use crate::canvas_view::{
    bbox_of_rects, display_size, group_members, hidden_by_collapsed, hit_card, hit_link_handle,
    hit_resize_handle, marquee_hit, snap_move, snap_size, world_from_screen, CanvasKey,
    CanvasMenuAction, CanvasMsg, CanvasSession, CanvasSnapshot, DragKind, GROUP_PAD, RefContent,
    SaveReason, View, CARD_HEADER_H, MARQUEE_MIN, SNAP_GRID, UNDO_DEPTH,
};
pub(crate) use crate::canvas_view::{LoadOut, RefRaw};
use crate::layout_view::{PopupOpen, PopupState};
use crate::{chat_actions, recent_view, vault, App, Message, RepoKind};

/// 画布历史记版合并窗口（与表格 / 空间侧口径一致）。
const HISTORY_COALESCE_MS: i64 = 60_000;
/// 粘贴整组避让的纵向搜索步长（世界坐标，同产出落卡）。
const PASTE_SPOT_STEP: f64 = 24.0;

/// 打开 .atlx（脏拦截已由 gate 处理）：置「上次打开」、清旧会话、走载入链。
pub fn open_canvas(app: &mut App, rel: &str) -> Task<Message> {
    app.ui.last_canvas_file = Some(rel.to_string());
    // 清掉上一画布会话：先停掉卡内 run，载入在途期间不留旧数据（防串画布写盘）
    crate::canvas_chat::detach_canvas(app);
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
                        app.note_image_epoch = app.note_image_epoch.wrapping_add(1);
                        if width > 0 && height > 0 {
                            app.note_image_sizes.insert(path.clone(), (width, height));
                        }
                        RefContent::Image { handle, width, height }
                    }
                    RefRaw::Table { summary, body } => RefContent::Table { summary, body },
                    RefRaw::Binary => RefContent::Binary,
                    RefRaw::Missing => RefContent::Missing,
                };
                session.view.refs.insert(path, content);
            }
            // 打开即呈现全貌（覆盖变换渲染；首个指针事件收口进会话状态）
            session.request_fit();
            // 旧会话（重载 / 回滚后重读）的卡内 run 与面板绑定先收口
            crate::canvas_chat::detach_canvas(app);
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
                body: atelyx_core::table_ops::table_to_text(&tb),
            },
            Err(_) => RefRaw::Missing,
        },
    }
}

/// 按路径扩展名判定读取口径（产出落卡后的新引用按此补内容）。
fn ref_kind_of_path(path: &str) -> RefKind {
    let name = path.rsplit('/').next().unwrap_or(path);
    if atelyx_core::vault::image_ext_of(name).is_some() {
        return RefKind::Image;
    }
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()) {
        Some(e) if e == "atb" => RefKind::Table,
        Some(e) if e == "md" => RefKind::Text,
        _ => RefKind::Media,
    }
}

/// 补读新增引用（本地产出落卡后调用；空间模式走服务端端点）。只读视图缓存，不动数据。
pub(crate) fn load_refs(app: &App, paths: Vec<String>) -> Task<Message> {
    if paths.is_empty() {
        return Task::none();
    }
    let gen = app.epoch();
    if matches!(app.repo_kind, RepoKind::Space(_)) {
        let Some(session) = crate::space_actions::session_of(app) else {
            return Task::none();
        };
        let client = session.client;
        let space_id = session.space_id;
        return Task::perform(
            async move {
                let mut out: HashMap<String, RefRaw> = HashMap::new();
                for path in paths {
                    let raw = match ref_kind_of_path(&path) {
                        RefKind::Image => match client.get_file_bytes(&space_id, &path).await {
                            Ok(bytes) => {
                                let (width, height) = image_dimensions(&bytes);
                                RefRaw::Image { bytes, width, height }
                            }
                            Err(_) => RefRaw::Missing,
                        },
                        RefKind::Table => match client.get_file_text(&space_id, &path).await {
                            Ok(t) => serde_json::from_str::<atelyx_core::vault::TableFile>(&t)
                                .map(|tb| RefRaw::Table {
                                    summary: format!("{} 字段 · {} 行", tb.fields.len(), tb.rows.len()),
                                    body: atelyx_core::table_ops::table_to_text(&tb),
                                })
                                .unwrap_or(RefRaw::Missing),
                            Err(_) => RefRaw::Missing,
                        },
                        RefKind::Text | RefKind::Media => match client.get_file_text(&space_id, &path).await {
                            Ok(t) => RefRaw::Text(t),
                            Err(_) => RefRaw::Missing,
                        },
                    };
                    out.insert(path, raw);
                }
                out
            },
            move |refs| Message::Canvas(CanvasMsg::RefsLoaded(gen, refs)),
        );
    }
    let Ok(root) = app.vault.root() else { return Task::none() };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let mut out = HashMap::new();
                for path in paths {
                    let raw = read_ref_local(&root, &path, ref_kind_of_path(&path));
                    out.insert(path, raw);
                }
                out
            })
            .await
            .unwrap_or_default()
        },
        move |refs| Message::Canvas(CanvasMsg::RefsLoaded(gen, refs)),
    )
}

/// 补读回执：合并引用缓存 + 重建文本卡正文（产出落卡的新笔记能直接显示）。
fn on_refs_loaded(app: &mut App, gen: u64, refs: HashMap<String, RefRaw>) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    let Some(session) = app.canvas.as_mut() else { return Task::none() };
    for (path, raw) in refs {
        let content = match raw {
            RefRaw::Text(t) => RefContent::Text(t),
            RefRaw::Image { bytes, width, height } => {
                let handle = iced::widget::image::Handle::from_bytes(bytes);
                app.note_images.insert(path.clone(), handle.clone());
                app.note_image_epoch = app.note_image_epoch.wrapping_add(1);
                if width > 0 && height > 0 {
                    app.note_image_sizes.insert(path.clone(), (width, height));
                }
                RefContent::Image { handle, width, height }
            }
            RefRaw::Table { summary, body } => RefContent::Table { summary, body },
            RefRaw::Binary => RefContent::Binary,
            RefRaw::Missing => RefContent::Missing,
        };
        session.view.refs.insert(path, content);
    }
    let text_raw: HashMap<String, RefRaw> = session
        .view
        .refs
        .iter()
        .filter_map(|(path, content)| match content {
            RefContent::Text(body) => Some((path.clone(), RefRaw::Text(body.clone()))),
            _ => None,
        })
        .collect();
    session.view.text_docs = prepare_text_docs(&session.data, &text_raw);
    session.redraw();
    Task::none()
}

/// 文本卡正文解析缓存全量重建（载入 / 引用补读 / 远端补丁替换文本卡后调用）。
pub(crate) fn rebuild_text_docs(session: &mut CanvasSession) {
    let text_raw: HashMap<String, RefRaw> = session
        .view
        .refs
        .iter()
        .filter_map(|(path, content)| match content {
            RefContent::Text(body) => Some((path.clone(), RefRaw::Text(body.clone()))),
            _ => None,
        })
        .collect();
    session.view.text_docs = prepare_text_docs(&session.data, &text_raw);
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
                // 内嵌标题随文件名（文件名是唯一真源；内容补丁不改名，此处按路径收取主名）
                disk.title = crate::layout_view::file_label(&file);
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
    // 身份守卫：gen 过期回执作废；写盘成功先广播文件变更（切换文件后回执也须落地事件），
    // 再按当前会话身份决定是否结算
    if gen != app.epoch() {
        return Task::none();
    }
    if result.is_ok() {
        app.plugins.on_file_changed(&file, atelyx_core::plugin_runtime::FileChangeKind::Updated, None);
    }
    if !app.canvas.as_ref().is_some_and(|s| s.file == file) {
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
    crate::canvas_chat::sync_card_chats(app);
    match msg {
        CanvasMsg::Loaded(gen, file, result) => on_loaded(app, gen, file, result),
        CanvasMsg::Saved(gen, file, reason, result, frozen) => on_saved(app, gen, file, reason, result, frozen),
        CanvasMsg::Chat(msg) => crate::canvas_chat::on_msg(app, msg),
        CanvasMsg::Hover(hover) => {
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            if s.view.hover != hover {
                s.view.hover = hover;
                s.redraw();
            }
            Task::none()
        }
        CanvasMsg::RefsLoaded(gen, refs) => on_refs_loaded(app, gen, refs),
        CanvasMsg::Press { pos, size } => on_press(app, pos, size),
        CanvasMsg::PanPress { pos, size } => on_pan_press(app, pos, size),
        CanvasMsg::Context { window, pos, size } => on_context(app, window, pos, size),
        CanvasMsg::DoublePress { window, pos, size } => on_double_press(app, window, pos, size),
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
        CanvasMsg::Menu(action) => {
            // 菜单条目来源：先关弹层再执行（对称表格 Menu）；窗口取自弹层（改名弹层需重开）
            let window = app.popup.as_ref().map(|(w, _)| *w);
            app.dismiss_popup();
            on_menu(app, window, action)
        }
        CanvasMsg::PasteText(text) => on_paste(app, text),
        CanvasMsg::Key(key) => on_key(app, key),
        CanvasMsg::GroupRenameDraft(text) => {
            if let Some((_, draft)) = app
                .canvas
                .as_mut()
                .and_then(|s| s.view.group_rename.as_mut())
            {
                *draft = text;
            }
            Task::none()
        }
        CanvasMsg::MinimapSeek { x, y, viewport } => {
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            // 空画布映射无解：小地图侧已不发 seek，此处再挡一道（防 inf 坐标进会话态）
            if !x.is_finite() || !y.is_finite() {
                return Task::none();
            }
            if s.view.fit_pending {
                s.apply_fit(viewport.0, viewport.1);
            }
            s.view.viewport = viewport;
            let (vw, vh) = viewport;
            s.view.pan.0 = vw / 2.0 - x as f32 * s.view.zoom;
            s.view.pan.1 = vh / 2.0 - y as f32 * s.view.zoom;
            s.view.grid_cache.clear();
            s.redraw();
            Task::none()
        }
    }
}

/// 提交待收口的适应视图（首个携带视口尺寸的指针事件；与视图层覆盖渲染同一纯函数）。
/// 顺带记录视口尺寸（粘贴落位 / 小地图跳转用）。
fn commit_fit(s: &mut CanvasSession, size: iced::Size) {
    s.view.viewport = (size.width, size.height);
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
        mutated: false,
    });
    s.redraw();
    Task::none()
}

fn on_press(app: &mut App, pos: iced::Point, size: iced::Size) -> Task<Message> {
    // 画布空白 / 卡体按压 = 输入面外点：对话卡草稿让出焦点
    crate::canvas_chat::clear_card_focus(app);
    let shift = app.mods.shift();
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    commit_fit(s, size);
    let view = View::of(&s.view);
    let world = world_from_screen(&view, pos.x, pos.y);
    // resize 把手优先（仅单选卡有把手）
    if let Some(id) = hit_resize_handle(&s.data, &s.view.selection, view.zoom, world) {
        push_undo(s);
        let orig = geometry_of(&s.data, &id);
        s.view.selection = vec![id.clone()];
        s.view.drag = Some(crate::canvas_view::DragState {
            kind: DragKind::Resize(id),
            start: (pos.x, pos.y),
            orig,
            mutated: false,
        });
        s.redraw();
        return Task::none();
    }
    // 连接点优先于卡体：悬停 / 选中的资产卡右侧圆点 = 拖线起手
    if let Some(source) = hit_link_handle(
        &s.data,
        s.view.hover.as_deref(),
        &s.view.selection,
        view.zoom,
        world,
    ) {
        s.view.selection = vec![source.clone()];
        s.view.drag = Some(crate::canvas_view::DragState {
            kind: DragKind::Link(source),
            start: (pos.x, pos.y),
            orig: HashMap::new(),
            mutated: false,
        });
        s.view.link_to = Some((pos.x, pos.y));
        s.redraw();
        return Task::none();
    }
    match hit_card(&s.data, world) {
        Some(i) => {
            let (id, node_type) = (s.data.nodes[i].id.clone(), s.data.nodes[i].node_type.clone());
            if shift {
                // Shift+点击 = 切换选中（多选），不起拖拽
                match s.view.selection.iter().position(|x| x == &id) {
                    Some(p) => {
                        s.view.selection.remove(p);
                    }
                    None => s.view.selection.push(id),
                }
                s.redraw();
                return Task::none();
            }
            // 点在已选集内 = 保持多选拖动整集；点在集外 = 单选
            let already = s.view.selection.iter().any(|x| x == &id);
            if !already {
                s.view.selection = vec![id.clone()];
            }
            // 分组不拖体（拖组整体即组头）；正文走原生浮层的卡（对话 / 文本）卡体按下
            // 只选中，拖动从卡头起手
            let overlay_body = matches!(node_type.as_str(), "conversation" | "text")
                && world.1 > s.data.nodes[i].y + CARD_HEADER_H as f64;
            if !overlay_body {
                push_undo(s);
                let orig = drag_geometry(&s.data, &s.view.selection);
                s.view.drag = Some(crate::canvas_view::DragState {
                    kind: DragKind::Card,
                    start: (pos.x, pos.y),
                    orig,
                    mutated: false,
                });
            }
            s.redraw();
        }
        None => {
            // 空白按压 = 框选起手（Shift 框选并入原选中）；平移走中键
            let base = if shift {
                std::mem::take(&mut s.view.selection)
            } else {
                s.view.selection = Vec::new();
                Vec::new()
            };
            s.view.guides.clear();
            s.view.marquee_to = Some((pos.x, pos.y));
            s.view.drag = Some(crate::canvas_view::DragState {
                kind: DragKind::Marquee { base },
                start: (pos.x, pos.y),
                orig: HashMap::new(),
                mutated: false,
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
        // 非拖拽：刷新悬停卡（连接点显形条件）
        let view = View::of(&s.view);
        let world = world_from_screen(&view, pos.x, pos.y);
        let hover = hit_card(&s.data, world).map(|i| s.data.nodes[i].id.clone());
        if hover != s.view.hover {
            s.view.hover = hover;
            s.redraw();
        }
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
        DragKind::Marquee { base } => {
            s.view.marquee_to = Some((pos.x, pos.y));
            // 实时选中：光标世界矩形相交卡 ∪ 起手选中（Shift 框选）
            let view = View::of(&s.view);
            let (ax, ay) = world_from_screen(&view, start.0, start.1);
            let (bx, by) = world_from_screen(&view, pos.x, pos.y);
            let mut hit = marquee_hit(&s.data.nodes, (ax.min(bx), ay.min(by), (ax - bx).abs(), (ay - by).abs()));
            for id in base {
                if !hit.contains(&id) {
                    hit.push(id);
                }
            }
            s.view.selection = hit;
            s.redraw();
        }
        DragKind::Card => {
            let raw_dx = ((pos.x - start.0) / s.view.zoom) as f64;
            let raw_dy = ((pos.y - start.1) / s.view.zoom) as f64;
            // 零位移不动手（点击不触发吸附跳变）
            if raw_dx == 0.0 && raw_dy == 0.0 {
                return Task::none();
            }
            // 移动集包围盒对静态节点吸附：参考线优先，无则网格取整
            let rects: Vec<(f64, f64, f64, f64)> = orig.values().copied().collect();
            let (bx, by, bw, bh) = bbox_of_rects(&rects);
            let moving: HashSet<&str> = orig.keys().map(|id| id.as_str()).collect();
            let statics: Vec<(f64, f64, f64, f64)> = s
                .data
                .nodes
                .iter()
                .filter(|n| {
                    !moving.contains(n.id.as_str()) && !hidden_by_collapsed(&s.data.nodes, &n.id)
                })
                .map(|n| {
                    let (w, h) = display_size(n);
                    (n.x, n.y, w, h)
                })
                .collect();
            let snap = snap_move(&statics, (bx + raw_dx, by + raw_dy, bw, bh));
            let (dx, dy) = (raw_dx + snap.dx, raw_dy + snap.dy);
            for (id, o) in &orig {
                if let Some(n) = s.data.nodes.iter_mut().find(|n| &n.id == id) {
                    n.x = o.0 + dx;
                    n.y = o.1 + dy;
                }
            }
            s.view.guides = snap.guides;
            mark_drag_mutated(s);
            // 拖动中只改内存 + 重绘：置脏但不排保存（松手收口才触发落盘期限）
            s.dirty = true;
            s.save_error = None;
            s.view.content_cache.clear();
        }
        DragKind::Resize(id) => {
            let raw_dx = ((pos.x - start.0) / s.view.zoom) as f64;
            let raw_dy = ((pos.y - start.1) / s.view.zoom) as f64;
            if raw_dx == 0.0 && raw_dy == 0.0 {
                return Task::none();
            }
            if let (Some(n), Some(o)) = (s.data.nodes.iter_mut().find(|n| n.id == id), orig.get(&id)) {
                let (w, h) = snap_size(o.2 + raw_dx, o.3 + raw_dy);
                n.width = Some(w);
                n.height = Some(h);
            }
            mark_drag_mutated(s);
            s.dirty = true;
            s.save_error = None;
            s.view.content_cache.clear();
        }
        DragKind::Link(_) => {
            s.view.link_to = Some((pos.x, pos.y));
            s.redraw();
        }
    }
    Task::none()
}

/// 标记拖拽会话已改写几何（松手判定无操作回退用）。
fn mark_drag_mutated(s: &mut CanvasSession) {
    if let Some(d) = s.view.drag.as_mut() {
        d.mutated = true;
    }
}

fn on_release(app: &mut App) -> Task<Message> {
    // 拖线收口：先取出会话与落点，再改画布数据（append_card_ref 要可变借用 App）
    let mut link: Option<(String, String)> = None;
    {
        let Some(s) = app.canvas.as_mut() else { return Task::none() };
        let to = s.view.link_to;
        let marquee_end = s.view.marquee_to;
        let (kind, mutated, base, start) = match s.view.drag.take() {
            Some(d) => {
                let base = match &d.kind {
                    DragKind::Marquee { base } => Some(base.clone()),
                    _ => None,
                };
                (Some(d.kind), d.mutated, base, Some(d.start))
            }
            None => (None, false, None, None),
        };
        s.view.link_to = None;
        s.view.marquee_to = None;
        s.view.guides.clear();
        match kind {
            Some(DragKind::Link(source)) => {
                let Some((x, y)) = to else { return Task::none() };
                let view = View::of(&s.view);
                let world = world_from_screen(&view, x, y);
                if let Some(i) = hit_card(&s.data, world) {
                    let target = &s.data.nodes[i];
                    if target.node_type == "conversation" && target.id != source {
                        link = Some((target.id.clone(), source));
                    }
                }
            }
            Some(DragKind::Card) | Some(DragKind::Resize(_)) => {
                if mutated {
                    // 拖拽期内容变化正式收口：版本自增触发主循环排自动保存
                    s.version += 1;
                } else {
                    // 起手未动 = 无操作：丢弃起手快照（点击不产生撤销单元）
                    s.view.undo.pop();
                }
                s.redraw();
            }
            Some(DragKind::Marquee { .. }) => {
                // 原地按下-松开（位移低于阈值）= 点击空白：还原起手选中，不产生框选
                if let (Some((sx, sy)), Some((tx, ty))) = (start, marquee_end) {
                    if (tx - sx).abs() < MARQUEE_MIN && (ty - sy).abs() < MARQUEE_MIN {
                        s.view.selection = base.unwrap_or_default();
                        s.redraw();
                    }
                }
            }
            _ => {}
        }
    }
    if let Some((target, source)) = link {
        crate::canvas_chat::append_card_ref(app, &target, &source);
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

/// 卡起始几何快照（width/height 按显示尺寸展开，resize 以显示尺寸为基准）。
fn geometry_of(canvas: &atelyx_core::vault::CanvasFile, id: &str) -> HashMap<String, (f64, f64, f64, f64)> {
    canvas
        .nodes
        .iter()
        .find(|n| n.id == id)
        .map(|n| {
            let (w, h) = display_size(n);
            HashMap::from([(n.id.clone(), (n.x, n.y, w, h))])
        })
        .unwrap_or_default()
}

/// 拖动几何快照：选中集 ∪ 选中分组的成员（收起态成员随组平移，展开后相对位置不变；
/// 单选拖组与多选混拖共用此口径，成员不因组移动而掉组）。
fn drag_geometry(
    canvas: &atelyx_core::vault::CanvasFile,
    selection: &[String],
) -> HashMap<String, (f64, f64, f64, f64)> {
    let mut ids: Vec<String> = selection.to_vec();
    for id in selection {
        if canvas.nodes.iter().any(|n| n.id == *id && n.node_type == "group") {
            for m in group_members(&canvas.nodes, id) {
                if !ids.contains(&m) {
                    ids.push(m);
                }
            }
        }
    }
    canvas
        .nodes
        .iter()
        .filter(|n| ids.iter().any(|id| id == &n.id))
        .map(|n| {
            let (w, h) = display_size(n);
            (n.id.clone(), (n.x, n.y, w, h))
        })
        .collect()
}

// ===== 撤销（快照式，对称表格） =====

/// 当前数据快照。
fn current_snapshot(s: &CanvasSession) -> CanvasSnapshot {
    CanvasSnapshot { nodes: s.data.nodes.clone(), edges: s.data.edges.clone() }
}

/// 压入撤销快照（变更前调用；新快照清空重做栈）。
pub(crate) fn push_undo(s: &mut CanvasSession) {
    s.view.redo.clear();
    push_snapshot(s);
}

/// 压入快照并维持栈深上限（重做路径回填撤销栈用，不动重做栈）。
fn push_snapshot(s: &mut CanvasSession) {
    s.view.undo.push(current_snapshot(s));
    if s.view.undo.len() > UNDO_DEPTH {
        s.view.undo.remove(0);
    }
}

/// 恢复快照：置脏 + 版本自增（主循环排自动保存），选中收敛到仍存在的卡。
fn restore_snapshot(s: &mut CanvasSession, snap: CanvasSnapshot) {
    s.data.nodes = snap.nodes;
    s.data.edges = snap.edges;
    s.view.selection.retain(|id| s.data.nodes.iter().any(|n| &n.id == id));
    s.view.hover = None;
    s.touch();
}

/// 任一对话卡在流式（含 Hub 在途 run）：数据回滚类操作（撤销 / 重做）不接，
/// 防 run 直写消息与快照回滚互相踩。
fn any_streaming(s: &CanvasSession) -> bool {
    s.view.card_chat.values().any(|c| c.streaming)
        || s
            .data
            .nodes
            .iter()
            .filter(|n| n.node_type == "conversation")
            .any(|n| chat_actions::hub().is_running(&crate::canvas_chat::session_id(&n.id)))
}

fn undo(app: &mut App) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    if any_streaming(s) {
        return Task::none();
    }
    let Some(snap) = s.view.undo.pop() else { return Task::none() };
    s.view.redo.push(current_snapshot(s));
    restore_snapshot(s, snap);
    Task::none()
}

fn redo(app: &mut App) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    if any_streaming(s) {
        return Task::none();
    }
    let Some(snap) = s.view.redo.pop() else { return Task::none() };
    push_snapshot(s);
    restore_snapshot(s, snap);
    Task::none()
}

// ===== 键盘命令 =====

/// 画布键盘命令就绪：画布在场 + 无弹层 + 卡输入面未聚焦 +
/// 最近按压窗口（缺省主窗）的活动视图是画布（键盘随焦点窗口走）。
pub(crate) fn key_ready(app: &App) -> bool {
    let Some(s) = app.canvas.as_ref() else { return false };
    // 拖拽 / 框选会话进行中不接键盘命令（中途回滚与几何改写互相踩）
    if app.popup.is_some()
        || s.view.drag.is_some()
        || s.view.card_chat.values().any(|c| c.draft.focused)
    {
        return false;
    }
    app.focused_window
        .or(app.main_id)
        .is_some_and(|w| crate::layout_view::window_shows_canvas(app, w))
}

/// commands::CommandId → 画布键盘消息（非画布命令 = None）。
pub(crate) fn canvas_key_msg(id: crate::commands::CommandId) -> Option<CanvasMsg> {
    use crate::commands::CommandId as C;
    Some(CanvasMsg::Key(match id {
        C::CanvasUndo => CanvasKey::Undo,
        C::CanvasRedo => CanvasKey::Redo,
        C::CanvasCopy => CanvasKey::Copy,
        C::CanvasPaste => CanvasKey::Paste,
        C::CanvasSelectAll => CanvasKey::SelectAll,
        C::CanvasClear => CanvasKey::DeleteSelection,
        _ => return None,
    }))
}

fn on_key(app: &mut App, key: CanvasKey) -> Task<Message> {
    match key {
        CanvasKey::Undo => undo(app),
        CanvasKey::Redo => redo(app),
        CanvasKey::Copy => copy_selection(app),
        CanvasKey::Paste => iced::clipboard::read()
            .map(|t| Message::Canvas(CanvasMsg::PasteText(t.unwrap_or_default()))),
        CanvasKey::SelectAll => {
            select_all(app);
            Task::none()
        }
        CanvasKey::DeleteSelection => delete_selection(app),
    }
}

/// 全选可见卡（不含分组——组是容器，单击可选中）。
fn select_all(app: &mut App) {
    let Some(s) = app.canvas.as_mut() else { return };
    s.view.selection = s
        .data
        .nodes
        .iter()
        .filter(|n| n.node_type != "group" && !hidden_by_collapsed(&s.data.nodes, &n.id))
        .map(|n| n.id.clone())
        .collect();
    s.redraw();
}

/// 删除选中：组只删容器（成员保留），卡连带关联边与卡内 run。
fn delete_selection(app: &mut App) -> Task<Message> {
    let Some(s) = app.canvas.as_ref() else { return Task::none() };
    if s.view.selection.is_empty() {
        return Task::none();
    }
    // 被对端独占编辑的对话卡不可删：静默跳过，不阻断其余选中卡的删除
    let locked_by_peer: Vec<String> = s
        .view
        .selection
        .iter()
        .filter(|id| {
            matches!(
                crate::space_actions::conversation_lock_holder(app, id),
                crate::space_actions::LockHolder::Peer { .. }
            )
        })
        .cloned()
        .collect();
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    push_undo(s);
    let ids: HashSet<String> = s
        .view
        .selection
        .drain(..)
        .filter(|id| !locked_by_peer.contains(id))
        .collect();
    let removed_conversations: Vec<String> = s
        .data
        .nodes
        .iter()
        .filter(|n| n.node_type == "conversation" && ids.contains(&n.id))
        .map(|n| n.id.clone())
        .collect();
    s.data.nodes.retain(|n| !ids.contains(&n.id));
    s.data.edges.retain(|e| !ids.contains(&e.source) && !ids.contains(&e.target));
    s.touch();
    for id in removed_conversations {
        // 停 run + 解绑面板同会话视图（card_chat 运行态由 sync_card_chats 收敛）
        crate::canvas_chat::detach_node(app, &id);
    }
    Task::none()
}

// ===== 复制粘贴 =====

/// 复制选中卡为带标记 JSON（组是容器不复制；对话卡消息全量随卡）。
/// 跨画布粘贴同格式可用；外部文本进画布被忽略。
fn copy_selection(app: &mut App) -> Task<Message> {
    let Some(s) = app.canvas.as_ref() else { return Task::none() };
    let mut cards = Vec::new();
    for n in &s.data.nodes {
        if n.node_type == "group" || !s.view.selection.iter().any(|sel| sel == &n.id) {
            continue;
        }
        match serde_json::to_value(n) {
            Ok(v) => cards.push(v),
            // 坐标含 NaN 等非法值：整次复制放弃并提示，不写半截剪贴板
            Err(e) => {
                app.notify(crate::NoticeLevel::Error, format!("复制失败：{e}"));
                return Task::none();
            }
        }
    }
    if cards.is_empty() {
        return Task::none();
    }
    let payload = json!({ "atelyxCanvas": 1, "cards": cards });
    iced::clipboard::write(payload.to_string())
}

/// 粘贴回执：解析带标记 JSON → 新 id（对话消息 id 一并重生成）→ 视口中心落位
/// （网格取整 + 逐行下移避让）→ 选中新卡。非画布格式忽略。
fn on_paste(app: &mut App, text: String) -> Task<Message> {
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    let Ok(payload) = serde_json::from_str::<Value>(&text) else {
        return Task::none();
    };
    if payload.get("atelyxCanvas").and_then(Value::as_u64) != Some(1) {
        return Task::none();
    }
    let Some(cards) = payload.get("cards").and_then(Value::as_array) else {
        return Task::none();
    };
    let parsed: Result<Vec<CanvasFileNode>, _> =
        cards.iter().cloned().map(serde_json::from_value).collect();
    let Ok(mut nodes) = parsed else {
        return Task::none();
    };
    if nodes.is_empty() {
        return Task::none();
    }
    // 新节点 id；对话消息 id 重生成防多副本同 id（消息内 refs 的 nodeId 保留原指向：
    // 消费判定按 id 匹配不命中 = 未消费，不产生错误数据）
    for n in &mut nodes {
        if n.node_type == "conversation" {
            if let Some(msgs) = n.data.get_mut("messages").and_then(Value::as_array_mut) {
                for m in msgs {
                    if let Some(mid) = m.get_mut("id") {
                        *mid = Value::String(chat_actions::uuid_v4());
                    }
                }
            }
        }
        n.id = chat_actions::uuid_v4();
    }
    // 落位：粘贴集包围盒左上对到视口中心（网格取整），整组逐行下移避让现有节点
    let rects: Vec<(f64, f64, f64, f64)> = nodes
        .iter()
        .map(|n| {
            let (w, h) = display_size(n);
            (n.x, n.y, w, h)
        })
        .collect();
    let (bx0, by0, bw, bh) = bbox_of_rects(&rects);
    let (vw, vh) = s.view.viewport;
    let view = View::of(&s.view);
    let (cx, cy) = world_from_screen(&view, vw / 2.0, vh / 2.0);
    let tx = ((cx - bw / 2.0) / SNAP_GRID).round() * SNAP_GRID;
    let mut ty = ((cy - bh / 2.0) / SNAP_GRID).round() * SNAP_GRID;
    while s.data.nodes.iter().any(|n| {
        let (w, h) = display_size(n);
        tx < n.x + w && tx + bw > n.x && ty < n.y + h && ty + bh > n.y
    }) {
        ty += PASTE_SPOT_STEP;
    }
    let (dx, dy) = (tx - bx0, ty - by0);
    for n in &mut nodes {
        n.x += dx;
        n.y += dy;
    }
    {
        let Some(s) = app.canvas.as_mut() else { return Task::none() };
        push_undo(s);
        let ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
        s.data.nodes.extend(nodes);
        s.view.selection = ids;
        s.touch();
    }
    // 新对话卡运行态即时对齐（否则输入面要等下一条画布消息才出现）
    crate::canvas_chat::sync_card_chats(app);
    Task::none()
}

// ===== 右键菜单与分组 =====

/// 右键菜单起手：命中卡（未选中则单选）→ 按形态选菜单；空白 = 空白菜单（选中保留）。
fn on_context(
    app: &mut App,
    window: iced::window::Id,
    pos: iced::Point,
    size: iced::Size,
) -> Task<Message> {
    crate::canvas_chat::clear_card_focus(app);
    let Some(s) = app.canvas.as_mut() else { return Task::none() };
    if s.view.drag.is_some() {
        return Task::none();
    }
    commit_fit(s, size);
    let view = View::of(&s.view);
    let world = world_from_screen(&view, pos.x, pos.y);
    let state = match hit_card(&s.data, world) {
        Some(i) => {
            let (id, node_type) = (s.data.nodes[i].id.clone(), s.data.nodes[i].node_type.clone());
            if !s.view.selection.iter().any(|x| x == &id) {
                s.view.selection = vec![id.clone()];
                s.redraw();
            }
            // 多选优先：含组的批量选中右键任一成员都出批量菜单（删除 / 编组作用整个选中集）
            if s.view.selection.len() > 1 {
                PopupState::CanvasMultiMenu
            } else if node_type == "group" {
                PopupState::CanvasGroupMenu { node_id: id }
            } else {
                PopupState::CanvasCardMenu { node_id: id }
            }
        }
        // 空白不清选中：菜单「复制」作用于当前选中集（左键原地点击仍清选）
        None => PopupState::CanvasBlankMenu { x: world.0, y: world.1 },
    };
    // 锚点取窗口坐标（pos 是画布部件局部坐标，弹层按窗口视口定位）；经 PopupOpen 统一复位 pane
    let point = app.cursor_for(window);
    Task::done(Message::PopupOpen(window, PopupOpen { point, flip_y: None, state, min_width: 0.0 }))
}

/// 右键菜单动作执行（弹层已由 Menu 入口关闭）。
fn on_menu(app: &mut App, window: Option<iced::window::Id>, action: CanvasMenuAction) -> Task<Message> {
    match action {
        CanvasMenuAction::DeleteSelection => delete_selection(app),
        CanvasMenuAction::CopySelection => copy_selection(app),
        CanvasMenuAction::Paste => iced::clipboard::read()
            .map(|t| Message::Canvas(CanvasMsg::PasteText(t.unwrap_or_default()))),
        CanvasMenuAction::CreateGroupFromSelection => {
            create_group_from_selection(app);
            Task::none()
        }
        CanvasMenuAction::CreateGroupAt { x, y } => {
            create_group_at(app, x, y);
            Task::none()
        }
        CanvasMenuAction::GroupColor { node, index } => {
            set_group_color(app, &node, index);
            Task::none()
        }
        CanvasMenuAction::GroupRenameStart { node } => {
            let Some(w) = window else { return Task::none() };
            let point = app.cursor_for(w);
            group_rename_start(app, node, w, point)
        }
        CanvasMenuAction::GroupRenameCommit { node, name } => {
            group_rename_commit(app, node, name);
            Task::none()
        }
        CanvasMenuAction::ToggleCollapse { node } => {
            toggle_collapse(app, &node);
            Task::none()
        }
        CanvasMenuAction::CreateConversationAt { x, y } => {
            create_conversation_at(app, x, y);
            Task::none()
        }
        CanvasMenuAction::CreateTextAt { x, y } => {
            create_text_at(app, x, y);
            Task::none()
        }
        CanvasMenuAction::OpenInPanel(id) => crate::canvas_chat::on_msg(
            app,
            crate::canvas_chat::CanvasChatMsg::OpenInPanel(id),
        ),
    }
}

/// 双击：分组标题 = 改名、组体 = 收起 / 展开；表格卡 / 笔记引用卡 = 打开文件。
fn on_double_press(
    app: &mut App,
    window: iced::window::Id,
    pos: iced::Point,
    size: iced::Size,
) -> Task<Message> {
    crate::canvas_chat::clear_card_focus(app);
    enum Double {
        Rename(String),
        Collapse(String),
        Open(String),
    }
    let hit = {
        let Some(s) = app.canvas.as_mut() else { return Task::none() };
        commit_fit(s, size);
        let view = View::of(&s.view);
        let world = world_from_screen(&view, pos.x, pos.y);
        let Some(i) = hit_card(&s.data, world) else { return Task::none() };
        let (id, node_type) = (s.data.nodes[i].id.clone(), s.data.nodes[i].node_type.clone());
        match node_type.as_str() {
            "group" if world.1 <= s.data.nodes[i].y + CARD_HEADER_H as f64 => Double::Rename(id),
            "group" => Double::Collapse(id),
            "table" | "text" => {
                // 表格卡双击开表、笔记引用卡双击开笔记（画布内文本无双开形态）
                match s.data.nodes[i].data.get("file").and_then(Value::as_str) {
                    Some(f) => Double::Open(f.to_string()),
                    None => return Task::none(),
                }
            }
            _ => return Task::none(),
        }
    };
    match hit {
        Double::Rename(node) => {
            // 弹层锚点用窗口坐标（pos 是画布部件局部坐标）
            let point = app.cursor_for(window);
            group_rename_start(app, node, window, point)
        }
        Double::Collapse(node) => {
            toggle_collapse(app, &node);
            Task::none()
        }
        Double::Open(rel) => Task::done(Message::OpenFile(rel)),
    }
}

/// 现有组数轮转色号（"1"-"6"）。
fn next_group_color(canvas: &atelyx_core::vault::CanvasFile) -> usize {
    let count = canvas.nodes.iter().filter(|n| n.node_type == "group").count();
    count % crate::ui::palette::CANVAS_GROUP_COLORS.len() + 1
}

/// 以选中卡建组：包围盒外扩 GROUP_PAD，成员按中心点自动归属（纯空间判定不落盘）。
fn create_group_from_selection(app: &mut App) {
    let Some(s) = app.canvas.as_mut() else { return };
    let rects: Vec<(f64, f64, f64, f64)> = s
        .data
        .nodes
        .iter()
        .filter(|n| n.node_type != "group" && s.view.selection.iter().any(|sel| sel == &n.id))
        .map(|n| {
            let (w, h) = display_size(n);
            (n.x, n.y, w, h)
        })
        .collect();
    if rects.is_empty() {
        return;
    }
    push_undo(s);
    let (x0, y0, w, h) = bbox_of_rects(&rects);
    let group = CanvasFileNode {
        id: chat_actions::uuid_v4(),
        node_type: "group".into(),
        x: x0 - GROUP_PAD,
        y: y0 - GROUP_PAD,
        width: Some(w + GROUP_PAD * 2.0),
        height: Some(h + GROUP_PAD * 2.0),
        data: json!({ "color": next_group_color(&s.data).to_string() }),
    };
    let gid = group.id.clone();
    s.data.nodes.push(group);
    s.view.selection = vec![gid];
    s.touch();
}

/// 空白处新建空组（尺寸缺省 = 类型默认，落点网格取整）。
fn create_group_at(app: &mut App, x: f64, y: f64) {
    let Some(s) = app.canvas.as_mut() else { return };
    push_undo(s);
    let group = CanvasFileNode {
        id: chat_actions::uuid_v4(),
        node_type: "group".into(),
        x: (x / SNAP_GRID).round() * SNAP_GRID,
        y: (y / SNAP_GRID).round() * SNAP_GRID,
        width: None,
        height: None,
        data: json!({ "color": next_group_color(&s.data).to_string() }),
    };
    let gid = group.id.clone();
    s.data.nodes.push(group);
    s.view.selection = vec![gid];
    s.touch();
}

/// 空白处新建对话卡（缺省尺寸；Agent 与模型不指定 = 发送时回落内置「对话」与仓库默认模型）。
fn create_conversation_at(app: &mut App, x: f64, y: f64) {
    create_card_at(app, "conversation", json!({}), x, y);
}

/// 空白处新建画布内文本卡（正文空，随 `.atlx` 内嵌）。
fn create_text_at(app: &mut App, x: f64, y: f64) {
    create_card_at(app, "text", json!({ "title": "文本", "bodyMd": "" }), x, y);
}

/// 落点网格取整建卡并选中（尺寸按类型缺省回落）。
fn create_card_at(app: &mut App, node_type: &str, data: Value, x: f64, y: f64) {
    let Some(s) = app.canvas.as_mut() else { return };
    push_undo(s);
    let node = CanvasFileNode {
        id: chat_actions::uuid_v4(),
        node_type: node_type.into(),
        x: (x / SNAP_GRID).round() * SNAP_GRID,
        y: (y / SNAP_GRID).round() * SNAP_GRID,
        width: None,
        height: None,
        data,
    };
    let id = node.id.clone();
    s.data.nodes.push(node);
    s.view.selection = vec![id];
    s.touch();
    if node_type == "conversation" {
        // 新对话卡运行态即时对齐（否则输入面要等下一条画布消息才出现）
        crate::canvas_chat::sync_card_chats(app);
    }
}

/// 分组改色（data.color = "1"-"6"）。
fn set_group_color(app: &mut App, node_id: &str, index: usize) {
    let Some(s) = app.canvas.as_mut() else { return };
    if !s
        .data
        .nodes
        .iter()
        .any(|n| n.id == node_id && n.node_type == "group")
    {
        return;
    }
    push_undo(s);
    if let Some(n) = s.data.nodes.iter_mut().find(|n| n.id == node_id) {
        if let Some(obj) = n.data.as_object_mut() {
            obj.insert("color".into(), Value::String(index.to_string()));
        }
    }
    s.touch();
}

/// 分组收起 / 展开（data.collapsed；展开态删字段回缺省）。
fn toggle_collapse(app: &mut App, node_id: &str) {
    let Some(s) = app.canvas.as_mut() else { return };
    let Some(g) = s
        .data
        .nodes
        .iter()
        .find(|n| n.id == node_id && n.node_type == "group")
    else {
        return;
    };
    let collapsing = g.data.get("collapsed").and_then(Value::as_bool) != Some(true);
    push_undo(s);
    if let Some(n) = s.data.nodes.iter_mut().find(|n| n.id == node_id) {
        if let Some(obj) = n.data.as_object_mut() {
            if collapsing {
                obj.insert("collapsed".into(), Value::Bool(true));
            } else {
                obj.remove("collapsed");
            }
        }
    }
    if collapsing {
        // 隐藏成员不再可见：从选中集移除（防删除 / 复制作用于不可见卡）
        let nodes = &s.data.nodes;
        s.view
            .selection
            .retain(|id| !hidden_by_collapsed(nodes, id));
    }
    s.touch();
}

/// 分组改名：初始化弹层草稿并打开（锚点 = 给定屏幕点）。
fn group_rename_start(
    app: &mut App,
    node: String,
    window: iced::window::Id,
    point: iced::Point,
) -> Task<Message> {
    let label = app
        .canvas
        .as_ref()
        .and_then(|s| s.data.nodes.iter().find(|n| n.id == node))
        .and_then(|n| n.data.get("label"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Some(s) = app.canvas.as_mut() {
        s.view.group_rename = Some((node.clone(), label));
    }
    Task::done(Message::PopupOpen(
        window,
        PopupOpen {
            point,
            flip_y: None,
            state: PopupState::CanvasGroupRename { node_id: node },
            min_width: 0.0,
        },
    ))
}

/// 分组改名提交（空名回占位：不落空串，删字段）。
fn group_rename_commit(app: &mut App, node: String, name: String) {
    let name = name.trim().to_string();
    let Some(s) = app.canvas.as_mut() else { return };
    if !s
        .data
        .nodes
        .iter()
        .any(|n| n.id == node && n.node_type == "group")
    {
        return;
    }
    push_undo(s);
    if let Some(n) = s.data.nodes.iter_mut().find(|n| n.id == node) {
        if let Some(obj) = n.data.as_object_mut() {
            if name.is_empty() {
                obj.remove("label");
            } else {
                obj.insert("label".into(), Value::String(name));
            }
        }
    }
    s.view.group_rename = None;
    s.touch();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 撤销栈语义：变更前 push 清重做栈（新分支生效），重做回填不清重做栈
    /// （多步重做不被单步清空）。
    #[test]
    fn 撤销栈压入与重做回填语义() {
        let empty = atelyx_core::vault::CanvasFile {
            schema: "atelyx-canvas/v1".into(),
            id: "c1".into(),
            title: "t".into(),
            nodes: vec![],
            edges: vec![],
            created_at: 0,
            updated_at: 0,
        };
        let mut s = CanvasSession::new("t.atlx".into(), empty);
        push_undo(&mut s);
        push_undo(&mut s);
        assert_eq!(s.view.undo.len(), 2);
        assert!(s.view.redo.is_empty());
        // 重做路径回填：撤销栈加深，重做栈不动
        s.view.redo.push(current_snapshot(&s));
        push_snapshot(&mut s);
        assert_eq!(s.view.undo.len(), 3);
        assert_eq!(s.view.redo.len(), 1);
    }

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
