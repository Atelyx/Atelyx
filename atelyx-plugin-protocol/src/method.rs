//! 能力面方法：插件→宿主的无前缀方法与宿主→插件的 `host:` 方法，方法名常量 + 定型 params/result。
//! 全部方法按调用方插件自动绑定插件 id（协议不暴露 id 参数，插件无法指定他人命名空间）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- 方法名常量 ----------

pub mod view {
    /// 插件注册一个贡献点（面板 / 表格视图 / 设置页），使其出现在宿主对应的入口里。
    pub const REGISTER: &str = "view.register";
    /// 插件按 id 撤销自己的一个贡献点。
    pub const UNREGISTER: &str = "view.unregister";
    /// 插件请求接管当前聚焦窗口（整页 / 全接管）。宿主按窗应用，退出入口由宿主常驻提供。
    pub const TAKEOVER: &str = "view.takeover";
    /// 宿主告知插件某个面已就绪 / 已移除（通知）。
    pub const HOST_ATTACHED: &str = "host:surface.attached";
    pub const HOST_DETACHED: &str = "host:surface.detached";
}

pub mod window {
    /// 插件请求打开一个独立窗口承载自己的面（声明的 `id` 即该窗口面的 `surfaceId`）。
    /// 缺省用系统窗口边框；`decorations: false` 时去掉系统边框，由宿主提供标题栏
    /// （标题 / 拖动 / 最小化 / 最大化 / 关闭）。
    pub const OPEN: &str = "window.open";
    /// 插件关闭自己打开的一个窗口。
    pub const CLOSE: &str = "window.close";
    /// 插件运行时改一个已打开窗口的属性（尺寸 / 置顶；「不进任务栏」只能在开窗时声明）。
    pub const UPDATE: &str = "window.update";
    /// 插件重新显示一个因失焦被隐藏的窗口（`hideOnBlur` 的唤起回路）。
    pub const SHOW: &str = "window.show";
    /// 插件请求拖动承载某个面的窗口（自绘标题栏走这里：宿主接管拖动，插件不必自己算窗口坐标）。
    pub const BEGIN_DRAG: &str = "window.beginDrag";
    /// 宿主告知插件某个窗口已被用户关闭（通知）。
    pub const HOST_CLOSED: &str = "host:window.closed";
    /// 宿主告知插件某个窗口因失焦被收起（通知）；插件可用 [`SHOW`] 重新显示。
    pub const HOST_HIDDEN: &str = "host:window.hidden";
}

