//! 文件面板插件：仓库文件树 + 展开集 / 排序 / 目录颜色 / 提示词标记。
//! 目录条目与界面状态（含展开集）都以宿主为真源，经 `vault.ui` 读、`vault.updateUi` 写。

mod model;
mod view;

use std::collections::HashMap;
use std::sync::Mutex;

use futures_util::future::join_all;

use atelyx_plugin_protocol::event::{self, PublishParams};
use atelyx_plugin_protocol::frame::{FrameInput, FrameInputParams, FrameTickParams, MouseButton};
use atelyx_plugin_protocol::method::{
    ContributionSlot, CreateEntryParams, DragEndParams, DragStartParams, DuplicateParams,
    MenuClosedParams, MenuItem, MenuOpenParams, PickColorParams, RemoveContainerParams,
    SurfaceAttachedParams, SurfaceDetachedParams, SwitchContainerParams, UiDirEntry, VaultUiParams,
    VaultUiPatch,
};
use atelyx_plugin_sdk::frame_ui::{FrameUi, UiEvent};
use atelyx_plugin_sdk::{Context, PluginHandler, PluginInfo};
use tokio::sync::Mutex as AsyncMutex;

use crate::model::{stem_of, DocType, DraftKind, InlineEdit, Model};

/// 插件 id（与 `plugin.json` 一致）。
const PLUGIN_ID: &str = "atelyx.files";
/// 面板贡献 id（同时是面标识的贡献段）。
const PANEL: &str = "files";
/// 工具条与仓库行按钮的命中区 id。
const OPEN_VAULT: &str = "open-vault";
const SPACE_MENU: &str = "space-menu";
const SORT: &str = "sort";
const REFRESH: &str = "refresh";
const TOGGLE_ALL: &str = "toggle-all";
/// 容器行的命中区前缀（`switch:<容器 id>`）。
const CONTAINER: &str = "switch:";
/// 容器行「确认移除」的菜单项前缀（`remove-container:<容器 id>`）。
const REMOVE_CONTAINER: &str = "remove-container:";
/// 行侧缝与列表下方空白的落点前缀（拖回仓库根）。
const DROP_ROOT: &str = "drop-root:";
/// 列表下方空白落点的命中区 id。
const DROP_BOTTOM: &str = "drop-root:bottom";
/// 就地输入框的控件 id。
const INLINE: &str = "inline-input";
/// 起拖阈值（逻辑像素）：与壳内行拖拽同一条判据。
const DRAG_THRESHOLD: f32 = 5.0;
/// 排序菜单里的四档。
const SORTS: [(&str, &str); 4] = [
    ("name-asc", "名称 (A-Z)"),
    ("name-desc", "名称 (Z-A)"),
    ("mtime-desc", "编辑时间 (从新到旧)"),
    ("mtime-asc", "编辑时间 (从旧到新)"),
];

struct FilesPlugin {
    model: Mutex<Model>,
    ui: AsyncMutex<FrameUi>,
    /// 面板面是否已挂载：未挂载时不取数也不提交帧。
    attached: Mutex<bool>,
    /// 每个目录的条目缓存（宿主回执按需填充）。
    children: Mutex<HashMap<String, Vec<UiDirEntry>>>,
    /// 按下但尚未起拖的行（路径 / 是否目录 / 按下点），移动超阈值才转成拖拽会话。
    pending_drag: Mutex<Option<(String, bool, f32, f32)>>,
}

impl FilesPlugin {
    fn new() -> Self {
        Self {
            model: Mutex::new(Model::new()),
            ui: AsyncMutex::new(FrameUi::new()),
            attached: Mutex::new(false),
            children: Mutex::new(HashMap::new()),
            pending_drag: Mutex::new(None),
        }
    }

    fn is_attached(&self) -> bool {
        *self.attached.lock().unwrap()
    }

    async fn apply_theme(&self, ctx: &Context) {
        if let Ok(theme) = ctx.query_theme().await {
            self.model.lock().unwrap().set_radii(theme.radii);
        }
    }

    /// 取界面状态与根目录条目，并补齐已展开目录的下一层。
    /// 宿主事件逐条处理（同一插件只有一条命令通道），因此不需要在途合并。
    async fn reload(&self, ctx: &Context) {
        let result = match ctx.vault_ui(&VaultUiParams { dir: Some(String::new()) }).await {
            Ok(result) => result,
            Err(error) => {
                // 空树的失败由失败态正文承载，不再叠一条顶部提示
                let empty_tree = self
                    .children
                    .lock()
                    .unwrap()
                    .get("")
                    .map_or(true, |rows| rows.is_empty());
                if empty_tree {
                    ctx.log_error(&format!("刷新文件树失败：{error}"));
                } else {
                    self.warn(ctx, "刷新文件树", &error);
                }
                self.model.lock().unwrap().fail(error.to_string());
                return;
            }
        };
        // 展开集以宿主为真源：随仓库界面状态一起给，进容器即恢复该容器自己的展开态
        let expanded = result.state.expanded.clone();
        let mut children = HashMap::new();
        children.insert(String::new(), result.entries);
        // 根条目上面已取过（展开集里也含根），不重复请求；其余目录并发取，避免深层展开时逐层叠加往返
        let reads = join_all(expanded.iter().filter(|dir| !dir.is_empty()).map(|dir| {
            let dir = dir.clone();
            async move {
                let next = ctx.vault_ui(&VaultUiParams { dir: Some(dir.clone()) }).await;
                (dir, next)
            }
        }));
        for (dir, next) in reads.await {
            match next {
                Ok(next) => {
                    children.insert(dir, next.entries);
                }
                Err(error) => self.warn(ctx, &format!("打开目录 {dir}"), &error),
            }
        }
        let active_files = ctx.open_files().await.unwrap_or_default();
        let mut cache = self.children.lock().unwrap();
        // 只替换本次真取到的目录：写入展开集触发的回执不得把刚取到的子项冲掉
        for (dir, entries) in children {
            cache.insert(dir, entries);
        }
        cache.retain(|dir, _| dir.is_empty() || expanded.iter().any(|item| item == dir));
        drop(cache);
        let state = result.state;
        let containers = result.containers;
        let doc_types = result
            .doc_types
            .into_iter()
            .map(|entry| DocType { extension: entry.extension, label: entry.label, icon: entry.icon })
            .collect();
        let mut model = self.model.lock().unwrap();
        model.apply(state);
        model.set_active_files(active_files);
        model.set_containers(containers);
        model.set_doc_types(doc_types);
    }

