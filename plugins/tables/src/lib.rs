//! 表格插件入口：整块表格视图（网格 / 编辑 / 样式 / 图片 / 灯箱 / 菜单）与保存链都在本插件，
//! 宿主只负责面板壳、输入转发与按帧绘制；表模型与纯运算来自格式库 `atelyx-format::table`。

mod edit;
mod editor;
mod format;
mod geom;
mod images;
mod input;
mod lightbox;
mod menu;
mod model;
mod paint;
mod presence;
mod prompt;
mod service;
mod session;
mod text;
mod theme;
mod xlsx;

use std::sync::{Arc, Mutex};

use atelyx_plugin_protocol::event::{self, PublishParams};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, SurfaceFrame};
use atelyx_plugin_protocol::method::{
    ContributionSlot, MenuOpenParams, MenuSearch, SurfaceAttachedParams, SurfaceDetachedParams,
};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};

use crate::model::{Session, Size};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.tables";
/// 表格视图贡献 id（面标识 `plugin:atelyx.tables:table` 的贡献段）。
const VIEW: &str = "table";
/// 面板头部声明的目标面板 id（壳侧表格面板视图 id）。
const PANEL_TARGET: &str = "table";
/// 长按判定期间的帧拍频率（缩略图按住等长按到点）。
const TICK_HZ: u16 = 30;
/// 编辑光标闪烁帧拍（500ms 一次翻转）。
const CARET_BLINK_HZ: u16 = 2;

/// 自动保存防抖（毫秒，与宿主保存链同口径）。
const AUTOSAVE_DEBOUNCE_MS: u64 = 500;

/// 运行态：会话 + 挂载标记 + 保存防抖。`Arc` 共享给防抖保存任务（任务只持状态，不持插件实例）。
pub(crate) struct PluginState {
    pub session: Mutex<Option<Session>>,
    attached: Mutex<bool>,
    /// 防抖保存：世代 + 是否已有工作在跑（高频编辑只保留一个等待线程）。
    save_debounce: Mutex<SaveDebounce>,
    /// 对端选区在场（同空间同表格）。
    presence: Mutex<presence::Presence>,
    /// 面最近一次上报的尺寸（会话未建立时也要记住，建立后补进视图态）。
    surface_size: Mutex<Size>,
}

/// 自动保存防抖状态：新改动推后世代，同一时刻最多一个等待线程。
#[derive(Default)]
struct SaveDebounce {
    generation: u64,
    running: bool,
}