/// 当前场景的布局面：插件声明自己期望的默认布局，宿主在首次启用时摆进当前场景。
pub mod layout {
    /// 声明默认布局（每个插件至多生效一次；用户可自行改名 / 删除）。
    pub const DECLARE_DEFAULT: &str = "layout.declareDefaultLayout";
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

/// 当前激活仓库的按路径读写（`path` 为相对仓库根的路径）。协作空间模式下同一能力面
/// 由宿主转发到服务端，插件不感知容器类型。写入与宿主自身写同出口（写盘队列 / 服务端内容面），
/// 插件不得用 `fs` 直改仓库（会与内存态和协作广播互踩）。
pub mod vault {
    pub const READ_TEXT: &str = "vault.readText";
    pub const WRITE_TEXT: &str = "vault.writeText";
    /// 读二进制文件（base64；单次上限见宿主实现，超限可读失败）。
    pub const READ_BINARY: &str = "vault.readBinary";
    /// 写二进制文件（base64；父目录自动创建；正在编辑器中打开的文件拒写；单次上限 32 MiB）。
    pub const WRITE_BINARY: &str = "vault.writeBinary";
    /// 把插件私有目录里的一个文件收进仓库（大文件走这条：字节不进 RPC 载荷）。
    pub const IMPORT_FILE: &str = "vault.importFile";
    /// 读图片的像素宽高（png / jpeg / gif / bmp / webp），不返回文件内容。
    pub const IMAGE_INFO: &str = "vault.imageInfo";
    pub const LIST: &str = "vault.list";
    pub const EXISTS: &str = "vault.exists";
    /// 删除一个仓库相对路径（文件或文件夹；文件夹连同内容递归删除）。仓库根不可删。
    /// 被编辑面打开或被当前画布节点引用的文件拒删。
    pub const REMOVE: &str = "vault.remove";
    /// 把一个仓库相对路径改名 / 移动到另一路径（文件或文件夹；目标不存在，父目录自动创建）。
    /// 引用同步：`.md` 内部链接与画布文件引用随改；源文件被编辑面打开或被当前画布节点引用时拒绝。
    pub const RENAME: &str = "vault.rename";
}

/// 协作空间内的插件消息与团队元数据：消息按「通道」隔离，宿主把插件 id 作为通道前缀，
/// 同空间的同插件实例互通（跨空间不达、不可抢占他人通道）；元数据是空间团队层共享键值。
pub mod collab {
    /// 当前空间的连接状态与在场成员清单（不含自己）。
    pub const PEERS: &str = "collab.peers";
    /// 向同空间的对等插件发一条消息：缺省广播给全部对端，给了目标即单播。
    pub const SEND: &str = "collab.send";
    /// 读当前空间的团队元数据（`key` 缺省 = 全部键）。团队所有插件共享同一份键空间。
    pub const META_GET: &str = "collab.metaGet";
    /// 按键写当前空间的团队元数据：只改给出的键，其余键保持原值。
    pub const META_PATCH: &str = "collab.metaPatch";
    /// 对端消息到达（通知）。
    pub const HOST_MESSAGE: &str = "host:collab.message";
}

/// 表格面：读写当前打开表格。写入与宿主自身编辑同一出口（宿主合并落盘并广播协作补丁），
/// 插件不得旁路读写 `.atb` 真源。
pub mod table {
    /// 读当前打开表格的只读快照（字段 / 行 / 仓库相对路径）；未打开表格 = `open: false`。
    pub const SNAPSHOT: &str = "table.snapshot";
    /// 逐格写：只改指定字段，不整行替换（不覆盖同行其它字段与并发编辑）。
    pub const UPDATE_CELL: &str = "table.updateCell";
    /// 新增一行（结果回传新行 id，避免按行 id 差集定位的并发歧义）。
    pub const ADD_ROW: &str = "table.addRow";
    pub const REMOVE_ROW: &str = "table.removeRow";
    /// 写宿主选中态（视图与表格联动用）。
    pub const SELECT_ROW: &str = "table.selectRow";
    /// 向图片字段单元格写入图片引用（图片须先经 `vault.writeBinary` 落到仓库）。
    pub const SET_CELL_IMAGE: &str = "table.setCellImage";
}

pub mod state {
    pub const READ: &str = "state.read";
    pub const WRITE: &str = "state.write";
}

pub mod notification {
    /// 在应用内弹一条通知（宿主已收尾、通知未投递时报可读错误）。
    pub const SHOW: &str = "notification.show";
}

/// 系统托盘：插件提交自己的菜单条目，宿主维护唯一托盘图标（无条目时图标消失）。
pub mod tray {
    /// 整发整换本插件的托盘菜单（空清单 = 收起）。宿主自有的「显示 / 退出」条目恒在末尾。
    pub const SET_ITEMS: &str = "tray.setItems";
    /// 用户点了本插件的某个托盘条目（通知）。
    pub const HOST_CLICK: &str = "host:tray.click";
}

pub mod shortcut {
    pub const REGISTER: &str = "shortcut.register";
    pub const UNREGISTER: &str = "shortcut.unregister";
    /// 事前查询某键位是否已被占用（返回占用方插件 id，或仅知「被系统 / 其他应用占用」）。
    pub const QUERY: &str = "shortcut.query";
    /// 全局快捷键触发（通知）。
    pub const HOST_TRIGGERED: &str = "host:shortcut.triggered";
}

pub mod clipboard {
    pub const WRITE_TEXT: &str = "clipboard.writeText";
    pub const READ_TEXT: &str = "clipboard.readText";
    /// 读剪贴板中的图片（PNG，base64；无图片时字段缺省）。
    pub const READ_IMAGE: &str = "clipboard.readImage";
    /// 写一张 PNG 图片到剪贴板（base64；单次上限 32 MiB，解码失败或超限即拒绝）。
    pub const WRITE_IMAGE: &str = "clipboard.writeImage";
}

pub mod dialog {
    pub const PICK_FILE: &str = "dialog.pickFile";
    pub const PICK_DIRECTORY: &str = "dialog.pickDirectory";
}

pub mod http {
    pub const REQUEST: &str = "http.request";
}

pub mod doc {
    /// 用户打开了插件注册的领域文档类型文件（通知）。`surfaceId` 为宿主分配的实例面标识，
    /// 插件据此以 `frame.submitShared` 提交该文档实例的界面。
    pub const HOST_OPEN: &str = "host:doc.open";
    /// 文档实例已关闭 / 已切换走（通知）。插件此后不得再向该 surfaceId 提交帧。
    pub const HOST_CLOSE: &str = "host:doc.close";
}

/// 面帧：整个面交给插件自绘，一帧即当前完整界面。
pub mod frame {
    /// 插件提交一份整面帧：内容写在共享内存槽里，params 只带槽位定位与校验信息。整帧替换。
    pub const SUBMIT_SHARED: &str = "frame.submitShared";
    /// 帧超出当前槽容量时请求宿主换一份更大的共享内存区域（应答 = 新区域描述）。
    pub const REGION: &str = "frame.region";
    /// 宿主回发原始输入（通知）：指针 / 滚轮 / 按键 / 输入法 / 焦点 / 尺寸。
    pub const HOST_INPUT: &str = "host:frame.input";
    /// 宿主按面声明的帧率发帧拍（通知）：插件收到后提交该帧的内容。
    pub const HOST_TICK: &str = "host:frame.tick";
}

/// 纯查询：不改界面、不产生帧。
pub mod query {
    /// 批量量算文字占位尺寸（排版按真实字宽摆放，不能按字符数估）。
    pub const MEASURE: &str = "query.measure";
    /// 取宿主主题表（颜色 / 字号档 / 圆角档），插件据此与宿主观感一致。
    pub const THEME: &str = "query.theme";
}

/// Markdown 解析：内容不变则结果不变，插件应按原文缓存，不必逐帧往返。
pub mod markdown {
    /// 把 Markdown 文本转成结构化块（与笔记同一套解析语义），排版与绘制仍归插件。
    /// 服务名 `markdown`、方法 `parse`，因此外部提供者可以整体替换解析实现。
    pub const PARSE: &str = "markdown.parse";
}

/// 日历服务（由日历插件提供）：方法名即 `calendar.<方法>`，也可经 `service.call` 以
/// `service = calendar` 调用。日程真源随容器走（本地仓库 / 协作空间团队 meta），插件不自留副本。
pub mod calendar {
    /// 服务名。
    pub const SERVICE: &str = "calendar";
    /// 当前仓库的日程（按日期、创建时间与 id 排序）。
    pub const LIST: &str = "list";
    /// 新增一条日程；`id` 与创建时刻由服务补齐。
    pub const ADD: &str = "add";
    /// 改写一条日程（只改给出的字段）。
    pub const UPDATE: &str = "update";
    /// 删除一条日程。
    pub const REMOVE: &str = "remove";
}

pub mod history {
    /// 按天聚合的仓库活动：新建 / 修改 / 删除的文件数合计。
    pub const ACTIVITY: &str = "history.activity";
    /// 本地某一天的版本记录（本地历史侧文件 / 空间聚合，宿主统一按本地日界归日）。
    pub const DAY: &str = "history.day";
    /// 在宿主界面打开某个文件的历史弹层（版本列表与回滚由宿主承载）。
    pub const OPEN: &str = "history.open";
}

/// 编辑器：当前打开笔记的选区读写（由笔记插件提供，其他插件据此做「改写选中内容」这类动作）。
/// 只在编辑状态有意义；阅读态选区不属于编辑器能力。
pub mod editor {
    /// 读当前文档路径与选区（无选区时 `from == to`、`text` 为空）。
    pub const SELECTION: &str = "editor.selection";
    /// 用给定文本替换当前选区（空串 = 删除选区）；写入经笔记同一套保存链。
    pub const REPLACE_SELECTION: &str = "editor.replaceSelection";
}

pub mod ai {
    /// 插件发起 / 续聊一轮对话（须声明 `ai` 权限）。回执 = run 已就位，事件随后经
    /// [`HOST_CHAT_EVENT`] 流达。会话 = 仓库普通会话（主面板可见）；可指定 Agent / 模型 /
    /// 图片（见 [`ChatSendParams`]），Agent 缺省沿用会话或内置「对话」、模型缺省跟随面板
    /// 当前选择；解析失败时拒绝并回传原因。
    pub const CHAT_SEND: &str = "ai.chat.send";
    /// 中断一个会话的活跃 run（幂等：无活跃 run 也成功）。
    pub const CHAT_STOP: &str = "ai.chat.stop";
    /// 读当前激活仓库的会话清单或单会话消息历史。
    pub const CHAT_HISTORY: &str = "ai.chat.history";
    /// 读当前仓库可用的模型清单（按供应商分组，只含 id / 名称，不含地址与密钥）
    /// 与面板当前的模型选择。
    pub const MODELS: &str = "ai.models";
    /// 读当前仓库可用的 Agent 清单（内置 + 仓库自定义）与面板当前生效的 Agent。
    pub const AGENTS: &str = "ai.agents";
    /// 重命名一个会话（写其元数据标题）。
    pub const CHAT_RENAME: &str = "ai.chat.rename";
    /// 删除一个会话（连同其消息文件）。
    pub const CHAT_DELETE: &str = "ai.chat.delete";
    /// 压缩一个会话（把较早消息总结为压缩起点；回执 = 已受理，结果经 `ai.chat.history` 复核）。
    pub const CHAT_COMPACT: &str = "ai.chat.compact";
    /// 对话事件流（通知）：快照节流拍 + 终态 + 命名结果。
    pub const HOST_CHAT_EVENT: &str = "host:ai.chat.event";
    /// 注册一个可被 Agent 调用的工具（名称全局唯一，不得与内置工具或其他插件的工具重名）。
    pub const REGISTER_TOOL: &str = "ai.registerTool";
    /// 撤销自己注册的工具（未注册的名称报可读错误）。
    pub const UNREGISTER_TOOL: &str = "ai.unregisterTool";
    /// 回报一次工具调用结果（`callId` 原样带回宿主下发值；不在册或不属于本插件的 `callId`
    /// 报可读错误——结果不会进入模型上下文）。
    pub const TOOL_RESULT: &str = "ai.toolResult";
    /// 宿主请求插件执行一次工具调用（通知）；插件处理完经 [`TOOL_RESULT`] 回报。
    pub const HOST_TOOL_CALL: &str = "host:ai.toolCall";
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
    /// 表格编辑器内的插件表格视图（挂载在表格面板内，替掉网格）。
    TableView,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceDetachedParams {
    pub surface_id: String,
}

/// 界面提交类方法的回执（`frame.submitShared`）：`applied` = 宿主已接受并生效。
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
    /// 设置页顶层 tab（注册即挂载：宿主立即回 `host:surface.attached`，插件常驻设置面）。
    Settings,
}

/// 内边距声明：数字 = 四边等距；`{horizontal, vertical}` = 分轴；`{top, right, bottom, left}` = 分边。
/// 两个对象形态都不接受未声明字段，避免分边写法被分轴形态吞掉。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaddingSpec {
    All(f32),
    Axes(AxesPadding),
    Edges(EdgesPadding),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AxesPadding {
    #[serde(default)]
    pub horizontal: f32,
    #[serde(default)]
    pub vertical: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EdgesPadding {
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub right: f32,
    #[serde(default)]
    pub bottom: f32,
    #[serde(default)]
    pub left: f32,
}

impl PaddingSpec {
    /// 四边取值（上 / 右 / 下 / 左）；宿主据此校验并换算成自身布局类型。
    pub fn edges(self) -> [f32; 4] {
        match self {
            Self::All(value) => [value; 4],
            Self::Axes(axes) => [axes.vertical, axes.horizontal, axes.vertical, axes.horizontal],
            Self::Edges(edges) => [edges.top, edges.right, edges.bottom, edges.left],
        }
    }
}

/// `view.register` 的 params：声明一个贡献点。`id` 插件内唯一且不得含 `:`（宿主的面标识以它作分隔），
/// 同时是该面挂载后的 `surfaceId`；`icon` 为宿主内置形状名或插件自备形状名
/// （内置名优先），缺省 / 未注册时宿主用拼图图标。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterParams {
    pub slot: ContributionSlot,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 面外壳内边距：数字 = 四边等距，也可按轴 / 按边声明（见 [`PaddingSpec`]）；
    /// 0 = 贴边，缺省 = 宿主默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub padding: Option<PaddingSpec>,
}

/// `window.update` 的 params：运行时改一个已打开窗口的属性（缺省字段不动）。至少给一项。
/// 「不进任务栏」是开窗期设置，运行时不支持改。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowUpdateParams {
    /// 插件声明的窗口 id（`window.open` 的 `id`）。
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_on_top: Option<bool>,
}

