//! 协作空间模式的壳侧编排：账号与会话恢复、进入/离开空间、内容面按空间身份分派服务端。
//! 纯逻辑与网络 I/O 在 core（space_client / collab / note_collab）；本模块只做
//! 「取 App 状态 → 调 core → 应用结果 → 触副作用」的编排，与 table_actions 同构。

use iced::Task;
use iced::widget::text_editor;

use crate::layout_view::{RenameState, RenameTarget};
use crate::canvas_actions;
use crate::table_view as tv;
use crate::ui::notify::Level as NoticeLevel;
use crate::{
    layout_view, vault, App, DraftKind, Message, NoteMode, SaveReason, SpaceField, SpaceForm,
    SpaceMsg,
};
use atelyx_core::space_client::{
    self, AcceptAck, InviteInfo, MemberInfo, SpaceClient, SpaceError, SpaceRole, SpaceSummary,
};
use atelyx_core::credentials;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// 历史合并窗口（毫秒），与本地侧车同口径。
const HISTORY_COALESCE_MS: i64 = 60_000;

/// 设备名缺省值（登录表单留空 / WS hello）。
pub(crate) const DEFAULT_DEVICE_NAME: &str = "桌面端";

/// 本端客户端版本（hello 上报，在线设备卡片展示）。
pub(crate) const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 本端平台标识（hello 上报；与 `std::env::consts::OS` 同词表）。
pub(crate) fn client_platform() -> &'static str {
    std::env::consts::OS
}

/// 平台标识 → 展示名（未知标识原样透出，缺失由调用方标注「未知平台」）。
pub(crate) fn platform_label(token: &str) -> String {
    match token.to_ascii_lowercase().as_str() {
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        "android" => "Android".into(),
        "macos" => "macOS".into(),
        other => other.to_string(),
    }
}

/// 协作在场色缺省值（用户未自定义 collab_color 时的设备标色）。
pub(crate) const DEFAULT_COLLAB_COLOR: &str = "#30bced";

/// 他端结构变更的树刷新合并窗口：连续建删（批量导入等）只刷一次。
const FS_TREE_REFRESH_COALESCE: std::time::Duration = std::time::Duration::from_millis(600);

/// 激活的协作空间会话（内容面按它分派服务端）。
#[derive(Clone)]
pub struct SpaceSession {
    pub client: Arc<SpaceClient>,
    pub server_url: String,
    pub space_id: String,
    pub name: String,
    pub role: SpaceRole,
}

/// 成员管理操作（成员弹层行内二段确认的确认对象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberConfirm {
    /// 转让管理权（自身降为编辑者）。
    Transfer,
    /// 移出空间。
    Remove,
}

impl MemberConfirm {
    fn done_label(self) -> &'static str {
        match self {
            MemberConfirm::Transfer => "转让失败",
            MemberConfirm::Remove => "移除成员失败",
        }
    }

    fn ok_notice(self) -> &'static str {
        match self {
            MemberConfirm::Transfer => "管理权已转让",
            MemberConfirm::Remove => "成员已移除",
        }
    }
}

/// 邀请码表单草稿（数字字段保留原文，空 = 服务端缺省语义）。
#[derive(Debug, Clone)]
pub struct InviteForm {
    pub role: SpaceRole,
    pub hours: String,
    pub uses: String,
}

impl Default for InviteForm {
    fn default() -> Self {
        InviteForm { role: SpaceRole::Editor, hours: String::new(), uses: String::new() }
    }
}

/// 服务端文件树 → 本地树模型（字段逐一对应，消费方复用本地遍历函数）。
fn to_file_tree(nodes: &[space_client::TreeNode]) -> Vec<vault::FileTreeNode> {
    nodes
        .iter()
        .map(|n| vault::FileTreeNode {
            name: n.name.clone(),
            path: n.path.clone(),
            is_dir: n.is_dir,
            updated_at: n.updated_at,
            children: to_file_tree(&n.children),
        })
        .collect()
}

/// 树中定位目录并产出子项列表（rel 为空 = 根）。目录不存在 = 空列表（与本地语义一致）。
pub(crate) fn children_of_tree(tree: &[space_client::TreeNode], rel: &str) -> Vec<vault::DirChild> {
    let mut current = tree;
    if !rel.is_empty() {
        for seg in rel.split('/') {
            match current.iter().find(|n| n.is_dir && n.name == seg) {
                Some(dir) => current = &dir.children,
                None => return Vec::new(),
            }
        }
    }
    current
        .iter()
        .map(|n| vault::DirChild { rel: n.path.clone(), is_dir: n.is_dir, mtime: n.updated_at })
        .collect()
}

/// 树中是否存在指定相对路径的文件（目录不算）。
pub(crate) fn tree_has_file(tree: &[space_client::TreeNode], rel: &str) -> bool {
    let parent = rel.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("");
    children_of_tree(tree, parent)
        .iter()
        .any(|child| !child.is_dir && child.rel == rel)
}

/// 从整树统计文件数（目录不计）。
fn count_tree_files(tree: &[space_client::TreeNode]) -> usize {
    tree.iter()
        .map(|n| if n.is_dir { count_tree_files(&n.children) } else { 1 })
        .sum()
}

pub fn session_of(app: &App) -> Option<SpaceSession> {
    match &app.repo_kind {
        crate::RepoKind::Space(s) => Some(s.clone()),
        crate::RepoKind::Local => None,
    }
}

/// 取会话并裁决写能力：viewer 直接提示不发起请求（服务端本会 403，无需空跑）。
fn require_write(app: &mut App) -> Option<SpaceSession> {
    let session = session_of(app)?;
    if !session.role.can_write() {
        app.notify(NoticeLevel::Warning, "该空间为只读身份，无法执行此操作");
        return None;
    }
    Some(session)
}

/// 空间身份键（展开集记忆 / 最近列表对齐用）。
pub fn space_key_of(app: &App) -> Option<String> {
    session_of(app).map(|s| format!("{}/{}", s.server_url, s.space_id))
}

/// 401 视为会话过期：不摘身份（打开中内容与在途回执保留），弹登录层预填服务器地址；
/// 重新登录后由 on_login_done 原地换发会话客户端。其余错误提示。
fn on_error(app: &mut App, e: SpaceError, prefix: &str) -> Task<Message> {
    if matches!(e, SpaceError::Unauthorized(_)) {
        app.notify(NoticeLevel::Error, format!("{prefix}：{e}"));
        app.space_form = SpaceForm {
            server: session_of(app)
                .map(|s| s.server_url)
                .or_else(|| app.global.last_identity.as_ref().map(|i| i.server_url.clone()))
                .unwrap_or_default(),
            ..Default::default()
        };
        return match app.main_id {
            Some(window) => open_login_popup(app, window),
            None => Task::none(),
        };
    }
    app.notify(NoticeLevel::Error, format!("{prefix}：{}", e));
    Task::none()
}

fn err_text(e: SpaceError) -> String {
    e.to_string()
}

/// 依据登录身份取可用客户端（设置页设备管理等无激活空间也需要的操作）：
/// 会话令牌优先，回落密钥链。Err = 不可用原因（无身份 / 密钥链读取失败 / 本机无令牌 / 地址无效），
/// 调用方须向用户呈现，不得静默吞掉。
fn client_from_identity(app: &App) -> Result<Arc<SpaceClient>, String> {
    if let crate::RepoKind::Space(s) = &app.repo_kind {
        return Ok(s.client.clone());
    }
    let identity = app
        .global
        .last_identity
        .clone()
        .ok_or_else(|| "没有登录身份".to_string())?;
    let session = app
        .session_token
        .as_ref()
        .filter(|(s, _)| *s == identity.server_url)
        .map(|(_, t)| t.clone());
    let token = match session {
        Some(t) => t,
        // 密钥链后端不可用（Err）与本机从未登录（None）分开报
        None => credentials::load_token(&identity.server_url, &identity.username)?
            .ok_or_else(|| "本机没有已保存的登录令牌，请重新登录".to_string())?,
    };
    Ok(Arc::new(SpaceClient::new(&identity.server_url, token)?))
}

/// 拉取设备会话列表（设置页「多人协作」打开时）。
pub fn fetch_devices(app: &mut App) -> Task<Message> {
    let client = match client_from_identity(app) {
        Ok(c) => c,
        Err(e) => {
            app.notify(NoticeLevel::Warning, format!("设备列表不可用：{e}"));
            return Task::none();
        }
    };
    Task::perform(
        async move { client.list_devices().await },
        move |result| Message::Space(SpaceMsg::DevicesLoaded { result }),
    )
}

/// 吊销指定设备会话。
pub fn revoke_device(app: &mut App, session_id: String) -> Task<Message> {
    let client = match client_from_identity(app) {
        Ok(c) => c,
        Err(e) => {
            app.notify(NoticeLevel::Warning, format!("操作不可用：{e}"));
            return Task::none();
        }
    };
    Task::perform(
        async move {
            let r = client.revoke_device(&session_id).await;
            (session_id, r)
        },
        move |(session_id, result)| Message::Space(SpaceMsg::DeviceRevoked { session_id, result }),
    )
}

/// 退出登录：清密钥链令牌、服务器清单与最近空间，摘掉激活空间身份。
fn logout(app: &mut App) -> Task<Message> {
    let identity = app.global.last_identity.clone();
    if let Some(identity) = &identity {
        let _ = credentials::delete_token(&identity.server_url, &identity.username);
    }
    app.dismiss_popup();
    teardown_pump(app);
    if matches!(app.repo_kind, crate::RepoKind::Space(_)) {
        leave_space_identity(app);
    }
    app.global.remove_server(
        &identity.map(|i| i.server_url).unwrap_or_default(),
    );
    app.space_devices.clear();
    app.session_token = None;
    app.space_token = None;
    app.notify(NoticeLevel::Success, "已退出登录");
    app.save_global()
}

/// 打开登录 / 注册弹层（表单草稿在 App.space_form；锚点 = 最近光标位置）。
pub fn open_login_popup(app: &mut App, window: iced::window::Id) -> Task<Message> {
    let register = app.space_form.register;
    let cursor = app.cursor_for(window);
    app.dismiss_popup();
    app.popup = Some((
        window,
        layout_view::PopupOpen {
            point: cursor,
            flip_y: None,
            state: layout_view::PopupState::SpaceLogin { register },
            min_width: 320.0,
        },
    ));
    Task::none()
}

// ===== 账号与会话 =====

/// 登录 / 注册提交：令牌入密钥链、身份入 global，随后拉空间列表。
fn login_submit(app: &mut App, register: bool) -> Task<Message> {
    if app.space_busy {
        return Task::none();
    }
    let form = app.space_form.clone();
    if form.server.trim().is_empty() || form.username.trim().is_empty() || form.password.is_empty() {
        app.notify(NoticeLevel::Warning, "服务器地址、用户名与密码不能为空");
        return Task::none();
    }
    app.space_busy = true;
    let server_url = space_client::normalize_server_url(&form.server);
    let server_url_for_msg = server_url.clone();
    let username = form.username.trim().to_string();
    let password = form.password.clone();
    let device = if form.device.trim().is_empty() {
        DEFAULT_DEVICE_NAME.to_string()
    } else {
        form.device.trim().to_string()
    };
    // 稳定设备 id：首次登录生成并随 global.json 持久化；服务端据此合并同账号会话
    let device_id = app
        .global
        .device_id
        .get_or_insert_with(|| nanoid::nanoid!(21))
        .clone();
    Task::perform(
        async move {
            let client = SpaceClient::new(&server_url, String::new()).map_err(SpaceError::Network)?;
            let auth = if register {
                client.register(&username, &password, None, Some(&device), Some(&device_id)).await?
            } else {
                client.login(&username, &password, Some(&device), Some(&device_id)).await?
            };
            Ok::<_, SpaceError>(auth)
        },
        move |result| Message::Space(SpaceMsg::LoginDone { server_url: server_url_for_msg, result }),
    )
}

fn on_login_done(
    app: &mut App,
    server_url: String,
    result: Result<space_client::AuthInfo, SpaceError>,
) -> Task<Message> {
    app.space_busy = false;
    // 登录层自身的失败不走会话过期收口（401 = 凭据错误，保留当前空间会话与表单重试）
    let auth = match result {
        Err(e) => {
            app.notify(NoticeLevel::Error, format!("登录失败：{e}"));
            return Task::none();
        }
        Ok(auth) => auth,
    };
    // 密码仅存活到提交：成功 / 失败都清，不滞留内存
    app.space_form.password.clear();
    app.global.set_identity(&server_url, &auth.username, &auth.display_name);
    app.space_token = Some(auth.token.clone());
    // 令牌入密钥链；不可用时降级会话内令牌（不落明文，提示说明）
    match credentials::store_token(&server_url, &auth.username, &auth.token) {
        Ok(()) => app.session_token = None,
        Err(e) => {
            app.session_token = Some((server_url.clone(), auth.token.clone()));
            app.notify(NoticeLevel::Warning, format!("令牌未能存入系统密钥链，仅本次会话有效：{e}"));
        }
    }
    let client = match SpaceClient::new(&server_url, auth.token) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            app.notify(NoticeLevel::Error, format!("服务器地址无效：{e}"));
            return Task::none();
        }
    };
    // 换号重登同一空间：原地换发会话客户端（令牌变更 → 订阅 spec 变更自动换泵）
    if let crate::RepoKind::Space(session) = &mut app.repo_kind {
        if session.server_url == server_url {
            session.client = client.clone();
            teardown_pump(app);
            // 换发令牌会踢掉旧连接：泵可能已因 Denied 置闸，重登后复位以重建长连接
            app.space_pump_denied = false;
        }
    }
    app.notify(NoticeLevel::Success, "已登录");
    // 协作展示色：首次登录随机指派（远端光标 / 名单；固定 7 色池）
    if app.global.collab_color.is_none() {
        const COLORS: [&str; 7] = [
            "#e06c75", "#61afef", "#98c379", "#e5c07b", "#c678dd", "#56b6c2", "#d19a66",
        ];
        let pick = COLORS[(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0))
            % COLORS.len()];
        app.global.collab_color = Some(pick.to_string());
    }
    // 登录前挂起的目标空间自动续接
    let pending = app.space_pending.take();
    let mut task = match app.main_id {
        Some(window) => {
            app.dismiss_popup();
            refresh_spaces(app, window, client.clone())
        }
        None => Task::none(),
    };
    task = task.chain(app.save_global());
    if let Some((server, space_id, name)) = pending {
        if server == server_url {
            task = task.chain(Task::done(Message::Space(SpaceMsg::SelectSpace {
                server_url: server,
                space_id,
                name,
                from_boot: false,
            })));
        }
    }
    task
}

/// 拉取空间列表（登录后 / 手动刷新共用）：成功后打开空间菜单展示。
fn refresh_spaces(_app: &mut App, window: iced::window::Id, client: Arc<SpaceClient>) -> Task<Message> {
    let server_url = client.base_url().to_string();
    Task::perform(
        async move {
            match client.list_spaces().await {
                Ok(list) => (server_url, Ok(list)),
                Err(e) => (server_url, Err(e)),
            }
        },
        move |(server_url, result)| {
            Message::Space(SpaceMsg::SpacesLoaded { server_url, result, window })
        },
    )
}

/// 文件面板云钮：打开空间菜单（未登录先弹登录层）；列表后台刷新对齐
/// （恢复会话等路径 space_list 可能陈旧，菜单即时呈现、回执后重弹更新）。
pub fn open_space_menu(app: &mut App, window: iced::window::Id, cursor: iced::Point) -> Task<Message> {
    if app.global.last_identity.is_none() {
        app.space_form = SpaceForm { server: String::new(), ..Default::default() };
        return open_login_popup(app, window);
    }
    app.dismiss_popup();
    app.popup = Some((
        window,
        layout_view::PopupOpen {
            point: iced::Point::new(cursor.x, cursor.y + 16.0),
            flip_y: Some(cursor.y - 12.0),
            state: layout_view::PopupState::SpaceMenu,
            min_width: 200.0,
        },
    ));
    client_from_identity(app)
        .map(|client| refresh_spaces(app, window, client))
        .unwrap_or_else(|_| Task::none())
}

// ===== 成员名册与邀请码 =====