    /// 写展开集：仓库界面状态是唯一真源，宿主回执不参与重建展开态。
    async fn save_expanded(&self, ctx: &Context, expanded: Vec<String>) {
        let patch = VaultUiPatch { expanded: Some(expanded), ..VaultUiPatch::default() };
        if let Err(error) = ctx.vault_update_ui(&patch).await {
            self.warn(ctx, "保存展开状态", &error);
        }
    }

    /// 失败落点：日志留可定位原因，面板顶部给一句话（下一次操作清掉）。
    fn warn(&self, ctx: &Context, what: &str, error: &impl std::fmt::Display) {
        ctx.log_error(&format!("{what}失败：{error}"));
        self.model.lock().unwrap().warn(format!("{what}失败：{error}"));
    }

    async fn render(&self, ctx: &Context) {
        if !self.is_attached() {
            return;
        }
        let snapshot = {
            let children = self.children.lock().unwrap().clone();
            let model = self.model.lock().unwrap();
            (model.view(&children), model.radii.clone())
        };
        let mut ui = self.ui.lock().await;
        if let Err(error) = ui
            .render(ctx, PANEL, move |frame| {
                let (view, radii) = &snapshot;
                view::frame(frame, view, radii);
            })
            .await
        {
            ctx.log_error(&format!("提交文件面板失败：{error}"));
        }
    }

    /// 切换一个目录：折叠即丢子树缓存，展开则按需取下一层。
    async fn toggle_dir(&self, ctx: &Context, rel: String) {
        let (expanded, open) = {
            let mut model = self.model.lock().unwrap();
            let open = model.toggle(&rel);
            let expanded = model.expanded().to_vec();
            (expanded, open)
        };
        if open {
            if !self.children.lock().unwrap().contains_key(&rel) {
                match ctx.vault_ui(&VaultUiParams { dir: Some(rel.clone()) }).await {
                    Ok(result) => {
                        self.children.lock().unwrap().insert(rel, result.entries);
                    }
                    Err(error) => self.warn(ctx, &format!("打开目录 {rel}"), &error),
                }
            }
        } else {
            let prefix = format!("{rel}/");
            self.children
                .lock()
                .unwrap()
                .retain(|dir, _| dir.is_empty() || !dir.starts_with(&prefix));
        }
        self.save_expanded(ctx, expanded).await;
        self.render(ctx).await;
    }

    /// 全部展开 / 全部收起：展开取宿主的整仓库目录清单，然后逐层补齐子项。
    async fn toggle_all(&self, ctx: &Context) {
        // 容器行（仓库根）的展开态不参与「全部」：全部展开/收起只作用于仓库内的条目
        let root_open = self.model.lock().unwrap().expanded().iter().any(String::is_empty);
        let mut expanded: Vec<String> = root_open.then(|| String::new()).into_iter().collect();
        if self
            .model
            .lock()
            .unwrap()
            .expanded()
            .iter()
            .any(|dir| !dir.is_empty())
        {
            self.model.lock().unwrap().set_expanded(expanded.clone());
            self.children.lock().unwrap().retain(|dir, _| dir.is_empty());
            self.save_expanded(ctx, expanded).await;
            self.render(ctx).await;
            return;
        }
        let dirs = match ctx.vault_list_dirs().await {
            Ok(dirs) => dirs,
            Err(error) => {
                self.warn(ctx, "取目录清单", &error);
                return;
            }
        };
        {
            let mut model = self.model.lock().unwrap();
            for dir in &dirs {
                model.expand(dir.clone());
            }
        }
        expanded.extend(dirs);
        self.save_expanded(ctx, expanded).await;
        // 一次把展开集内所有目录的条目取回来：之后的行摊平不再按需请求
        self.reload(ctx).await;
        self.render(ctx).await;
    }

