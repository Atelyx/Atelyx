//! 能力面方法：插件→宿主的无前缀方法与宿主→插件的 `host:` 方法，方法名常量 + 定型 params/result。
//! 全部方法按调用方插件自动绑定插件 id（协议不暴露 id 参数，插件无法指定他人命名空间）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- 方法名常量 ----------

pub mod view {
    /// 插件提交一棵界面树到某个面（面板 / 窗口 / 接管区）。宿主校验后按键 diff 挂载。
    pub const SUBMIT: &str = "view.submit";
    /// 插件注册一个贡献点（面板 / 表格视图），使其出现在宿主对应的入口里。
    pub const REGISTER: &str = "view.register";
    /// 插件按 id 撤销自己的一个贡献点。
    pub const UNREGISTER: &str = "view.unregister";
    /// 宿主回发节点事件（通知）。
    pub const HOST_EVENT: &str = "host:view.event";
    /// 宿主告知插件某个面已就绪 / 已移除（通知）。
    pub const HOST_ATTACHED: &str = "host:surface.attached";
    pub const HOST_DETACHED: &str = "host:surface.detached";
}

pub mod fs {
    pub const PRIVATE_DIR: &str = "fs.privateDir";
    pub const READ_TEXT: &str = "fs.readText";
    pub const WRITE_TEXT: &str = "fs.writeText";
    pub const READ_BINARY: &str = "fs.readBinary";
    pub const WRITE_BINARY: &str = "fs.writeBinary";
    pub const REMOVE: &str = "fs.remove";
    pub const EXISTS: &str = "fs.exists";
    pub const LIST: &str = "fs.list";
}

pub mod process {
    pub const SPAWN: &str = "process.spawn";
    pub const WRITE_STDIN: &str = "process.writeStdin";
    pub const CLOSE_STDIN: &str = "process.closeStdin";
    pub const KILL: &str = "process.kill";
    /// 托管进程的输出与退出（通知）。
    pub const HOST_EVENT: &str = "host:process.event";
}

pub mod state {
    pub const READ: &str = "state.read";
    pub const WRITE: &str = "state.write";
}

pub mod notification {
    pub const SHOW: &str = "notification.show";
}

pub mod shortcut {
    pub const REGISTER: &str = "shortcut.register";
    pub const UNREGISTER: &str = "shortcut.unregister";
    /// 全局快捷键触发（通知）。
    pub const HOST_TRIGGERED: &str = "host:shortcut.triggered";
}

pub mod clipboard {
    pub const WRITE_TEXT: &str = "clipboard.writeText";
    pub const READ_TEXT: &str = "clipboard.readText";
}

pub mod dialog {
    pub const PICK_FILE: &str = "dialog.pickFile";
    pub const PICK_DIRECTORY: &str = "dialog.pickDirectory";
}

pub mod http {
    pub const REQUEST: &str = "http.request";
}

pub mod doc {
    /// 用户打开了插件注册的领域文档类型文件（通知）。
    pub const HOST_OPEN: &str = "host:doc.open";
}

pub mod canvas {
    /// 插件提交一份绘图指令清单到某个画布节点。宿主合帧应用（渲染每帧只取最新一份）。
    pub const SUBMIT: &str = "canvas.submit";
}