/// 拉取空间成员名册（协作房间视图与成员弹层共用缓存；各展示方自带装载守卫）。
pub fn fetch_members(app: &mut App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    app.space_members_loading = true;
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.list_members(&space_id).await },
        move |result| Message::Space(SpaceMsg::MembersLoaded { gen, result }),
    )
}

/// 成员名册懒装载（PersistTick 搭载）：名册视图或成员弹层在展示且无有效缓存时拉取。
/// 加载失败不自动重试（重开弹层即重拉），防失败后周期空转。
pub fn members_tick(app: &mut App) -> Task<Message> {
    if app.space_members_loading
        || app.space_members.is_some()
        || app.space_members_error.is_some()
    {
        return Task::none();
    }
    let shown = layout_view::view_displayed(app, "collabroom")
        || matches!(
            app.popup.as_ref().map(|(_, open)| &open.state),
            Some(layout_view::PopupState::SpaceMembers)
        );
    if !shown || session_of(app).is_none() {
        return Task::none();
    }
    fetch_members(app)
}

fn on_members_loaded(
    app: &mut App,
    gen: u64,
    result: Result<Vec<MemberInfo>, SpaceError>,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    app.space_members_loading = false;
    match result {
        Ok(list) => {
            app.space_members = Some(list);
            app.space_members_error = None;
        }
        Err(e) => app.space_members_error = Some(e.to_string()),
    }
    Task::none()
}

/// 成员操作前置：仅 owner 可管理（入口已隐藏，此处兜底防在途角色漂移）。
fn require_owner(app: &mut App) -> Option<SpaceSession> {
    let session = session_of(app)?;
    if session.role != SpaceRole::Owner {
        app.notify(NoticeLevel::Warning, "仅空间所有者可执行此操作");
        return None;
    }
    Some(session)
}

/// 转让管理权（owner 专属）：成功后自身降为编辑者，重拉名册并刷新空间列表对齐角色。
fn member_transfer(app: &mut App, user_id: String) -> Task<Message> {
    let Some(session) = require_owner(app) else { return Task::none() };
    if app.space_busy {
        return Task::none();
    }
    app.space_busy = true;
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.transfer_space(&space_id, &user_id).await },
        move |result| {
            Message::Space(SpaceMsg::MemberOpDone { gen, op: MemberConfirm::Transfer, result })
        },
    )
}

/// 移出成员（owner 专属）。
fn member_remove(app: &mut App, user_id: String) -> Task<Message> {
    let Some(session) = require_owner(app) else { return Task::none() };
    if app.space_busy {
        return Task::none();
    }
    app.space_busy = true;
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.remove_member(&space_id, &user_id).await },
        move |result| {
            Message::Space(SpaceMsg::MemberOpDone { gen, op: MemberConfirm::Remove, result })
        },
    )
}

fn on_member_op_done(
    app: &mut App,
    gen: u64,
    op: MemberConfirm,
    result: Result<(), SpaceError>,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    app.space_busy = false;
    app.space_member_confirm = None;
    match result {
        Err(e) => on_error(app, e, op.done_label()),
        Ok(()) => {
            app.notify(NoticeLevel::Success, op.ok_notice());
            let task = fetch_members(app);
            // 转让后自身角色变化：刷新空间列表（会话角色 / 行菜单权限随回执对齐）
            if op == MemberConfirm::Transfer {
                if let Some(session) = session_of(app) {
                    let window = app
                        .popup
                        .as_ref()
                        .map(|(w, _)| *w)
                        .or(app.main_id)
                        .unwrap_or_else(iced::window::Id::unique);
                    return task.chain(refresh_spaces(app, window, session.client));
                }
            }
            task
        }
    }
}

/// 拉取邀请码列表（owner 专属端点；非 owner 打开弹层清缓存只提示不空跑）。
pub fn fetch_invites(app: &mut App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    if session.role != SpaceRole::Owner {
        // 角色已非 owner（如转让后）：缓存连同撤销入口一并失效
        app.space_invites = None;
        app.space_invites_loading = false;
        app.space_invites_error = Some("邀请码仅空间所有者可管理".to_string());
        return Task::none();
    }
    app.space_invites_loading = true;
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.list_invites(&space_id).await },
        move |result| Message::Space(SpaceMsg::InvitesLoaded { gen, result }),
    )
}

fn on_invites_loaded(
    app: &mut App,
    gen: u64,
    result: Result<Vec<InviteInfo>, SpaceError>,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    app.space_invites_loading = false;
    match result {
        Ok(list) => {
            app.space_invites = Some(list);
            app.space_invites_error = None;
        }
        Err(e) => app.space_invites_error = Some(e.to_string()),
    }
    Task::none()
}

/// 生成邀请码（表单草稿 → 服务端；数字字段空 = 服务端缺省）。
fn create_invite(app: &mut App) -> Task<Message> {
    let Some(session) = require_owner(app) else { return Task::none() };
    if app.space_busy {
        return Task::none();
    }
    let form = app.space_invite_form.clone();
    let hours = match form.hours.trim().parse::<i64>() {
        Ok(h) => Some(h),
        Err(_) if form.hours.trim().is_empty() => None,
        Err(_) => {
            app.notify(NoticeLevel::Warning, "过期小时须为整数");
            return Task::none();
        }
    };
    let uses = match form.uses.trim().parse::<u32>() {
        Ok(n) => Some(n),
        Err(_) if form.uses.trim().is_empty() => None,
        Err(_) => {
            app.notify(NoticeLevel::Warning, "次数上限须为正整数");
            return Task::none();
        }
    };
    app.space_busy = true;
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    let role = form.role;
    Task::perform(
        async move { client.create_invite(&space_id, role, hours, uses).await },
        move |result| Message::Space(SpaceMsg::InviteCreated { gen, result }),
    )
}

fn on_invite_created(
    app: &mut App,
    gen: u64,
    result: Result<InviteInfo, SpaceError>,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    app.space_busy = false;
    match result {
        Err(e) => on_error(app, e, "生成邀请码失败"),
        Ok(info) => {
            app.space_invite_result = Some(info);
            app.space_invite_copied = false;
            app.space_invite_confirm = None;
            fetch_invites(app)
        }
    }
}

/// 撤销邀请码（owner 专属；二段确认后调用）。
fn invite_revoke(app: &mut App, code: String) -> Task<Message> {
    let Some(session) = require_owner(app) else { return Task::none() };
    if app.space_busy {
        return Task::none();
    }
    app.space_busy = true;
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let r = client.revoke_invite(&space_id, &code).await;
            (code, r)
        },
        move |(code, result)| Message::Space(SpaceMsg::InviteRevoked { gen, code, result }),
    )
}

fn on_invite_revoked(
    app: &mut App,
    gen: u64,
    code: String,
    result: Result<(), SpaceError>,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    app.space_busy = false;
    app.space_invite_confirm = None;
    match result {
        Err(e) => on_error(app, e, "撤销邀请码失败"),
        Ok(()) => {
            app.notify(NoticeLevel::Success, "邀请码已撤销");
            if app.space_invite_result.as_ref().is_some_and(|i| i.code == code) {
                app.space_invite_result = None;
            }
            fetch_invites(app)
        }
    }
}

/// 恢复会话并进入目标空间：密钥链 / 会话内令牌 → list_spaces 校验 → 进入或弹登录。
fn restore_and_enter(app: &mut App, server_url: String, space_id: String, name: String, from_boot: bool) -> Task<Message> {
    let username = app.global.last_identity.as_ref().map(|i| i.username.clone());
    let token = username
        .as_deref()
        .and_then(|u| credentials::load_token(&server_url, u).ok().flatten())
        .or_else(|| {
            app.session_token
                .as_ref()
                .filter(|(s, _)| *s == server_url)
                .map(|(_, t)| t.clone())
        });
    let Some(token) = token else {
        // 无可恢复会话：启动自动进入静默停留；手动点击弹登录层（登录成功自动续接）
        app.space_pending = Some((server_url.clone(), space_id, name));
        app.space_form = SpaceForm { server: server_url, ..Default::default() };
        if from_boot {
            return Task::none();
        }
        return match app.main_id {
            Some(window) => open_login_popup(app, window),
            None => Task::none(),
        };
    };
    app.space_busy = true;
    app.space_token = Some(token.clone());
    let server_url_for_msg = server_url.clone();
    Task::perform(
        async move {
            let client = SpaceClient::new(&server_url, token).map_err(SpaceError::Network)?;
            // 列表兼可达性/令牌校验（401 → 会话过期），并喂给 space_list 与角色判定
            // （恢复路径没有其他拉取点，否则空间菜单恒空、角色恒回落）
            let spaces = client.list_spaces().await?;
            Ok::<_, SpaceError>((Arc::new(client), spaces))
        },
        move |result| Message::Space(SpaceMsg::RestoreDone { server_url: server_url_for_msg, space_id, name, from_boot, result }),
    )
}

fn on_restore_done(
    app: &mut App,
    server_url: String,
    space_id: String,
    name: String,
    from_boot: bool,
    result: Result<(Arc<SpaceClient>, Vec<SpaceSummary>), SpaceError>,
) -> Task<Message> {
    app.space_busy = false;
    match result {
        Err(e) => on_error(app, e, "进入空间失败"),
        Ok((client, spaces)) => {
            app.space_list = spaces;
            let role = app
                .space_list
                .iter()
                .find(|s| s.space_id == space_id)
                .map(|s| s.role)
                .unwrap_or(SpaceRole::Editor);
            let session = SpaceSession { client, server_url, space_id, name, role };
            enter_space(app, session, from_boot)
        }
    }
}

/// 进入空间：推进世代使本地在途回执过期，清空两域会话态，拉整树与元数据。
/// 启动自动进入（from_boot）按「自动恢复」设置载入上次打开的文件；
/// 运行中切换在 gate 保存后关闭当前打开的文件，进入新空间为未打开态。
pub fn enter_space(app: &mut App, session: SpaceSession, from_boot: bool) -> Task<Message> {
    // 换空间 = 换泵：旧连接先 bye 再重建（订阅按 topic 换血）
    teardown_pump(app);
    // 展开集按空间独立记忆
    if let Some(key) = space_key_of(app) {
        app.expanded_by_space.insert(key, app.ui.file_explorer_expanded.clone());
    }
    // 本地仓库世代推进：在途的本地异步回执全部过期
    app.vault.bump_generation();
    app.repo_kind = crate::RepoKind::Space(session.clone());
    app.vault_root = None;
    app.vault_switch_target = None;
    // 容器已换：未声明保活的插件重载、声明的收到 vault:switch（旧容器上下文此刻起失效）
    app.plugins.on_vault_switch(atelyx_core::plugin_runtime::VaultSwitchPayload::Space {
        space_id: session.space_id.clone(),
    });
    clear_session_state(app);
    app.space_tree = None;
    let key = format!("{}/{}", session.server_url, session.space_id);
    // 展开集按空间独立：切走时记忆当前集合，切回时恢复；全新进入（无记忆）保底展开根行
    app.ui.file_explorer_expanded = match app.expanded_by_space.get(&key) {
        Some(set) => set.clone(),
        None => vec![String::new()],
    };
    app.global.bump_space(&session.server_url, &session.space_id, &session.name, crate::now_secs());
    let mut task = refresh_space_tree(app);
    task = task.chain(refresh_space_backlinks(app));
    task = task.chain(refresh_space_tags(app));
    task = task.chain(load_space_meta(app));
    task = task.chain(app.save_global());
    // 运行中切换：当前打开的笔记、表格与画布一并关闭（保存已由 gate 完成），
    // 「上次打开」记录随之清除，进入新空间为未打开态
    if !from_boot {
        app.ui.last_note_file = None;
        app.ui.last_table_file = None;
        app.ui.last_canvas_file = None;
        app.note_mode = crate::NoteMode::Read;
        app.note_edit_caret_pending = false;
        app.note_press_inside = false;
        app.history.clear();
    }
    if from_boot && app.global.auto_restore {
        if let Some(rel) = app.ui.last_note_file.clone() {
            task = task.chain(space_load_note(app, &rel));
        }
        if let Some(rel) = app.ui.last_table_file.clone() {
            task = task.chain(space_load_table(app, &rel));
        }
        if let Some(rel) = app.ui.last_canvas_file.clone() {
            task = task.chain(crate::canvas_actions::load_canvas(app, &rel));
        }
    }
    task
}

/// 进入/离开仓库容器共用的会话态清空（镜像 enter_vault 清单）。
fn clear_session_state(app: &mut App) {
    app.children.clear();
    app.note_text.clear();
    app.source_editor = text_editor::Content::new();
    app.edit.reset(0);
    app.edit_doc = None;
    app.edit_meta.clear();
    app.preview_doc = None;
    app.dirty = false;
    app.save_error = None;
    app.reset_props();
    app.file_count = None;
    app.expand_all = false;
    app.expand_pending = 0;
    app.rename_notice = None;
    app.table = None;
    app.canvas = None;
    app.space_note_flushed = None;
    app.space_note_save_inflight = false;
    app.space_note_save_flush_queued = false;
    app.backlinks.clear();
    app.tag_vocab.clear();
    app.note_files.clear();
    app.history.clear();
    app.history_gen = 0;
    app.history_loading = false;
    app.history_expanded = None;
    app.history_preview = None;
    app.history_confirm = None;
    // 未落盘的回滚提示随容器切换作废（旧保存链回执不再消费）
    app.history_restore_pending = false;
    // 仓库历史面板缓存随容器切换失效（进面板后重取）
    app.repo_history = None;
    app.repo_history_loading = false;
    // 日历会话态随容器切换重置（日程 / 选中天 / 活动密度一并重取）
    app.calendar = crate::calendar_actions::CalendarState::default();
    app.rebuild_links = crate::settings_view::editor::RebuildLinksState::Idle;
    app.remote_cursors.clear();
    // 仓库级配置基线与 AI 编辑态随容器切换清空（VaultMetaLoaded 落地后回填），
    // 残留会把上一容器的供应商清单写进下一容器
    app.repo_config = vault::VaultConfig::default();
    app.agents.clear();
    app.ai = crate::settings_view::ai::AiState::default();
    // 成员名册与邀请码缓存随空间会话失效（视图 / 弹层展示时重取）
    // 忙标志同属会话态：容器切换清零，防旧回执被 gen 守卫丢弃后卡死新容器操作
    app.space_busy = false;
    app.space_connected = false;
    app.space_pump_denied = false;
    app.space_members = None;
    app.space_members_loading = false;
    app.space_members_error = None;
    app.space_member_confirm = None;
    app.space_invites = None;
    app.space_invites_loading = false;
    app.space_invites_error = None;
    app.space_invite_result = None;
    app.space_invite_confirm = None;
    app.space_invite_copied = false;
    app.space_invite_form = InviteForm::default();
}

/// 离开当前空间身份（进入本地仓库 / 登出时调用）：摘泵、清空间会话态并推进世代
/// （在途的空间回执全部过期，防穿越到本地模式）。
pub fn leave_space_identity(app: &mut App) {
    if let Some(key) = space_key_of(app) {
        app.expanded_by_space.insert(key, app.ui.file_explorer_expanded.clone());
    }
    teardown_pump(app);
    app.space_token = None;
    app.repo_kind = crate::RepoKind::Local;
    app.space_tree = None;
    // 空间在途回执按 enter_space 时的 gen 判定：离开必须推进，否则穿越身份切换
    app.vault.bump_generation();
    clear_session_state(app);
    // 空间会话真源随身份离开而不可达：停 run 并清投影
    crate::chat_actions::detach_vault(app);
}

// ===== 消息入口 =====