    /// 右键菜单：目录给新建 / 颜色 / 副本 / 重命名 / 删除，文件给提示词 / 副本 / 重命名 / 删除；
    /// 仓库根（空路径）只有新建入口与路径类动作。
    async fn open_row_menu(&self, ctx: &Context, path: &str, is_dir: bool) {
        let pane = 0;
        let mut items = Vec::new();
        if is_dir {
            for kind in [DraftKind::Canvas, DraftKind::Note, DraftKind::Table] {
                items.push(menu_action(
                    &format!("draft:{}:{path}", kind.wire()),
                    &kind.label(),
                    kind.icon(),
                ));
            }
            let doc_types = self.model.lock().unwrap().doc_types().to_vec();
            for kind in doc_types {
                items.push(menu_action(
                    &format!("draft:{}:{path}", kind.extension),
                    &format!("新建{}", kind.label),
                    if kind.icon.is_empty() { "file-text" } else { &kind.icon },
                ));
            }
            items.push(menu_action(
                &format!("draft:folder:{path}"),
                &DraftKind::Folder.label(),
                DraftKind::Folder.icon(),
            ));
            if !path.is_empty() {
                items.push(MenuItem::Divider { pane });
                items.push(menu_action(&format!("color:{path}"), "图标颜色", "palette"));
                items.push(MenuItem::Divider { pane });
                items.push(menu_action(&format!("duplicate:{path}|d"), "创建副本", "copy"));
            }
        } else {
            if path.ends_with(".md") {
                let marked = self.model.lock().unwrap().prompt_notes().iter().any(|n| n == path);
                items.push(MenuItem::Action {
                    id: format!("prompt:{path}"),
                    label: if marked { "注销提示词" } else { "注册为提示词" }.into(),
                    icon: Some(if marked { "bookmark-minus" } else { "bookmark-plus" }.into()),
                    danger: false,
                    disabled: false,
                    note: None,
                    hint: None,
                    pane,
                });
                items.push(MenuItem::Divider { pane });
            }
            items.push(menu_action(&format!("duplicate:{path}|f"), "创建副本", "copy"));
        }
        // 本机路径类条目：协作空间的内容在服务端，整组不出现
        if self.model.lock().unwrap().in_local_container() {
            items.push(MenuItem::Divider { pane });
            items.push(menu_action(&format!("copy-path:{path}|a"), "复制绝对路径", "link"));
            if !path.is_empty() {
                items.push(menu_action(&format!("copy-path:{path}|r"), "复制相对路径", "link"));
            }
            let flag = if is_dir { "d" } else { "f" };
            items.push(menu_action(
                &format!("reveal:{path}|{flag}"),
                "在文件管理器中显示",
                "folder-open",
            ));
        }
        if !path.is_empty() {
            let flag = if is_dir { "d" } else { "f" };
            items.push(MenuItem::Divider { pane });
            items.push(menu_action(&format!("rename:{path}"), "重命名", "pencil"));
            items.push(MenuItem::Action {
                id: format!("delete:{path}|{flag}"),
                label: "删除".into(),
                icon: Some("trash-2".into()),
                danger: true,
                disabled: false,
                note: None,
                hint: None,
                pane,
            });
        }
        self.open_menu(ctx, items, 224.0).await;
    }

    /// 删除就地确认：危险动作二段菜单（与旧面板的确认 pane 同内容）。
    async fn open_delete_confirm(&self, ctx: &Context, path: &str, is_dir: bool) {
        let flag = if is_dir { "d" } else { "f" };
        let mut items = Vec::new();
        if is_dir {
            items.push(MenuItem::Text {
                text: "文件夹会连同其中的内容一起删除".into(),
                pane: 0,
            });
        }
        items.push(MenuItem::Text { text: "删除后不可恢复".into(), pane: 0 });
        items.push(MenuItem::Action {
            id: format!("delete-confirm:{path}|{flag}"),
            label: "确认删除".into(),
            icon: Some("trash-2".into()),
            danger: true,
            disabled: false,
            note: None,
            hint: None,
            pane: 0,
        });
        items.push(MenuItem::Action {
            id: format!("delete-cancel:{path}|{flag}"),
            label: "取消".into(),
            icon: None,
            danger: false,
            disabled: false,
            note: None,
            hint: None,
            pane: 0,
        });
        self.open_menu(ctx, items, 224.0).await;
    }

    /// 弹出一个宿主菜单。
    async fn open_menu(&self, ctx: &Context, items: Vec<MenuItem>, width: f32) {
        let params = MenuOpenParams { items, width: Some(width), search: None };
        if let Err(error) = ctx.open_menu(&params).await {
            self.warn(ctx, "弹出菜单", &error);
        }
    }

    /// 排序菜单（当前档位打勾）。
    async fn open_sort_menu(&self, ctx: &Context) {
        let current = self.model.lock().unwrap().sort().to_string();
        let items = SORTS
            .iter()
            .map(|(key, label)| {
                if *key == current {
                    MenuItem::Checked {
                        id: format!("sort:{key}"),
                        label: (*label).into(),
                        pane: 0,
                    }
                } else {
                    MenuItem::Action {
                        id: format!("sort:{key}"),
                        label: (*label).into(),
                        icon: None,
                        danger: false,
                        disabled: false,
                        note: None,
                        hint: None,
                        pane: 0,
                    }
                }
            })
            .collect();
        self.open_menu(ctx, items, 176.0).await;
    }