// ---------- view / surface ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceAttachedParams {
    pub surface_id: String,
    /// 面的类型：面板 / 插件窗口 / 整页接管 / 全接管 / 设置页。
    pub kind: SurfaceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceKind {
    Panel,
    Window,
    Page,
    Takeover,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceDetachedParams {
    pub surface_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitParams {
    pub surface_id: String,
    pub tree: crate::view::ViewNode,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResult {
    pub applied: bool,
}

/// 贡献点位置（封闭枚举；扩容随协议升版）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContributionSlot {
    /// 工作区面板（「添加面板 / 切换视图」菜单条目）。
    Panel,
    /// 表格编辑器工具条「视图」下拉条目。
    TableView,
    /// 设置页顶层 tab（注册即挂载：宿主立即回 `host:surface.attached`，插件常驻设置树）。
    Settings,
}

/// `view.register` 的 params：声明一个贡献点。`id` 插件内唯一且不得含 `:`（宿主的面标识以它作分隔），
/// 同时是该面挂载后的 `surfaceId`；`icon` 缺省时宿主用拼图图标。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    pub slot: ContributionSlot,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// `view.unregister` 的 params：按 id 撤销自己的贡献；id 未注册时报可读错误。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterParams {
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewEventParams {
    pub surface_id: String,
    pub node_id: String,
    pub event: crate::view::ViewEvent,
}

/// `canvas.submit` 的 params：清单整发整换（无增量协议），空清单 = 清空画布。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSubmitParams {
    pub surface_id: String,
    /// 目标画布节点 id（须在当前 `view.submit` 树里且为 canvas 节点）。
    pub node_id: String,
    pub commands: Vec<crate::canvas::DrawCommand>,
}

// ---------- fs ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateDirResult {
    pub path: String,
}

/// `path` 一律绝对路径。信任模型与进程托管一致：插件本就是任意权限的用户进程，
/// 声明式权限是透明度机制而非沙箱边界；`fs` 服务的价值在统一出口与审计事实。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathParams {
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryContent {
    pub base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextParams {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteBinaryParams {
    pub path: String,
    pub base64: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistsResult {
    pub exists: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub entries: Vec<FsEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub kind: FsEntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FsEntryKind {
    File,
    Dir,
}

// ---------- process ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnParams {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 追加 / 覆盖宿主环境（不清空——被托管服务需要 PATH/TEMP 等才能正常行为）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnResult {
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidParams {
    pub pid: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteStdinParams {
    pub pid: u32,
    pub data: String,
}

/// 托管进程事件负载（[`process::HOST_EVENT`]）。形态与宿主进程托管的上报一一对应。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ProcessEvent {
    Stdout { data: String },
    Stderr { data: String },
    Terminated { code: Option<i32> },
    Error { message: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessEventParams {
    pub pid: u32,
    pub event: ProcessEvent,
}

// ---------- state / notification / shortcut ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateReadResult {
    /// 该插件的自持数据；从未写过时为 null。
    pub json: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateWriteParams {
    pub json: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowNotificationParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub level: NotificationLevel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationLevel {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterShortcutParams {
    /// 加速键描述（如 `Shift+Alt+E`）；与既注册项冲突时报可读错误。
    pub accelerator: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceleratorParams {
    pub accelerator: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutTriggeredParams {
    pub accelerator: String,
}

// ---------- clipboard / dialog / http ----------

/// 剪贴板写 params 与读 result 共用：一段纯文本。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPayload {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickFileParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// true = 保存对话框；取消时 result 的 `path` 字段缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save: Option<bool>,
    /// 预填路径：目录部分作起始目录，文件名部分预填进名称栏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FileFilter>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFilter {
    pub name: String,
    /// 不带点的扩展名列表（如 `["png", "jpg"]`）。
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickDirectoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedPath {
    /// 用户取消时该字段缺省（serde 对 null 与缺省等价处理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequestParams {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpResult {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body_base64: String,
}

// ---------- doc ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocOpenParams {
    /// 仓库容器内的相对路径（本地仓库）或服务端内容路径（协作空间），与 vault 口径一致。
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_and_surface_kind_serde_uses_camel_case() {
        assert_eq!(serde_json::to_value(ContributionSlot::Panel).unwrap(), "panel");
        assert_eq!(serde_json::to_value(ContributionSlot::TableView).unwrap(), "tableView");
        assert_eq!(serde_json::to_value(ContributionSlot::Settings).unwrap(), "settings");
        assert_eq!(serde_json::to_value(SurfaceKind::Settings).unwrap(), "settings");
        let kind: SurfaceKind = serde_json::from_value(serde_json::json!("settings")).unwrap();
        assert_eq!(kind, SurfaceKind::Settings);
        let slot: ContributionSlot = serde_json::from_value(serde_json::json!("settings")).unwrap();
        assert_eq!(slot, ContributionSlot::Settings);
    }
}