pub fn handle(app: &mut App, msg: SpaceMsg) -> Task<Message> {
    match msg {
        SpaceMsg::LoginField(field, value) => {
            match field {
                SpaceField::Server => app.space_form.server = value,
                SpaceField::Username => app.space_form.username = value,
                SpaceField::Password => app.space_form.password = value,
                SpaceField::Device => app.space_form.device = value,
                SpaceField::Name => app.space_form.name = value,
                SpaceField::Code => app.space_form.code = value,
                SpaceField::InviteHours => app.space_invite_form.hours = value,
                SpaceField::InviteUses => app.space_invite_form.uses = value,
            }
            Task::none()
        }
        SpaceMsg::LoginSubmit { register } => {
            app.space_form.register = register;
            login_submit(app, register)
        }
        SpaceMsg::LoginDone { server_url, result } => on_login_done(app, server_url, result),
        SpaceMsg::SpacesLoaded { server_url, result, window } => {
            on_spaces_loaded(app, server_url, result, window)
        }
        SpaceMsg::SelectSpace { server_url, space_id, name, from_boot } => {
            app.dismiss_popup();
            // 已在该空间：忽略重复进入
            if let Some(s) = session_of(app) {
                if s.server_url == server_url && s.space_id == space_id {
                    return Task::none();
                }
            }
            restore_and_enter(app, server_url, space_id, name, from_boot)
        }
        SpaceMsg::RestoreDone { server_url, space_id, name, from_boot, result } => {
            on_restore_done(app, server_url, space_id, name, from_boot, result)
        }
        SpaceMsg::CreateSpace => create_space(app),
        SpaceMsg::CreateSpaceDone { result } => on_space_created(app, result),
        SpaceMsg::JoinSpace => join_space(app),
        SpaceMsg::JoinSpaceDone { result } => on_space_joined(app, result),
        SpaceMsg::TreeLoaded { gen, result } => on_tree_loaded(app, gen, result),
        SpaceMsg::NoteSaved { gen, rel, frozen, reason, result } => {
            on_note_saved(app, gen, &rel, frozen, reason, result)
        }
        SpaceMsg::TablePatchDone { gen, file, new_file, reason, result, frozen } => {
            on_table_patch_done(app, gen, file, new_file, reason, result, frozen)
        }
        SpaceMsg::CanvasPatchDone { gen, file, new_file, reason, result, frozen } => {
            on_canvas_patch_done(app, gen, file, new_file, reason, result, frozen)
        }
        SpaceMsg::ForceDeleteFolder { gen, rel } => {
            app.dismiss_popup();
            app.delete_confirm = None;
            match session_of(app) {
                Some(session) => space_force_delete_folder(&session, gen, &rel),
                // 确认 pane 打开期间会话可能已失效（登出 / 进入本地仓库）
                None => {
                    app.notify(NoticeLevel::Warning, "空间会话已失效，删除未执行");
                    Task::none()
                }
            }
        }
        SpaceMsg::HistoryRecorded { err } => {
            if let Some(e) = err {
                // 历史为尽力而为侧车：失败提示但不阻塞正文链（文案由发起方按失败点拼装）
                app.notify(NoticeLevel::Warning, e);
            }
            // 侧文件有新版本：面板聚合缓存失效，下个 tick 重取（展示中）
            app.repo_history = None;
            Task::none()
        }
        SpaceMsg::CanvasRestored { gen, rel, result } => {
            // 世代守卫：切空间 / 切仓库后回执过期，不得越会话提示或触发重读
            if gen != app.epoch() {
                return Task::none();
            }
            match result {
                Err(e) => {
                    app.notify(NoticeLevel::Warning, e);
                    app.repo_history = None;
                    Task::none()
                }
                Ok(()) => {
                    app.notify(NoticeLevel::Success, "已回滚");
                    app.repo_history = None;
                    // 快照直写不经会话链：仍打开的画布须按服务端事实重读，否则界面与 `saved` 基线停在回滚前
                    if app.ui.last_canvas_file.as_deref() == Some(rel.as_str()) {
                        space_load_canvas(app, &rel)
                    } else {
                        Task::none()
                    }
                }
            }
        }
        SpaceMsg::DevicesLoaded { result } => match result {
            Err(e) => on_error(app, e, "设备列表获取失败"),
            Ok(list) => {
                app.space_devices = list;
                Task::none()
            }
        },
        SpaceMsg::RevokeDevice(session_id) => revoke_device(app, session_id),
        SpaceMsg::DeviceRevoked { result, .. } => match result {
            Err(e) => on_error(app, e, "吊销设备失败"),
            Ok(()) => fetch_devices(app),
        },
        SpaceMsg::Logout => logout(app),
        SpaceMsg::FsTreeRefresh => on_fs_tree_refresh_tick(app),
        SpaceMsg::PumpReady { server_url, space_id, cmds } => {
            // 身份回验：订阅取消与回执之间存在竞窗，过期泵的入口不得顶替当前会话
            if session_of(app)
                .is_some_and(|s| s.server_url == server_url && s.space_id == space_id)
            {
                app.space_cmds = Some(cmds);
            }
            Task::none()
        }
        SpaceMsg::Event { server_url, space_id, event } => {
            // 身份回验：teardown 与事件在途存在竞窗，过期泵事件不得落入新会话
            if session_of(app)
                .is_some_and(|s| s.server_url == server_url && s.space_id == space_id)
            {
                on_space_event(app, *event)
            } else {
                Task::none()
            }
        }
        SpaceMsg::MembersLoaded { gen, result } => on_members_loaded(app, gen, result),
        SpaceMsg::MemberAsk(user_id, op) => {
            app.space_member_confirm = Some((user_id, op));
            Task::none()
        }
        SpaceMsg::MemberAskCancel => {
            app.space_member_confirm = None;
            Task::none()
        }
        SpaceMsg::MemberTransfer(user_id) => member_transfer(app, user_id),
        SpaceMsg::MemberRemove(user_id) => member_remove(app, user_id),
        SpaceMsg::MemberOpDone { gen, op, result } => on_member_op_done(app, gen, op, result),
        SpaceMsg::InvitesLoaded { gen, result } => on_invites_loaded(app, gen, result),
        SpaceMsg::InviteRolePick(role) => {
            app.space_invite_form.role = role;
            app.popup_pane = 0;
            Task::none()
        }
        SpaceMsg::CreateInvite => create_invite(app),
        SpaceMsg::InviteCreated { gen, result } => on_invite_created(app, gen, result),
        SpaceMsg::InviteRevokeAsk(code) => {
            app.space_invite_confirm = Some(code);
            Task::none()
        }
        SpaceMsg::InviteRevokeCancel => {
            app.space_invite_confirm = None;
            Task::none()
        }
        SpaceMsg::RevokeInvite(code) => invite_revoke(app, code),
        SpaceMsg::InviteRevoked { gen, code, result } => on_invite_revoked(app, gen, code, result),
        SpaceMsg::InviteCopy(code) => {
            app.space_invite_copied = true;
            iced::clipboard::write(code)
        }
        SpaceMsg::OpenCollabSettings => {
            // 已在设置页只切分区，否则切分区后再开整页
            let tab = Task::done(Message::SettingsTab("collab".to_string()));
            if app.settings_open {
                tab
            } else {
                tab.chain(Task::done(Message::SettingsToggle))
            }
        }
    }
}

fn on_spaces_loaded(
    app: &mut App,
    server_url: String,
    result: Result<Vec<SpaceSummary>, SpaceError>,
    window: iced::window::Id,
) -> Task<Message> {
    match result {
        Err(e) => on_error(app, e, "空间列表获取失败"),
        Ok(list) => {
            app.space_list = list;
            // 最近空间名与激活身份和服务端对齐（改名跟随）
            let names: HashMap<&str, &str> = app
                .space_list
                .iter()
                .map(|s| (s.space_id.as_str(), s.name.as_str()))
                .collect();
            for space in app.global.recent_spaces.iter_mut() {
                if space.server_url == server_url {
                    if let Some(n) = names.get(space.space_id.as_str()) {
                        space.name = (*n).to_string();
                    }
                }
            }
            if let crate::RepoKind::Space(session) = &mut app.repo_kind {
                if session.server_url == server_url {
                    if let Some(s) = app.space_list.iter().find(|s| s.space_id == session.space_id) {
                        session.role = s.role;
                        session.name = s.name.clone();
                    }
                }
            }
            // 改名跟随写入最近列表（回执时机晚于登录落盘，这里单独补一次持久化）
            let task = app.save_global();
            // 用户已关菜单（或切换到其他弹层）则不再强制重开
            let menu_open = matches!(
                app.popup.as_ref().map(|(_, open)| &open.state),
                Some(layout_view::PopupState::SpaceMenu)
            );
            if !menu_open {
                return task;
            }
            let cursor = app.cursor_for(window);
            app.dismiss_popup();
            app.popup = Some((
                window,
                layout_view::PopupOpen {
                    point: iced::Point::new(cursor.x, cursor.y + 16.0),
                    flip_y: Some(cursor.y - 12.0),
                    state: layout_view::PopupState::SpaceMenu,
                    min_width: 200.0,
                },
            ));
            task
        }
    }
}

/// 登录身份的服务器地址（激活空间会话优先，回落最近登录身份；创建/加入空间回执定位用）。
fn identity_server_url(app: &App) -> String {
    session_of(app)
        .map(|s| s.server_url)
        .or_else(|| app.global.last_identity.as_ref().map(|i| i.server_url.clone()))
        .unwrap_or_default()
}

fn create_space(app: &mut App) -> Task<Message> {
    // 创建/加入是服务器级操作：登录即可（无需已在某空间内）
    let client = match client_from_identity(app) {
        Ok(c) => c,
        Err(e) => {
            app.notify(NoticeLevel::Warning, format!("无法创建空间：{e}"));
            return Task::none();
        }
    };
    let name = app.space_form.name.trim().to_string();
    if name.is_empty() {
        app.notify(NoticeLevel::Warning, "空间名不能为空");
        return Task::none();
    }
    app.space_busy = true;
    Task::perform(
        async move { client.create_space(&name).await },
        move |result| Message::Space(SpaceMsg::CreateSpaceDone { result }),
    )
}

fn on_space_created(app: &mut App, result: Result<SpaceSummary, SpaceError>) -> Task<Message> {
    app.space_busy = false;
    match result {
        Err(e) => on_error(app, e, "创建空间失败"),
        Ok(space) => {
            app.dismiss_popup();
            app.notify(NoticeLevel::Success, "空间已创建");
            Task::done(Message::Space(SpaceMsg::SelectSpace {
                server_url: identity_server_url(app),
                space_id: space.space_id,
                name: space.name,
                from_boot: false,
            }))
        }
    }
}

fn join_space(app: &mut App) -> Task<Message> {
    let client = match client_from_identity(app) {
        Ok(c) => c,
        Err(e) => {
            app.notify(NoticeLevel::Warning, format!("无法加入空间：{e}"));
            return Task::none();
        }
    };
    let code = app.space_form.code.trim().to_string();
    if code.is_empty() {
        app.notify(NoticeLevel::Warning, "邀请码不能为空");
        return Task::none();
    }
    app.space_busy = true;
    Task::perform(
        async move { client.accept_invite(&code).await },
        move |result| Message::Space(SpaceMsg::JoinSpaceDone { result }),
    )
}

fn on_space_joined(app: &mut App, result: Result<AcceptAck, SpaceError>) -> Task<Message> {
    app.space_busy = false;
    match result {
        Err(e) => on_error(app, e, "加入空间失败"),
        Ok(ack) => {
            app.dismiss_popup();
            app.notify(NoticeLevel::Success, "已加入空间");
            Task::done(Message::Space(SpaceMsg::SelectSpace {
                server_url: identity_server_url(app),
                space_id: ack.space_id,
                name: ack.name,
                from_boot: false,
            }))
        }
    }
}

// ===== 整树与内容面 =====

/// 拉取空间整树（children / note_files / file_count 的共同数据源）。
pub fn refresh_space_tree(app: &App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.tree(&space_id).await },
        move |result| Message::Space(SpaceMsg::TreeLoaded { gen, result }),
    )
}

fn on_tree_loaded(app: &mut App, gen: u64, result: Result<Vec<space_client::TreeNode>, SpaceError>) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    match result {
        Err(e) => on_error(app, e, "空间内容加载失败"),
        Ok(tree) => {
            // children 按目录切片填充（懒加载目录在整树内全部就绪）
            app.children.clear();
            fill_children(&tree, "", &mut app.children);
            app.space_tree = Some(tree.clone());
            let file_tree = to_file_tree(&tree);
            let mut files = Vec::new();
            crate::collect_md_files(&file_tree, &mut files);
            app.note_files = files;
            app.file_count = Some(count_tree_files(&tree));
            // 树在途期间发起的「展开全部」在此续接：全目录 children 已就绪，
            // 从根发一次展开回执完成展开集回填与 pending 归零
            if app.expand_all && app.expand_pending > 0 {
                let gen = app.epoch();
                return Task::done(Message::ExpandChildrenLoaded(
                    String::new(),
                    gen,
                    Ok(children_of_tree(&tree, "")),
                ));
            }
            Task::none()
        }
    }
}

/// 递归填充各目录的 children 缓存（目录自身也进表，含根 ""）。
/// `tree` 恒为 `dir` 的直接子节点数组（切片按 "" 取直接条目），`dir` 只是缓存键。
fn fill_children(tree: &[space_client::TreeNode], dir: &str, out: &mut HashMap<String, Vec<vault::DirChild>>) {
    out.insert(dir.to_string(), children_of_tree(tree, ""));
    for n in tree.iter().filter(|n| n.is_dir) {
        fill_children(&n.children, &n.path, out);
    }
}

/// 空间模式读目录子项：整树切片（树未就绪时先拉树）。
pub fn space_load_children(app: &mut App, rel: &str) -> Task<Message> {
    match &app.space_tree {
        Some(tree) => {
            let entries = children_of_tree(tree, rel);
            app.children.insert(rel.to_string(), entries);
            Task::none()
        }
        None => refresh_space_tree(app),
    }
}

/// 递归展开的目录读取（空间模式 = 整树切片，复用 ExpandChildrenLoaded 回执链续接递归）。
pub fn space_load_children_for_expand(app: &mut App, rel: &str) -> Task<Message> {
    let gen = app.epoch();
    match &app.space_tree {
        Some(tree) => {
            let entries = children_of_tree(tree, rel);
            Task::done(Message::ExpandChildrenLoaded(rel.to_string(), gen, Ok(entries)))
        }
        None => refresh_space_tree(app),
    }
}

/// 打开笔记（空间模式）：GET 文本，复用 FileLoaded 回执链。
pub fn space_load_note(app: &App, rel: &str) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let rel = rel.to_string();
    let rel_for_cb = rel.clone();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.get_file_text(&space_id, &rel).await },
        move |result| {
            Message::FileLoaded(
                gen,
                result.map(|text| (rel_for_cb, text)).map_err(err_text),
            )
        },
    )
}

/// 打开表格（空间模式）：GET 文本 → 解析 TableFile → 复用 TableMsg::Loaded。
pub fn space_load_table(app: &App, rel: &str) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let rel = rel.to_string();
    let rel_for_cb = rel.clone();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let text = client.get_file_text(&space_id, &rel).await.map_err(err_text)?;
            let mut table: atelyx_core::vault::TableFile =
                serde_json::from_str(&text).map_err(|e| format!("表格解析失败：{e}"))?;
            for row in &mut table.rows {
                atelyx_core::table_ops::normalize_table_row(row);
            }
            Ok(table)
        },
        move |result| Message::Table(tv::TableMsg::Loaded(gen, rel_for_cb, result)),
    )
}