    /// 容器行右键：本机仓库可「在文件管理器中打开」，非激活容器可移除（二段确认），
    /// 末尾列出其余容器供切换。
    async fn open_container_menu(&self, ctx: &Context, id: &str) {
        let container = self
            .model
            .lock()
            .unwrap()
            .containers()
            .iter()
            .find(|entry| entry.id == id)
            .cloned();
        let Some(container) = container else { return };
        let mut items = Vec::new();
        if let Some(root) = container.root.clone() {
            items.push(menu_action(
                &format!("reveal-vault:{root}"),
                "在文件管理器中打开",
                "folder-open",
            ));
            items.push(MenuItem::Divider { pane: 0 });
        }
        if container.active {
            items.push(MenuItem::Action {
                id: format!("{REMOVE_CONTAINER}{}", container.id),
                label: "从列表移除".into(),
                icon: Some("unlink".into()),
                danger: true,
                disabled: true,
                note: Some("当前容器".into()),
                hint: None,
                pane: 0,
            });
        } else {
            items.push(MenuItem::Submenu {
                label: "从列表移除".into(),
                icon: Some("unlink".into()),
                disabled: false,
                pane: 0,
                target: 1,
            });
            items.push(MenuItem::Text {
                text: "只从列表移除，不影响磁盘文件".into(),
                pane: 1,
            });
            items.push(MenuItem::Action {
                id: format!("{REMOVE_CONTAINER}{}", container.id),
                label: "确认移除".into(),
                icon: Some("unlink".into()),
                danger: true,
                disabled: false,
                note: None,
                hint: None,
                pane: 1,
            });
        }
        self.open_menu(ctx, items, 240.0).await;
    }
}

/// 一条普通菜单动作（带图标、不带角标与禁用态）。
fn menu_action(id: &str, label: &str, icon: &str) -> MenuItem {
    MenuItem::Action {
        id: id.to_string(),
        label: label.to_string(),
        icon: Some(icon.to_string()),
        danger: false,
        disabled: false,
        note: None,
        hint: None,
        pane: 0,
    }
}

/// 容器行命中区 id：`switch:<容器 id>`（切换即把 id 原样交回宿主）。
fn container_id(id: &str) -> String {
    format!("{CONTAINER}{id}")
}

/// 行命中区 id：`row:<目录标志>:<仓库相对路径>`。
fn row_id(path: &str, is_dir: bool) -> String {
    format!("row:{}:{path}", if is_dir { "d" } else { "f" })
}

/// 解析行命中区 id →（路径、是否目录）；路径里的冒号只按首个分隔符切开。
fn parse_row_id(id: &str) -> Option<(String, bool)> {
    let rest = id.strip_prefix("row:")?;
    let (flag, path) = rest.split_once(':')?;
    Some((path.to_string(), flag == "d"))
}

#[async_trait::async_trait]
impl PluginHandler for FilesPlugin {
    async fn on_ready(&self, ctx: &Context) {
        if let Err(error) = ctx
            .register(ContributionSlot::Panel, PANEL, "文件", Some("files"), None)
            .await
        {
            ctx.log_error(&format!("注册文件面板失败：{error}"));
        }
        for name in [
            event::name::VAULT_FILES_CHANGED,
            event::name::VAULT_SWITCH,
            event::name::DRAG_STATE,
            event::name::EDITOR_OPEN_CHANGED,
        ] {
            if let Err(error) = ctx.subscribe(name).await {
                ctx.log_error(&format!("订阅 {name} 失败：{error}"));
            }
        }
        self.apply_theme(ctx).await;
    }

    async fn on_surface_attached(&self, ctx: &Context, params: SurfaceAttachedParams) {
        if params.surface_id != PANEL {
            return;
        }
        *self.attached.lock().unwrap() = true;
        self.ui.lock().await.refresh_theme();
        self.apply_theme(ctx).await;
        // 先交一帧：首份状态未到时画加载态，重挂时画上次的树
        self.render(ctx).await;
        self.reload(ctx).await;
        self.render(ctx).await;
    }

    async fn on_surface_detached(&self, _ctx: &Context, params: SurfaceDetachedParams) {
        if params.surface_id == PANEL {
            *self.attached.lock().unwrap() = false;
        }
    }

    async fn on_host_event(&self, ctx: &Context, params: PublishParams) {
        if !self.is_attached() {
            return;
        }
        match params.name.as_str() {
            event::name::VAULT_FILES_CHANGED => {
                self.reload(ctx).await;
                self.render(ctx).await;
            }
            event::name::VAULT_SWITCH => {
                // 切容器：旧容器的树不能当新容器的空态，缓存与展开集先清掉
                self.children.lock().unwrap().clear();
                self.model.lock().unwrap().begin_load();
                self.render(ctx).await;
                self.reload(ctx).await;
                self.render(ctx).await;
            }
            event::name::EDITOR_OPEN_CHANGED => {
                // 打开集合变了（本面点击 / 别的面板打开或关闭）：重取一次，命中行高亮
                match ctx.open_files().await {
                    Ok(files) => self.model.lock().unwrap().set_active_files(files),
                    Err(error) => ctx.log_error(&format!("取打开文件清单失败：{error}")),
                }
                self.render(ctx).await;
            }
            event::name::DRAG_STATE => {
                let state: atelyx_plugin_protocol::method::DragStateParams =
                    serde_json::from_value(params.payload).unwrap_or_default();
                // 只更新落点高亮：宿主在指针离开所有行时推「无落点」，那不是会话结束
                self.model
                    .lock()
                    .unwrap()
                    .set_drag_state(state.target.clone(), state.reject);
                // 宿主把悬停的折叠目录展开后（springing 转 false）本面跟着展开，
                // 否则落点永远够不到折叠目录的下一层
                if !state.springing {
                    if let Some(dir) = state.target.filter(|dir| !dir.is_empty()) {
                        self.follow_host_expand(ctx, dir).await;
                    }
                }
                self.render(ctx).await;
            }
            _ => {}
        }
    }