/// `window.show` / `host:window.hidden` 的 params：按插件声明的窗口 id。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowIdParams {
    pub id: String,
}

/// `shortcut.query` 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutQueryParams {
    pub accelerator: String,
}

/// `shortcut.query` 的 result：`available` = 可注册；`owner` = 占用方插件 id（被系统 / 其他应用占用
/// 或无占用时为缺省——此时只有 `available` 可信，宿主无从得知占用者身份）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutQueryResult {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// `view.unregister` 的 params：按 id 撤销自己的贡献；id 未注册时报可读错误。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterParams {
    pub id: String,
}

/// 接管范围（封闭枚举）。`page` = 内容区全给插件（宿主标题栏保留）；`full` = 整窗归插件呈现
/// （含标题栏区域，窗口拖动 / 缩放 / 关闭由宿主代执行）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TakeoverScope {
    Page,
    Full,
}

/// `view.takeover` 的 params：把本插件的一个贡献面接管到当前聚焦窗口。`surfaceId` 须为
/// 已注册的贡献 id（接管的就是该面自己的整面帧）；退出接管入口由宿主常驻提供，插件无需声明退路。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeoverParams {
    pub scope: TakeoverScope,
    pub surface_id: String,
}

/// `window.open` 的 params：`id` 插件内唯一、非空且不含 `:`，同时是该窗口面的 `surfaceId`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowOpenParams {
    pub id: String,
    pub title: String,
    /// 逻辑像素；缺省由宿主取默认尺寸。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    /// 置顶浮窗。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_on_top: Option<bool>,
    /// 不进任务栏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_taskbar: Option<bool>,
    /// 失焦即收起（隐藏窗口，从托盘 / 快捷键等入口再唤起是插件自己的事）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_on_blur: Option<bool>,
    /// 系统窗口边框；缺省 true。`false` 时由宿主提供标题栏与窗口控制（拖动 / 最小化 /
    /// 最大化 / 关闭），插件内容占满剩余区域。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decorations: Option<bool>,
}

/// `window.close` 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCloseParams {
    pub id: String,
}

/// [`window::HOST_CLOSED`] 的 params：用户（而非插件）关闭了该窗口。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowClosedParams {
    pub id: String,
}

/// `window.beginDrag` 的 params：要拖动的窗口按承载该面的窗口解析（插件窗口 / 接管面 / 所在面板）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDragParams {
    pub surface_id: String,
}

/// [`query::MEASURE`] 的 result：占位宽高（逻辑像素）与换行后的行数（单行量算恒为 1）。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureTextResult {
    pub width: f32,
    pub height: f32,
    pub lines: u32,
}

/// 单次批量量算的条目上限。
pub const MAX_MEASURE_ITEMS: usize = 512;

/// [`query::MEASURE`] 的一条：字段与 [`MeasureBatchParams`] 的条目同口径。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureItem {
    pub text: String,
    pub size: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<crate::paint::TextWeight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<crate::paint::TextFamily>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
}

impl MeasureItem {
    fn validate(&self) -> Result<(), String> {
        if !self.size.is_finite() || self.size <= 0.0 {
            return Err("字号必须大于 0".into());
        }
        if let Some(max_width) = self.max_width {
            if !max_width.is_finite() || max_width <= 0.0 {
                return Err("可用宽度必须大于 0".into());
            }
        }
        Ok(())
    }
}

/// [`query::MEASURE`] 的 params：一次量算多段文字（排一屏文本的量算往返要落在单批里）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureBatchParams {
    pub items: Vec<MeasureItem>,
}

impl MeasureBatchParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.items.len() > MAX_MEASURE_ITEMS {
            return Err(format!("单次量算不得超过 {MAX_MEASURE_ITEMS} 条"));
        }
        for (index, item) in self.items.iter().enumerate() {
            item.validate()
                .map_err(|e| format!("第 {index} 条：{e}"))?;
        }
        Ok(())
    }
}

/// [`query::MEASURE`] 的 result：顺序与 params 的 `items` 一一对应。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureBatchResult {
    pub sizes: Vec<MeasureTextResult>,
}

/// [`query::THEME`] 的 result：颜色 / 字号档 / 圆角档 / 事件色板
/// （值随用户的主题与字号设置变化）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeResult {
    pub colors: ThemeColors,
    pub font_sizes: FontSizes,
    pub radii: Radii,
    /// 事件条目可选色板（十六进制，宿主界面同序）；插件按它给用户选色。
    #[serde(default)]
    pub event_colors: Vec<String>,
}

/// 主题色：十六进制 `#rrggbb` / `#rrggbbaa`，字段名与 [`crate::paint::ColorToken`] 的 wire 名一致。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    pub text: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub accent: String,
    pub accent_soft: String,
    pub danger: String,
    pub danger_soft: String,
    pub success: String,
    pub success_soft: String,
    pub warning: String,
    pub warning_soft: String,
    pub surface: String,
    pub surface_muted: String,
    pub surface_sunken: String,
    pub hover: String,
    pub border: String,
    pub border_strong: String,
    pub transparent: String,
}

/// 字号档（绝对逻辑像素）：`micro` / `caption` / `ui` / `body` / `h2` / `h1`。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontSizes {
    pub micro: f32,
    pub caption: f32,
    pub ui: f32,
    pub body: f32,
    pub h2: f32,
    pub h1: f32,
}

/// 圆角档（逻辑像素）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Radii {
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
}

// ---------- editor ----------

/// 单次替换文本的字节上限（超限按可读错误拒绝）。
pub const MAX_EDITOR_TEXT_BYTES: usize = 512 * 1024;

/// [`editor::SELECTION`] 的 result：当前文档与选区。
/// `path` 缺省 = 未打开笔记；`from` / `to` 是正文字节偏移（frontmatter 之后的部分）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub text: String,
    pub from: usize,
    pub to: usize,
}