/// 空间模式笔记保存：GET 回读合流（他人落盘不丢）→ PUT → 服务端历史。
/// 合流后的最终全文随回执带回；落盘期间的新编辑保持脏、下一轮防抖重写。
/// 在途互斥：空间保存是 GET→PUT 网络往返且无写队列保序，并发两笔后到先写会回退服务端，
/// 重叠请求按来源分流——AutoSave/Manual 重挂防抖顺延，Flush 记入待续由回执后串行补发。
pub fn space_submit_save(app: &mut App, reason: SaveReason) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    if !session.role.can_write() {
        // viewer 的自动保存静默跳过（编辑面只读）；手动保存与脏拦截放行前明确告知
        if !matches!(reason, SaveReason::AutoSave) {
            app.notify(NoticeLevel::Warning, "该空间为只读身份，无法保存");
        }
        return Task::none();
    }
    let Some(rel) = app.ui.last_note_file.clone() else {
        app.notify(NoticeLevel::Warning, "没有打开的文件");
        return Task::none();
    };
    if app.space_note_save_inflight {
        // 在途回执后串行补发收尾（关窗/切文件的脏拦截依赖它）
        if let SaveReason::Flush = reason {
            app.space_note_save_flush_queued = true;
        }
        // 手动/自动顺延：在途保存的冻结内容已覆盖提交时刻的全文；此后若仍有输入，
        // 编辑链自会排防抖计时器接续，无需在此重挂（避免留下无定时器的死期限）
        return Task::none();
    }
    app.space_note_save_inflight = true;
    let gen = app.epoch();
    let content = app.note_text.clone();
    let content_for_msg = content.clone();
    let rel_for_msg = rel.clone();
    let base = app.space_note_flushed.clone();
    let client = session.client;
    let space_id = session.space_id;
    let author = crate::history_author(app);
    Task::perform(
        async move {
            // PUT 前回读：服务端内容与上次落盘基线不一致 = 他人已落盘，三方合流后整文上行
            let server_text = match client.get_file_text(&space_id, &rel).await {
                Ok(t) => t,
                // 首次保存：文件尚不存在，直接上行本端内容
                Err(SpaceError::NotFound) => String::new(),
                Err(e) => return Err(e),
            };
            // 编辑器内容未载入（打开失败的空态）禁止保存：本端空 + 服务端有正文时 PUT 会
            // 清空他人可见的文件。正常清空已有全文走 base（落盘基线）在场的合流路径，不受影响
            if base.is_none() && content.is_empty() && !server_text.is_empty() {
                return Err(SpaceError::BadRequest("文件内容未载入，已取消保存".into()));
            }
            let merged = match &base {
                Some(b) if *b != server_text => {
                    atelyx_core::merge3::merge3(b, &content, &server_text, atelyx_core::merge3::ConflictSide::Ours)
                }
                _ => content.clone(),
            };
            client.put_file_text(&space_id, &rel, &merged).await?;
            // 摘要对比 PUT 前回读的服务端内容（本次被替换的上一版；他人已写入时合并前的真源）；
            // 其他端记录的内容与序号以服务端为准，此处仅算展示用摘要
            let summary = atelyx_core::history::version_summary(
                atelyx_core::history::HistoryKind::Note,
                &server_text,
                &merged,
            );
            // 历史尽力而为：失败不阻塞正文链
            let _ = client
                .history_record(
                    &space_id,
                    &space_client::HistoryRecordReq {
                        kind: "note".into(),
                        file: rel.clone(),
                        content: merged.clone(),
                        action: "edit".into(),
                        author: space_client::HistoryAuthorReq {
                            id: author.id.clone(),
                            name: author.name.clone(),
                            device: author.device.clone(),
                        },
                        summary: Some(summary),
                        note: None,
                        coalesce_edit_ms: Some(HISTORY_COALESCE_MS),
                        max_versions: None,
                        byte_budget: Some(atelyx_core::history::HISTORY_BYTE_BUDGET),
                    },
                )
                .await;
            Ok(merged)
        },
        move |result| {
            Message::Space(SpaceMsg::NoteSaved {
                gen,
                rel: rel_for_msg,
                frozen: content_for_msg,
                reason,
                result: result.map_err(err_text),
            })
        },
    )
}

fn on_note_saved(
    app: &mut App,
    gen: u64,
    rel: &str,
    frozen: String,
    reason: SaveReason,
    result: Result<String, String>,
) -> Task<Message> {
    // 回执即清在途标志（无论回执归属）：过期回执不会再有第二笔，不清会永久阻塞后续保存
    app.space_note_save_inflight = false;
    let queued = std::mem::take(&mut app.space_note_save_flush_queued);
    // 世代 + 文件身份双守卫：保存期间切换文件后，旧文件回执不得回填新文件的编辑面与合流基线
    if gen != app.epoch() || app.ui.last_note_file.as_deref() != Some(rel) {
        return Task::none();
    }
    let mut task = Task::none();
    match result {
        Err(e) => {
            app.settle_history_restore(false);
            app.save_error = Some(e.clone());
            if matches!(reason, SaveReason::Flush) {
                app.notify(NoticeLevel::Error, format!("保存失败，操作未执行：{e}"));
            } else {
                app.notify(NoticeLevel::Error, format!("保存失败：{e}"));
            }
        }
        Ok(merged) => {
            app.save_error = None;
            app.space_note_flushed = Some(merged.clone());
            // 协作基线祖先推进（CRDT 三方合并的 base）
            collab_note_flushed(app, &merged);
            // 合流结果与本屏不同时的回填按差异来源分派：保存期间的新输入（note_text ⊃ frozen）
            // 不得被回退——以发起时冻结内容为共同祖先重基，本端在途输入为 ours（冲突时保留：
            // 输入晚于落盘合流，被旧结论覆盖即丢失键击）
            if app.note_text != merged {
                let rebased = if app.note_text != frozen {
                    atelyx_core::merge3::merge3(&frozen, &app.note_text, &merged, atelyx_core::merge3::ConflictSide::Ours)
                } else {
                    merged.clone()
                };
                apply_remote_text(app, rebased);
            }
            // 合流结果回喂 CRDT 文档（REMOTE_ORIGIN 静默对齐不广播）：否则下次本地编辑的
            // 差分以旧文档为基准，会把对端已落盘内容当本端新增广播，对端文档出现重复文本
            if app.note_collab_file.as_deref() == Some(rel) {
                let aligned = app.note_text.clone();
                if let Some(nc) = app.note_collab.as_mut() {
                    nc.align_silent(&aligned);
                }
            }
            let stable = app.note_text == merged;
            if stable {
                app.settle_history_restore(true);
            }
            match reason {
                SaveReason::Flush => {
                    if stable {
                        app.dirty = false;
                        task = task.chain(app.after_any_flush());
                    } else {
                        task = task.chain(space_submit_save(app, SaveReason::Flush));
                    }
                }
                SaveReason::Manual => {
                    if stable {
                        app.dirty = false;
                        app.notify(NoticeLevel::Success, "已保存");
                    } else {
                        app.notify(NoticeLevel::Info, "保存完成，期间有新修改");
                    }
                }
                SaveReason::AutoSave => {
                    if stable {
                        app.dirty = false;
                    }
                }
            }
            // 回执后仍有未落盘的新输入且无在排计时器（在途期间到命的自动保存已被互斥顺延）：
            // 补排一次，防停笔后内容滞留脏态
            if app.dirty && app.autosave_deadline.is_none() {
                app.autosave_deadline = Some(Instant::now() + crate::AUTOSAVE_DEBOUNCE);
                task = task.chain(Task::perform(
                    async move { tokio::time::sleep(crate::AUTOSAVE_DEBOUNCE).await },
                    |_| crate::Message::AutosaveTick,
                ));
            }
            // 在途保存期间到来的 Flush（关窗/切文件拦截）：串行补发一笔收尾
            if queued {
                return task.chain(space_submit_save(app, SaveReason::Flush));
            }
            task = task.chain(reload_space_history(app));
        }
    }
    task
}

/// 远端文本落地（服务端合流回填 / 协作远端合入共用）：旧文本压撤销检查点（快照
/// 只含本端输入历史，不得借撤销把远端内容整体回滚）、光标按差异区间映射、
/// 解析产物重建、不置脏。frontmatter 前缀保持不变（差异按正文计算）。
pub fn apply_remote_text(app: &mut App, new_text: String) {
    let old = app.note_text.clone();
    if app.note_mode == NoteMode::Edit {
        app.edit.snapshot(&old);
    }
    let old_body_start = atelyx_core::frontmatter::block_end(&old).unwrap_or(0);
    let new_body_start = atelyx_core::frontmatter::block_end(&new_text).unwrap_or(0);
    let old_body = &old[old_body_start..];
    let new_body = &new_text[new_body_start..];
    let hunks = atelyx_core::merge3::diff_hunks(old_body, new_body);
    app.note_text = new_text.clone();
    if app.note_mode == NoteMode::Edit {
        // 光标映射（正文相对坐标）：hunks 全基于旧文本坐标，p 保持原坐标、按区间相对位置
        // 累计平移量（否则 p 的「部分映射坐标」与后续 hunk 的旧坐标混用，多 hunk 时漂移）
        let map = |pos: usize| -> usize {
            let p = pos.min(old_body.len());
            let mut shift: i64 = 0;
            for h in &hunks {
                if p <= h.at {
                    break;
                }
                if p >= h.at + h.remove {
                    shift += h.insert.len() as i64 - h.remove as i64;
                } else {
                    // 落入改动区间：钳到区间起点（含此前累计平移）
                    return h.at + shift.max(0) as usize;
                }
            }
            (p as i64 + shift).clamp(0, new_body.len() as i64) as usize
        };
        app.edit.caret = map(app.edit.caret);
        app.edit.anchor = map(app.edit.anchor);
    } else {
        // Source 档编辑面回填：远端合入后 source_editor 若滞留旧全文，本端继续输入会以
        // 旧全文覆盖 note_text（静默回退远端内容）
        app.source_editor = text_editor::Content::with_text(&new_text);
    }
    app.refresh_preview();
    app.refresh_edit();
}

// ===== 表格保存（空间模式）=====

/// 空间模式表格保存：基线 diff → 服务端补丁合并仲裁（id 不符 400 → 回退全量 PUT）。
pub fn space_submit_table_save(app: &mut App, reason: tv::SaveReason) -> Task<Message> {
    let Some(s) = app.table.as_ref() else { return Task::none() };
    if !s.dirty {
        return Task::none();
    }
    let Some(session) = session_of(app) else { return Task::none() };
    if !session.role.can_write() {
        if !matches!(reason, tv::SaveReason::AutoSave) {
            app.notify(NoticeLevel::Warning, "该空间为只读身份，无法保存");
        }
        return Task::none();
    }
    let gen = app.epoch();
    let file = s.file.clone();
    let baseline = s.saved.clone();
    let frozen = s.data.clone();
    let client = session.client;
    let space_id = session.space_id;
    let author = crate::history_author(app);
    let frozen_for_msg = frozen.clone();
    let file_for_msg = file.clone();
    Task::perform(
        async move {
            let new_file = match atelyx_core::vault::compute_table_patch(&baseline, &frozen) {
                Some(p) => match client.table_patch(&space_id, &file, &p).await {
                    Ok(ack) => ack.file,
                    // 补丁被拒（id 不符 / 文件损坏等）：回退前对拍服务端——文件仍为本端
                    // 保存基线才整文恢复；已被他人改写时不覆盖（内容红线），提示刷新
                    Err(SpaceError::BadRequest(_)) => {
                        let server = client.get_file_text(&space_id, &file).await?;
                        let server_id = serde_json::from_str::<serde_json::Value>(&server)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_owned()));
                        if server_id.as_deref() != Some(baseline.id.as_str()) {
                            return Err(SpaceError::Conflict(
                                "表格已被他人修改，请刷新后重试".to_string(),
                            ));
                        }
                        let json = serde_json::to_string_pretty(&frozen).map_err(|e| SpaceError::Network(e.to_string()))?;
                        client.put_file_text(&space_id, &file, &json).await?;
                        file.clone()
                    }
                    Err(e) => return Err(e),
                },
                None => {
                    // 无差异（编辑后复原 / undo 回到基线）：不盲目整文 PUT——服务端可能已有
                    // 本端未收到的改动（漏帧），以服务端现状与本端保存基线对拍后才动作
                    let server = client.get_file_text(&space_id, &file).await?;
                    let server_id = serde_json::from_str::<serde_json::Value>(&server)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_owned()));
                    if server_id.as_deref() != Some(baseline.id.as_str()) {
                        return Err(SpaceError::Conflict(
                            "表格已被他人修改，请刷新后重试".to_string(),
                        ));
                    }
                    file.clone()
                }
            };
            // 摘要对比保存基线（上版全文）：字段/行/单元格级实体计数
            let content_json = serde_json::to_string(&frozen).unwrap_or_default();
            let summary = atelyx_core::history::version_summary(
                atelyx_core::history::HistoryKind::Table,
                &serde_json::to_string(&baseline).unwrap_or_default(),
                &content_json,
            );
            let _ = client
                .history_record(
                    &space_id,
                    &space_client::HistoryRecordReq {
                        kind: "table".into(),
                        file: file.clone(),
                        content: content_json,
                        action: "edit".into(),
                        author: space_client::HistoryAuthorReq {
                            id: author.id.clone(),
                            name: author.name.clone(),
                            device: author.device.clone(),
                        },
                        summary: Some(summary),
                        note: None,
                        coalesce_edit_ms: Some(HISTORY_COALESCE_MS),
                        max_versions: None,
                        byte_budget: Some(atelyx_core::history::HISTORY_BYTE_BUDGET),
                    },
                )
                .await;
            Ok(new_file)
        },
        move |result| {
            let (new_file, inner) = match &result {
                Ok(f) => (f.clone(), Ok(f.clone())),
                Err(e) => (file_for_msg.clone(), Err(e.to_string())),
            };
            Message::Space(SpaceMsg::TablePatchDone { gen, file: file_for_msg, new_file, reason, result: inner, frozen: frozen_for_msg })
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn on_table_patch_done(
    app: &mut App,
    gen: u64,
    file: String,
    new_file: String,
    reason: tv::SaveReason,
    result: Result<String, String>,
    frozen: atelyx_core::vault::TableFile,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    // 身份守卫：gen + 文件路径（或标题漂移后的新路径）双校验
    if !app.table.as_ref().is_some_and(|s| s.file == file || s.file == new_file) {
        return Task::none();
    }
    let mut task = Task::none();
    match result {
        Err(e) => {
            app.settle_history_restore(false);
            let Some(s) = app.table.as_mut() else { return Task::none() };
            s.save_error = Some(e.clone());
            // 404 = 文件被改名（他端标题漂移未广播时）或删除：明示刷新，不做路径猜测
            let hint = if e.contains("不存在") {
                "表格已被他人改名或删除，请刷新空间后重新打开"
            } else {
                "保存失败"
            };
            if matches!(reason, tv::SaveReason::Flush) {
                app.notify(NoticeLevel::Error, format!("{hint}，操作未执行：{e}"));
            } else {
                app.notify(NoticeLevel::Error, format!("{hint}：{e}"));
            }
            return refresh_space_tree(app);
        }
        Ok(_) => {
            // 标题漂移：服务端按 title 重算路径，本端跟随（会话与最近表路径）
            if new_file != file {
                if let Some(s) = app.table.as_mut() {
                    s.file = new_file.clone();
                }
                if app.ui.last_table_file.as_deref() == Some(file.as_str()) {
                    app.ui.last_table_file = Some(new_file.clone());
                    app.schedule_persist();
                }
                task = task.chain(refresh_space_tree(app));
            }
            let stable = app
                .table
                .as_ref()
                .is_some_and(|s| s.data.fields == frozen.fields && s.data.rows == frozen.rows);
            {
                let Some(s) = app.table.as_mut() else { return Task::none() };
                s.save_error = None;
                if stable {
                    s.dirty = false;
                    s.saved = frozen.clone();
                }
            }
            if stable {
                app.settle_history_restore(true);
            }
            // Flush 回执期间有新编辑（unstable）：重收一轮 Flush——脏拦截已在放行前 flush，
            // 此轮丢失会让关闭/切换动作静默挂起（对齐笔记保存链）
            if let tv::SaveReason::Flush = reason {
                if stable {
                    task = task.chain(app.after_any_flush());
                } else {
                    task = task.chain(space_submit_table_save(app, tv::SaveReason::Flush));
                }
            }
            task = task.chain(reload_space_history(app));
        }
    }
    task
}

// ===== 画布（空间模式） =====

/// 空间模式载入 .atlx：画布文本 + 引用内容一并拉取（文本 / 媒体 / 图片取字节端点，表格走文本端点）。
pub fn space_load_canvas(app: &App, rel: &str) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let rel_owned = rel.to_string();
    let rel_for_cb = rel_owned.clone();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let rel = rel_owned;
            let text = client.get_file_text(&space_id, &rel).await.map_err(err_text)?;
            let canvas: atelyx_core::vault::CanvasFile =
                serde_json::from_str(&text).map_err(|e| format!("画布解析失败：{e}"))?;
            let docs = canvas_actions::prepare_docs(&canvas);
            let mut refs: HashMap<String, canvas_actions::RefRaw> = HashMap::new();
            for (path, kind) in canvas_actions::collect_ref_paths(&canvas) {
                // 与本地同口径：取原始字节，UTF-8 可解码 = 文本，否则 = 二进制占位，读不到 = 缺失。
                // 不经文本端点——它按 lossy 解码返回，非文本内容与缺失在响应里无从区分
                let raw = match kind {
                    canvas_actions::RefKind::Text | canvas_actions::RefKind::Media => {
                        match client.get_file_bytes(&space_id, &path).await {
                            Ok(bytes) => match String::from_utf8(bytes) {
                                Ok(t) => canvas_actions::RefRaw::Text(t),
                                Err(_) => canvas_actions::RefRaw::Binary,
                            },
                            Err(_) => canvas_actions::RefRaw::Missing,
                        }
                    }
                    canvas_actions::RefKind::Image => match client.get_file_bytes(&space_id, &path).await {
                        Ok(bytes) => {
                            let (width, height) = canvas_actions::image_dimensions(&bytes);
                            canvas_actions::RefRaw::Image { bytes, width, height }
                        }
                        Err(_) => canvas_actions::RefRaw::Missing,
                    },
                    canvas_actions::RefKind::Table => match client.get_file_text(&space_id, &path).await {
                        Ok(t) => serde_json::from_str::<atelyx_core::vault::TableFile>(&t)
                            .map(|tb| canvas_actions::RefRaw::Table {
                                summary: format!("{} 字段 · {} 行", tb.fields.len(), tb.rows.len()),
                            })
                            .unwrap_or(canvas_actions::RefRaw::Missing),
                        Err(_) => canvas_actions::RefRaw::Missing,
                    },
                };
                refs.insert(path, raw);
            }
            let text_docs = canvas_actions::prepare_text_docs(&canvas, &refs);
            Ok(canvas_actions::LoadOut { canvas, docs, text_docs, refs })
        },
        move |result| Message::Canvas(crate::canvas_view::CanvasMsg::Loaded(gen, rel_for_cb, result)),
    )
}