    async fn on_menu_closed(&self, ctx: &Context, params: MenuClosedParams) {
        let Some(selected) = params.selected else { return };
        self.on_menu_action(ctx, selected).await;
        self.render(ctx).await;
    }

    async fn on_frame_tick(&self, ctx: &Context, _params: FrameTickParams) {
        self.render(ctx).await;
    }

    async fn on_frame_input(&self, ctx: &Context, params: FrameInputParams) {
        if params.surface_id != PANEL {
            return;
        }
        let input = params.input.clone();
        // 右键 = 弹出该行的宿主菜单（几何由插件给出，渲染与定位在宿主）
        if let FrameInput::PointerDown { hit, button, x, y } = &input {
            if *button == MouseButton::Right {
                if let Some(id) = hit.as_deref().and_then(|id| id.strip_prefix(CONTAINER)) {
                    let id = id.to_string();
                    self.open_container_menu(ctx, &id).await;
                    return;
                }
                let row = hit.as_deref().and_then(parse_row_id);
                match row {
                    Some((path, is_dir)) => self.open_row_menu(ctx, &path, is_dir).await,
                    None => self.open_row_menu(ctx, "", true).await,
                }
                let _ = (x, y);
                return;
            }
            // 左键按下在行上 = 拖拽候选（移动超过阈值才起拖，与壳内行拖拽同一条判据）
            if *button == MouseButton::Left {
                // 新的一次按下说明上一会话的收尾没回到本插件（宿主已取消）：先按未落位收掉
                if self.model.lock().unwrap().dragging() {
                    self.model.lock().unwrap().set_dragging(false);
                    if let Err(error) = ctx.end_drag(&DragEndParams { cancelled: true }).await {
                        self.warn(ctx, "结束拖拽", &error);
                    }
                }
                if let Some((path, is_dir)) = hit.as_deref().and_then(parse_row_id) {
                    if !path.is_empty() {
                        self.pending_drag.lock().unwrap().replace((path, is_dir, *x, *y));
                    }
                }
            }
        }
        // 按住后移动超过阈值 = 起拖（与壳内行拖拽同一判据）
        if let FrameInput::PointerMove { x, y, .. } = &input {
            let pending = { self.pending_drag.lock().unwrap().clone() };
            if let Some((path, is_dir, from_x, from_y)) = pending {
                if (x - from_x).abs() > DRAG_THRESHOLD || (y - from_y).abs() > DRAG_THRESHOLD {
                    self.pending_drag.lock().unwrap().take();
                    let params = DragStartParams {
                        item: atelyx_plugin_protocol::method::DragItem {
                            path,
                            is_dir,
                            label: None,
                        },
                        surface_id: PANEL.to_string(),
                    };
                    match ctx.begin_drag(&params).await {
                        Ok(()) => self.model.lock().unwrap().set_dragging(true),
                        Err(error) => self.warn(ctx, "发起拖拽", &error),
                    }
                }
            }
        }
        // 拖拽中：指针移到行上 / 离开行时把落点候选上报宿主（可放与否由宿主复核）
        if self.model.lock().unwrap().dragging() {
            if let FrameInput::PointerMove { hit, .. } = &input {
                let target = self.drop_target_of(hit.as_deref());
                let params = atelyx_plugin_protocol::method::DragHoverParams {
                    target: target.as_ref().map(|(path, _)| path.clone()),
                    is_dir: target.is_some_and(|(_, is_dir)| is_dir),
                };
                if let Err(error) = ctx.drag_hover(&params).await {
                    self.warn(ctx, "上报拖拽落点", &error);
                }
            }
            // 松手即收尾：宿主按落点落位（没有落点则什么也不做），会话不得留在宿主手里
            if let FrameInput::PointerUp { .. } = &input {
                self.pending_drag.lock().unwrap().take();
                self.model.lock().unwrap().set_dragging(false);
                if let Err(error) = ctx.end_drag(&DragEndParams { cancelled: false }).await {
                    self.warn(ctx, "结束拖拽", &error);
                }
            }
        } else if matches!(input, FrameInput::PointerUp { .. } | FrameInput::PointerLeave) {
            // 按下后没拖动就松手 = 点击，候选不该留到下一次
            self.pending_drag.lock().unwrap().take();
        }
        let events = {
            let mut frame_ui = self.ui.lock().await;
            frame_ui
                .event(ctx, &input, |id| {
                    (id == INLINE).then(|| {
                        let model = self.model.lock().unwrap();
                        model.inline().map(|edit| edit.value().to_string()).unwrap_or_default()
                    })
                })
                .await
        };
        let mut action: Option<String> = None;
        let mut typed: Option<String> = None;
        let mut submitted = false;
        let mut cancelled = false;
        let mut blurred = false;
        for event in events {
            match event {
                UiEvent::Click { id } => action = Some(id),
                UiEvent::Text { id, value } if id == INLINE => typed = Some(value),
                UiEvent::Submit { id } if id == INLINE => submitted = true,
                UiEvent::Cancel { id } if id == INLINE => cancelled = true,
                UiEvent::Focus { id, focused } if id == INLINE => blurred = !focused,
                _ => {}
            }
        }
        if let Some(value) = typed {
            let mut model = self.model.lock().unwrap();
            if let Some(edit) = model.inline_mut() {
                edit.set_value(value);
            }
        }
        // Esc 放弃、失焦提交：与双击改名同一套就地输入语义
        if cancelled {
            self.model.lock().unwrap().take_inline();
        } else if submitted || blurred {
            self.commit_inline(ctx).await;
        } else if let Some(id) = action {
            self.on_action(ctx, id).await;
        }
        self.render(ctx).await;
    }
}