impl EditorSelection {
    /// 阅读态或未打开笔记时的空选区（`path` 仍带回当前文档）。
    pub fn empty(path: Option<String>) -> Self {
        Self { path, text: String::new(), from: 0, to: 0 }
    }
}

/// [`editor::REPLACE_SELECTION`] 的 params：替换文本（空串 = 删除当前选区）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorReplaceParams {
    pub text: String,
}

impl EditorReplaceParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > MAX_EDITOR_TEXT_BYTES {
            return Err(format!("替换文本不得超过 {MAX_EDITOR_TEXT_BYTES} 字节"));
        }
        Ok(())
    }
}

// ---------- calendar ----------

/// 一条日程（`date` 为本地日期 `YYYY-MM-DD`；`color` 为 hex，缺省 = 默认强调色）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// 创建时刻（Unix 毫秒），同日条目按它排序。
    #[serde(default)]
    pub created_at: i64,
}

/// 标题字符上限（超限按可读错误拒绝）。
pub const MAX_CALENDAR_TITLE_CHARS: usize = 200;

/// `YYYY-MM-DD` 形态校验（只查形状与范围，不查该日是否存在）。
pub(crate) fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let digits = |range: std::ops::Range<usize>| {
        bytes[range].iter().all(u8::is_ascii_digit)
    };
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let month: u32 = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..10].parse().unwrap_or(0);
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn validate_calendar_title(title: &str) -> Result<(), String> {
    if title.chars().count() > MAX_CALENDAR_TITLE_CHARS {
        return Err(format!("标题不得超过 {MAX_CALENDAR_TITLE_CHARS} 字"));
    }
    Ok(())
}

/// [`calendar::LIST`] 的 result：当前仓库的日程（按开始日期排序）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarListResult {
    pub items: Vec<CalendarItem>,
}

/// [`calendar::ADD`] 的 params：`id` / `createdAt` 由日历服务补齐；`color` 未给出即按强调色显示。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAddParams {
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl CalendarAddParams {
    pub fn validate(&self) -> Result<(), String> {
        if !is_calendar_date(&self.date) {
            return Err(format!("日期须为 YYYY-MM-DD：{}", self.date));
        }
        if self.title.trim().is_empty() {
            return Err("标题不能为空".to_string());
        }
        validate_calendar_title(&self.title)
    }
}

/// [`calendar::UPDATE`] 的 params：只改给出的字段（`None` = 保持原值；空串清空备注）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarUpdateParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl CalendarUpdateParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("id 不能为空".to_string());
        }
        if let Some(date) = &self.date {
            if !is_calendar_date(date) {
                return Err(format!("日期须为 YYYY-MM-DD：{date}"));
            }
        }
        if let Some(title) = &self.title {
            if title.trim().is_empty() {
                return Err("标题不能为空".to_string());
            }
            validate_calendar_title(title)?;
        }
        Ok(())
    }
}

/// [`calendar::REMOVE`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarIdParams {
    pub id: String,
}

impl CalendarIdParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("id 不能为空".to_string());
        }
        Ok(())
    }
}

// ---------- history ----------

/// 一天的活动计数（`day` = 自纪元天数）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DayActivity {
    pub day: i64,
    pub count: u32,
}

/// [`history::ACTIVITY`] 的 result：按天升序的活动计数。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryActivityResult {
    pub days: Vec<DayActivity>,
}

/// [`history::DAY`] 的 params：`day` 为本地日期 `YYYY-MM-DD`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDayParams {
    pub day: String,
}

impl HistoryDayParams {
    pub fn validate(&self) -> Result<(), String> {
        if !is_calendar_date(&self.day) {
            return Err(format!("日期须为 YYYY-MM-DD：{}", self.day));
        }
        Ok(())
    }
}

/// 一条当日版本记录：`kind` / `action` 取宿主历史侧文件的取值域（`note` / `canvas` / `table`、
/// `edit` / `restore`）；`file` 为仓库相对路径，`ts` 为毫秒时间戳。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDayEntry {
    pub kind: String,
    pub file: String,
    pub ts: i64,
    pub action: String,
    pub author_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// [`history::DAY`] 的 result：按时间新→旧排列。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryDayResult {
    pub entries: Vec<HistoryDayEntry>,
}

/// 历史弹层支持的文件类型。
pub const HISTORY_KINDS: &[&str] = &["note", "canvas", "table"];

/// [`history::OPEN`] 的 params：打开某文件的宿主历史弹层。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryOpenParams {
    pub kind: String,
    pub file: String,
}

impl HistoryOpenParams {
    pub fn validate(&self) -> Result<(), String> {
        if !HISTORY_KINDS.contains(&self.kind.as_str()) {
            return Err(format!("未知的历史类型：{}", self.kind));
        }
        if self.file.trim().is_empty() {
            return Err("文件路径不能为空".to_string());
        }
        Ok(())
    }
}

// ---------- markdown ----------

/// 单次转换的源文本上限（字节）：超限按可读错误拒绝，插件应自行分段。
pub const MAX_MARKDOWN_BYTES: usize = 512 * 1024;

/// [`markdown::PARSE`] 的 params：交给宿主按笔记同一套解析语义转成结构化块。
/// 宿主只做解析（块与行内片段），排版与绘制仍归插件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownParams {
    pub content: String,
}

impl MarkdownParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.content.len() > MAX_MARKDOWN_BYTES {
            return Err(format!("Markdown 文本不得超过 {MAX_MARKDOWN_BYTES} 字节"));
        }
        Ok(())
    }
}

/// [`markdown::PARSE`] 的 result：按源序排列的块。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownResult {
    pub blocks: Vec<MarkdownBlock>,
}

/// 行内片段：同一段文字里的样式差异（字重、等宽、链接目标）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownSpan {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strong: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// 块：`depth` 为嵌套层数（列表 / 引用内部块递增），由插件换算缩进。
/// 表格单元格只给纯文本（单元格行内样式不展开）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum MarkdownBlock {
    Heading {
        level: u8,
        depth: u32,
        spans: Vec<MarkdownSpan>,
    },
    Paragraph {
        depth: u32,
        spans: Vec<MarkdownSpan>,
    },
    /// 列表项：`index` 对有序列表取源里的序号（`3.` 就是 3），无序列表恒为 1；`ordered` 决定画圆点还是序号。
    ListItem {
        depth: u32,
        ordered: bool,
        index: u32,
        spans: Vec<MarkdownSpan>,
    },
    /// 引用段落：插件在左侧画竖条。
    Quote {
        depth: u32,
        spans: Vec<MarkdownSpan>,
    },
    Code {
        depth: u32,
        lang: String,
        content: String,
    },
    /// 表格：`rows[0]` 是表头行，其余为数据行；单元格只给纯文本。
    Table {
        depth: u32,
        rows: Vec<Vec<String>>,
    },
    Divider,
}

// ---------- layout ----------

/// `layout.declareDefaultLayout` 的 params：给宿主一套默认面板布局。`name` 为布局名
/// （与既有布局重名时宿主追加序号），`spec` 只描述结构与视图 kind。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclareDefaultLayoutParams {
    pub name: String,
    pub spec: crate::layout::LayoutSpecNode,
}

// ---------- fs ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateDirResult {
    pub path: String,
}

/// `path` 的口径由能力面决定：`fs` 用绝对路径，`vault` 用相对仓库根的路径。
/// 信任模型与进程托管一致：插件本就是任意权限的用户进程，
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

/// [`vault::RENAME`] 的 params：`from` / `to` 均为仓库相对路径，`to` 不可已存在。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultRenameParams {
    pub from: String,
    pub to: String,
}