/// 空间模式画布保存：基线 diff → 服务端补丁仲裁（id 不符回退全量），回执走 Space 消息。
/// 对称 space_submit_table_save；title 漂移 = 服务端改路径，本端随回执跟随。
pub fn space_submit_canvas_save(app: &mut App, reason: crate::canvas_view::SaveReason) -> Task<Message> {
    let Some(s) = app.canvas.as_ref() else { return Task::none() };
    if !s.dirty {
        return Task::none();
    }
    let Some(session) = session_of(app) else { return Task::none() };
    if !session.role.can_write() {
        if !matches!(reason, crate::canvas_view::SaveReason::AutoSave) {
            app.notify(NoticeLevel::Warning, "该空间为只读身份，无法保存");
        }
        return Task::none();
    }
    let gen = app.epoch();
    let file = s.file.clone();
    let baseline = s.saved.clone();
    let frozen = s.data.clone();
    let client = session.client;
    let space_id = session.space_id;
    let author = crate::history_author(app);
    let frozen_for_msg = frozen.clone();
    let file_for_msg = file.clone();
    Task::perform(
        async move {
            let new_file = match atelyx_core::vault::compute_canvas_patch(&baseline, &frozen) {
                Some(p) => match client.canvas_patch(&space_id, &file, &p).await {
                    Ok(ack) => ack.file,
                    // 补丁被拒（id 不符 / 文件损坏等）：回退前对拍服务端——文件仍为本端
                    // 保存基线才整文恢复；已被他人改写时不覆盖（内容红线），提示刷新
                    Err(SpaceError::BadRequest(_)) => {
                        let server = client.get_file_text(&space_id, &file).await?;
                        let server_id = serde_json::from_str::<serde_json::Value>(&server)
                            .ok()
                            .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_owned()));
                        if server_id.as_deref() != Some(baseline.id.as_str()) {
                            return Err(SpaceError::Conflict(
                                "画布已被他人修改，请刷新后重试".to_string(),
                            ));
                        }
                        let json = serde_json::to_string_pretty(&frozen)
                            .map_err(|e| SpaceError::Network(e.to_string()))?;
                        client.put_file_text(&space_id, &file, &json).await?;
                        file.clone()
                    }
                    Err(e) => return Err(e),
                },
                None => {
                    // 无差异（编辑后复原 / 拖回原位）：不盲目整文 PUT——服务端可能已有
                    // 本端未收到的改动（漏帧），以服务端现状与本端保存基线对拍后才动作
                    let server = client.get_file_text(&space_id, &file).await?;
                    let server_id = serde_json::from_str::<serde_json::Value>(&server)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_owned()));
                    if server_id.as_deref() != Some(baseline.id.as_str()) {
                        return Err(SpaceError::Conflict(
                            "画布已被他人修改，请刷新后重试".to_string(),
                        ));
                    }
                    file.clone()
                }
            };
            // 摘要对比保存基线（上版全文）：卡 / 连线 / 消息计数
            let content_json = serde_json::to_string(&frozen).unwrap_or_default();
            let summary = atelyx_core::history::version_summary(
                atelyx_core::history::HistoryKind::Canvas,
                &serde_json::to_string(&baseline).unwrap_or_default(),
                &content_json,
            );
            let _ = client
                .history_record(
                    &space_id,
                    &space_client::HistoryRecordReq {
                        kind: "canvas".into(),
                        file: file.clone(),
                        content: content_json,
                        action: "edit".into(),
                        author: space_client::HistoryAuthorReq {
                            id: author.id.clone(),
                            name: author.name.clone(),
                            device: author.device.clone(),
                        },
                        summary: Some(summary),
                        note: None,
                        coalesce_edit_ms: Some(HISTORY_COALESCE_MS),
                        max_versions: None,
                        byte_budget: Some(atelyx_core::history::HISTORY_BYTE_BUDGET),
                    },
                )
                .await;
            Ok(new_file)
        },
        move |result| {
            let (new_file, inner) = match &result {
                Ok(f) => (f.clone(), Ok(f.clone())),
                Err(e) => (file_for_msg.clone(), Err(e.to_string())),
            };
            Message::Space(SpaceMsg::CanvasPatchDone {
                gen,
                file: file_for_msg,
                new_file,
                reason,
                result: inner,
                frozen: frozen_for_msg,
            })
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn on_canvas_patch_done(
    app: &mut App,
    gen: u64,
    file: String,
    new_file: String,
    reason: crate::canvas_view::SaveReason,
    result: Result<String, String>,
    frozen: atelyx_core::vault::CanvasFile,
) -> Task<Message> {
    if gen != app.epoch() {
        return Task::none();
    }
    // 身份守卫：gen + 文件路径（或标题漂移后的新路径）双校验
    if !app.canvas.as_ref().is_some_and(|s| s.file == file || s.file == new_file) {
        return Task::none();
    }
    let mut task = Task::none();
    match result {
        Err(e) => {
            app.settle_history_restore(false);
            let Some(s) = app.canvas.as_mut() else { return Task::none() };
            s.save_error = Some(e.clone());
            // 404 = 文件被改名（他端标题漂移未广播时）或删除：明示刷新，不做路径猜测
            let hint = if e.contains("不存在") {
                "画布已被他人改名或删除，请刷新空间后重新打开"
            } else {
                "保存失败"
            };
            if matches!(reason, crate::canvas_view::SaveReason::Flush) {
                app.notify(NoticeLevel::Error, format!("{hint}，操作未执行：{e}"));
            } else {
                app.notify(NoticeLevel::Error, format!("{hint}：{e}"));
            }
            return refresh_space_tree(app);
        }
        Ok(_) => {
            // 标题漂移：服务端按 title 重算路径，本端跟随（会话与最近画布路径）
            if new_file != file {
                if let Some(s) = app.canvas.as_mut() {
                    s.file = new_file.clone();
                }
                if app.ui.last_canvas_file.as_deref() == Some(file.as_str()) {
                    app.ui.last_canvas_file = Some(new_file.clone());
                    app.schedule_persist();
                }
                task = task.chain(refresh_space_tree(app));
            }
            // 只认磁盘事实：冻结快照与当前数据一致才算落盘（时间戳不参与）
            let stable = app
                .canvas
                .as_ref()
                .is_some_and(|s| {
                    s.data.title == frozen.title
                        && s.data.nodes == frozen.nodes
                        && s.data.edges == frozen.edges
                });
            {
                let Some(s) = app.canvas.as_mut() else { return Task::none() };
                s.save_error = None;
                if stable {
                    s.dirty = false;
                    s.saved = frozen.clone();
                }
            }
            if stable {
                app.settle_history_restore(true);
            }
            // Flush 回执期间有新编辑（unstable）：重收一轮 Flush——脏拦截已在放行前 flush，
            // 此轮丢失会让关闭/切换动作静默挂起（对齐笔记 / 表格保存链）
            if let crate::canvas_view::SaveReason::Flush = reason {
                if stable {
                    task = task.chain(app.after_any_flush());
                } else {
                    task = task.chain(space_submit_canvas_save(app, crate::canvas_view::SaveReason::Flush));
                }
            }
            task = task.chain(reload_space_history(app));
        }
    }
    task
}

// ===== 元数据 / 索引 / 图片 / 历史 =====

/// 空间模式元数据读取（仓库配置 / 提示词标记 / 文件夹颜色：`.atelyx/` 下文件，缺失走默认）。
pub fn load_space_meta(app: &App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let config: vault::VaultConfig =
                match client.get_file_text(&space_id, ".atelyx/config.json").await {
                    Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
                    Err(_) => vault::VaultConfig::default(),
                };
            let prompts = read_json_string_list(&client, &space_id, ".atelyx/prompt-notes.json").await;
            let colors: HashMap<String, String> =
                match client.get_file_text(&space_id, ".atelyx/folder-colors.json").await {
                    Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
                    Err(_) => HashMap::new(),
                };
            Ok((config, prompts, colors))
        },
        move |result| {
            Message::VaultMetaLoaded(gen, result.map_err(err_text))
        },
    )
}

async fn read_json_string_list(client: &SpaceClient, space_id: &str, path: &str) -> Vec<String> {
    match client.get_file_text(space_id, path).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// 提示词标记切换（空间模式：读改写 `.atelyx/prompt-notes.json`）。
pub fn space_toggle_prompt_note(app: &mut App, rel: &str) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let rel = rel.to_string();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let mut notes = read_json_string_list(&client, &space_id, ".atelyx/prompt-notes.json").await;
            if let Some(i) = notes.iter().position(|n| n == &rel) {
                notes.remove(i);
            } else {
                notes.push(rel.clone());
            }
            client
                .put_file_text(&space_id, ".atelyx/prompt-notes.json", &serde_json::to_string(&notes).unwrap_or_default())
                .await?;
            Ok::<_, SpaceError>(notes)
        },
        move |result| Message::PromptNotesLoaded(gen, result.map_err(err_text)),
    )
}

/// 文件夹颜色（空间模式：读改写 `.atelyx/folder-colors.json`）。
pub fn space_set_folder_color(app: &mut App, rel: &str, color: Option<String>) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let rel = rel.to_string();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let mut colors: HashMap<String, String> =
                match client.get_file_text(&space_id, ".atelyx/folder-colors.json").await {
                    Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
                    Err(_) => HashMap::new(),
                };
            match color {
                Some(c) => colors.insert(rel, c),
                None => colors.remove(&rel),
            };
            client
                .put_file_text(&space_id, ".atelyx/folder-colors.json", &serde_json::to_string(&colors).unwrap_or_default())
                .await?;
            Ok::<_, SpaceError>(colors)
        },
        move |result| Message::FolderColorsLoaded(gen, result.map_err(err_text)),
    )
}

/// 排序方式（空间模式：读改写 `.atelyx/config.json` 的 fileExplorerSort）。
pub fn space_set_sort(app: &mut App, sort: &str) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let value = sort.to_string();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let mut config: serde_json::Value =
                match client.get_file_text(&space_id, ".atelyx/config.json").await {
                    Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
                    Err(_) => serde_json::json!({}),
                };
            config["fileExplorerSort"] = serde_json::Value::String(value);
            client
                .put_file_text(&space_id, ".atelyx/config.json", &serde_json::to_string_pretty(&config).unwrap_or_default())
                .await?;
            Ok::<_, SpaceError>(())
        },
        move |result| Message::ConfigSaved(gen, result.map_err(err_text)),
    )
}

/// 仓库级配置补丁（空间模式：读改写 `.atelyx/config.json`，按补丁键合并；null = 删键）。
pub fn space_patch_config(app: &mut App, patch: serde_json::Value) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let mut config: serde_json::Value =
                match client.get_file_text(&space_id, ".atelyx/config.json").await {
                    Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({})),
                    Err(_) => serde_json::json!({}),
                };
            if let (serde_json::Value::Object(base), serde_json::Value::Object(keys)) =
                (&mut config, patch)
            {
                // 与本地补丁同口径：null = 删键（缺省语义回正）
                for (key, value) in keys {
                    if value.is_null() {
                        base.remove(&key);
                    } else {
                        base.insert(key, value);
                    }
                }
            }
            client
                .put_file_text(&space_id, ".atelyx/config.json", &serde_json::to_string_pretty(&config).unwrap_or_default())
                .await?;
            Ok::<_, SpaceError>(())
        },
        move |result| Message::ConfigSaved(gen, result.map_err(err_text)),
    )
}

/// Agent 配置读取（空间模式：GET `.atelyx/agents.json`，缺失/损坏按空清单）。
pub fn space_load_agents(app: &App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            match client.get_file_text(&space_id, ".atelyx/agents.json").await {
                Ok(raw) => {
                    Ok::<_, SpaceError>(
                        serde_json::from_str::<vault::AgentConfigFile>(&raw)
                            .map(|f| f.agents)
                            .unwrap_or_default(),
                    )
                }
                // 缺失 = 尚无配置（与本地读路径同口径，不算错误）
                Err(SpaceError::NotFound) => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        },
        move |result| Message::AgentsLoaded(gen, result.map_err(err_text)),
    )
}

/// Agent 配置写盘（空间模式：整表 PUT 后重读回执；viewer 由 require_write 拦截）。
pub fn space_write_agents(app: &mut App, agents: Vec<vault::AgentConfig>) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let file = vault::AgentConfigFile { schema: vault::AGENTS_SCHEMA.to_string(), agents };
            let raw = serde_json::to_string_pretty(&file).unwrap_or_default();
            client.put_file_text(&space_id, ".atelyx/agents.json", &raw).await?;
            match client.get_file_text(&space_id, ".atelyx/agents.json").await {
                Ok(raw) => Ok(serde_json::from_str::<vault::AgentConfigFile>(&raw)
                    .map(|f| f.agents)
                    .unwrap_or_default()),
                Err(SpaceError::NotFound) => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        },
        move |result| Message::AgentsLoaded(gen, result.map_err(err_text)),
    )
}

/// 反链（空间模式：服务端索引端点）。
pub fn refresh_space_backlinks(app: &App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let Some(rel) = app.ui.last_note_file.clone() else { return Task::none() };
    let title = rel.rsplit('/').next().unwrap_or(&rel).trim_end_matches(".md").to_string();
    let gen = app.epoch();
    let rel_for_cb = rel.clone();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            client.backlinks(&space_id, &title, &rel).await.map(|rows| {
                rows.into_iter()
                    .map(|r| vault::BacklinkRow { file: r.file, title: r.title })
                    .collect::<Vec<_>>()
            })
        },
        move |result| Message::BacklinksLoaded(gen, rel_for_cb, result.map_err(err_text)),
    )
}

/// 标签词汇表（空间模式：服务端索引端点）。
pub fn refresh_space_tags(app: &App) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            client.tags(&space_id).await.map(|rows| {
                rows.into_iter()
                    .map(|r| vault::TagRow { tag: r.tag, count: r.count as u32 })
                    .collect::<Vec<_>>()
            })
        },
        move |result| Message::TagVocabLoaded(gen, result.map_err(err_text)),
    )
}

/// 笔记图片（空间模式：仓库相对路径走服务端字节读取；外链路径不变）。
pub fn space_load_image(app: &App, gen: u64, src: String) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let client = session.client;
    let space_id = session.space_id;
    let src_for_cb = src.clone();
    Task::perform(
        async move { client.get_file_bytes(&space_id, &src).await },
        move |result| Message::ImageLoaded(gen, src_for_cb, result.map_err(err_text)),
    )
}