impl FilesPlugin {
    /// 打开就地输入并把焦点交给输入框：不聚焦则键入进不到这一行。
    async fn start_inline(&self, edit: InlineEdit) {
        let value = edit.value().to_string();
        self.model.lock().unwrap().begin_inline(edit);
        self.ui.lock().await.focus_input(INLINE, &value);
    }

    /// 点一个容器行：激活容器收起 / 展开根（与旧面板的三角钮同义），其余容器切过去。
    async fn on_container(&self, ctx: &Context, id: &str) {
        let active = self
            .model
            .lock()
            .unwrap()
            .containers()
            .iter()
            .any(|entry| entry.id == id && entry.active);
        if active {
            self.toggle_dir(ctx, String::new()).await;
            return;
        }
        let params = SwitchContainerParams { id: id.to_string() };
        if let Err(error) = ctx.switch_container(&params).await {
            self.warn(ctx, "切换容器", &error);
        }
    }

    /// 处理一次点击（工具条 / 行）。
    async fn on_action(&self, ctx: &Context, id: String) {
        self.model.lock().unwrap().clear_notice();
        match id.as_str() {
            OPEN_VAULT => {
                if let Err(error) = ctx.pick_vault().await {
                    self.warn(ctx, "打开文件夹选择器", &error);
                }
            }
            SPACE_MENU => {
                if let Err(error) = ctx.open_space_menu().await {
                    self.warn(ctx, "打开协作空间菜单", &error);
                }
            }
            SORT => self.open_sort_menu(ctx).await,
            REFRESH => {
                // 先让宿主重读它自己的文件树缓存（别处也读它），再重取本面的条目
                if let Err(error) = ctx.refresh_tree().await {
                    self.warn(ctx, "刷新宿主文件树", &error);
                }
                self.reload(ctx).await;
                self.render(ctx).await;
            }
            TOGGLE_ALL => self.toggle_all(ctx).await,
            _ => {
                // 容器行：激活容器收起 / 展开根，其余容器切过去
                if let Some(id) = id.strip_prefix(CONTAINER) {
                    self.on_container(ctx, id).await;
                    return;
                }
                if let Some((path, is_dir)) = parse_row_id(&id) {
                    if is_dir {
                        self.toggle_dir(ctx, path).await;
                    } else if let Err(error) = ctx.open_file_quiet(&path).await {
                        // 打开但不改布局：有对应视图就显示在里面，没有也不新建标签
                        self.warn(ctx, &format!("打开文件 {path}"), &error);
                    }
                    return;
                }

            }
        }
    }

    /// 菜单选中：新建 / 颜色 / 提示词 / 重命名 / 删除 / 排序 / 切容器。
    async fn on_menu_action(&self, ctx: &Context, selected: String) {
        self.model.lock().unwrap().clear_notice();
        if let Some(kind) = selected.strip_prefix("sort:") {
            let patch = VaultUiPatch { sort: Some(kind.to_string()), ..VaultUiPatch::default() };
            if let Err(error) = ctx.vault_update_ui(&patch).await {
                self.warn(ctx, "写排序", &error);
            }
            self.reload(ctx).await;
            return;
        }
        if let Some(id) = selected.strip_prefix("switch:") {
            let params =
                atelyx_plugin_protocol::method::SwitchContainerParams { id: id.to_string() };
            if let Err(error) = ctx.switch_container(&params).await {
                self.warn(ctx, "切换容器", &error);
            }
            return;
        }
        if let Some(id) = selected.strip_prefix(REMOVE_CONTAINER) {
            let params = RemoveContainerParams { id: id.to_string() };
            match ctx.remove_container(&params).await {
                Ok(()) => self.reload(ctx).await,
                Err(error) => self.warn(ctx, &format!("移除容器 {id}"), &error),
            }
            return;
        }
        if let Some(rest) = selected.strip_prefix("draft:") {
            let Some((kind, dir)) = rest.split_once(':') else { return };
            let kind = match kind {
                "note" => DraftKind::Note,
                "table" => DraftKind::Table,
                "canvas" => DraftKind::Canvas,
                "folder" => DraftKind::Folder,
                other => DraftKind::Doc(other.to_string()),
            };
            self.start_inline(InlineEdit::Draft {
                dir: dir.to_string(),
                kind,
                value: String::new(),
            })
            .await;
            return;
        }
        if let Some(path) = selected.strip_prefix("color:") {
            let initial = self.model.lock().unwrap().folder_colors().get(path).cloned();
            let params = PickColorParams { initial };
            match ctx.pick_color(&params).await {
                Ok(result) => {
                    if let Some(color) = result.color {
                        self.set_folder_color(ctx, path, Some(color)).await;
                    }
                }
                Err(error) => self.warn(ctx, "取色", &error),
            }
            return;
        }
        if let Some(path) = selected.strip_prefix("prompt:") {
            self.toggle_prompt_note(ctx, path).await;
            return;
        }
        if let Some(path) = selected.strip_prefix("rename:") {
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            let is_dir = path.is_empty() || self.is_dir(&path);
            self.start_inline(InlineEdit::Rename {
                path: path.to_string(),
                is_dir,
                value: if is_dir { name } else { stem_of(&name).to_string() },
            })
            .await;
            return;
        }
        if let Some(id) = selected.strip_prefix("delete:") {
            let (path, flag) = id.rsplit_once('|').unwrap_or((id, "f"));
            self.open_delete_confirm(ctx, path, flag == "d").await;
            return;
        }
        if let Some(id) = selected.strip_prefix("delete-confirm:") {
            let (path, _) = id.rsplit_once('|').unwrap_or((id, "f"));
            if let Err(error) = ctx.vault_remove(path).await {
                self.warn(ctx, &format!("删除 {path}"), &error);
            }
            self.reload(ctx).await;
            return;
        }
        if let Some(id) = selected.strip_prefix("delete-cancel:") {
            let (path, flag) = id.rsplit_once('|').unwrap_or((id, "f"));
            self.open_row_menu(ctx, path, flag == "d").await;
            return;
        }
        if let Some(root) = selected.strip_prefix("reveal-vault:") {
            self.reveal(ctx, root, true).await;
            return;
        }
        if let Some(rest) = selected.strip_prefix("reveal:") {
            let (path, is_dir) = rest.rsplit_once('|').unwrap_or((rest, "f"));
            self.reveal(ctx, path, is_dir == "d").await;
            return;
        }
        if let Some(rest) = selected.strip_prefix("copy-path:") {
            let (path, absolute) = rest.rsplit_once('|').unwrap_or((rest, "f"));
            self.copy_path(ctx, path, absolute == "a").await;
            return;
        }
        if let Some(rest) = selected.strip_prefix("duplicate:") {
            let (path, is_dir) = rest.rsplit_once('|').unwrap_or((rest, "f"));
            let params = DuplicateParams { path: path.to_string(), is_dir: is_dir == "d" };
            match ctx.duplicate_entry(&params).await {
                Ok(_) => self.reload(ctx).await,
                Err(error) => self.warn(ctx, "创建副本", &error),
            }
        }
    }