/// [`vault::IMPORT_FILE`] 的 params：`path` 为仓库相对目标，`source` 为插件私有目录内的绝对路径。
/// 复制语义：落库成功后源文件仍归插件，由插件自行清理。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileParams {
    pub path: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExistsResult {
    pub exists: bool,
}

/// [`vault::IMAGE_INFO`] 的 result：图片固有像素尺寸（不随界面缩放）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfoResult {
    pub width: u32,
    pub height: u32,
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

/// 剪贴板图片：统一编成 PNG（插件不需要自己处理系统剪贴板的像素格式）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardImage {
    pub base64: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardImageResult {
    /// 剪贴板里没有图片时整个字段缺省（与「有图片但为空」不做区分——空图片不成立）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ClipboardImage>,
}

/// [`clipboard::WRITE_IMAGE`] 的 params：PNG 字节的 base64。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardWriteImageParams {
    pub base64: String,
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
    /// 宿主为本实例分配的面标识：插件据此以 `frame.submitShared` 提交该文档实例的界面。
    pub surface_id: String,
}

/// [`doc::HOST_CLOSE`] 的 params：实例随标签关闭 / 换文件被关闭。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocCloseParams {
    pub surface_id: String,
    pub path: String,
}

// ---------- ai ----------

/// [`ai::CHAT_SEND`] 的 params：`session_id` 缺省 = 新建会话；非缺省 = 在该会话续聊
/// （会话须存在于当前仓库，否则可读拒绝）。同会话已有活跃 run 时拒绝（每会话单 run）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendParams {
    /// 正文；为空时 `images` 必须非空（纯图片消息）。
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Agent id（当前仓库名册）；缺省 = 新建会话用内置「对话」工具集、续聊沿用会话已记录的
    /// Agent。新建会话时写入会话记录，续聊时只作用于本轮；指定但不存在 = 拒绝。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 模型 id（供应商配置里的模型名）；缺省 = 面板当前选择，指定但未配置 = 拒绝。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 供应商 id：给 `model` 消歧（同模型多供应商时必填；单独给出 = 拒绝）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 仓库内图片相对路径（png / jpg / jpeg / webp / gif）；逐张校验，任一不可用 = 整条拒绝。
    /// 校验通过后到 run 读取之间文件可能消失：读不到的那张在该轮请求里跳过。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// [`ai::CHAT_SEND`] 的 result：run 已就位；流式事件随后经 [`ai::HOST_CHAT_EVENT`] 流达。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub session_id: String,
    /// 本轮 assistant 消息 id（后续事件载荷同源）。
    pub message_id: String,
}

/// [`ai::CHAT_STOP`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStopParams {
    pub session_id: String,
}

/// [`ai::CHAT_HISTORY`] 的 params：`session_id` 缺省 = 列会话清单；非缺省 = 读该会话消息。
/// 单会话模式可给 `offset` / `query` 做分页与搜索（`offset` 为末尾起算的跳过条数）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 消息条数上限（读单会话时生效；缺省全量，超出取末尾）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// 从末尾起跳过的条数（分页：先按 `query` 过滤，再取 `[len-offset-limit, len-offset)`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    /// 关键词过滤（在消息文本中不区分大小写搜索；缺省不过滤）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// [`ai::CHAT_RENAME`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRenameParams {
    pub session_id: String,
    /// 新标题（非空；空白视为非法）。
    pub title: String,
}

/// [`ai::CHAT_DELETE`] / [`ai::CHAT_COMPACT`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionParams {
    pub session_id: String,
}

/// 一条会话摘要（清单模式）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub updated_at: i64,
    pub message_count: u64,
}

/// 单会话读取模式的 result：元数据 + 消息数组（消息形态与会话磁盘格式同源：
/// `id` / `role` / `content` / `displayContent` / `steps` / 压缩注解）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionWire {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub messages: Vec<Value>,
}

/// [`ai::CHAT_HISTORY`] 的 result：清单模式只填 `sessions`；单会话模式只填 `session`。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<ChatSessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<ChatSessionWire>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModel {
    /// 模型名（`ChatSendParams.model` 的取值）。
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

/// 一个供应商及其模型（不含地址与密钥）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModelProvider {
    pub id: String,
    pub name: String,
    pub models: Vec<AiModel>,
}

/// 面板当前的模型选择（供应商 id + 模型名）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModelPick {
    pub provider_id: String,
    pub model: String,
}

/// [`ai::MODELS`] 的 result：`current` = 面板当前选择，未显式选择时缺省。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModelsResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<AiModelProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<AiModelPick>,
}

/// 一个可用 Agent（内置 + 仓库自定义）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAgent {
    pub id: String,
    pub name: String,
    /// 预置 Agent（不可删除，可改配置）。
    pub builtin: bool,
}

/// [`ai::AGENTS`] 的 result：`current` = 面板当前生效的 Agent id，无生效项时缺省。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAgentsResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AiAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

/// [`ai::REGISTER_TOOL`] 的 params：`name` 为模型可见的工具名（字母 / 数字 / `_` / `-`，
/// 最长 64 字符），`parameters` 为 JSON Schema 对象。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterToolParams {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// [`ai::UNREGISTER_TOOL`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterToolParams {
    pub name: String,
}

/// [`ai::HOST_TOOL_CALL`] 的 params：插件须用同一 `callId` 经 [`ai::TOOL_RESULT`] 回报。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallParams {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

/// [`ai::TOOL_RESULT`] 的 params：`content` 即模型可见结果文本。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultParams {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// [`ai::HOST_CHAT_EVENT`] 的 params：载荷按 [`ChatEvent`] 分型。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEventParams {
    pub session_id: String,
    pub message_id: String,
    pub event: ChatEvent,
}

/// 对话事件（快照级）：整条消息替换渲染，丢拍由后拍自愈；终态只发一次。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatEvent {
    /// 流式节流拍：该消息当前完整形态（含 steps 行）。
    Message { rev: u64, message: Value },
    /// run 收敛（含中断：`reason` = `stop` 正常收敛 / `interrupted` 用户中断，中断的行已归一）。
    Finished { reason: String },
    /// run 失败收场。
    Failed { error: String },
    /// 话题自动命名完成（`None` = 命名未启用或失败，插件维持原展示）。
    Titled { title: Option<String> },
}

// ---------- table ----------

/// [`table::SNAPSHOT`] 的 result：当前打开表格的只读快照。未打开表格 = `open: false`（其余字段缺省）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableSnapshot {
    pub open: bool,
    /// 表格稳定 id（改名 / 移动后不变；未打开表格时缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// 当前打开表格的仓库相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<TableField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<TableRow>,
    /// 用户当前选区（未打开表格或无选区时缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<TableSelection>,
    /// 其他协作者的当前选区（不含本端；未打开表格或无人时缺省）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_selections: Vec<TableRemoteSelection>,
}

/// 快照里的字段：id / 名称 / 类型 / 选项 / 列宽。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableField {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// 用户拖拽调整后的列宽（px）；缺省 = 宿主按字段名自适应。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
}

/// 快照里的行：单元格值按字段 id 存（缺 key = 空单元格）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRow {
    pub id: String,
    #[serde(default)]
    pub values: serde_json::Map<String, Value>,
    /// 单元格显示样式按字段 id 存（粗体 / 斜体 / 删除线 / 字色 / 底色 / 字号等，形态随宿主
    /// 表格格式模型）。原样透传：宿主不解释内容，插件按需取用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styles: Option<serde_json::Map<String, Value>>,
}