/// 历史版本列表（空间模式：服务端侧文件端点；缺失 / 损坏 = 空列表）。
pub fn reload_space_history(app: &App) -> Task<Message> {
    let (kind, rel) = match app.history_kind {
        atelyx_core::history::HistoryKind::Note => {
            (atelyx_core::history::HistoryKind::Note, app.ui.last_note_file.clone())
        }
        atelyx_core::history::HistoryKind::Table => {
            (atelyx_core::history::HistoryKind::Table, app.ui.last_table_file.clone())
        }
        atelyx_core::history::HistoryKind::Canvas => {
            (atelyx_core::history::HistoryKind::Canvas, app.ui.last_canvas_file.clone())
        }
    };
    match rel {
        Some(rel) => space_history_task(app, kind, &rel),
        None => Task::none(),
    }
}

/// 按 kind+file 拉取历史版本列表（历史弹层显式定位入口；缺失 / 损坏 = 空列表）。
pub fn space_history_task(app: &App, kind: atelyx_core::history::HistoryKind, rel: &str) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let gen = app.epoch();
    let rel_for_cb = rel.to_string();
    let rel = rel.to_string();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move { client.history_versions(&space_id, kind, &rel).await },
        move |result| Message::HistoryLoaded(gen, kind, rel_for_cb, result.unwrap_or_default()),
    )
}

/// 服务端历史记录（note/table × edit/restore 共用出口；失败仅提示不阻塞正文链）。
fn record_space_history(app: &App, kind: &str, rel: String, content: String, action: &str) -> Task<Message> {
    let Some(session) = session_of(app) else { return Task::none() };
    let client = session.client;
    let space_id = session.space_id;
    let author = crate::history_author(app);
    let kind = kind.to_string();
    let action = action.to_string();
    Task::perform(
        async move {
            client
                .history_record(
                    &space_id,
                    &space_client::HistoryRecordReq {
                        kind,
                        file: rel,
                        content,
                        action,
                        author: space_client::HistoryAuthorReq {
                            id: author.id.clone(),
                            name: author.name.clone(),
                            device: author.device.clone(),
                        },
                        summary: None,
                        note: None,
                        coalesce_edit_ms: None,
                        max_versions: None,
                        byte_budget: Some(atelyx_core::history::HISTORY_BYTE_BUDGET),
                    },
                )
                .await
        },
        move |result| {
            Message::Space(SpaceMsg::HistoryRecorded { err: result.err().map(|e| format!("历史版本记录失败：{e}")) })
        },
    )
}

/// 笔记回滚记录（空间模式）。
pub fn space_record_note_restore(app: &App, rel: String, content: String) -> Task<Message> {
    record_space_history(app, "note", rel, content, "restore")
}

/// 表格回滚记录（空间模式）。
pub fn space_record_table_restore(app: &App, rel: String, content: String) -> Task<Message> {
    record_space_history(app, "table", rel, content, "restore")
}

/// 画布回滚（空间模式）：快照写回服务端真源后记 restore 历史；打开中的画布随服务端事实重读
/// （快照直写不经会话链，不重读则界面与 `saved` 基线都停在回滚前）。
pub fn space_restore_canvas(app: &mut App, rel: String, content: String) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    let author = crate::history_author(app);
    let rel_cb = rel.clone();
    Task::perform(
        async move {
            client
                .put_file_text(&space_id, &rel, &content)
                .await
                .map_err(|e| format!("回滚写入失败：{e}"))?;
            let req = space_client::HistoryRecordReq {
                kind: "canvas".into(),
                file: rel,
                content,
                action: "restore".into(),
                author: space_client::HistoryAuthorReq {
                    id: author.id.clone(),
                    name: author.name.clone(),
                    device: author.device.clone(),
                },
                summary: None,
                note: None,
                coalesce_edit_ms: None,
                max_versions: None,
                byte_budget: Some(atelyx_core::history::HISTORY_BYTE_BUDGET),
            };
            client
                .history_record(&space_id, &req)
                .await
                .map_err(|e| format!("历史版本记录失败：{e}"))
        },
        move |result| Message::Space(SpaceMsg::CanvasRestored { gen, rel: rel_cb, result }),
    )
}

// ===== 文件操作 =====

/// 新建条目（空间模式）：文件夹 = create_folder；笔记 / 插件领域文档 = 空文本；表格 = 初始单字段结构。
pub fn space_create_entry(app: &mut App, dir: &str, kind: DraftKind, name: &str) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let dir = dir.to_string();
    let base = match &kind {
        DraftKind::Note => format!("{name}.md"),
        DraftKind::Folder => name.to_string(),
        DraftKind::Table => format!("{name}.atb"),
        DraftKind::Canvas => format!("{name}.atlx"),
        DraftKind::Doc { extension, .. } => format!("{name}.{extension}"),
    };
    let client = session.client;
    let space_id = session.space_id;
    let dir_for_cb = dir.clone();
    let initial = match kind {
        DraftKind::Table => serde_json::to_string_pretty(&atelyx_core::table_ops::new_table_file(name))
            .unwrap_or_default(),
        DraftKind::Canvas => {
            // 初始 = 空画布（标题随文件名起稿）
            let now = crate::now_secs() as i64;
            serde_json::to_string_pretty(&atelyx_core::vault::CanvasFile {
                schema: atelyx_core::vault::CANVAS_SCHEMA.into(),
                id: uuid::Uuid::new_v4().to_string(),
                title: name.to_string(),
                nodes: vec![],
                edges: vec![],
                created_at: now,
                updated_at: now,
            })
            .unwrap_or_default()
        }
        _ => String::new(),
    };
    Task::perform(
        async move {
            // 同名去重：name、name-2、name-3 …（与服务端 rename 409 前置避让）。
            // 树获取失败不得按空名单落盘（PUT 会覆盖既有同名文件），直接报错
            let tree = client.tree(&space_id).await?;
            let taken = children_of_tree(&tree, &dir)
                .into_iter()
                .map(|c| c.rel.rsplit('/').next().unwrap_or(&c.rel).to_string())
                .collect::<Vec<_>>();
            let (stem, ext) = match base.rfind('.') {
                Some(0) | None => (base.clone(), String::new()),
                Some(i) => (base[..i].to_string(), base[i..].to_string()),
            };
            let mut candidate = base.clone();
            let mut seq = 2;
            while taken.contains(&candidate) {
                candidate = format!("{stem}-{seq}{ext}");
                seq += 1;
            }
            let rel = if dir.is_empty() { candidate.clone() } else { format!("{dir}/{candidate}") };
            match kind {
                DraftKind::Folder => client.create_folder(&space_id, &rel).await,
                _ => client.put_file_text(&space_id, &rel, &initial).await,
            }?;
            Ok::<_, SpaceError>(rel)
        },
        move |result| {
            Message::OpDone(gen, dir_for_cb, result.map_err(err_text), crate::FollowUp::None)
        },
    )
}

/// 删除入口（空间模式）：文件直接删；文件夹先探需确认（needsConfirm + 条目数进确认 pane）。
/// `confirm` = 调用方已确认（确认 pane 的动作）：文件夹跳过探测直接 force 落删。
/// 确认与否由调用方参数表达，不得读环境态（确认 pane 的收尾会先清状态，读状态必然退化成再探测）。
pub fn space_delete_entry(app: &mut App, rel: &str, is_dir: bool, confirm: bool) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    if is_dir && confirm {
        let confirmed = rel.to_string();
        return Task::done(Message::Space(SpaceMsg::ForceDeleteFolder { gen, rel: confirmed }));
    }
    let rel = rel.to_string();
    let client = session.client;
    let space_id = session.space_id;
    if !is_dir {
        let rel_for_cb = rel.clone();
        Task::perform(
            async move { client.delete_file(&space_id, &rel).await.map(|_| rel) },
            move |result| {
                Message::OpDone(
                    gen,
                    parent_of(&rel_for_cb),
                    result.map_err(err_text),
                    crate::FollowUp::Deleted { rel: rel_for_cb, is_dir: false },
                )
            },
        )
    } else {
        let rel_for_cb = rel.clone();
        // 服务端探测结果分三路：需确认 / 空目录直删 / 失败（分支各回不同消息）
        enum FolderDelete {
            Confirm(u64),
            Force,
            Failed(String),
        }
        Task::perform(
            async move {
                match client.delete_folder(&space_id, &rel, false).await {
                    Ok(ack) if ack.needs_confirm => FolderDelete::Confirm(ack.item_count),
                    Ok(_) => FolderDelete::Force,
                    Err(e) => FolderDelete::Failed(e.to_string()),
                }
            },
            move |step| match step {
                FolderDelete::Confirm(count) => Message::FolderCounted(rel_for_cb, count as usize),
                // 空目录直接删（本地口径一致：空目录免确认）——经 ForceDelete 消息统一走 force 步
                FolderDelete::Force => Message::Space(SpaceMsg::ForceDeleteFolder { gen, rel: rel_for_cb }),
                FolderDelete::Failed(e) => Message::OpDone(
                    gen,
                    parent_of(&rel_for_cb),
                    Err(e),
                    crate::FollowUp::None,
                ),
            },
        )
    }
}

/// 文件夹确认删除（force = true）。
fn space_force_delete_folder(session: &SpaceSession, gen: u64, rel: &str) -> Task<Message> {
    let client = session.client.clone();
    let space_id = session.space_id.clone();
    let rel = rel.to_string();
    let rel_for_cb = rel.clone();
    Task::perform(
        async move { client.delete_folder(&space_id, &rel, true).await.map(|_| rel) },
        move |result| {
            Message::OpDone(
                gen,
                parent_of(&rel_for_cb),
                result.map_err(err_text),
                crate::FollowUp::Deleted { rel: rel_for_cb, is_dir: true },
            )
        },
    )
}

/// 重命名 / 移动（空间模式）：服务端 rename（目标已存在 409 提示）+ 客户端引用改写
/// （全库 .md 读改写，wiki 名与路径形式；尽力而为，不阻塞改名本身）。
pub fn space_commit_rename(app: &mut App, r: RenameState) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let (old_rel, new_rel, _is_dir) = match &r.target {
        RenameTarget::File(old) => {
            let title = vault::sanitize_filename(r.value.trim());
            if title.is_empty() {
                return Task::none();
            }
            // 无扩展名文件（或仅前导点）按原名改名，不补扩展名（与本地口径一致）
            let new_name = match old.rfind('.') {
                Some(0) | None => title,
                Some(i) => format!("{title}.{}", &old[i + 1..]),
            };
            let parent = parent_of(old);
            let new_rel = if parent.is_empty() { new_name } else { format!("{parent}/{new_name}") };
            (old.clone(), new_rel, false)
        }
        RenameTarget::Folder(old_dir) => {
            let name = vault::sanitize_filename(r.value.trim());
            if name.is_empty() {
                return Task::none();
            }
            let new_rel = match old_dir.rfind('/') {
                Some(i) => format!("{}/{name}", &old_dir[..i]),
                None => name,
            };
            (old_dir.clone(), new_rel, true)
        }
        _ => return Task::none(),
    };
    if old_rel == new_rel {
        return Task::none();
    }
    let gen = app.epoch();
    let client = session.client;
    let space_id = session.space_id;
    let old_for_cb = old_rel.clone();
    Task::perform(
        async move {
            client.rename_file(&space_id, &old_rel, &new_rel).await?;
            // 树现拉（不复用面板缓存）：改名/移动后引用改写要基于最新全库清单，空间树
            // 未就绪时也不静默零执行
            let tree = client.tree(&space_id).await?;
            rewrite_links_after_rename(&client, &space_id, &tree, &old_rel, &new_rel).await;
            Ok::<_, SpaceError>(new_rel)
        },
        move |result| match result {
            Ok(full) => Message::OpDone(
                gen,
                parent_of(&old_for_cb),
                Ok(full.clone()),
                crate::FollowUp::Renamed { from: old_for_cb, to: full, rewritten: Vec::new() },
            ),
            Err(e) => Message::OpDone(gen, parent_of(&old_for_cb), Err(err_text(e)), crate::FollowUp::None),
        },
    )
}

/// 全库 .md 引用改写：逐文件读 → 重写 → 有变化才写回。尽力而为：单文件失败跳过。
async fn rewrite_links_after_rename(
    client: &SpaceClient,
    space_id: &str,
    tree: &[space_client::TreeNode],
    old_rel: &str,
    new_rel: &str,
) {
    let mut md_paths = Vec::new();
    collect_md_paths(tree, &mut md_paths);
    for path in md_paths {
        if path == old_rel || path == new_rel {
            continue;
        }
        let Ok(text) = client.get_file_text(space_id, &path).await else { continue };
        let Some(rewritten) = rewrite_note_links(&text, old_rel, new_rel) else { continue };
        let _ = client.put_file_text(space_id, &path, &rewritten).await;
    }
}

fn collect_md_paths(tree: &[space_client::TreeNode], out: &mut Vec<String>) {
    for n in tree {
        if n.is_dir {
            collect_md_paths(&n.children, out);
        } else if n.path.ends_with(".md") {
            out.push(n.path.clone());
        }
    }
}

/// 链接改写（纯文本，与本地 `vault_refs` 同语义）：只精确改写路径形链接——目标等于旧路径，
/// 或文件夹移动时命中 `旧目录/` 前缀；wiki 名链接按笔记名动态解析，不改写（同名笔记跨目录
/// 不误伤）。经链接跨度引擎逐跨度处理（跳过 frontmatter / 代码块 / 行内代码 / 图片链接）。
/// 返回 None = 无需改写。
fn rewrite_note_links(text: &str, old_rel: &str, new_rel: &str) -> Option<String> {
    let old_prefix = format!("{old_rel}/");
    let new_prefix = format!("{new_rel}/");
    let mut changed = false;
    let out = atelyx_core::vault::rewrite_link_spans(text, &mut |span: &str| {
        let Some(path) = atelyx_core::vault::markdown_link_path(span) else {
            return span.to_string();
        };
        let new_target = if path == old_rel {
            new_rel.to_string()
        } else if let Some(rest) = path.strip_prefix(&old_prefix) {
            format!("{new_prefix}{rest}")
        } else {
            return span.to_string();
        };
        changed = true;
        // 只换 target，label 原样（span 形如 `[..](..)`，取首个 `](` 前的标签段）
        let label_end = span.find("](").unwrap_or(span.len() - 1);
        format!("{}]({new_target})", &span[..label_end])
    });
    changed.then_some(out)
}

/// 创建同目录副本（空间模式）：copy 端点 + 实体 id 重建（防两文件同 id 串补丁）。
pub fn space_duplicate_entry(app: &mut App, rel: &str, is_dir: bool) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let rel = rel.to_string();
    let rel_for_cb = rel.clone();
    let client = session.client;
    let space_id = session.space_id;
    Task::perform(
        async move {
            let parent = parent_of(&rel);
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            // 同名去重：name-2、name-3 …（服务端 copy 目标已存在 409 兜底，前置避让减少报错）
            let tree = client.tree(&space_id).await?;
            let taken = children_of_tree(&tree, &parent)
                .into_iter()
                .map(|c| c.rel.rsplit('/').next().unwrap_or(&c.rel).to_string())
                .collect::<Vec<_>>();
            let (stem, ext) = match name.rfind('.') {
                Some(0) | None => (name.clone(), String::new()),
                Some(i) => (name[..i].to_string(), name[i..].to_string()),
            };
            let mut target = name.clone();
            if taken.iter().any(|t| t == &target) {
                let mut seq = 2;
                loop {
                    target = format!("{stem}-{seq}{ext}");
                    if !taken.iter().any(|t| t == &target) {
                        break;
                    }
                    seq += 1;
                }
            }
            let new_rel = if parent.is_empty() { target.clone() } else { format!("{parent}/{target}") };
            client.copy_file(&space_id, &rel, &new_rel).await?;
            // 服务端 copy 不重建实体 id：非目录副本读改写换新 id
            if !is_dir && (new_rel.ends_with(".atb") || new_rel.ends_with(".atlx")) {
                if let Ok(text) = client.get_file_text(&space_id, &new_rel).await {
                    if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) {
                        value["id"] = serde_json::Value::String(atelyx_core::vault::new_entity_id());
                        let _ = client
                            .put_file_text(
                                &space_id,
                                &new_rel,
                                &serde_json::to_string_pretty(&value).unwrap_or_default(),
                            )
                            .await;
                    }
                }
            }
            Ok::<_, SpaceError>(new_rel)
        },
        move |result| {
            let parent = parent_of(&rel_for_cb);
            Message::OpDone(gen, parent, result.map_err(err_text), crate::FollowUp::None)
        },
    )
}