    /// 在系统文件管理器中显示（本地仓库才有本机路径）。
    async fn reveal(&self, ctx: &Context, path: &str, is_dir: bool) {
        if let Err(error) = ctx.reveal_in_file_manager(path, is_dir).await {
            self.warn(ctx, "打开文件管理器", &error);
        }
    }

    /// 复制条目的绝对 / 相对路径。
    async fn copy_path(&self, ctx: &Context, path: &str, absolute: bool) {
        let text = match absolute {
            true => match ctx.vault_absolute_path(path).await {
                Ok(text) => text,
                Err(error) => {
                    self.warn(ctx, "取绝对路径", &error);
                    return;
                }
            },
            false => path.to_string(),
        };
        if let Err(error) = ctx.write_clipboard(&text).await {
            self.warn(ctx, "复制路径", &error);
        }
    }

    /// 跟宿主展开一个目录（拖拽悬停自动展开用）：已展开、或不是目录即无操作。
    async fn follow_host_expand(&self, ctx: &Context, dir: String) {
        if self.model.lock().unwrap().expanded().iter().any(|item| item == &dir) {
            return;
        }
        if !self.is_dir(&dir) {
            return;
        }
        self.model.lock().unwrap().expand(dir.clone());
        let expanded = self.model.lock().unwrap().expanded().to_vec();
        if !self.children.lock().unwrap().contains_key(&dir) {
            match ctx.vault_ui(&VaultUiParams { dir: Some(dir.clone()) }).await {
                Ok(result) => {
                    self.children.lock().unwrap().insert(dir, result.entries);
                }
                Err(error) => self.warn(ctx, &format!("打开目录 {dir}"), &error),
            }
        }
        self.save_expanded(ctx, expanded).await;
    }

    /// 拖拽落点：行 → 该条目；行侧缝 / 列表下方空白 / 激活容器行 → 仓库根（空路径）；其余不接受落入。
    fn drop_target_of(&self, hit: Option<&str>) -> Option<(String, bool)> {
        let id = hit?;
        if id.starts_with(DROP_ROOT) {
            return Some((String::new(), true));
        }
        if let Some(container) = id.strip_prefix(CONTAINER) {
            let active = self
                .model
                .lock()
                .unwrap()
                .containers()
                .iter()
                .any(|entry| entry.id == container && entry.active);
            return active.then(|| (String::new(), true));
        }
        parse_row_id(id).filter(|(path, _)| !path.is_empty())
    }

    /// 某路径是否为目录（查已取到的目录条目缓存）。
    fn is_dir(&self, path: &str) -> bool {
        let parent = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
        self.children
            .lock()
            .unwrap()
            .get(parent)
            .map(|entries| entries.iter().any(|entry| entry.path == path && entry.is_dir))
            .unwrap_or(false)
    }