/// 表格选区形态（与宿主表格会话同构）：插件据此把视图对齐到用户正在看的行 / 格。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(rename_all_fields = "camelCase")]
pub enum TableSelection {
    /// 单个单元格。
    Cell { row_id: String, field_id: String },
    /// 矩形区域：`anchor` 为拖选起点，另一组为当前端点。
    Range { anchor_row_id: String, anchor_field_id: String, row_id: String, field_id: String },
    /// 整行。
    Row { row_id: String },
    /// 整列。
    Column { field_id: String },
    /// 全表。
    All,
}

/// 快照里的远端协作者选区：显示名、身份色（`#rrggbb`）与范围。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRemoteSelection {
    pub name: String,
    pub color: String,
    pub selection: TableSelection,
}

/// [`table::UPDATE_CELL`] 的 params：`value` 为单元格新值（`null` = 清空；形态随字段类型）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableUpdateCellParams {
    pub row_id: String,
    pub field_id: String,
    pub value: Value,
}

/// [`table::ADD_ROW`] 的 result：新行 id（宿主分配，插件按它继续引用该行）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableAddRowResult {
    pub row_id: String,
}

/// [`table::REMOVE_ROW`] 与 [`table::SELECT_ROW`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRowIdParams {
    pub row_id: String,
}

/// [`table::SET_CELL_IMAGE`] 的 params：整格替换图片引用列表（`images` 为仓库相对路径，
/// 须先经 `vault.writeBinary` 落盘）。`display` 为显示形态（如 `grid`），缺省保持宿主默认。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSetCellImageParams {
    pub row_id: String,
    pub field_id: String,
    pub images: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

// ---------- tray / collab ----------

/// 托盘菜单条目上限（超出拒绝）。
const MAX_TRAY_ITEMS: usize = 32;
/// 协作通道名长度上限（字符数）。
const MAX_CHANNEL_CHARS: usize = 64;

/// 托盘菜单条目：`id` 插件内唯一、非空，用户点击时原样随 [`tray::HOST_CLICK`] 回发。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayItemSpec {
    pub id: String,
    /// 条目文字（非空）。
    pub label: String,
    /// 置灰不可点（缺省 true）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// [`tray::SET_ITEMS`] 的 params：整发整换本插件的托盘菜单，空清单 = 收起。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTrayItemsParams {
    pub items: Vec<TrayItemSpec>,
}

impl SetTrayItemsParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.items.len() > MAX_TRAY_ITEMS {
            return Err(format!("托盘条目不能超过 {MAX_TRAY_ITEMS} 条"));
        }
        let mut ids = std::collections::HashSet::new();
        for item in &self.items {
            if item.id.trim().is_empty() {
                return Err("托盘条目 id 不能为空".into());
            }
            if !ids.insert(item.id.as_str()) {
                return Err(format!("托盘条目 id 重复：{}", item.id));
            }
            if item.label.trim().is_empty() {
                return Err(format!("托盘条目 {} 的文字不能为空", item.id));
            }
        }
        Ok(())
    }
}

/// [`tray::HOST_CLICK`] 的 params：用户点击的条目 id。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayClickParams {
    pub id: String,
}

/// 同空间的一个在场成员。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabPeer {
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// [`collab::PEERS`] 的 result：`connected` = 当前空间连接可用；未连接时 `selfId` 与 `peers` 缺省。
/// `peers` 不含自己，宿主已剔除。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabPeersResult {
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<CollabPeer>,
}

/// 协作消息投递语义。`reliable` = 广播帧按房间序号缓存、重连后可补投（内容类增量）；
/// `ephemeral` = 不缓存、不补投、不占序号（光标 / 选中这类随时可被新状态覆盖的在场数据）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CollabSendQos {
    #[default]
    Reliable,
    Ephemeral,
}

/// [`collab::SEND`] 的 params：`channel` 为插件自定义的通道名（非空、不含 `:`、≤ 64 字符），
/// 宿主在投递时加插件前缀；`payload` 任意 JSON。`targetPeerId` 取 [`CollabPeersResult`] 里的
/// `peerId` 原值，缺省 = 广播给全部对端。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabSendParams {
    pub channel: String,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_peer_id: Option<String>,
    /// 投递语义，缺省 = `reliable`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos: Option<CollabSendQos>,
}

impl CollabSendParams {
    pub fn validate(&self) -> Result<(), String> {
        let channel = &self.channel;
        if channel.is_empty() {
            return Err("协作通道名不能为空".into());
        }
        // 通道名会原样拼进宿主命名空间，首尾空白不报错只会让收发两端名字对不上
        if channel.trim() != channel {
            return Err("协作通道名首尾不能含空白".into());
        }
        if channel.contains(':') {
            return Err("协作通道名不能含 `:`".into());
        }
        if channel.chars().count() > MAX_CHANNEL_CHARS {
            return Err(format!("协作通道名不能超过 {MAX_CHANNEL_CHARS} 字符"));
        }
        Ok(())
    }
}

/// [`collab::SEND`] 的 result：`queued` = 消息已交本机空间连接发出。
/// 发出不等于送达：广播不确认，单播目标离线时被服务端丢弃，`queued` 仍为 true；
/// 未连接空间时 `queued` = false。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabSendResult {
    pub queued: bool,
}

/// [`collab::HOST_MESSAGE`] 的 params：`channel` 为发送方给的通道名（不含插件前缀），
/// `peerId` / `peerName` 标识发送方。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabMessageParams {
    pub channel: String,
    pub payload: Value,
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_name: Option<String>,
}

/// 团队元数据键的字节上限。
pub const MAX_META_KEY_BYTES: usize = 200;
/// 团队元数据单键值的字节上限（与服务端默认单键上限同口径）。
pub const MAX_META_VALUE_BYTES: usize = 10 * 1024 * 1024;

/// 团队元数据键的合法形态（与服务端同一套分段规则：按 `/` 分段，段内仅字母数字与 `- _ .`，
/// 段不得为空 / 为 `.` / `..` / 以 `.` 开头或结尾；Windows 保留设备名由服务端另行拒绝）。
fn validate_meta_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("键不能为空".to_string());
    }
    if key.len() > MAX_META_KEY_BYTES {
        return Err(format!("键不得超过 {MAX_META_KEY_BYTES} 字节"));
    }
    for segment in key.split('/') {
        if segment.is_empty() {
            return Err(format!("键不得含空段：{key}"));
        }
        if segment == "." || segment == ".." {
            return Err(format!("键含非法段：{key}"));
        }
        if segment.starts_with('.') || segment.ends_with('.') {
            return Err(format!("键段不得以 . 开头或结尾：{key}"));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!("键含非法字符（仅允许字母数字 - _ . /）：{key}"));
        }
    }
    Ok(())
}

/// [`collab::META_GET`] 的 params：`key` 缺省 = 读全部键。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabMetaGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl CollabMetaGetParams {
    pub fn validate(&self) -> Result<(), String> {
        match &self.key {
            Some(key) => validate_meta_key(key),
            None => Ok(()),
        }
    }
}

/// 团队元数据快照：键 → UTF-8 文本值（键按字典序，读全量时输出稳定）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabMetaResult {
    pub values: std::collections::BTreeMap<String, String>,
}

/// 一次按键写入。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabMetaEntry {
    pub key: String,
    pub value: String,
}

/// [`collab::META_PATCH`] 的 params：只写给出的键，未列出的键保持原值。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabMetaPatchParams {
    pub entries: Vec<CollabMetaEntry>,
}

impl CollabMetaPatchParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.is_empty() {
            return Err("至少给出一个键".to_string());
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &self.entries {
            validate_meta_key(&entry.key)?;
            if entry.value.len() > MAX_META_VALUE_BYTES {
                return Err(format!("键 {} 的值不得超过 {MAX_META_VALUE_BYTES} 字节", entry.key));
            }
            if !seen.insert(entry.key.as_str()) {
                return Err(format!("键重复：{}", entry.key));
            }
        }
        Ok(())
    }
}