impl PluginState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            session: Mutex::new(None),
            attached: Mutex::new(false),
            save_debounce: Mutex::new(SaveDebounce::default()),
            presence: Mutex::new(presence::Presence::default()),
            surface_size: Mutex::new(Size { width: 0.0, height: 0.0 }),
        })
    }

    fn is_attached(&self) -> bool {
        self.attached.lock().map(|attached| *attached).unwrap_or(false)
    }

    /// 取会话做只读操作（无会话 = None）。
    pub(crate) fn with_session<R>(&self, read: impl FnOnce(&Session) -> R) -> Option<R> {
        let guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_ref().map(read)
    }

    /// 取会话做写操作（无会话 = None）。
    pub(crate) fn with_session_mut<R>(&self, write: impl FnOnce(&mut Session) -> R) -> Option<R> {
        let mut guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_mut().map(write)
    }

    /// 提交整面帧：网格 + 命中区 + 键盘声明。
    /// 先在估算行段下出帧，缺项按批向量算（每批 ≤ 512 条），落地后重建布局再出精确帧。
    pub(crate) async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        for _ in 0..2 {
            let misses = self
                .with_session_mut(|session| {
                    session
                        .view
                        .layout
                        .as_mut()
                        .map(|layout| layout.wrap.take_misses())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            if misses.is_empty() {
                break;
            }
            if !self.measure_into_cache(ctx, misses).await {
                break;
            }
            self.with_session_mut(|session| session.rebuild_layout());
        }
        let size = self
            .surface_size
            .lock()
            .map(|size| *size)
            .unwrap_or(Size { width: 0.0, height: 0.0 });
        self.with_session_mut(|session| session.view.canvas_size = size);
        let remote = {
            let guard = match self.session.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard
                .as_ref()
                .map(|session| {
                    self.presence
                        .lock()
                        .map(|mut presence| {
                            presence.prune();
                            presence.rects(session)
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()
        };
        let frame = {
            let guard = match self.session.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            build_frame(guard.as_ref(), &remote, size)
        };
        if let Err(error) = ctx.submit_frame(VIEW, frame).await {
            ctx.log_error(&format!("提交表格视图失败：{error}"));
        }
    }

    /// 探明灯箱当前图能否载入：尺寸可读即收起加载占位，失败给出同框文案。
    /// 只对仍在看的那张图生效——翻页在途的旧回执不能收起新图的占位。
    pub(crate) async fn probe_lightbox(&self, ctx: &Context, path: &str) {
        let params = atelyx_plugin_protocol::method::PathParams { path: path.to_string() };
        let Ok(value) = serde_json::to_value(&params) else { return };
        let failed = match ctx
            .call_raw(atelyx_plugin_protocol::method::vault::IMAGE_INFO, value)
            .await
        {
            Ok(_) => None,
            Err(error) => {
                ctx.log_error(&format!("读取图片信息失败（{path}）：{error}"));
                Some("无法加载图片".to_string())
            }
        };
        let current = self
            .with_session(|session| {
                session
                    .view
                    .lightbox
                    .as_ref()
                    .and_then(|lightbox| crate::lightbox::current_image(session, lightbox))
                    .filter(|current| current.as_str() == path)
            })
            .flatten();
        if current.is_none() {
            return;
        }
        self.with_session_mut(|session| {
            if let Some(lightbox) = session.view.lightbox.as_mut() {
                lightbox.loading = false;
                lightbox.error = failed;
            }
        });
        self.render(ctx).await;
    }

    /// 批量补齐断行量算：每批 512 条，宿主按同口径折行并回传逐行行段。
    async fn measure_into_cache(
        &self,
        ctx: &Context,
        misses: Vec<(crate::text::WrapKey, f32)>,
    ) -> bool {
        for chunk in misses.chunks(atelyx_plugin_protocol::method::MAX_MEASURE_ITEMS) {
            let items: Vec<atelyx_plugin_protocol::method::MeasureItem> = chunk
                .iter()
                .map(|(key, wrap_w)| atelyx_plugin_protocol::method::MeasureItem {
                    text: key.text.clone(),
                    size: f32::from_bits(key.size_bits),
                    weight: key.weight,
                    family: key.family,
                    family_name: key.family_name.clone(),
                    max_width: Some(*wrap_w),
                    layout: Some(atelyx_plugin_protocol::paint::TextLayout {
                        line_height: Some(crate::text::LINE_FACTOR),
                        wrap: Some(atelyx_plugin_protocol::paint::TextWrap::WordOrGlyph),
                        shaping: Some(atelyx_plugin_protocol::paint::TextShaping::Advanced),
                    }),
                    segments: true,
                })
                .collect();
            let result = match ctx.measure_batch(items).await {
                Ok(result) => result,
                Err(error) => {
                    ctx.log_error(&format!("表格文字量算失败，按估算排版：{error}"));
                    return false;
                }
            };
            let mut pairs = chunk.iter().zip(result.sizes.into_iter());
            let measured: Vec<(crate::text::WrapKey, Vec<crate::text::TextLine>)> = pairs
                .by_ref()
                .map(|((key, _), size)| {
                    let lines = size
                        .segments
                        .unwrap_or_default()
                        .into_iter()
                        .map(|segment| crate::text::TextLine {
                            text: segment.text,
                            width: segment.width,
                        })
                        .collect();
                    (key.clone(), lines)
                })
                .collect();
            self.with_session_mut(|session| {
                if let Some(layout) = session.view.layout.as_mut() {
                    for (key, lines) in measured {
                        layout.wrap.apply(key, lines);
                    }
                }
            });
        }
        true
    }

    /// 声明面板头部（脏标记 / 统计 / 缩放 / 菜单项）；面摘除或未打开表格时收起头部。
    pub(crate) async fn publish_header(&self, ctx: &Context) {
        // 面已摘除（切走视图 / 关表格）：宿主据此删条目，否则会保留上一张表的胶囊与菜单
        let header = if !self.is_attached() {
            None
        } else {
            self.with_session(|session| atelyx_plugin_protocol::method::PanelHeaderState {
            status: session.dirty.then(|| atelyx_plugin_protocol::method::HeaderStatus {
                text: "未保存".to_string(),
                tone: atelyx_plugin_protocol::method::HeaderTone::Pending,
            }),
            error: session.save_error.clone(),
            pills: self
                .presence
                .lock()
                .map(|presence| {
                    presence
                        .collaborators()
                        .into_iter()
                        .map(|(name, color)| atelyx_plugin_protocol::method::HeaderPill {
                            text: name,
                            color,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            texts: vec![
                format!("{}%", (session.view.zoom * 100.0).round() as i32),
                format!("{} 行 · {} 列", session.data.rows.len(), session.data.fields.len()),
            ],
            items: vec![
                atelyx_plugin_protocol::method::HeaderItem {
                    id: "history".to_string(),
                    label: "历史记录".to_string(),
                },
                atelyx_plugin_protocol::method::HeaderItem {
                    id: "export".to_string(),
                    label: "导出 xlsx".to_string(),
                },
            ],
            view_slot: Some(ContributionSlot::TableView),
            // 在途草稿也是未落盘内容：脏拦截据此先要求落盘
            dirty: session.dirty || session.view.editing.is_some(),
            })
        };
        let params = atelyx_plugin_protocol::method::PanelHeaderParams {
            target: PANEL_TARGET.to_string(),
            header,
        };
        if let Err(error) = ctx.panel_header(&params).await {
            ctx.log_error(&format!("声明面板头部失败：{error}"));
        }
    }

    /// 字体清单可能有几十项：只给样式字体菜单加搜索行，其余菜单不声明。
    fn font_menu_search(request: &input::MenuRequest) -> Option<MenuSearch> {
        matches!(request, input::MenuRequest::StyleFont).then(|| MenuSearch {
            placeholder: "搜索字体".into(),
            empty: Some("没有匹配的字体".into()),
        })
    }

    /// 打开右键菜单（宿主渲染、选中回传）。
    async fn open_menu(&self, ctx: &Context, request: input::MenuRequest) {
        let Some(items) = self.with_session(|session| menu::build(session, &request)) else {
            return;
        };
        let params = MenuOpenParams { items, width: None, search: Self::font_menu_search(&request) };
        if let Err(error) = ctx.open_menu(&params).await {
            ctx.log_error(&format!("打开表格菜单失败：{error}"));
        }
    }

    /// 菜单选中：剪贴板动作用宿主能力，其余落成模型变更。
    async fn apply_menu_choice(self: &Arc<Self>, ctx: &Context, id: &str) {
        match id {
            "copy" => return self.run_clipboard(ctx, input::ClipboardAction::Copy).await,
            "cut" => return self.run_clipboard(ctx, input::ClipboardAction::Cut).await,
            "paste" => return self.run_clipboard(ctx, input::ClipboardAction::Paste).await,
            "lb:copy" => return self.copy_lightbox_image(ctx).await,
            "lb:download" => return self.download_lightbox_image(ctx).await,
            "clear" => {
                let changed = self
                    .with_session_mut(input::clear_selection_public)
                    .unwrap_or(false);
                if changed {
                    self.schedule_save(ctx);
                }
                self.render(ctx).await;
                return;
            }
            _ => {}
        }
        if let Some(request) = menu::prompt_request(id) {
            self.open_prompt(ctx, request).await;
            return;
        }
        if let Some(rest) = id.strip_prefix("style:") {
            let changed = self
                .with_session_mut(|session| menu::apply_style(session, rest))
                .unwrap_or(false);
            if changed {
                self.schedule_save(ctx);
                self.render(ctx).await;
            }
            return;
        }
        let changed = self.with_session_mut(|session| menu::apply(session, id)).unwrap_or(false);
        if changed {
            self.schedule_save(ctx);
            self.publish_header(ctx).await;
        }
        self.render(ctx).await;
    }

    /// 灯箱当前图：读仓库字节（空间走同构内容面）。
    async fn lightbox_bytes(&self, ctx: &Context) -> Option<(String, Vec<u8>)> {
        let rel = self
            .with_session(|session| {
                session
                    .view
                    .lightbox
                    .as_ref()
                    .and_then(|lightbox| crate::lightbox::current_image(session, lightbox))
            })
            .flatten()?;
        match ctx.vault_read_binary(&rel).await {
            Ok(bytes) => Some((rel, bytes)),
            Err(error) => {
                ctx.log_error(&format!("读取灯箱图片失败（{rel}）：{error}"));
                None
            }
        }
    }

    /// 复制灯箱当前图到系统剪贴板（统一转 PNG：宿主剪贴板只认 PNG）。
    async fn copy_lightbox_image(&self, ctx: &Context) {
        let Some((_rel, bytes)) = self.lightbox_bytes(ctx).await else { return };
        let png = match png_of(&bytes) {
            Ok(png) => png,
            Err(error) => {
                ctx.log_error(&format!("图片转 PNG 失败：{error}"));
                return;
            }
        };
        match ctx.clipboard_write_image(&png).await {
            Ok(()) => {
                let _ = ctx.notify("已复制图片", None, atelyx_plugin_protocol::method::NotificationLevel::Success).await;
            }
            Err(error) => ctx.log_error(&format!("写剪贴板失败：{error}")),
        }
    }

    /// 下载灯箱当前图到系统下载目录（重名自动加序号）。
    async fn download_lightbox_image(&self, ctx: &Context) {
        let Some((rel, bytes)) = self.lightbox_bytes(ctx).await else { return };
        let dir = match ctx.downloads_dir().await {
            Ok(dir) => dir,
            Err(error) => {
                ctx.log_error(&format!("定位下载目录失败：{error}"));
                return;
            }
        };
        let name = std::path::Path::new(&rel)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "图片.png".to_string());
        let path = unique_path(&dir, &name);
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                let _ = ctx
                    .notify(
                        &format!("已保存到 {}", path.display()),
                        None,
                        atelyx_plugin_protocol::method::NotificationLevel::Success,
                    )
                    .await;
            }
            Err(error) => ctx.log_error(&format!("写入下载文件失败：{error}")),
        }
    }

    /// 打开浮层输入（锚点取当前光标与选区左上格的较小者）。
    async fn open_prompt(self: &Arc<Self>, ctx: &Context, request: input::MenuRequest) {
        let opened = self.with_session_mut(|session| {
            let anchor = session.view.selection.as_ref().and_then(|selection| {
                atelyx_format::table::ops::region_of(
                    Some(selection),
                    &session.data.fields,
                    &session.data.rows,
                )
                .and_then(|region| {
                    crate::geom::geo(
                        &session.view.layout,
                        session.view.scroll_x,
                        session.view.scroll_y,
                        session.view.zoom,
                        session.view.canvas_size,
                    )
                    .map(|geo| {
                        let cell = geo.cell_rect(region.row_start, region.col_start);
                        crate::model::Point::new(cell.x + 8.0, cell.y + 8.0)
                    })
                })
            });
            let anchor = anchor.unwrap_or_else(|| crate::model::Point::new(40.0, 40.0));
            let kind = match request {
                input::MenuRequest::RenameField { field_id } => {
                    Some(crate::prompt::PromptKind::RenameField { field_id })
                }
                input::MenuRequest::InsertColumn { field_id, side } => {
                    Some(crate::prompt::PromptKind::InsertColumn { field_id, side })
                }
                input::MenuRequest::AddField => Some(crate::prompt::PromptKind::AddField {
                    field_type: "text".to_string(),
                }),
                input::MenuRequest::EditOptions { field_id } => {
                    Some(crate::prompt::PromptKind::Options { field_id })
                }
                _ => None,
            };
            kind.map(|kind| {
                session.view.prompt = Some(crate::prompt::open(session, kind, anchor));
            })
        });
        if opened.flatten().is_some() {
            self.render(ctx).await;
        }
    }

    /// 导出 xlsx：保存对话框 → 预读图片字节 → 生成工作簿（空间模式下外置附件不内嵌）。
    async fn export_xlsx(&self, ctx: &Context) {
        let Some((table, title)) = self.with_session(|session| {
            (session.data.clone(), crate::session::title_of(&session.file))
        }) else {
            return;
        };
        let params = atelyx_plugin_protocol::method::PickFileParams {
            title: Some("导出 xlsx".to_string()),
            save: Some(true),
            multiple: false,
            default_path: Some(format!("{title}.xlsx")),
            filters: vec![atelyx_plugin_protocol::method::FileFilter {
                name: "Excel 工作簿".to_string(),
                extensions: vec!["xlsx".to_string()],
            }],
        };
        let Some(target) = ctx.pick_file(params).await.ok().flatten() else {
            return;
        };
        let space = space_container(ctx).await;
        // 预读外置附件图片（空间模式读不到，与原导出口径一致）
        let mut images = std::collections::HashMap::new();
        if !space {
            for row in &table.rows {
                for field in &table.fields {
                    if field.field_type != "image" {
                        continue;
                    }
                    let Some(value) = row.values.get(&field.id) else { continue };
                    let Some(entry) = atelyx_format::table::ops::image_entries(value).first().copied()
                    else {
                        continue;
                    };
                    if entry.starts_with("data:") || images.contains_key(entry) {
                        continue;
                    }
                    if let Ok(bytes) = ctx.vault_read_binary(entry).await {
                        images.insert(entry.to_string(), bytes);
                    }
                }
            }
        }
        let target_path = std::path::PathBuf::from(&target);
        let table_for_task = table.clone();
        let result = tokio::task::spawn_blocking(move || {
            xlsx::export(&xlsx::ExportInput {
                table: &table_for_task,
                images: &images,
                space,
                target: &target_path,
            })
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result);
        match result {
            Ok(0) => {
                let _ = ctx
                    .notify("已导出 xlsx", None, atelyx_plugin_protocol::method::NotificationLevel::Success)
                    .await;
            }
            Ok(skipped) => {
                let text = if space {
                    format!("已导出 xlsx（{skipped} 张图片附件在服务端，未嵌入）")
                } else {
                    format!("已导出 xlsx（{skipped} 张图片无法嵌入）")
                };
                let _ = ctx
                    .notify(&text, None, atelyx_plugin_protocol::method::NotificationLevel::Warning)
                    .await;
            }
            Err(error) => {
                let _ = ctx
                    .notify(&format!("导出失败：{error}"), None, atelyx_plugin_protocol::method::NotificationLevel::Error)
                    .await;
            }
        }
    }

    /// 取色器选字色 / 底色并应用到当前选区。
    async fn pick_style_color(self: &Arc<Self>, ctx: &Context, background: bool) {
        let Some(initial) = self.with_session(|session| format::current_color(session, background))
        else {
            return;
        };
        let params = atelyx_plugin_protocol::method::PickColorParams { initial };
        let color = match ctx.pick_color(&params).await {
            Ok(result) => result.color,
            Err(error) => {
                ctx.log_error(&format!("取色失败：{error}"));
                return;
            }
        };
        let Some(color) = color else { return };
        let changed = self
            .with_session_mut(|session| format::apply(session, format::patch_color(Some(color), background)))
            .unwrap_or(false);
        if changed {
            self.schedule_save(ctx);
            self.render(ctx).await;
        }
    }

    /// 导入图片：多选对话框 → 逐张落仓库（`.atelyx/temp/tables/<表 id>/`）→ 一次撤销合并进单元格。
    /// 单张超过 50 MiB 或扩展名不在白名单的跳过并记日志，不阻断其余图片。
    async fn import_images(self: &Arc<Self>, ctx: &Context, row_id: &str, field_id: &str) {
        let params = atelyx_plugin_protocol::method::PickFileParams {
            title: Some("选择图片".to_string()),
            save: None,
            multiple: true,
            default_path: None,
            filters: vec![atelyx_plugin_protocol::method::FileFilter {
                name: "图片".to_string(),
                extensions: vec!["png".into(), "jpg".into(), "jpeg".into(), "webp".into(), "gif".into()],
            }],
        };
        let paths = match ctx.pick_files(params).await {
            Ok(paths) => paths,
            Err(error) => {
                ctx.log_error(&format!("选择图片失败：{error}"));
                return;
            }
        };
        if paths.is_empty() {
            return;
        }
        let Some(table_id) = self.with_session(|session| session.data.id.clone()) else {
            return;
        };
        let mut written: Vec<String> = Vec::new();
        for path in paths {
            let file_name = std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Ok(rel) = atelyx_format::table::table_image_rel_path(&table_id, &file_name) else {
                ctx.log_error(&format!("跳过不支持的图片：{file_name}"));
                continue;
            };
            let metadata = std::fs::metadata(&path);
            if metadata.as_ref().map(|meta| meta.len()).unwrap_or(0) > MAX_IMAGE_BYTES {
                ctx.log_error(&format!("图片超过 {} MiB，已跳过：{file_name}", MAX_IMAGE_BYTES >> 20));
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    ctx.log_error(&format!("读取图片失败（{path}）：{error}"));
                    continue;
                }
            };
            if let Err(error) = ctx.vault_write_binary(&rel, &bytes).await {
                ctx.log_error(&format!("写入图片失败（{rel}）：{error}"));
                continue;
            }
            written.push(rel);
        }
        if written.is_empty() {
            return;
        }
        let changed = self
            .with_session_mut(|session| append_images(session, row_id, field_id, &written))
            .unwrap_or(false);
        if changed {
            self.schedule_save(ctx);
        }
        self.render(ctx).await;
    }

    /// 剪贴板动作：复制 / 剪切写系统剪贴板，粘贴读回后落格。
    async fn run_clipboard(self: &Arc<Self>, ctx: &Context, action: input::ClipboardAction) {
        // 单元格编辑中：复制 / 剪切 / 粘贴作用于编辑框文本
        if self.with_session(|session| session.view.editing.is_some()).unwrap_or(false) {
            match action {
                input::ClipboardAction::Copy | input::ClipboardAction::Cut => {
                    let cut = action == input::ClipboardAction::Cut;
                    let text = self
                        .with_session_mut(|session| {
                            let edit = session.view.editing.as_mut()?;
                            let text = edit.editor.selected_text();
                            if cut {
                                edit.editor.delete_selection_now();
                            }
                            Some(text)
                        })
                        .flatten()
                        .unwrap_or_default();
                    if !text.is_empty() {
                        let _ = ctx.write_clipboard(&text).await;
                    }
                    if cut {
                        self.render(ctx).await;
                    }
                }
                input::ClipboardAction::Paste => {
                    if let Ok(text) = ctx.clipboard_read().await {
                        let changed = self
                            .with_session_mut(|session| {
                                session
                                    .view
                                    .editing
                                    .as_mut()
                                    .map(|edit| edit.editor.paste(&text))
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false);
                        if changed {
                            self.render(ctx).await;
                        }
                    }
                }
            }
            return;
        }
        match action {
            input::ClipboardAction::Copy | input::ClipboardAction::Cut => {
                let text = self
                    .with_session_mut(|session| {
                        if action == input::ClipboardAction::Cut {
                            input::cut_tsv(session)
                        } else {
                            input::copy_tsv(session)
                        }
                    })
                    .flatten();
                let Some(text) = text else { return };
                if let Err(error) = ctx.write_clipboard(&text).await {
                    ctx.log_error(&format!("写剪贴板失败：{error}"));
                }
                if action == input::ClipboardAction::Cut {
                    self.schedule_save(ctx);
                    self.render(ctx).await;
                }
            }
            input::ClipboardAction::Paste => {
                let text = match ctx.clipboard_read().await {
                    Ok(text) => text,
                    Err(error) => {
                        ctx.log_error(&format!("读剪贴板失败：{error}"));
                        return;
                    }
                };
                let changed = self
                    .with_session_mut(|session| input::paste_tsv(session, &text))
                    .unwrap_or(false);
                if changed {
                    self.schedule_save(ctx);
                    self.render(ctx).await;
                }
            }
        }
    }

    /// 立即保存（Ctrl+S）：失败提示可读原因，成功提示已保存。
    async fn save_now(&self, ctx: &Context) {
        match crate::session::save(self, ctx, crate::session::SaveReason::Manual).await {
            Ok(_) => {
                crate::session::notify_changed(self, ctx).await;
                let _ = ctx
                    .notify(
                        "已保存",
                        None,
                        atelyx_plugin_protocol::method::NotificationLevel::Success,
                    )
                    .await;
            }
            Err(error) => {
                ctx.log_error(&format!("保存表格失败：{error}"));
                self.with_session_mut(|session| session.save_error = Some(error.clone()));
                let _ = ctx
                    .notify(
                        &format!("保存失败：{error}"),
                        None,
                        atelyx_plugin_protocol::method::NotificationLevel::Error,
                    )
                    .await;
            }
        }
        self.publish_header(ctx).await;
        self.render(ctx).await;
    }

    /// 排一次防抖保存（500ms；期间新改动把世代推后，只有最后一拍落盘）。
    pub(crate) fn schedule_save(self: &Arc<Self>, ctx: &Context) {
        let generation = {
            let Ok(mut debounce) = self.save_debounce.lock() else {
                return;
            };
            debounce.generation += 1;
            if debounce.running {
                return;
            }
            debounce.running = true;
            debounce.generation
        };
        let state = self.clone();
        let ctx = ctx.clone();
        // 插件回调不保证跑在 Tokio 运行时里：防抖线程自建一个 current-thread 运行时来跑保存链。
        // 只保留一个等待线程：期间编辑只推世代，落盘后世代仍变就再睡一拍，避免高频输入堆线程。
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build()
            else {
                if let Ok(mut debounce) = state.save_debounce.lock() {
                    debounce.running = false;
                }
                return;
            };
            let mut saved = generation;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(AUTOSAVE_DEBOUNCE_MS));
                let pending = match state.save_debounce.lock() {
                    Ok(debounce) => debounce.generation,
                    Err(poisoned) => poisoned.into_inner().generation,
                };
                if pending != saved {
                    saved = pending;
                    continue;
                }
                runtime.block_on(async {
                    match crate::session::save(&state, &ctx, crate::session::SaveReason::Auto).await {
                        Ok(_) => crate::session::notify_changed(&state, &ctx).await,
                        Err(error) => {
                            ctx.log_error(&format!("保存表格失败：{error}"));
                            state.with_session_mut(|session| session.save_error = Some(error));
                        }
                    }
                    state.publish_header(&ctx).await;
                });
                let mut debounce = match state.save_debounce.lock() {
                    Ok(debounce) => debounce,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if debounce.generation == saved {
                    debounce.running = false;
                    break;
                }
                saved = debounce.generation;
            }
        });
    }

    /// 历史回滚把版本交回当前表格：整体替换数据并置脏。
    fn apply_restore(&self, payload: &serde_json::Value) -> bool {
        let Ok(params) = serde_json::from_value::<atelyx_plugin_protocol::method::TableContentParams>(
            payload.clone(),
        ) else {
            return false;
        };
        let Some(path) = params.path.clone().or_else(|| crate::session::current_file(self)) else {
            return false;
        };
        crate::session::restore_snapshot(self, &path, &params.content)
    }

    /// 空间远端补丁合入。
    fn apply_remote(&self, payload: &serde_json::Value) -> bool {
        let Ok(params) =
            serde_json::from_value::<atelyx_plugin_protocol::method::SpaceFilePatchPayload>(
                payload.clone(),
            )
        else {
            return false;
        };
        if params.kind != "table" {
            return false;
        }
        crate::session::apply_remote_patch(self, &params.path, &params.patch, params.landed)
    }

    /// 头部菜单项：历史走宿主弹层，导出走本地 xlsx 生成。
    async fn handle_header_action(&self, ctx: &Context, item: &str) {
        match item {
            "history" => {
                let Some(path) = crate::session::current_file(self) else { return };
                let params = atelyx_plugin_protocol::method::HistoryOpenParams {
                    kind: "table".to_string(),
                    file: path,
                };
                if let Ok(value) = serde_json::to_value(&params) {
                    let _ = ctx
                        .call_raw(atelyx_plugin_protocol::method::history::OPEN, value)
                        .await;
                }
            }
            "export" => self.export_xlsx(ctx).await,
            other => ctx.log_error(&format!("未知的面板头部动作：{other}")),
        }
    }
}

/// 组帧：按当前会话与面尺寸画出整面（无会话 = 空态）。
fn build_frame(
    session: Option<&Session>,
    remote: &[presence::RemoteCursor],
    fallback: Size,
) -> SurfaceFrame {
    let size = match session {
        Some(session)
            if session.view.canvas_size.width > 0.0 && session.view.canvas_size.height > 0.0 =>
        {
            session.view.canvas_size
        }
        _ => fallback,
    };
    let (commands, hits) = paint::paint(session, size, remote);
    let ime = session.and_then(|session| paint::ime_rect(session, size));
    // 长按缩略图 30fps 判长按；编辑态按光标闪烁 2fps；其余 1fps 驱动在场保活与过期剔除
    let fps = session.map(|session| {
        if session.view.image_press.is_some() {
            crate::TICK_HZ
        } else if session.view.editing.is_some() {
            CARET_BLINK_HZ
        } else {
            1
        }
    });
    let popup = session.and_then(|session| {
        prompt::popup_frame(session, size)
            .or_else(|| lightbox::popup_frame(session, size))
            .or_else(|| format::popup_frame(session, size))
    });
    SurfaceFrame {
        size: Some(atelyx_plugin_protocol::frame::FrameSize { w: size.width, h: size.height }),
        commands,
        hits,
        ime,
        keys: session.is_some(),
        fps,
        popup,
    }
}

/// 插件实例：处理器契约与共享运行态分离（后台任务只需 `Arc<PluginState>`）。
struct TablesPlugin {
    state: Arc<PluginState>,
}

impl TablesPlugin {
    fn new() -> Self {
        Self { state: PluginState::new() }
    }
}

#[async_trait::async_trait]
impl PluginHandler for TablesPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::TableView, VIEW, "表格", Some("table"), None)
            .await
        {
            ctx.log_error(&format!("注册表格视图失败：{error}"));
        }
        if let Err(error) = ctx
            .register_service(service::SERVICE, service::METHODS)
            .await
        {
            ctx.log_error(&format!("登记表格服务失败：{error}"));
        }
        for name in [
            event::name::EDITOR_OPEN_CHANGED,
            event::name::TABLE_RESTORE,
            event::name::SPACE_FILE_PATCH,
            event::name::PANEL_FLUSH,
            event::name::PANEL_HEADER_ACTION,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        if let Ok(theme) = ctx.query_theme().await {
            theme::apply_font_sizes(theme.font_sizes);
        }
        match ctx.query_fonts().await {
            Ok(families) => crate::menu::set_font_families(families),
            Err(error) => ctx.log_error(&format!("取字体族清单失败：{error}")),
        }
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != VIEW {
            return;
        }
        if let Ok(mut attached) = self.state.attached.lock() {
            *attached = true;
        }
        crate::presence::publish_selection(&self.state, ctx).await;
        self.state.render(ctx).await;
        self.state.publish_header(ctx).await;
    }

    async fn on_surface_detached(&self, ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == VIEW {
            if let Ok(mut attached) = self.state.attached.lock() {
                *attached = false;
            }
            if let Some(path) = crate::session::current_file(&self.state) {
                crate::presence::publish_leave(&self.state, ctx, &path).await;
            }
            // 宿主不会自己清头部：摘面时主动发一次 None
            self.state.publish_header(ctx).await;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        match params.name.as_str() {
            event::name::EDITOR_OPEN_CHANGED => {
                crate::session::reload_from_open_files(&self.state, ctx).await;
                self.state.render(ctx).await;
                self.state.publish_header(ctx).await;
            }
            event::name::TABLE_RESTORE => {
                if self.state.apply_restore(&params.payload) {
                    self.state.schedule_save(ctx);
                    self.state.render(ctx).await;
                    self.state.publish_header(ctx).await;
                }
            }
            event::name::SPACE_FILE_PATCH => {
                if self.state.apply_remote(&params.payload) {
                    // 远端合入也是一次内容结算：订阅 table:changed 的第三方视图据此重取
                    crate::session::notify_changed(&self.state, ctx).await;
                    self.state.render(ctx).await;
                }
            }
            event::name::PANEL_FLUSH => {
                let Ok(params) = serde_json::from_value::<
                    atelyx_plugin_protocol::method::PanelFlushParams,
                >(params.payload.clone())
                else {
                    return;
                };
                // flush 要求把写盘在途期间产生的新编辑也落盘：未收敛就续写，最多 8 轮
                let mut result =
                    crate::session::save(&self.state, ctx, crate::session::SaveReason::Flush).await;
                for _ in 0..7 {
                    if !matches!(result, Ok(false)) {
                        break;
                    }
                    result =
                        crate::session::save(&self.state, ctx, crate::session::SaveReason::Flush)
                            .await;
                }
                if matches!(result, Ok(false)) {
                    result = Err("保存未收敛（持续有未落盘编辑）".to_string());
                }
                if result.is_ok() {
                    crate::session::notify_changed(&self.state, ctx).await;
                }
                let done = atelyx_plugin_protocol::method::PanelFlushDoneParams {
                    target: Some(params.target.clone()),
                    token: params.token,
                    ok: result.is_ok(),
                    error: result.err(),
                };
                let _ = ctx.panel_flush_done(&done).await;
                self.state.render(ctx).await;
                self.state.publish_header(ctx).await;
            }
            event::name::PANEL_HEADER_ACTION => {
                let Ok(action) = serde_json::from_value::<
                    atelyx_plugin_protocol::method::PanelHeaderActionParams,
                >(params.payload.clone())
                else {
                    return;
                };
                self.state.handle_header_action(ctx, &action.item).await;
            }
            _ => {}
        }
    }

    async fn on_service_call(
        &self,
        ctx: &Context,
        params: atelyx_plugin_protocol::service::HostCallParams,
    ) {
        service::handle(&self.state, ctx, params).await;
    }

    async fn on_collab_message(
        &self,
        ctx: &Context,
        params: atelyx_plugin_protocol::method::CollabMessageParams,
    ) {
        if params.channel != "table-presence" {
            return;
        }
        let Some(file) = crate::session::current_file(&self.state) else {
            return;
        };
        let changed = self
            .state
            .presence
            .lock()
            .map(|mut presence| {
                presence.apply(&params.peer_id, params.peer_name.as_deref(), &params.payload, &file);
                true
            })
            .unwrap_or(false);
        crate::session::notify_selection(&self.state, ctx).await;
        if changed {
            self.state.render(ctx).await;
            self.state.publish_header(ctx).await;
        }
    }

    async fn on_menu_closed(
        &self,
        ctx: &Context,
        params: atelyx_plugin_protocol::method::MenuClosedParams,
    ) {
        if let Some(selected) = params.selected {
            self.state.apply_menu_choice(ctx, &selected).await;
        }
    }

    async fn on_frame_tick(&self, ctx: &Context, params: atelyx_plugin_protocol::frame::FrameTickParams) {
        if params.surface_id != VIEW {
            return;
        }
        let presence_changed = crate::presence::tick(&self.state, ctx).await;
        let started = self
            .state
            .with_session_mut(crate::input::tick_image_press)
            .unwrap_or(false);
        let blinked = self
            .state
            .with_session_mut(crate::input::tick_caret)
            .unwrap_or(false);
        if started || blinked || presence_changed {
            self.state.render(ctx).await;
            if presence_changed {
                self.state.publish_header(ctx).await;
            }
        }
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != VIEW {
            return;
        }
        if let FrameInput::Resize { width, height } = params.input {
            let size = Size { width, height };
            if let Ok(mut current) = self.state.surface_size.lock() {
                *current = size;
            }
            self.state
                .with_session_mut(|session| session.view.canvas_size = size);
            self.state.render(ctx).await;
            return;
        }
        let (outcome, menu_request) = self
            .state
            .with_session_mut(|session| input::handle(session, &params.input))
            .unwrap_or_default();
        let lightbox_image = outcome.lightbox_image.clone();
        if let Some(action) = outcome.clipboard {
            self.state.run_clipboard(ctx, action).await;
        }
        if outcome.manual_save {
            self.state.save_now(ctx).await;
        }
        if let Some((row_id, field_id)) = outcome.image_import {
            self.state.import_images(ctx, &row_id, &field_id).await;
        }
        if let Some(background) = outcome.pick_color {
            self.state.pick_style_color(ctx, background).await;
        }
        if outcome.selection {
            crate::session::notify_selection(&self.state, ctx).await;
            presence::publish_selection(&self.state, ctx).await;
        }
        if outcome.selection || outcome.header || outcome.dirty {
            self.state.publish_header(ctx).await;
        }
        if let Some(request) = menu_request {
            self.state.open_menu(ctx, request).await;
        }
        if outcome.dirty {
            self.state.schedule_save(ctx);
        }
        if outcome.paint || outcome.dirty {
            self.state.render(ctx).await;
        }
        if let Some(path) = lightbox_image {
            // 探针走后台任务：宿主在空间模式下要整文件下载，不能堵住输入队列
            let state = self.state.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move { state.probe_lightbox(&ctx, &path).await });
        }
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "表格".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(TablesPlugin::new()),
    )
}

/// 单张导入图片的大小上限（字节，与宿主表格图片同一档）。
const MAX_IMAGE_BYTES: u64 = 50 << 20;

/// 把新导入的图片引用追加到图片格（一次撤销、一次置脏）。
fn append_images(session: &mut Session, row_id: &str, field_id: &str, rels: &[String]) -> bool {
    let Some(row) = session.data.rows.iter().find(|row| row.id == row_id) else {
        return false;
    };
    let Some(value) = row.values.get(field_id) else {
        return false;
    };
    let mut list: Vec<String> = crate::images::entries(Some(value));
    let display = crate::images::display_mode(Some(value)).to_string();
    list.extend(rels.iter().cloned());
    let next = serde_json::json!({ "images": list, "display": display });
    crate::input::push_undo(session);
    let Some(row) = session.data.rows.iter_mut().find(|row| row.id == row_id) else {
        return false;
    };
    let changed = atelyx_format::table::ops::set_cell_value(row, field_id, Some(next));
    if changed {
        session.touch();
    } else {
        session.view.undo.pop();
    }
    changed
}

/// 图片字节 → PNG（剪贴板只认 PNG；已是 PNG 直接透传）。
fn png_of(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Ok(bytes.to_vec());
    }
    let image = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// 下载落盘路径：同名已存在时按「主名 (n).扩展名」递增，不覆盖已有文件。
fn unique_path(dir: &str, name: &str) -> std::path::PathBuf {
    let base = std::path::Path::new(name);
    let stem = base.file_stem().and_then(|stem| stem.to_str()).unwrap_or("图片");
    let ext = base.extension().and_then(|ext| ext.to_str()).unwrap_or("png");
    let mut path = std::path::Path::new(dir).join(format!("{stem}.{ext}"));
    let mut index = 1;
    while path.exists() {
        path = std::path::Path::new(dir).join(format!("{stem} ({index}).{ext}"));
        index += 1;
    }
    path
}

/// 当前容器是否协作空间（`space.room` 的 `kind`）。
async fn space_container(ctx: &Context) -> bool {
    let Ok(value) = ctx
        .call_raw(atelyx_plugin_protocol::method::space::ROOM, serde_json::Value::Null)
        .await
    else {
        return false;
    };
    value
        .get("kind")
        .and_then(|kind| kind.as_str())
        .is_some_and(|kind| kind == "space")
}

#[cfg(test)]
mod tests {
    use super::build_frame;
    use crate::model::Size;
    use atelyx_plugin_protocol::paint::DrawCommand;

    // 面 320×200：标题与说明按面宽居中，整块（图标 + 标题 + 说明）上下沿中心 = 100。
    #[test]
    fn empty_frame_uses_surface_size_and_standard_placeholder() {
        use atelyx_plugin_protocol::paint::TextAlign;

        let frame = build_frame(None, &[], Size { width: 320.0, height: 200.0 });
        let size = frame.size.expect("整面帧必须带尺寸");
        assert_eq!(size.w, 320.0);
        assert_eq!(size.h, 200.0);

        let text = |wanted: &str| {
            frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text { content, x, y, size, max_width, align, .. } if content == wanted => {
                        Some((*x, *y, *size, *max_width, *align))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("空态缺少文字：{wanted}"))
        };
        let title = text("打开表格");
        let desc = text("在文件面板或搜索面板单击一个 .atb 表格开始编辑。");
        for (label, (x, _, _, max_width, align)) in [("标题", title), ("说明", desc)] {
            assert_eq!(max_width, Some(320.0), "空态{label}应按面宽对齐");
            assert_eq!(align, Some(TextAlign::Center), "空态{label}应居中");
            assert!((x + 320.0 / 2.0 - 160.0).abs() < 0.5, "空态{label}应水平居中");
        }
        let group_h = 64.0 + 16.0 + title.2 * 1.2 + 16.0 + desc.2 * 1.2;
        let group_top = (200.0 - group_h) / 2.0;
        assert!((title.1 - (group_top + 64.0 + 16.0)).abs() < 0.5, "空态标题应垂直居中");
        let desc_y = group_top + 64.0 + 16.0 + title.2 * 1.2 + 16.0;
        assert!((desc.1 - desc_y).abs() < 0.5, "空态说明应垂直居中");
        let block_center = (group_top + desc_y + desc.2 * 1.2) / 2.0;
        assert!((block_center - 100.0).abs() < 0.5, "空态内容块应垂直居中");
    }

    // 布局过渡首帧可能上报 0 尺寸：空态只留背景，整帧仍须可提交。
    #[test]
    fn empty_frame_tolerates_zero_surface_size() {
        let frame = build_frame(None, &[], Size { width: 0.0, height: 0.0 });
        assert!(
            !frame
                .commands
                .iter()
                .any(|command| matches!(command, DrawCommand::Text { .. })),
            "零尺寸空态不应产文字指令"
        );
        frame.validate().expect("零尺寸空态帧必须通过校验");
    }

    // 极窄面按真实宽度排版：文字可用宽度为正即可提交。
    #[test]
    fn empty_frame_stays_valid_at_tiny_width() {
        let frame = build_frame(None, &[], Size { width: 1.0, height: 200.0 });
        frame.validate().expect("1 像素宽的空态帧必须通过校验");
    }

    #[test]
    fn font_menu_is_the_only_menu_declaring_search() {
        use super::PluginState;
        use crate::input::MenuRequest;

        let search = PluginState::font_menu_search(&MenuRequest::StyleFont)
            .expect("字体菜单应声明搜索行");
        assert_eq!(search.placeholder, "搜索字体");
        assert_eq!(search.empty.as_deref(), Some("没有匹配的字体"));
        for request in [
            MenuRequest::StyleSize,
            MenuRequest::All,
            MenuRequest::Cell { row: "r1".into(), field: "f1".into() },
        ] {
            assert!(
                PluginState::font_menu_search(&request).is_none(),
                "只有字体菜单声明搜索行：{request:?}"
            );
        }
    }
}