    /// 提交就地输入（重命名走 `vault.rename`，新建走 `vault.create`）。
    async fn commit_inline(&self, ctx: &Context) {
        let Some(edit) = self.model.lock().unwrap().take_inline() else { return };
        match edit {
            InlineEdit::Draft { dir, kind, value } => {
                let name = value.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let params = CreateEntryParams { dir, name, kind: kind.wire().to_string() };
                if let Err(error) = ctx.create_entry(&params).await {
                    self.warn(ctx, "新建条目", &error);
                }
                self.reload(ctx).await;
            }
            InlineEdit::Rename { path, is_dir, value } => {
                let name = value.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let to = rename_target(&path, is_dir, &name);
                if to == path {
                    return;
                }
                if let Err(error) = ctx.vault_rename(&path, &to).await {
                    self.warn(ctx, "重命名", &error);
                }
                self.reload(ctx).await;
            }
        }
    }

    /// 写一份目录颜色（整表替换，宿主落到仓库侧文件）。
    async fn set_folder_color(&self, ctx: &Context, path: &str, color: Option<String>) {
        let mut colors = self.model.lock().unwrap().folder_colors().clone();
        match color {
            Some(value) => {
                colors.insert(path.to_string(), value);
            }
            None => {
                colors.remove(path);
            }
        }
        let patch = VaultUiPatch { folder_colors: Some(colors), ..VaultUiPatch::default() };
        if let Err(error) = ctx.vault_update_ui(&patch).await {
            self.warn(ctx, "写目录颜色", &error);
        }
        self.reload(ctx).await;
    }

    /// 切换提示词标记（整表替换）。
    async fn toggle_prompt_note(&self, ctx: &Context, path: &str) {
        let mut notes = self.model.lock().unwrap().prompt_notes().to_vec();
        if let Some(index) = notes.iter().position(|item| item == path) {
            notes.remove(index);
        } else {
            notes.push(path.to_string());
        }
        let patch = VaultUiPatch { prompt_notes: Some(notes), ..VaultUiPatch::default() };
        if let Err(error) = ctx.vault_update_ui(&patch).await {
            self.warn(ctx, "写提示词标记", &error);
        }
        self.reload(ctx).await;
    }
}

/// 重命名目标路径：输入里手打的原后缀先剥掉再拼回，`b.md` 重命名 `a.md` 得 `b.md` 而不是 `b.md.md`。
fn rename_target(path: &str, is_dir: bool, typed: &str) -> String {
    let parent = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let original = path.rsplit('/').next().unwrap_or(path);
    // 输入框初值就是主名：主名本身带同后缀的文件（a.md.md）直接回车不是改名
    if !is_dir && typed == crate::model::stem_of(original) {
        return path.to_string();
    }
    let name = match if is_dir { None } else { crate::model::extension_of(original) } {
        Some(ext) => match typed.rsplit_once('.') {
            Some((stem, typed_ext)) if !stem.is_empty() && typed_ext.eq_ignore_ascii_case(ext) => {
                format!("{stem}.{ext}")
            }
            _ => format!("{typed}.{ext}"),
        },
        None => typed.to_string(),
    };
    if parent.is_empty() {
        name
    } else {
        format!("{parent}/{name}")
    }
}

#[no_mangle]
pub extern "C" fn atelyx_plugin_api() -> *const atelyx_plugin_sdk::abi::PluginApi {
    atelyx_plugin_sdk::in_process::plugin_api(
        || PluginInfo {
            id: PLUGIN_ID.into(),
            name: "文件".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        || Box::new(FilesPlugin::new()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 随包安装读取的是 `plugin.json`，与动态库导出的身份、契约版本必须一致。
    #[test]
    fn manifest_matches_the_crate_identity() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(manifest["id"], PLUGIN_ID);
        assert_eq!(manifest["name"], "文件");
        assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest["apiVersion"], atelyx_plugin_protocol::API_VERSION);
        let (platform, entry) = if cfg!(windows) {
            ("windows", "atelyx_plugin_files.dll")
        } else {
            ("linux", "libatelyx_plugin_files.so")
        };
        assert_eq!(manifest["entry"][platform], entry);
    }

    #[test]
    fn row_ids_round_trip() {
        assert_eq!(parse_row_id(&row_id("notes/a.md", false)), Some(("notes/a.md".into(), false)));
        assert_eq!(parse_row_id(&row_id("docs", true)), Some(("docs".into(), true)));
        assert_eq!(parse_row_id("toggle-all"), None);
        // 路径里的冒号只按首个分隔符切，其余留给路径
        assert_eq!(parse_row_id("row:f:a:b"), Some(("a:b".into(), false)));
    }

    /// 手打后缀不该和原后缀叠成 `b.md.md`；换别的后缀则原样保留用户输入。
    #[test]
    fn rename_target_strips_a_retyped_extension() {
        assert_eq!(rename_target("a.md", false, "b.md"), "b.md");
        assert_eq!(rename_target("a.md", false, "b.MD"), "b.md");
        assert_eq!(rename_target("a.md", false, "b"), "b.md");
        assert_eq!(rename_target("a.md", false, "b.txt"), "b.txt.md");
        assert_eq!(rename_target("docs/a.md", false, "b"), "docs/b.md");
        assert_eq!(rename_target("docs", true, "notes"), "notes");
        assert_eq!(rename_target(".gitignore", false, ".env"), ".env");
    }

    /// 输入框初值就是主名：主名本身带同后缀的文件直接回车不是改名。
    #[test]
    fn rename_target_keeps_a_double_suffixed_name_unchanged() {
        assert_eq!(rename_target("a.md.md", false, "a.md"), "a.md.md");
        assert_eq!(rename_target("docs/表.md.md", false, "表.md"), "docs/表.md.md");
    }
}