/// 拖拽移动（空间模式）：保持文件名移入目标目录（目标重名 409 提示，不加序号静默改位）；
/// 全库引用改写与改名同链（移动 = 同名改名）。
pub fn space_move_entry(app: &mut App, from: &str, to_dir: &str) -> Task<Message> {
    let Some(session) = require_write(app) else { return Task::none() };
    let gen = app.epoch();
    let name = from.rsplit('/').next().unwrap_or(from).to_string();
    let new_rel = if to_dir.is_empty() { name.clone() } else { format!("{to_dir}/{name}") };
    let from = from.to_string();
    let from_for_cb = from.clone();
    let client = session.client;
    let space_id = session.space_id;
    let tree = app.space_tree.clone().unwrap_or_default();
    Task::perform(
        async move {
            client.rename_file(&space_id, &from, &new_rel).await?;
            rewrite_links_after_rename(&client, &space_id, &tree, &from, &new_rel).await;
            Ok::<_, SpaceError>(new_rel)
        },
        move |result| match result {
            Ok(to) => Message::OpDone(
                gen,
                parent_of(&from_for_cb),
                Ok(to.clone()),
                crate::FollowUp::Renamed { from: from_for_cb.clone(), to, rewritten: Vec::new() },
            ),
            Err(e) => Message::OpDone(gen, parent_of(&from_for_cb), Err(err_text(e)), crate::FollowUp::None),
        },
    )
}

/// 挂断当前帧泵（空间切换 / 离开时）：先发协作离场帧与 bye（防心跳窗口幽灵在线），再丢弃命令入口。
fn teardown_pump(app: &mut App) {
    // 离场 awareness 帧（对端即时移除本端光标）先于 Bye 上行
    if let Some(cmds) = &app.space_cmds {
        if let Some(nc) = app.note_collab.as_mut() {
            if let (Some(file), Some(leave)) = (app.note_collab_file.clone(), nc.close()) {
                let _ = cmds.send(atelyx_core::collab::SpaceCmd::NoteAware { file, payload: leave });
            }
        }
    }
    app.note_collab = None;
    app.note_collab_file = None;
    if let Some(cmds) = app.space_cmds.take() {
        let _ = cmds.send(atelyx_core::collab::SpaceCmd::Bye);
    }
    app.space_peers.clear();
    app.space_own_peer_id = None;
    app.space_connected = false;
    app.remote_cursors.clear();
}

/// 摘除当前笔记的协作绑定（关闭笔记 / 删除打开中笔记）：先广播离场 awareness 帧（对端即时
/// 移除本端光标）再清绑定，presence 随之回收（否则名册滞留「正在编辑已关闭的文件」，
/// 且压住后续表格打开的在场上报）。
pub fn collab_unbind_note(app: &mut App) {
    if let Some(nc) = app.note_collab.as_mut() {
        if let (Some(file), Some(leave)) = (app.note_collab_file.clone(), nc.close()) {
            send_cmd(app, atelyx_core::collab::SpaceCmd::NoteAware { file, payload: leave });
        }
    }
    app.note_collab = None;
    app.note_collab_file = None;
    app.remote_cursors.clear();
    send_presence(app);
}

/// 协作上行通道（泵未就绪时静默丢弃：重连握手后会重发必要状态）。
fn send_cmd(app: &App, cmd: atelyx_core::collab::SpaceCmd) {
    if let Some(cmds) = &app.space_cmds {
        let _ = cmds.send(cmd);
    }
}

/// 插件通道前缀分隔：出站拼 `<插件 id>:<通道>`，入站按第一个分隔符拆回。
/// 插件通道名自身不含 `:`（协议已校验），故首个分隔符恒为宿主加的前缀。
pub(crate) const PLUGIN_CHANNEL_SEP: char = ':';

/// 插件消息上行：通道在投递前加插件命名空间，同空间其他实例据此路由回本插件。
pub(crate) fn send_plugin_message(
    app: &App,
    plugin_id: &str,
    channel: &str,
    payload: serde_json::Value,
    target_peer_id: Option<u64>,
) {
    send_cmd(
        app,
        atelyx_core::collab::SpaceCmd::PluginMsg {
            channel: format!("{plugin_id}{PLUGIN_CHANNEL_SEP}{channel}"),
            payload,
            target_peer_id,
        },
    );
}

/// note-sync 通道帧批量上行（帧 = 完整通道载荷，由 note_collab 产出）。
fn send_note_frames(app: &App, file: &str, frames: Vec<Vec<u8>>) {
    if frames.is_empty() {
        return;
    }
    if let Some(cmds) = &app.space_cmds {
        for frame in frames {
            let _ = cmds.send(atelyx_core::collab::SpaceCmd::NoteSync {
                file: file.to_string(),
                payload: frame,
            });
        }
    }
}

/// note-aware 通道帧上行（编辑选区随行与保活重发共用出口）。
fn send_awareness(app: &App, file: &str, payload: Vec<u8>) {
    send_cmd(app, atelyx_core::collab::SpaceCmd::NoteAware { file: file.to_string(), payload });
}

/// awareness 保活重发（UiTick 搭载，core 内 15s 限频）：空闲端身份帧否则被对端 30s 超时剔除。
pub(crate) fn awareness_keepalive(app: &mut App) {
    let Some(file) = app.note_collab_file.clone() else { return };
    let Some(frame) = app
        .note_collab
        .as_mut()
        .and_then(|nc| nc.keepalive_if_due(std::time::Instant::now()))
    else {
        return;
    };
    send_awareness(app, &file, frame);
}

/// 当前打开文件路径集合（笔记 + 表格 + 画布平级，≤3；presence、协作房间视图与删除命中判定共用）。
pub(crate) fn open_file_paths(app: &App) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(f) = &app.ui.last_note_file {
        files.push(f.clone());
    }
    if let Some(f) = &app.ui.last_table_file {
        files.push(f.clone());
    }
    if let Some(f) = &app.ui.last_canvas_file {
        files.push(f.clone());
    }
    files.truncate(3);
    files
}

/// 当前打开文件集合的 presence 上行形态（笔记 / 表格 / 画布平级；多窗口共用同一份打开态）。
fn presence_open_files(app: &App) -> Vec<serde_json::Value> {
    open_file_paths(app)
        .into_iter()
        .map(|f| {
            let view = if f.ends_with(".atb") {
                "table"
            } else if f.ends_with(".atlx") {
                "canvas"
            } else {
                "note"
            };
            serde_json::json!({ "file": f, "view": view })
        })
        .collect()
}

/// 本端 presence 上行（打开文件清单；名册展示与跳转用）。
/// 旧字段 file/view 保留为主打开文件（旧版客户端只认这两个字段）。
pub fn send_presence(app: &App) {
    let open_files = presence_open_files(app);
    let presence = match open_files.first() {
        Some(primary) => serde_json::json!({
            "file": primary["file"], "view": primary["view"],
            "openFiles": open_files,
        }),
        None => serde_json::json!({ "file": null, "view": null, "openFiles": [] }),
    };
    send_cmd(app, atelyx_core::collab::SpaceCmd::Presence { presence });
}

/// 对端 presence 的打开文件清单（优先 openFiles，回退旧版单 file 字段；空 = 无打开文件）。
pub(crate) fn peer_open_files(presence: Option<&serde_json::Value>) -> Vec<String> {
    let Some(p) = presence else { return Vec::new() };
    let from_list: Vec<String> = p
        .get("openFiles")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.get("file").and_then(|f| f.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !from_list.is_empty() {
        return from_list;
    }
    p.get("file")
        .and_then(|f| f.as_str())
        .filter(|f| !f.is_empty())
        .map(|f| vec![f.to_string()])
        .unwrap_or_default()
}

/// 本端 awareness 身份 JSON（昵称 / 协作色；core `set_local_state` 外层再包 user 键，此处传裸对象）。
fn local_user_json(app: &App) -> String {
    let name = app
        .global
        .last_identity
        .as_ref()
        .map(|i| i.display_name.clone())
        .unwrap_or_else(|| "用户".into());
    let color = app.global.collab_color.clone().unwrap_or_else(|| DEFAULT_COLLAB_COLOR.into());
    serde_json::json!({ "name": name, "color": color, "colorLight": format!("{color}33") })
        .to_string()
}

/// 字节偏移对（全文，可越界）→ awareness 选区的 UTF-16 偏移对。
fn selection_units(text: &str, anchor: usize, head: usize) -> Option<(u32, u32)> {
    let unit = |byte: usize| {
        u32::try_from(atelyx_core::space_frame::byte_to_utf16_offset(
            text,
            byte.min(text.len()),
        ))
        .ok()
    };
    Some((unit(anchor)?, unit(head)?))
}

/// 纯文本编辑面光标位置（行号 + 行内字节列）→ 全文字节偏移。列按字节计（与编辑面
/// 内部命中口径一致）；行号越界（文本比光标短）时钳到末行。
fn source_offset(text: &str, pos: text_editor::Position) -> usize {
    let mut line = 0;
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        if line == pos.line {
            break;
        }
        if ch == '\n' {
            line += 1;
            start = i + 1;
        }
    }
    let line_len = text[start..].find('\n').unwrap_or(text.len() - start);
    start + pos.column.min(line_len)
}

/// 本端选区（awareness 口径：全文 UTF-16，anchor = 锚点、head = 光标）。源码档取纯文本
/// 编辑面光标；预览编辑档取正文编辑面（偏移相对正文，剥 frontmatter 前缀）；只读档无光标。
fn local_selection(app: &App) -> Option<(u32, u32)> {
    let text = app.note_text.as_str();
    match app.note_mode {
        NoteMode::Read => None,
        NoteMode::Source => {
            let cursor = app.source_editor.cursor();
            let anchor = source_offset(text, cursor.selection.unwrap_or(cursor.position));
            selection_units(text, anchor, source_offset(text, cursor.position))
        }
        NoteMode::Edit => {
            let base = atelyx_core::frontmatter::block_end(text).unwrap_or(0);
            selection_units(text, base + app.edit.anchor, base + app.edit.caret)
        }
    }
}

/// 选区 / 光标变化上行（不涉内容）：点选、拖选、方向键移动都要让对端可见，
/// 内容变化路径（`collab_after_edit`）另走差分与 Update 帧。限频与补发在 core。
pub fn collab_after_caret(app: &mut App) {
    if !matches!(app.repo_kind, crate::RepoKind::Space(_)) || app.note_collab_file.is_none() {
        return;
    }
    let Some(rel) = app.note_collab_file.clone() else { return };
    let user_json = local_user_json(app);
    let selection = local_selection(app);
    let aware = app
        .note_collab
        .as_mut()
        .and_then(|nc| nc.set_local_state(&user_json, selection));
    if let Some(frame) = aware {
        send_awareness(app, &rel, frame);
    }
}

/// 限频压下的选区变化补发（UiTick 搭载）：拖选收尾后对端仍须拿到最终选区。
pub(crate) fn awareness_flush(app: &mut App) {
    let Some(file) = app.note_collab_file.clone() else { return };
    let Some(frame) = app
        .note_collab
        .as_mut()
        .and_then(|nc| nc.flush_awareness(Instant::now()))
    else {
        return;
    };
    send_awareness(app, &file, frame);
}

/// 绑定当前打开笔记到协作文档（打开 / 重连后调用）：基线通告 + 握手 + 身份帧。
pub fn collab_bind_note(app: &mut App) {
    let Some(rel) = app.ui.last_note_file.clone() else { return };
    if app
        .note_collab_file
        .as_deref()
        .is_some_and(|f| f == rel && app.note_collab.is_some())
    {
        // 重连场景：文档还在，重发握手（note-sync 通道）与身份帧（note-aware 通道，对端
        // 旧 awareness 条目将在 30s 过期）
        let (note_frames, aware) = {
            let nc = app.note_collab.as_mut().expect("上方已确认存在");
            let mut frames = Vec::new();
            if let Some(step1) = nc.sync_step1() {
                frames.push(step1);
            }
            let aware = nc.resend_awareness();
            (frames, aware)
        };
        if let Some(aw) = aware {
            send_cmd(
                app,
                atelyx_core::collab::SpaceCmd::NoteAware { file: rel.clone(), payload: aw },
            );
        }
        send_note_frames(app, &rel, note_frames);
        send_presence(app);
        return;
    }
    let author = collab_author();
    let text = app.note_text.clone();
    let user_json = local_user_json(app);
    // 换文件：旧文档先销毁并广播离场帧（对端即时移除旧文件上的本端光标）
    if let Some(old_file) = app.note_collab_file.clone() {
        if old_file != rel {
            if let Some(nc) = app.note_collab.as_mut() {
                if let Some(leave) = nc.close() {
                    send_cmd(
                        app,
                        atelyx_core::collab::SpaceCmd::NoteAware { file: old_file, payload: leave },
                    );
                }
            }
        }
    }
    let outcome = {
        let nc = app
            .note_collab
            .get_or_insert_with(|| atelyx_core::note_collab::NoteCollab::new(author));
        let out = nc.open(&rel, &text);
        let aware = nc.set_local_state(&user_json, None);
        (out, aware)
    };
    app.note_collab_file = Some(rel.clone());
    let frames = vec![outcome.0.baseline_frame, outcome.0.sync_step1];
    send_note_frames(app, &rel, frames);
    // 身份帧走 note-aware 通道（note-sync 通道解码器不认 awareness 载荷，塞进去对端必丢）
    if let Some(aw) = outcome.1 {
        send_cmd(
            app,
            atelyx_core::collab::SpaceCmd::NoteAware { file: rel, payload: aw },
        );
    }
    send_presence(app);
}

fn collab_author() -> String {
    // 会话稳定身份（重开应用换新身份）：时间戳 + 随机 id，无跨端语义
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{now}-{}", atelyx_core::vault::new_entity_id())
}

/// 本地编辑推进协作文档：差分基于文档真值（core 侧以文档当前文本求差，保存合流造成的
/// 文档落后会被自动补齐）→ Update 帧上行；awareness 选区随行。
pub fn collab_after_edit(app: &mut App) {
    if !matches!(app.repo_kind, crate::RepoKind::Space(_)) || app.note_collab_file.is_none() {
        return;
    }
    let rel = match app.note_collab_file.clone() {
        Some(f) => f,
        None => return,
    };
    let new = app.note_text.clone();
    let user_json = local_user_json(app);
    let selection = local_selection(app);
    let mut update_frame = None;
    if let Some(nc) = app.note_collab.as_mut() {
        let inner = nc.local_edit(&new);
        if let Some(inner) = inner {
            if let Some(tag) = nc.tag().cloned() {
                update_frame = Some(atelyx_core::space_frame::encode_note_sync(&tag, &inner));
            }
        }
    }
    if let Some(frame) = update_frame {
        send_note_frames(app, &rel, vec![frame]);
    }
    let aware = app
        .note_collab
        .as_mut()
        .and_then(|nc| nc.set_local_state(&user_json, selection));
    if let Some(aw) = aware {
        send_awareness(app, &rel, aw);
    }
}

/// 保存落地后推进协作基线祖先（空间模式三方合流的 base）。
pub fn collab_note_flushed(app: &mut App, flushed: &str) {
    let Some(rel) = app.note_collab_file.clone() else { return };
    if let Some(nc) = app.note_collab.as_mut() {
        nc.mark_disk_write(&rel, flushed);
    }
}

/// awareness 远端状态 → 编辑面光标视图（UTF-16 → note_text 字节偏移）。
/// 事件驱动与周期 tick（过期剔除节流）共用。
pub(crate) fn refresh_remote_cursors(app: &mut App) {
    let now = std::time::Instant::now();
    let text = app.note_text.clone();
    let cursors = app
        .note_collab
        .as_mut()
        .map(|nc| nc.remote_states(now))
        .unwrap_or_default();
    app.remote_cursors = cursors
        .into_iter()
        .map(|c| {
            let byte = |unit: u32| atelyx_core::space_frame::utf16_to_byte_offset(&text, unit as usize);
            crate::RemoteCursorView {
                name: c.name,
                color: if c.color.is_empty() {
                    "#9aa0a6".to_string()
                } else {
                    c.color
                },
                anchor: c.anchor.map(byte),
                head: c.head.map(byte),
            }
        })
        .collect();
}