/// 平台自身的内置插件 id：存储、协作、出网这类基础能力归它，不可停用。
pub const PLATFORM_PLUGIN: &str = "atelyx.platform";

/// 内置能力目录：服务名 → 归属插件 → 它提供的方法（全名，形如 `vault.readText`）。
///
/// 这是「能力只有一种描述方式」的落地：方法名本身就是 `服务.方法`，目录按第一个 `.` 切分即得；
/// `host:` 方法的宿主→插件通知不在其列（那不是插件可调用的能力）。条目引用常量而非字面量，
/// 改名 / 删除常量会直接编译失败，目录不会悄悄漂移。服务随归属插件启停：停用笔记插件后
/// 别的插件调不到 `markdown`，除非另有外部提供者接管。
pub const BUILTIN_SERVICES: &[(&str, &str, &[&str])] = &[
    ("fs", PLATFORM_PLUGIN, &[fs::PRIVATE_DIR, fs::READ_TEXT, fs::WRITE_TEXT, fs::READ_BINARY, fs::WRITE_BINARY, fs::REMOVE, fs::EXISTS, fs::LIST]),
    ("process", PLATFORM_PLUGIN, &[process::SPAWN, process::WRITE_STDIN, process::CLOSE_STDIN, process::KILL]),
    ("vault", PLATFORM_PLUGIN, &[vault::READ_TEXT, vault::WRITE_TEXT, vault::READ_BINARY, vault::WRITE_BINARY, vault::IMPORT_FILE, vault::IMAGE_INFO, vault::LIST, vault::EXISTS, vault::REMOVE, vault::RENAME]),
    ("collab", PLATFORM_PLUGIN, &[collab::PEERS, collab::SEND, collab::META_GET, collab::META_PATCH]),
    ("table", "atelyx.tables", &[table::SNAPSHOT, table::UPDATE_CELL, table::ADD_ROW, table::REMOVE_ROW, table::SELECT_ROW, table::SET_CELL_IMAGE]),
    ("state", PLATFORM_PLUGIN, &[state::READ, state::WRITE]),
    ("notification", PLATFORM_PLUGIN, &[notification::SHOW]),
    ("tray", PLATFORM_PLUGIN, &[tray::SET_ITEMS]),
    ("shortcut", PLATFORM_PLUGIN, &[shortcut::REGISTER, shortcut::UNREGISTER, shortcut::QUERY]),
    ("clipboard", PLATFORM_PLUGIN, &[clipboard::WRITE_TEXT, clipboard::READ_TEXT, clipboard::READ_IMAGE, clipboard::WRITE_IMAGE]),
    ("dialog", PLATFORM_PLUGIN, &[dialog::PICK_FILE, dialog::PICK_DIRECTORY]),
    ("http", PLATFORM_PLUGIN, &[http::REQUEST]),
    // 工具名册与结果回报是宿主记账，不是可由外部提供者接管的能力，故不在目录里
    ("ai", "atelyx.ai", &[ai::CHAT_SEND, ai::CHAT_STOP, ai::CHAT_HISTORY, ai::MODELS, ai::AGENTS, ai::CHAT_RENAME, ai::CHAT_DELETE, ai::CHAT_COMPACT]),
    ("window", PLATFORM_PLUGIN, &[window::OPEN, window::CLOSE, window::UPDATE, window::SHOW, window::BEGIN_DRAG]),
    ("query", PLATFORM_PLUGIN, &[query::MEASURE, query::THEME]),
    ("markdown", "atelyx.notes", &[markdown::PARSE]),
    ("editor", "atelyx.notes", &[editor::SELECTION, editor::REPLACE_SELECTION]),
    ("history", "atelyx.history", &[history::ACTIVITY, history::DAY, history::OPEN]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_and_surface_kind_serde_uses_camel_case() {
        assert_eq!(serde_json::to_value(ContributionSlot::Panel).unwrap(), "panel");
        assert_eq!(serde_json::to_value(ContributionSlot::TableView).unwrap(), "tableView");
        assert_eq!(serde_json::to_value(ContributionSlot::Settings).unwrap(), "settings");
        assert_eq!(serde_json::to_value(SurfaceKind::Settings).unwrap(), "settings");
        assert_eq!(serde_json::to_value(SurfaceKind::TableView).unwrap(), "tableView");
        assert_eq!(serde_json::to_value(TakeoverScope::Page).unwrap(), "page");
        assert_eq!(serde_json::to_value(TakeoverScope::Full).unwrap(), "full");
        let kind: SurfaceKind = serde_json::from_value(serde_json::json!("settings")).unwrap();
        assert_eq!(kind, SurfaceKind::Settings);
        let slot: ContributionSlot = serde_json::from_value(serde_json::json!("settings")).unwrap();
        assert_eq!(slot, ContributionSlot::Settings);
        let scope: TakeoverScope = serde_json::from_value(serde_json::json!("full")).unwrap();
        assert_eq!(scope, TakeoverScope::Full);
        // table 面：字段名与类型字段走 camelCase / `type` 原名，缺省行按空值集
        let snapshot: TableSnapshot = serde_json::from_value(serde_json::json!({
            "open": true,
            "path": "a.atb",
            "fields": [{ "id": "f1", "name": "名称", "type": "text", "options": ["甲", "乙"] }],
            "rows": [{ "id": "r1", "values": { "f1": "甲" } }]
        }))
        .unwrap();
        assert!(snapshot.open);
        assert_eq!(snapshot.fields[0].field_type, "text");
        assert_eq!(snapshot.fields[0].options, vec!["甲", "乙"]);
        assert_eq!(snapshot.rows[0].values["f1"], "甲");
        let closed: TableSnapshot = serde_json::from_value(serde_json::json!({ "open": false })).unwrap();
        assert_eq!(closed, TableSnapshot::default(), "未打开表格 = 全缺省");
        // ai 面：事件 tagged 形态与 send 缺省 sessionId
        assert_eq!(
            serde_json::to_value(ChatEvent::Finished { reason: "stop".into() }).unwrap(),
            serde_json::json!({ "kind": "finished", "reason": "stop" })
        );
        let send: ChatSendParams = serde_json::from_value(serde_json::json!({ "text": "hi" })).unwrap();
        assert_eq!(send.session_id, None);
        assert_eq!(send.agent_id, None);
        assert_eq!(send.model, None);
        assert_eq!(send.provider_id, None);
        assert!(send.images.is_empty());
    }

    #[test]
    fn table_selection_variants_and_clipboard_image_shapes() {
        let cases = [
            (
                TableSelection::Cell { row_id: "r1".into(), field_id: "f1".into() },
                serde_json::json!({ "kind": "cell", "rowId": "r1", "fieldId": "f1" }),
            ),
            (
                TableSelection::Row { row_id: "r1".into() },
                serde_json::json!({ "kind": "row", "rowId": "r1" }),
            ),
            (
                TableSelection::Column { field_id: "f1".into() },
                serde_json::json!({ "kind": "column", "fieldId": "f1" }),
            ),
            (TableSelection::All, serde_json::json!({ "kind": "all" })),
        ];
        for (selection, wire) in cases {
            assert_eq!(serde_json::to_value(&selection).unwrap(), wire);
            assert_eq!(serde_json::from_value::<TableSelection>(wire).unwrap(), selection);
        }

        let image = ClipboardImageResult {
            image: Some(ClipboardImage { base64: "AA==".into(), width: 1, height: 1 }),
        };
        let wire = serde_json::to_value(&image).unwrap();
        assert_eq!(wire["image"]["width"], 1);
        assert_eq!(serde_json::to_value(ClipboardImageResult { image: None }).unwrap(), serde_json::json!({}));
    }

    #[test]
    fn tray_items_validate_ids_labels_and_capacity() {
        let ok = SetTrayItemsParams {
            items: vec![TrayItemSpec {
                id: "open".into(),
                label: "打开".into(),
                enabled: None,
            }],
        };
        assert!(ok.validate().is_ok());
        let dup = SetTrayItemsParams {
            items: vec![
                TrayItemSpec { id: "a".into(), label: "甲".into(), enabled: None },
                TrayItemSpec { id: "a".into(), label: "乙".into(), enabled: Some(false) },
            ],
        };
        assert!(dup.validate().unwrap_err().contains("重复"));
        let blank = SetTrayItemsParams {
            items: vec![TrayItemSpec { id: "a".into(), label: " ".into(), enabled: None }],
        };
        assert!(blank.validate().unwrap_err().contains("文字"));
        let many = SetTrayItemsParams {
            items: (0..=MAX_TRAY_ITEMS)
                .map(|i| TrayItemSpec { id: format!("i{i}"), label: format!("条目 {i}"), enabled: None })
                .collect(),
        };
        assert!(many.validate().unwrap_err().contains("不能超过"));
        // 空清单合法（收起菜单）
        assert!(SetTrayItemsParams { items: vec![] }.validate().is_ok());
    }

    #[test]
    fn collab_send_channel_rules_and_peers_shape() {
        let ok = CollabSendParams {
            channel: "cursor".into(),
            payload: serde_json::json!({ "line": 3 }),
            target_peer_id: None,
            qos: None,
        };
        assert!(ok.validate().is_ok());
        for bad in ["", "  ", "a:b", &"x".repeat(MAX_CHANNEL_CHARS + 1)] {
            let params = CollabSendParams {
                channel: bad.to_string(),
                payload: serde_json::Value::Null,
                target_peer_id: None,
                qos: None,
            };
            assert!(params.validate().is_err(), "应拒绝通道：{bad}");
        }
        // 投递档位缺省不落线（旧插件帧形状不变），显式档位按小驼峰编码
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            serde_json::json!({ "channel": "cursor", "payload": { "line": 3 } })
        );
        let ephemeral = CollabSendParams {
            channel: "cursor".into(),
            payload: serde_json::json!({ "line": 3 }),
            target_peer_id: None,
            qos: Some(CollabSendQos::Ephemeral),
        };
        assert_eq!(serde_json::to_value(&ephemeral).unwrap()["qos"], "ephemeral");
        assert_eq!(
            serde_json::from_value::<CollabSendParams>(
                serde_json::json!({ "channel": "c", "payload": {}, "qos": "reliable" })
            )
            .unwrap()
            .qos,
            Some(CollabSendQos::Reliable)
        );
        let peers = CollabPeersResult {
            connected: true,
            self_id: Some("p1".into()),
            peers: vec![CollabPeer { peer_id: "p2".into(), name: Some("队友".into()) }],
        };
        let wire = serde_json::to_value(&peers).unwrap();
        assert_eq!(wire["selfId"], "p1");
        assert_eq!(wire["peers"][0]["peerId"], "p2");
        assert_eq!(
            serde_json::to_value(CollabPeersResult::default()).unwrap(),
            serde_json::json!({ "connected": false })
        );
    }

    #[test]
    fn calendar_params_validate_shape_and_defaults() {
        let ok = CalendarAddParams {
            date: "2026-10-30".into(),
            title: "会议".into(),
            note: None,
            color: None,
        };
        assert!(ok.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            serde_json::json!({ "date": "2026-10-30", "title": "会议" })
        );
        for date in ["2026-1-1", "2026-13-01", "2026-10-32", " 2026-10-30"] {
            let bad = CalendarAddParams { date: date.into(), ..ok.clone() };
            assert!(bad.validate().is_err(), "应拒绝日期：{date}");
        }
        let blank = CalendarAddParams { title: "  ".into(), ..ok.clone() };
        assert!(blank.validate().is_err());
        let long = CalendarAddParams {
            title: "字".repeat(MAX_CALENDAR_TITLE_CHARS + 1),
            ..ok
        };
        assert!(long.validate().is_err());

        // 未给出的字段不落线（更新语义：缺省 = 保持原值）
        let update = CalendarUpdateParams {
            id: "ev-1".into(),
            date: None,
            title: Some("改".into()),
            note: None,
            color: None,
        };
        assert!(update.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({ "id": "ev-1", "title": "改" })
        );
        assert!(CalendarUpdateParams { id: String::new(), ..update }.validate().is_err());

        assert!(CalendarIdParams { id: "ev-1".into() }.validate().is_ok());
        assert!(CalendarIdParams { id: String::new() }.validate().is_err());
    }

    #[test]
    fn collab_meta_keys_follow_the_server_shape() {
        assert!(CollabMetaGetParams { key: None }.validate().is_ok());
        for good in ["calendar", "app/board", "a-b_c.d", "Team1/notes"] {
            assert!(
                CollabMetaGetParams { key: Some(good.into()) }.validate().is_ok(),
                "应接受键：{good}"
            );
        }
        for bad in [
            "",
            "a//b",
            ".hidden",
            "a/./b",
            "a/..",
            "key.",
            "带中文",
            "a:b",
            &"k".repeat(MAX_META_KEY_BYTES + 1),
        ] {
            assert!(
                CollabMetaGetParams { key: Some(bad.into()) }.validate().is_err(),
                "应拒绝键：{bad}"
            );
        }
        let ok = CollabMetaPatchParams {
            entries: vec![CollabMetaEntry { key: "calendar".into(), value: "{}".into() }],
        };
        assert!(ok.validate().is_ok());
        assert!(CollabMetaPatchParams { entries: vec![] }.validate().is_err());
        let dup = CollabMetaPatchParams {
            entries: vec![
                CollabMetaEntry { key: "a".into(), value: "1".into() },
                CollabMetaEntry { key: "a".into(), value: "2".into() },
            ],
        };
        assert!(dup.validate().unwrap_err().contains("重复"));
    }

    #[test]
    fn history_day_and_open_validate_shape() {
        assert!(HistoryDayParams { day: "2026-10-30".into() }.validate().is_ok());
        assert!(HistoryDayParams { day: "2026-10-3".into() }.validate().is_err());
        let open = HistoryOpenParams { kind: "note".into(), file: "notes/a.md".into() };
        assert!(open.validate().is_ok());
        assert!(HistoryOpenParams { kind: "pdf".into(), ..open.clone() }.validate().is_err());
        assert!(HistoryOpenParams { file: " ".into(), ..open }.validate().is_err());
        let entry = HistoryDayEntry {
            kind: "note".into(),
            file: "notes/a.md".into(),
            ts: 1_700_000_000_000,
            action: "edit".into(),
            author_name: "作者".into(),
            summary: None,
        };
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            serde_json::json!({
                "kind": "note",
                "file": "notes/a.md",
                "ts": 1_700_000_000_000_i64,
                "action": "edit",
                "authorName": "作者"
            }),
            "缺省摘要不落线"
        );
    }

    #[test]
    fn theme_result_carries_event_colors() {
        let theme = ThemeResult {
            event_colors: vec!["#112233".into()],
            ..ThemeResult::default()
        };
        let wire = serde_json::to_value(&theme).unwrap();
        assert_eq!(wire["eventColors"], serde_json::json!(["#112233"]));
        let mut legacy = serde_json::to_value(ThemeResult::default()).unwrap();
        legacy.as_object_mut().unwrap().remove("eventColors");
        let old: ThemeResult = serde_json::from_value(legacy).unwrap();
        assert!(old.event_colors.is_empty(), "旧宿主不带色板时按空清单");
    }
}