/// 帧泵事件分派。
fn on_space_event(app: &mut App, event: atelyx_core::collab::SpaceEvent) -> Task<Message> {
    use atelyx_core::collab::SpaceEvent as E;
    match event {
        E::Ready { peer_id } => {
            app.space_own_peer_id = Some(peer_id);
            app.space_connected = true;
            app.notify(NoticeLevel::Success, "协作已连接");
            collab_bind_note(app);
            Task::none()
        }
        E::Peers(list) => {
            // 名册里消失的连接 = 对端退出 / 断链：其光标条目立即摘除（不必等 awareness 超时）
            let gone: Vec<String> = app
                .space_peers
                .iter()
                .map(|p| p.peer_id.clone())
                .filter(|id| !list.iter().any(|p| &p.peer_id == id))
                .collect();
            // 名册不含自己（hello-ack 分配的 peerId 过滤）：工具条胶囊与在线人数都只算他人
            app.space_peers = list
                .into_iter()
                .filter(|p| Some(p.peer_id.as_str()) != app.space_own_peer_id.as_deref())
                .collect();
            let mut dropped = false;
            if let Some(nc) = app.note_collab.as_mut() {
                for peer in &gone {
                    dropped |= nc.drop_peer(peer);
                }
            }
            if dropped {
                refresh_remote_cursors(app);
            }
            Task::none()
        }
        E::Presence { peer_id, presence } => {
            if let Some(peer) = app.space_peers.iter_mut().find(|p| p.peer_id == peer_id) {
                peer.presence = Some(presence);
            }
            Task::none()
        }
        E::NoteSync { peer_id, file, payload } => {
            collab_remote_note(app, &peer_id, &file, &payload);
            Task::none()
        }
        E::NoteAware { peer_id, file, payload } => {
            let now = std::time::Instant::now();
            if app.note_collab_file.as_deref() == Some(file.as_str()) {
                if let Some(nc) = app.note_collab.as_mut() {
                    nc.apply_awareness(&peer_id, &payload, now);
                }
                refresh_remote_cursors(app);
            }
            Task::none()
        }
        E::TablePatch { peer_id, file, patch, landed } => {
            on_remote_table_patch(app, peer_id, file, patch, landed)
        }
        E::MetaChanged { key } => {
            if !matches!(app.repo_kind, crate::RepoKind::Space(_)) {
                return Task::none();
            }
            // 日历日程落在团队 meta：他人变更到达时重载（不在展示中则仅失效，保留导航态）
            let calendar_task = if key == crate::calendar_actions::META_KEY {
                if layout_view::view_displayed(app, "calendar") {
                    crate::calendar_actions::reload(app)
                } else {
                    app.calendar.ready = false;
                    Task::none()
                }
            } else {
                Task::none()
            };
            calendar_task.chain(load_space_meta(app))
        }
        E::Renamed { old_path, new_path } => {
            on_space_renamed(app, &old_path, &new_path)
        }
        E::FsChanged => on_fs_changed(app),
        E::Resync => {
            if let (Some(nc), Some(file)) = (app.note_collab.as_mut(), app.note_collab_file.clone()) {
                if let Some(step1) = nc.sync_step1() {
                    send_note_frames(app, &file, vec![step1]);
                }
            }
            Task::none()
        }
        E::PluginMsg { peer_id, channel, payload } => {
            // 通道前缀由发送方宿主加：拆不出来或该插件不在跑即丢弃（消息不跨插件串道）
            let Some((plugin_id, channel)) = channel.split_once(PLUGIN_CHANNEL_SEP) else {
                return Task::none();
            };
            let peer_name = app
                .space_peers
                .iter()
                .find(|peer| peer.peer_id == peer_id)
                .map(|peer| peer.nickname.clone());
            app.plugins.notify_collab_message(
                plugin_id,
                &atelyx_core::plugin_runtime::CollabMessageParams {
                    channel: channel.to_string(),
                    payload,
                    peer_id,
                    peer_name,
                },
            );
            Task::none()
        }
        E::Error { message } => {
            // 连接失败（地址校验被拒 / 网络不可达）：泵仍按退避重连，命令经泵缓冲
            // 断线期照常上行；命令入口保留（PumpReady 只在流建立时回填一次，清了无法再生）
            app.space_connected = false;
            app.notify(NoticeLevel::Warning, format!("协作连接失败：{message}"));
            Task::none()
        }
        E::Denied { message } => {
            // 服务端拒绝（令牌无效 / 非成员 / 被移出空间）：泵已终结，停闸防订阅重算
            // 重生泵形成无退避风暴；容器切换复位标志，重进空间重新放行
            app.space_pump_denied = true;
            app.space_cmds = None;
            app.space_connected = false;
            app.notify(NoticeLevel::Error, format!("协作：{message}"));
            Task::none()
        }
        E::Disconnected => {
            app.space_connected = false;
            app.notify(NoticeLevel::Warning, "协作连接断开，正在重连");
            Task::none()
        }
        E::Reconnecting { .. } => Task::none(),
    }
}

/// 远端 note-sync 帧合入：协议应答上行、文档变化回填编辑面、换路跟随。
fn collab_remote_note(app: &mut App, peer_id: &str, file: &str, payload: &[u8]) {
    if app.note_collab_file.as_deref() != Some(file) {
        return;
    }
    let Some(nc) = app.note_collab.as_mut() else { return };
    let out = nc.apply_remote(peer_id, file, payload);
    send_note_frames(app, file, out.reply);
    if let Some(aw) = out.awareness {
        send_cmd(app, atelyx_core::collab::SpaceCmd::NoteAware { file: file.to_string(), payload: aw });
    }
    if out.doc_changed {
        if let Some(text) = out.text {
            apply_remote_text(app, text);
            refresh_remote_cursors(app);
        }
    }
    if let Some((old, new)) = out.relocated {
        // 旧对端换路：绑定跟随新路径（文档挂靠已由 apply_remote 内部迁移）
        collab_follow_rename(app, &old, &new);
        if app.ui.last_note_file.as_deref() == Some(old.as_str()) {
            app.ui.last_note_file = Some(new);
            app.schedule_persist();
        }
    }
}

/// 远端表格补丁（peer_id = 他人 WS 透传或落地帧回填的发起者 peerId；landed = 服务端已落盘）。
/// 自声落地帧（peer_id 匹配本端）跳过：数据已在本端，重复合入徒增重绘且撤销栈被作废。
/// 远端合入不入撤销栈；仅服务端已落盘且会话干净时推进 saved 基线（脏会话防吞未上行改动）。
fn on_remote_table_patch(
    app: &mut App,
    peer_id: Option<String>,
    file: String,
    patch: serde_json::Value,
    landed: bool,
) -> Task<Message> {
    if peer_id.is_some() && peer_id == app.space_own_peer_id {
        return Task::none();
    }
    let opened = app.table.as_ref().map(|s| s.file.clone());
    if opened.as_deref() != Some(file.as_str()) {
        return Task::none();
    }
    let Ok(p) = serde_json::from_value::<atelyx_core::vault::TablePatch>(patch) else {
        return Task::none();
    };
    // 防串表：补丁 id 与文件 id 不符即丢（服务端落地帧已仲裁，转发帧信任 file 匹配）
    if app
        .table
        .as_ref()
        .is_some_and(|s| p.id != s.data.id)
    {
        return Task::none();
    }
    let mut refresh = false;
    {
        let Some(s) = app.table.as_mut() else { return Task::none() };
        if p.title.is_some() {
            refresh = true;
        }
        atelyx_core::vault::apply_table_patch(&mut s.data, &p);
        s.version += 1;
        s.rebuild_layout();
        // 远端改动合入后撤销栈整体作废：栈内快照是合入前的本端全量数据，按 Ctrl+Z 会把
        // 协作者刚落盘的行/单元格一并回滚并随下次保存广播（内容丢失）
        s.view.undo.clear();
        s.view.redo.clear();
        if landed && !s.dirty {
            s.saved = s.data.clone();
        }
    }
    // 远端补丁落地即广播：打开中的插件表格视图由此重读（本地结算点不覆盖这条路径）
    app.plugins.on_table_changed(Some(file.as_str()));
    if refresh {
        return refresh_space_tree(app);
    }
    Task::none()
}

/// 服务端改名广播：打开中文件路径与协作绑定跟随（内容不动，不重载）。
fn on_space_renamed(app: &mut App, old_path: &str, new_path: &str) -> Task<Message> {
    if app.ui.last_note_file.as_deref() == Some(old_path) {
        app.ui.last_note_file = Some(new_path.to_string());
        app.schedule_persist();
    }
    if app.ui.last_table_file.as_deref() == Some(old_path) {
        app.ui.last_table_file = Some(new_path.to_string());
        app.schedule_persist();
        if let Some(s) = app.table.as_mut() {
            s.file = new_path.to_string();
        }
    }
    if app.ui.last_canvas_file.as_deref() == Some(old_path) {
        app.ui.last_canvas_file = Some(new_path.to_string());
        app.schedule_persist();
        // 会话路径与内嵌标题一并跟随：滞留旧路径让后续补丁打到不存在的文件；
        // 标题落后于文件名则服务端下次保存按 title 推导路径，会把已改的名漂回旧名
        let name = new_path.rsplit('/').next().unwrap_or(new_path);
        let stem = name.strip_suffix(".atlx").unwrap_or(name);
        let touched = match app.canvas.as_mut() {
            Some(s) => {
                s.file = new_path.to_string();
                if s.data.title != stem {
                    s.data.title = stem.to_string();
                    s.touch();
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if touched {
            app.autosave_deadline = Some(Instant::now() + crate::AUTOSAVE_DEBOUNCE);
        }
    }
    collab_follow_rename(app, old_path, new_path);
    let prefix = format!("{old_path}/");
    app.children = app
        .children
        .drain()
        .map(|(k, v)| {
            if k == old_path {
                (new_path.to_string(), v)
            } else if let Some(rest) = k.strip_prefix(&prefix) {
                (format!("{new_path}/{rest}"), v)
            } else {
                (k, v)
            }
        })
        .collect();
    refresh_space_tree(app)
}

/// 他端结构变更（新建/删除/复制）：重拉整树。600ms 窗口合并连续事件（批量建删/图片
/// 导入逐张广播），窗口内只排一次延迟刷新兜底——最后一个事件也要落地。
fn on_fs_changed(app: &mut App) -> Task<Message> {
    let now = std::time::Instant::now();
    match app.space_tree_refresh_at {
        Some(last) if now.duration_since(last) < FS_TREE_REFRESH_COALESCE => {
            if !app.fs_tree_refresh_pending {
                app.fs_tree_refresh_pending = true;
                Task::perform(
                    tokio::time::sleep(FS_TREE_REFRESH_COALESCE),
                    |_| Message::Space(SpaceMsg::FsTreeRefresh),
                )
            } else {
                Task::none()
            }
        }
        _ => {
            app.space_tree_refresh_at = Some(now);
            refresh_space_tree(app)
        }
    }
}

/// 延迟刷新兜底 tick：窗口结束后落地最终一次整树刷新。
fn on_fs_tree_refresh_tick(app: &mut App) -> Task<Message> {
    app.fs_tree_refresh_pending = false;
    app.space_tree_refresh_at = Some(std::time::Instant::now());
    refresh_space_tree(app)
}

/// 改名/移动的协作绑定跟随（本端 OpDone 与远端广播共用；前缀下文件一并跟随，幂等）。
pub fn collab_follow_rename(app: &mut App, old: &str, new: &str) {
    let map = |p: &str| -> Option<String> {
        if p == old {
            Some(new.to_owned())
        } else {
            p.strip_prefix(old)
                .and_then(|r| r.strip_prefix('/'))
                .map(|rest| format!("{new}/{rest}"))
        }
    };
    if let Some(next) = app.note_collab_file.clone().and_then(|p| map(&p)) {
        app.note_collab_file = Some(next);
    }
    if let Some(nc) = app.note_collab.as_mut() {
        nc.relocate(old, new);
    }
}

fn parent_of(rel: &str) -> String {
    match rel.rfind('/') {
        Some(i) => rel[..i].to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_core::space_client::TreeNode;

    fn node(name: &str, path: &str, is_dir: bool, children: Vec<TreeNode>) -> TreeNode {
        TreeNode { name: name.to_string(), path: path.to_string(), is_dir, updated_at: 0, children }
    }

    #[test]
    fn 整树填充各目录切片() {
        // 三层树：a/{b/{c.md}, d.md}、顶层 x.md——每个目录键的切片都应含直接子项
        let tree = vec![
            node(
                "a",
                "a",
                true,
                vec![
                    node("b", "a/b", true, vec![node("c.md", "a/b/c.md", false, Vec::new())]),
                    node("d.md", "a/d.md", false, Vec::new()),
                ],
            ),
            node("x.md", "x.md", false, Vec::new()),
        ];
        let mut out = HashMap::new();
        fill_children(&tree, "", &mut out);
        assert_eq!(out[""].len(), 2, "顶层应含 a 与 x.md");
        assert_eq!(out["a"].len(), 2, "a 应含 b 与 d.md");
        assert_eq!(out["a/b"].len(), 1, "a/b 应含 c.md");
        assert!(!out.contains_key("a/b/c.md"), "文件节点不进缓存（只目录需要子项表）");
    }

    #[test]
    fn 目录切片按路径逐段定位() {
        let tree = vec![node(
            "a",
            "a",
            true,
            vec![node("b", "a/b", true, vec![node("c.md", "a/b/c.md", false, Vec::new())])],
        )];
        // 整树按路径切片：两级目录都能取到直接子项
        assert_eq!(children_of_tree(&tree, "a").len(), 1);
        assert_eq!(children_of_tree(&tree, "a/b").len(), 1);
        assert_eq!(children_of_tree(&tree, "a/b/c.md").len(), 0, "文件无子项");
        assert_eq!(children_of_tree(&tree, "缺的").len(), 0, "不存在的目录 = 空列表");
    }

    #[test]
    fn 链接改写只动精确命中的路径形链接() {
        let text = "见 [笔记](a/old.md) 与 [[old]]（wiki 名动态解析，不改写）\n\n```rust\n// [x](a/old.md) 代码块不动\n```\n\n![图](a/old.md) 图片不动\n\n[其他目录同名](b/old.md) 不误伤";
        let out = rewrite_note_links(text, "a/old.md", "a/new.md").expect("有路径形命中应改写");
        assert!(out.contains("[笔记](a/new.md)"));
        assert!(out.contains("[[old]]"), "wiki 名不参与改名改写");
        assert!(out.contains("// [x](a/old.md)"), "代码块内不改写");
        assert!(out.contains("![图](a/old.md)"), "图片链接不改写");
        assert!(out.contains("[其他目录同名](b/old.md)"), "异目录同名路径不误伤");
    }

    #[test]
    fn 链接改写文件夹移动按前缀替换() {
        let text = "[页](dir/sub/n.md) 与 [页2](dir/other.md)";
        let out = rewrite_note_links(text, "dir", "dir2").expect("前缀命中应改写");
        assert!(out.contains("[页](dir2/sub/n.md)"));
        assert!(out.contains("[页2](dir2/other.md)"));
        assert_eq!(rewrite_note_links("[无关](x/n.md)", "dir", "dir2"), None);
    }

    #[test]
    fn 对端打开文件清单优先open_files并回退旧版单字段() {
        let multi = serde_json::json!({
            "file": "a/n.md", "view": "note",
            "openFiles": [{ "file": "a/n.md", "view": "note" }, { "file": "t.atb", "view": "table" }]
        });
        assert_eq!(peer_open_files(Some(&multi)), vec!["a/n.md".to_string(), "t.atb".to_string()]);
        // 旧版客户端只发 file/view 单字段
        let legacy = serde_json::json!({ "file": "t.atb", "view": "table" });
        assert_eq!(peer_open_files(Some(&legacy)), vec!["t.atb".to_string()]);
        // 空在场：新旧两种空形态都解析为空
        let none = serde_json::json!({ "file": null, "view": null, "openFiles": [] });
        assert!(peer_open_files(Some(&none)).is_empty());
        assert!(peer_open_files(None).is_empty());
    }
}
