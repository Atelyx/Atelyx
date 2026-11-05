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
    /// 插件运行时改一个已打开窗口的属性（尺寸 / 置顶 / 标题；「不进任务栏」只能在开窗时声明）。
    pub const UPDATE: &str = "window.update";
    /// 插件最小化自己打开的一个窗口。
    pub const MINIMIZE: &str = "window.minimize";
    /// 插件切换自己打开窗口的最大化态。
    pub const TOGGLE_MAXIMIZE: &str = "window.toggleMaximize";
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
    /// 系统下载目录的绝对路径（定位失败回落用户主目录）：图片 / 附件下载落盘用。
    pub const DOWNLOADS_DIR: &str = "fs.downloadsDir";
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
    /// 全仓库标签词表（宿主缓存的快照，按出现次数降序、最多 1000 条）：属性标签值候选这类
    /// 需要「仓库里已有哪些标签」的能力。刚写入的新标签要等宿主下次重建词表才出现。
    pub const TAGS: &str = "vault.tags";
    /// 删除一个仓库相对路径（文件或文件夹；文件夹连同内容递归删除）。仓库根不可删。
    /// 被编辑面打开或被当前画布节点引用的文件拒删。
    pub const REMOVE: &str = "vault.remove";
    /// 把一个仓库相对路径改名 / 移动到另一路径（文件或文件夹；目标不存在，父目录自动创建）。
    /// 引用同步：`.md` 内部链接与画布文件引用随改；源文件被编辑面打开或被当前画布节点引用时拒绝。
    pub const RENAME: &str = "vault.rename";
    /// 引用某篇文档的其它文档（反链；按 wiki 链接索引查询）。
    pub const BACKLINKS: &str = "vault.backlinks";
    /// 当前仓库可检索文件清单（按宿主文件树的树序；本地仓库与协作空间同构）。
    /// 与 [`LIST`] 的区别：一次给全仓库文件、带行图标与可打开判定，供按名检索类界面用。
    pub const FILES: &str = "vault.files";
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

/// 协作空间状态与成员名册：房间这类需要「当前是不是空间、谁在线、成员有哪些」的界面用。
/// 名册读宿主缓存（与宿主成员管理弹层共用），缓存为空时宿主顺带发起一次拉取。
pub mod space {
    /// 当前容器与在线成员快照（含本端身份与各端打开的文件）。给房间这类展示界面用；
    /// 消息投递用的对端 id / 名字在 `collab.peers`。
    pub const ROOM: &str = "space.room";
    /// 空间成员名册快照（列表 / 加载中 / 错误）。
    pub const MEMBERS: &str = "space.members";
    /// 打开宿主的「协作空间」菜单（登录与空间列表入口）。
    pub const OPEN_MENU: &str = "space.openMenu";
    /// 打开宿主的「协作设置」页。
    pub const OPEN_SETTINGS: &str = "space.openSettings";
    /// 提交一次文本文件的补丁保存（空间内容真源）：宿主按 `kind` 把「基线 → 冻结快照」的实体差异
    /// 交服务端仲裁，回执带服务端最终路径（补丁带标题时服务端按标题改名）。
    pub const FILE_PATCH: &str = "space.file.patch";
}

/// 表格面：读写当前打开表格。方法由表格插件提供（模型与保存链都在该插件内），其它插件
/// （表格视图贡献）经这些方法读数据、写改动；改动与插件自身编辑同一出口（同撤销栈、同保存链、
/// 同协作广播）。
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

/// 画布面：读取当前打开画布的结构化快照（选中卡属性 / 画布统计）。方法由画布插件提供，
/// 属性面板等第三方视图经它与画布数据同源。
pub mod canvas {
    /// 读当前打开画布的只读快照；未打开画布 = `path: None` 与全零统计。
    pub const SNAPSHOT: &str = "canvas.snapshot";
}

/// 面板头部：插件声明自己面板顶部那一行工具条的内容，宿主按当前主题渲染（状态胶囊 / 错误文本 /
/// 协作者胶囊 / 右侧文本 / 菜单项 / 视图下拉）。头部数据全部由插件提供，宿主只负责画与转发点击。
pub mod panel {
    /// 声明某个面板视图的头部状态；`header` 缺省 = 收起头部。
    pub const HEADER: &str = "panel.header";
    /// 回应 [`crate::event::name::PANEL_FLUSH`]：`ok: false` 时带可读原因。
    pub const FLUSH_DONE: &str = "panel.flushDone";
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
    /// 打开一条流式出网连接：宿主建连后按块下发 [`HOST_STREAM_CHUNK`]，结束 / 失败经
    /// [`HOST_STREAM_END`]。地址按供应商口径校验（公网目标拒内网，本机 / 局域网服务放行）；
    /// [`REQUEST`] 是不过地址校验的便利层。
    pub const STREAM_OPEN: &str = "http.stream.open";
    /// 取消一条仍在进行的流式连接（幂等：已结束的流也成功）。
    pub const STREAM_CANCEL: &str = "http.stream.cancel";
    /// 流式分片的原始字节（base64；通知）。
    pub const HOST_STREAM_CHUNK: &str = "host:http.stream.chunk";
    /// 流式连接结束（正常 / 失败 / 取消；通知）。
    pub const HOST_STREAM_END: &str = "host:http.stream.end";
}

/// 会话真源读写（`chat.session.*`）：宿主保持消息形状与写盘链，插件提交声明式批次。
pub mod chat {
    /// 读单个会话（元数据 + 消息）。
    pub const SESSION_READ: &str = "chat.session.read";
    /// 提交一个批次（创建 / 元数据 / 追加 / 补丁 / 截断 / 删除 / 覆盖项）。
    pub const SESSION_WRITE: &str = "chat.session.write";
    /// 读全仓库快照（会话清单 + 覆盖层 + 落盘失败）。
    pub const SESSION_SNAPSHOT: &str = "chat.session.snapshot";
}

pub mod doc {
    /// 打开一个文档会话：宿主读盘、建立会话（本地与协作空间同构），回执即当前正文与可写性。
    pub const OPEN: &str = "doc.open";
    /// 提交一次本地编辑（完整正文 + 光标 / 选区，字节偏移）。内容有变化时宿主置脏并接入
    /// 协作增量；无变化时只更新在场选区。
    pub const EDIT: &str = "doc.edit";
    /// 请求一次保存（手动 / 自动）。回执只表示已受理，落盘结果经 `doc:changed` 到达。
    pub const SAVE: &str = "doc.save";
    /// 关闭当前文档会话（换文件 / 关标签 / 切仓库）。
    pub const CLOSE: &str = "doc.close";
    /// 上报本端光标 / 选区（全文字节偏移；`None` = 无选区）。只对协作空间生效。
    pub const AWARENESS: &str = "doc.awareness";
    /// 把当前文档改名为新的主名（同目录、保留扩展名；引用与历史随宿主既有改名链）。
    /// 有未落盘修改时拒绝，插件须先 `doc.save`。
    pub const RENAME: &str = "doc.rename";
    /// 读当前文档的 frontmatter 块原文与可写性（属性面板）。
    pub const PROPS_READ: &str = "doc.props.read";
    /// 用新 frontmatter 块替换当前文档的属性区：正文原样保留，走文档同一条保存链。
    pub const PROPS_WRITE: &str = "doc.props.write";
    /// 用户打开了插件注册的领域文档类型文件（通知）。`surfaceId` 为宿主分配的实例面标识，
    /// 插件据此为该面提交整面帧。
    pub const HOST_OPEN: &str = "host:doc.open";
    /// 文档实例已关闭 / 已切换走（通知）。插件此后不得再向该 surfaceId 提交帧。
    pub const HOST_CLOSE: &str = "host:doc.close";
}

/// 面帧：整个面交给插件自绘，一帧即当前完整界面。
pub mod frame {
    /// 宿主回发原始输入（通知）：指针 / 滚轮 / 按键 / 输入法 / 焦点 / 尺寸。
    pub const HOST_INPUT: &str = "host:frame.input";
    /// 宿主按面声明的帧率发帧拍（通知）：插件收到后提交该帧的内容。
    pub const HOST_TICK: &str = "host:frame.tick";
}

/// 纯查询：不改界面、不产生帧。
pub mod query {
    /// 批量量算文字占位尺寸（排版按真实字宽摆放，不能按字符数估）。
    pub const MEASURE: &str = "query.measure";
    /// 系统已安装字体族清单（排序后的族名；单元格样式里的字体选择用）。
    pub const FONTS: &str = "query.fonts";
    /// 取宿主主题表（颜色 / 字号档 / 圆角档），插件据此与宿主观感一致。
    pub const THEME: &str = "query.theme";
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
    /// 全仓库按文件分组的版本聚合（组按最近版本时间新→旧，只给组与版本条数）。
    pub const GROUPS: &str = "history.groups";
    /// 在宿主界面打开某个文件的历史弹层（版本列表与回滚由宿主承载）。
    pub const OPEN: &str = "history.open";
    /// 域插件落盘后记一版（本地记侧文件；与上一版内容相同则不新增）。
    pub const RECORD: &str = "history.record";
}

/// 最近打开：宿主按仓库容器身份记录的打开清单（记录与失效剔除都在宿主，插件只读）。
pub mod recent {
    /// 服务名。
    pub const SERVICE: &str = "recent";
    /// 当前容器的最近打开条目（按打开时刻新→旧）。
    pub const LIST: &str = "recent.list";
}

/// 编辑器：当前打开笔记的选区读写（宿主随笔记会话自持，其他插件据此做「改写选中内容」这类动作）。
/// 只在编辑状态有意义；阅读态选区不属于编辑器能力。
pub mod editor {
    /// 读当前文档路径与选区（无选区时 `from == to`、`text` 为空）。
    pub const SELECTION: &str = "editor.selection";
    /// 用给定文本替换当前选区（空串 = 删除选区）；写入经笔记同一套保存链。
    pub const REPLACE_SELECTION: &str = "editor.replaceSelection";
    /// 读当前打开的文件（仓库相对路径，按打开顺序）。候选列表这类要「打开中的优先」的界面用。
    pub const OPEN_FILES: &str = "editor.openFiles";
    /// 让宿主打开一个仓库内文件（按扩展名分流；回执 = 已受理）。`reveal` 见
    /// [`EditorOpenFileParams`]。
    pub const OPEN_FILE: &str = "editor.openFile";
    /// 读全局编辑偏好（软换行 / 页面内标题 / 正文宽度）；变更时宿主另发
    /// [`crate::event::name::EDITOR_PREFS_CHANGED`]。
    pub const PREFS: &str = "editor.prefs";
}

/// 仓库界面状态（随仓库走的界面数据）：展开集 / 排序 / 目录颜色 / 提示词标记这类
/// 「宿主与插件都读、重启后要保留」的键值，落仓库配置与对应侧文件，不落插件私有目录。
pub mod vault_ui {
    /// 读一组仓库界面状态：缺省键回落到宿主默认值。
    pub const READ: &str = "vault.ui";
    /// 按键写仓库界面状态：只改给出的键（`null` 删除该键），返回写后的完整快照。
    pub const WRITE: &str = "vault.updateUi";
    /// 新建一个条目（笔记 / 表格 / 画布 / 文件夹 / 插件文档类型）：宿主起空内容并落盘。
    pub const CREATE: &str = "vault.create";
    /// 切换当前容器（本地仓库或协作空间）。
    pub const SWITCH_CONTAINER: &str = "vault.switchContainer";
    /// 从容器列表移除一个容器（只移出最近列表，不动磁盘内容与服务端数据）。
    pub const REMOVE_CONTAINER: &str = "vault.removeContainer";
    /// 列出全仓库的目录（仓库相对路径；「展开全部」这类需要整棵骨架的入口用）。
    pub const LIST_DIRS: &str = "vault.listDirs";
    /// 让宿主重读文件树（磁盘无监听，外部改动只能靠刷新感知）。
    pub const REFRESH_TREE: &str = "vault.refreshTree";
}

/// [`vault_ui::LIST_DIRS`] 的回执：全部目录的仓库相对路径（不含仓库根）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirListResult {
    pub dirs: Vec<String>,
}

/// 宿主自有的系统单例界面与容器入口：插件只发起，实际动作与结果回填都在壳。
pub mod shell {
    /// 打开宿主的取色器（HSV 拖动预览 / 松手提交），回执 = 选中色（取消时缺省）。
    pub const PICK_COLOR: &str = "shell.pickColor";
    /// 让宿主选一个文件夹并作为仓库打开（选择器与进入仓库链都在壳）。
    pub const PICK_VAULT: &str = "shell.pickVault";
    /// 在系统文件管理器中显示一个仓库内条目（空路径 = 仓库根；协作空间没有本机路径，宿主拒绝）。
    pub const REVEAL: &str = "shell.reveal";
    /// 取一个仓库内条目的绝对路径（空路径 = 仓库根；协作空间没有本机路径，宿主拒绝）。
    pub const ABSOLUTE_PATH: &str = "shell.absolutePath";
    /// 在同目录创建副本（同名自动加序号；.atlx / .atb 连同内部 id 重建），回执 = 落定的新路径。
    pub const DUPLICATE: &str = "shell.duplicate";
    /// 用系统默认浏览器打开一个外链 URL（只接受 http(s) / mailto，其余由宿主拒绝）。
    pub const OPEN_URL: &str = "shell.openUrl";
}

/// 文件拖拽会话：ghost、落点判定与悬停自动展开都在宿主，插件只发起 / 更新落点 / 结束。
pub mod drag {
    /// 发起一次拖拽会话（同一插件同一时刻至多一个；重复发起以最新的为准）。
    pub const START: &str = "drag.start";
    /// 插件面在指针移动时上报本面内的落点候选（命中区 id 由插件给出）。
    pub const HOVER: &str = "drag.hover";
    /// 结束拖拽会话（释放即落位；取消则不落位）。
    pub const END: &str = "drag.end";
}

/// 宿主弹层菜单：插件声明条目（可带子面板与只读文字行），宿主按统一外观弹出并回传选中项。
pub mod menu {
    /// 打开一个宿主菜单，锚点为当前指针位置。关闭时经 [`HOST_CLOSED`] 回传原因。
    pub const OPEN: &str = "menu.open";
    /// 宿主关闭了菜单（选中某项 / 点击外点 / Esc）；`selected` 缺省 = 未选中任何项。
    pub const HOST_CLOSED: &str = "host:menu.closed";
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
    /// 让模型重新生成会话标题（结果写回元数据，并经 [`HOST_CHAT_EVENT`] 发 `Titled`）。
    pub const CHAT_TITLE: &str = "ai.chat.title";
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
    /// 列当前可用的工具声明（内置 + 已注册插件工具；按调用方给的工具 id 过滤）。
    pub const TOOL_LIST: &str = "ai.tool.list";
    /// 执行一次工具调用：内置工具由宿主执行，插件工具转发给提供者。结果即模型可见文本。
    pub const TOOL_EXECUTE: &str = "ai.tool.execute";
    /// 插件向宿主发布一条对话事件；宿主点对点发给订阅了 [`HOST_CHAT_EVENT`] 的插件，
    /// 并经无界通道交给壳侧。
    pub const CHAT_EMIT: &str = "ai.chat.emit";
    /// 起一轮直写型 run：消息真源归调用方（画布卡），宿主解析模型配置后交给 AI 核心插件。
    /// 事件经 [`HOST_CHAT_EVENT`] 以 `sessionId` 归属，调用方据此把消息写回自己的容器。
    pub const CHAT_SEND_DIRECT: &str = "ai.chat.sendDirect";
    /// 压缩调用方给出的一段消息（真源不在会话语料里）：只回摘要，调用方自行落存储。
    pub const CHAT_COMPACT_DIRECT: &str = "ai.chat.compactDirect";
    /// 读与 AI 运行相关的仓库设置（落卡开关等）；插件据此决定本地派生行为。
    pub const SETTINGS: &str = "ai.settings";
}

/// 宿主与 AI 核心插件之间的私有服务（`ai.core`）：只由宿主调用，不登记进能力目录。
pub mod run {
    pub const SERVICE: &str = "ai.core";
    /// 起一轮存储型 run（会话由插件落真源）。
    pub const START: &str = "run.start";
    /// 起一轮直写型 run（含画布卡：真源归调用方，插件不写会话）。
    pub const START_DIRECT: &str = "run.startDirect";
    /// 中断一个会话 / 卡片的活跃 run（幂等）。
    pub const STOP: &str = "run.stop";
    /// 压缩一个会话（把较早消息总结为压缩注解）。
    pub const COMPACT: &str = "run.compact";
    /// 压缩一段调用方给出的消息（真源不在会话语料里，如画布卡）：只回摘要，不写任何记录。
    pub const COMPACT_DIRECT: &str = "run.compactDirect";
    /// 重新生成会话标题（AI 命名）。
    pub const TITLE: &str = "run.title";
    /// 中断全部活跃 run（换仓 / 切空间）。
    pub const STOP_ALL: &str = "run.stopAll";
}

/// AI 核心插件的固定 id：宿主把 run 编排交给它，并只接受它发布的对话事件。
pub const AI_CORE_PLUGIN: &str = "atelyx.ai-core";

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

/// 整面帧提交（`submit_frame`）的回执：`applied` = 宿主已接受并生效。
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
    /// 缺省 / 0 = 贴边。
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
    /// 改窗口标题：宿主自绘标题栏上的文字立即更新；系统边框窗口的 OS 标题由系统绘制，
    /// 运行期不随之变化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    /// 仅 `decorations: false` 时生效：缺省 true = 宿主画标题栏；false = 不画，插件自绘标题栏
    /// （拖动走 [`window::BEGIN_DRAG`]，窗口控制走 [`window::MINIMIZE`] / [`window::TOGGLE_MAXIMIZE`]
    /// / [`window::CLOSE`]）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub titlebar: Option<bool>,
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

/// [`fs::DOWNLOADS_DIR`] 的 result：系统下载目录的绝对路径。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadsDirResult {
    pub path: String,
}

/// [`query::FONTS`] 的 result：系统已安装字体族（去重排序；宿主启动时扫描一次）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontFamiliesResult {
    pub families: Vec<String>,
}

/// [`query::MEASURE`] 的 result：占位宽高（逻辑像素）与换行后的行数（单行量算恒为 1）。
/// `segments` 在条目声明 `segments` 时给出逐行文本与宽度，供调用方自行逐行排版与绘制。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureTextResult {
    pub width: f32,
    pub height: f32,
    pub lines: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<MeasureSegment>>,
}

/// 量算结果的单行：`text` 是该行的源文本切片，`width` 是实测行宽。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasureSegment {
    pub text: String,
    pub width: f32,
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
    /// 显式字体族名（与绘制指令同口径；缺省 = 按 `family` 取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_width: Option<f32>,
    /// 排版口径（行高倍数 / 换行 / 整形）：与绘制指令的 `layout` 同口径，量算与绘制才对得上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<crate::paint::TextLayout>,
    /// 要逐行行段（结果带 `segments`）：逐行排版 / 逐行绘制的调用方用。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub segments: bool,
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
        if let Some(layout) = &self.layout {
            if let Some(line_height) = layout.line_height {
                if !line_height.is_finite() || line_height <= 0.0 {
                    return Err("行高倍数必须大于 0".into());
                }
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
    pub background: String,
    pub border_subtle: String,
    pub selection: String,
    pub scrim: String,
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

/// [`editor::OPEN_FILES`] 的 result：当前打开的文件（仓库相对路径，按打开顺序）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorOpenFiles {
    #[serde(default)]
    pub paths: Vec<String>,
}

/// [`editor::OPEN_FILE`] 的 params：`path` 为仓库相对路径。
/// `reveal` 缺省 = 打开并让对应视图显示（布局里没有就新增该视图标签）；
/// `false` = 只打开不改布局（列表类入口用，已有对应视图照常显示当前文件）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorOpenFileParams {
    pub path: String,
    #[serde(default = "reveal_default")]
    pub reveal: bool,
}

fn reveal_default() -> bool {
    true
}

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

/// [`editor::PREFS`] 的 result：全局编辑偏好。`content_width` = 正文列最大宽度
/// （0 = 不限，铺满可用宽度）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorPrefs {
    /// 宽松换行：开 = 段内单个换行保留为换行；关 = 折叠为空格。
    #[serde(default)]
    pub soft_wrap: bool,
    /// 正文列顶部显示文件名（点击就地改名）。
    #[serde(default)]
    pub in_page_title: bool,
    pub content_width: u32,
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

// ---------- 文档会话 ----------

/// 单次提交的文档正文上限（超限按可读错误拒绝）。
pub const MAX_DOC_TEXT_BYTES: usize = 8 * 1024 * 1024;

/// [`doc::OPEN`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocOpenParams {
    /// 仓库相对路径（本地仓库）/ 服务端内容路径（协作空间），与 `vault` 口径一致。
    pub path: String,
}

impl DocOpenParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.trim().is_empty() {
            return Err("路径不能为空".to_string());
        }
        Ok(())
    }
}

/// [`doc::OPEN`] 的 result：当前正文与是否可写（viewer 只读）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocOpenResult {
    pub path: String,
    pub text: String,
    pub can_write: bool,
    /// 宿主当前是否有未落盘修改（插件重启后据此恢复「未保存」状态）。
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_error: Option<String>,
}

/// [`doc::EDIT`] 的 params：完整正文 + 本端光标与选区锚点（全文字节偏移；
/// `anchor` / `head` 缺省 = 无选区）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocEditParams {
    pub path: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<usize>,
}

impl DocEditParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.trim().is_empty() {
            return Err("路径不能为空".to_string());
        }
        if self.text.len() > MAX_DOC_TEXT_BYTES {
            return Err(format!("文档正文不得超过 {MAX_DOC_TEXT_BYTES} 字节"));
        }
        Ok(())
    }
}

/// [`doc::RENAME`] 的 params：当前路径与新主名（不含扩展名）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocRenameParams {
    pub path: String,
    pub name: String,
}

/// [`doc::PROPS_READ`] 的结果：当前文档的属性区与可写性。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocPropsResult {
    /// 当前文档路径；没有打开文档时为空串。
    pub path: String,
    /// 读取时正文的内容指纹；写回须原样回传，正文已变即被宿主拒绝。
    pub fingerprint: String,
    /// frontmatter 块原文（含首尾 `---` 定界）；文档没有属性块时为空串。
    pub frontmatter: String,
    /// 文档是否有属性区（含未闭合的 `---` 起始行）。
    #[serde(default)]
    pub present: bool,
    /// 属性区 YAML 是否可解析（未闭合或语法错误为 `false`，面板锁编辑）。
    pub parsed: bool,
    /// 当前文档是否可写（只读身份 / 未打开文档为 `false`）。
    pub editable: bool,
}

/// [`doc::PROPS_WRITE`] 的入参：新的 frontmatter 块原文（含 `---` 定界；空串 = 删除属性区）。
/// `path` + `fingerprint` 来自读属性回执；与当前文档不符时宿主拒绝写回。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocPropsWriteParams {
    pub frontmatter: String,
    pub path: String,
    pub fingerprint: String,
}

impl DocRenameParams {
    pub fn validate(&self) -> Result<(), String> {
        if self.path.trim().is_empty() {
            return Err("路径不能为空".to_string());
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err("文件名不能为空".to_string());
        }
        if name.contains(['/', '\\', ':']) {
            return Err("文件名不能包含路径分隔符".to_string());
        }
        Ok(())
    }
}

/// [`doc::SAVE`] 的 params：`manual` = 手动保存（成功 / 失败均有提示）；缺省 = 自动保存。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocSaveParams {
    pub path: String,
    #[serde(default)]
    pub manual: bool,
}

/// [`doc::AWARENESS`] 的 params：本端选区（全文字节偏移，`None` = 只上报身份）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocAwarenessParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<usize>,
}

/// 一条反链：引用该文档的文件路径与显示标题。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinkEntry {
    pub path: String,
    pub title: String,
}

/// [`vault::BACKLINKS`] 的 result。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacklinksResult {
    pub entries: Vec<BacklinkEntry>,
}

/// [`vault::TAGS`] 的 result：仓库里出现过的标签（按出现次数降序）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultTagsResult {
    pub tags: Vec<String>,
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

/// [`history::GROUPS`] 的 params：`refresh` = 丢弃宿主聚合缓存后重扫（手动刷新 / 重试用）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryGroupsParams {
    pub refresh: bool,
}

/// 一个文件的历史分组：`kind` 取 [`HISTORY_KINDS`] 的取值域，`count` = 版本条数。
/// 版本明细不在聚合里给出——浏览与回滚走 [`history::OPEN`] 的宿主弹层。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFileGroup {
    pub kind: String,
    pub file: String,
    pub count: u32,
}

/// [`history::GROUPS`] 的 result：`ready` = 当前容器可用（false 时 `groups` 为空）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryGroupsResult {
    pub ready: bool,
    pub groups: Vec<HistoryFileGroup>,
}

/// [`HistoryRecordParams`] 的 kind 与 action 取值域见 [`HISTORY_KINDS`]。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecordParams {
    /// [`HISTORY_KINDS`] 的取值。
    pub kind: String,
    /// 仓库相对路径。
    pub file: String,
    /// 该版本的全文内容。
    pub content: String,
    /// `edit` / `restore`，缺省 `edit`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// 编辑合并窗口（毫秒）：同窗口内的连续编辑并成一版。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ---------- recent ----------

/// 最近打开条目的文件类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecentKind {
    Note,
    Table,
    Canvas,
}

/// 一条最近打开记录：仓库相对路径 + 类型 + 打开时刻（unix 秒）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    pub file: String,
    pub kind: RecentKind,
    pub opened_at: i64,
}

/// [`recent::LIST`] 的 result：`ready` = 当前容器可用（false 时 `entries` 为空）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentListResult {
    pub ready: bool,
    pub entries: Vec<RecentEntry>,
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

// ---------- 仓库界面状态 / 拖拽 / 菜单 ----------

/// 展开集上限（条目数）：防止插件把无限增长的路径塞进仓库配置。
pub const MAX_EXPANDED_ENTRIES: usize = 4096;
/// 目录颜色条目上限。
pub const MAX_FOLDER_COLORS: usize = 4096;
/// 提示词标记条目上限。
pub const MAX_PROMPT_NOTES: usize = 1024;
/// 单条路径 / 键值的字节上限。
pub const MAX_UI_STATE_TEXT: usize = 1024;

/// 文件树的下一层条目。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiDirEntry {
    /// 相对本目录的名字（不含父路径）。
    pub name: String,
    /// 相对仓库根的路径（`/` 分隔）。
    pub path: String,
    pub is_dir: bool,
    /// 修改时刻（unix 秒）：按时间排序用。
    pub mtime: i64,
}

/// 随仓库走的界面状态：展开集 / 排序 / 目录颜色 / 提示词标记。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultUiState {
    /// 文件树展开的目录（仓库相对路径；空串 = 仓库根）。
    #[serde(default)]
    pub expanded: Vec<String>,
    /// 排序键（`name-asc` / `name-desc` / `mtime-desc` / `mtime-asc`）。
    pub sort: String,
    /// 目录图标颜色（目录相对路径 → hex 色）。
    #[serde(default)]
    pub folder_colors: std::collections::BTreeMap<String, String>,
    /// 已注册为系统提示词的笔记（仓库相对路径）。
    #[serde(default)]
    pub prompt_notes: Vec<String>,
    /// 重命名撞名自动顺延后的提醒文案（`None` = 无提醒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename_notice: Option<String>,
}

/// [`vault_ui::READ`] 的 params：`dir` 给定时另带该目录条目。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VaultUiParams {
    /// 需要一个目录的条目（空 = 不要）。
    pub dir: Option<String>,
}


/// [`vault_ui::WRITE`] 的 params：只改给出的键。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VaultUiPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_colors: Option<std::collections::BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_notes: Option<Vec<String>>,
    /// `Some(true)` = 用户关闭重名提醒条（宿主清掉该条）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismiss_rename_notice: Option<bool>,
}

impl VaultUiPatch {
    /// 校验条目数与单条长度：越界即拒绝，不做截断。
    pub fn validate(&self) -> Result<(), String> {
        if let Some(expanded) = &self.expanded {
            ensure_list("展开集", expanded, MAX_EXPANDED_ENTRIES)?;
        }
        if let Some(sort) = &self.sort {
            ensure_text("排序键", sort)?;
        }
        if let Some(colors) = &self.folder_colors {
            if colors.len() > MAX_FOLDER_COLORS {
                return Err(format!("目录颜色不得超过 {MAX_FOLDER_COLORS} 条"));
            }
            for (path, color) in colors {
                ensure_text("目录路径", path)?;
                ensure_text("颜色值", color)?;
            }
        }
        if let Some(notes) = &self.prompt_notes {
            ensure_list("提示词标记", notes, MAX_PROMPT_NOTES)?;
        }
        Ok(())
    }
}

fn ensure_menu_id<'a>(
    ids: &mut std::collections::HashSet<&'a str>,
    id: &'a str,
) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("菜单项 id 不能为空".into());
    }
    if !ids.insert(id) {
        return Err(format!("菜单项 id 重复：{id}"));
    }
    Ok(())
}

fn ensure_list(name: &str, items: &[String], limit: usize) -> Result<(), String> {
    if items.len() > limit {
        return Err(format!("{name}不得超过 {limit} 条"));
    }
    for item in items {
        ensure_text(name, item)?;
    }
    Ok(())
}

fn ensure_text(name: &str, value: &str) -> Result<(), String> {
    if value.len() > MAX_UI_STATE_TEXT {
        return Err(format!("{name}不得超过 {MAX_UI_STATE_TEXT} 字节"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{name}不得含控制字符"));
    }
    Ok(())
}

/// 拖拽会话里被拖的条目。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragItem {
    /// 仓库相对路径。
    pub path: String,
    pub is_dir: bool,
    /// ghost 上显示的文案（缺省 = 路径末段）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// [`drag::START`] 的 params：指针起点取宿主当前指针位置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragStartParams {
    pub item: DragItem,
    /// 承载发起面所在的窗口面标识（宿主据此把 ghost 画在正确窗口）。
    pub surface_id: String,
}

/// [`drag::HOVER`] 的 params：本面上指针所指的落点候选。
/// 几何（哪一行）由插件判定，能不能落由宿主按同一套规则复核。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DragHoverParams {
    /// 落点目录（仓库相对路径；空串 = 仓库根）；缺省 = 本面不接受落入。
    pub target: Option<String>,
    /// 该落点是不是目录（文件行不接受落入）。
    pub is_dir: bool,
}

/// [`drag::END`] 的 params：`cancelled` = 不落位。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DragEndParams {
    pub cancelled: bool,
}

/// 宿主回推给插件的拖拽状态（[`crate::event::name::DRAG_STATE`]）：驱动落点高亮与拒绝提示。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DragStateParams {
    /// 当前落点（仓库相对路径）；`None` = 指针不在本面的任何落点上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 落点不可用时的可读原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reject: Option<String>,
    /// 悬停折叠目录到点会自动展开。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub springing: bool,
}

/// 菜单条目：与宿主内置菜单同一套形态。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MenuItem {
    /// 动作项：`id` 随选中结果回传。
    Action {
        id: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        danger: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        disabled: bool,
        /// 行尾角标（如「当前」）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// 悬停提示。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        /// 所属面板（0 = 根面板）。
        #[serde(default)]
        pane: usize,
    },
    /// 当前选中项（强调色文字 + 右侧对勾）。
    Checked {
        id: String,
        label: String,
        #[serde(default)]
        pane: usize,
    },
    /// 钻入子面板。
    Submenu {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        disabled: bool,
        /// 所属面板（0 = 根面板）。
        #[serde(default)]
        pane: usize,
        /// 点击后钻入的面板号（必须 ≥ 1：根面板不能再钻回自己）。
        target: usize,
    },
    /// 只读文字行（确认说明这类）。
    Text {
        text: String,
        #[serde(default)]
        pane: usize,
    },
    /// 分组头（灰底小字）。
    Group {
        label: String,
        #[serde(default)]
        pane: usize,
    },
    /// 分隔线。
    Divider {
        #[serde(default)]
        pane: usize,
    },
}

impl MenuItem {
    /// 条目所属面板号。
    pub fn pane(&self) -> usize {
        match self {
            MenuItem::Action { pane, .. }
            | MenuItem::Checked { pane, .. }
            | MenuItem::Text { pane, .. }
            | MenuItem::Group { pane, .. }
            | MenuItem::Divider { pane } => *pane,
            MenuItem::Submenu { pane, .. } => *pane,
        }
    }
}

/// 菜单条目上限。
pub const MAX_MENU_ITEMS: usize = 512;
/// 菜单面板数上限。
pub const MAX_MENU_PANES: usize = 8;

/// 菜单顶部搜索行；声明后宿主渲染输入框并按标签过滤条目。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuSearch {
    /// 输入框提示文字。
    pub placeholder: String,
    /// 无匹配结果时的提示；缺省不显示空态行。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty: Option<String>,
}

/// [`menu::OPEN`] 的 params：`items` 顺序即渲染顺序。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuOpenParams {
    pub items: Vec<MenuItem>,
    /// 菜单宽度（逻辑像素；宿主按内容与视口钳制）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    /// 声明后宿主在菜单顶部渲染搜索行：只匹配条目标签（去首尾空白、忽略大小写的子串）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<MenuSearch>,
}

impl MenuOpenParams {
    /// 校验条目：非空、id 非空唯一、面板号在范围内、文字长度受限。
    pub fn validate(&self) -> Result<(), String> {
        if self.items.is_empty() {
            return Err("菜单至少需要一条".into());
        }
        if self.items.len() > MAX_MENU_ITEMS {
            return Err(format!("菜单条目不得超过 {MAX_MENU_ITEMS} 条"));
        }
        let mut ids = std::collections::HashSet::new();
        let mut max_pane = 0;
        for item in &self.items {
            max_pane = max_pane.max(item.pane());
            match item {
                MenuItem::Action { id, label, note, hint, .. } => {
                    ensure_menu_id(&mut ids, id)?;
                    ensure_text("菜单项文字", label)?;
                    for value in [note.as_deref(), hint.as_deref()].into_iter().flatten() {
                        ensure_text("菜单项文字", value)?;
                    }
                }
                MenuItem::Checked { id, label, .. } => {
                    ensure_menu_id(&mut ids, id)?;
                    ensure_text("菜单项文字", label)?;
                }
                MenuItem::Submenu { label, target, .. } => {
                    ensure_text("菜单项文字", label)?;
                    if *target == 0 || *target >= MAX_MENU_PANES {
                        return Err(format!("子面板入口的目标面板须在 1..{}", MAX_MENU_PANES));
                    }
                }
                MenuItem::Group { label, .. } => ensure_text("菜单项文字", label)?,
                MenuItem::Text { text, .. } => ensure_text("菜单项文字", text)?,
                MenuItem::Divider { .. } => {}
            }
        }
        if max_pane >= MAX_MENU_PANES {
            return Err(format!("菜单面板号不得超过 {}", MAX_MENU_PANES - 1));
        }
        if let Some(search) = &self.search {
            ensure_text("菜单搜索提示", &search.placeholder)?;
            if let Some(empty) = search.empty.as_deref() {
                ensure_text("菜单搜索空态", empty)?;
            }
        }
        Ok(())
    }
}

/// [`menu::HOST_CLOSED`] 的 params：关闭原因与选中的条目 id。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MenuClosedParams {
    /// 选中的动作 / 勾选项 id；未选中（点击外部 / Esc）时缺省。
    pub selected: Option<String>,
}

// ---------- 文件面板：容器、条目操作与取色 ----------

/// 一个可切换的仓库容器（本地仓库或协作空间）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerEntry {
    /// 唯一键（本地 = `local:<root>`，空间 = `space:<server>#<spaceId>`）。
    pub id: String,
    pub label: String,
    /// 本地仓库根路径（空间缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// 协作空间标识（本地缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// 当前激活的容器。
    #[serde(default)]
    pub active: bool,
}

/// [`vault_ui::READ`] 的 result：状态快照 + 可选目录条目 + 容器清单。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultUiResult {
    pub state: VaultUiState,
    /// 请求了 `dir` 时的条目（宿主已按当前排序排好）；未请求 = 空。
    #[serde(default)]
    pub entries: Vec<UiDirEntry>,
    /// 可切换的容器清单（本地仓库在前、激活项在前）。
    #[serde(default)]
    pub containers: Vec<ContainerEntry>,
    /// 插件注册的文档类型（「新建」菜单条目用；扩展名不带点）。
    #[serde(default)]
    pub doc_types: Vec<DocTypeEntry>,
}

/// 一个插件注册的文档类型（文件面板「新建」菜单条目）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocTypeEntry {
    /// 扩展名（不带点）。
    pub extension: String,
    /// 菜单标签（如「笔记」）。
    pub label: String,
    /// 宿主图标形状名（文件树条目与新建菜单用；空 = 按扩展名兜底）。
    #[serde(default)]
    pub icon: String,
}

/// 容器切换（`vault.switchContainer`）：切到清单里给的那个容器。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchContainerParams {
    /// [`ContainerEntry::id`] 的原值。
    pub id: String,
}

/// 容器移除（`vault.removeContainer`）：把该容器移出最近列表。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveContainerParams {
    /// [`ContainerEntry::id`] 的原值。
    pub id: String,
}

/// 新建一个条目（`vault.create`）：宿主按 `kind` 起空内容并落盘（同名自动加序号）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEntryParams {
    /// 目标目录（仓库相对路径；空串 = 仓库根）。
    pub dir: String,
    /// 条目名（不含扩展名；`kind = folder` 时即文件夹名）。
    pub name: String,
    /// `note` / `table` / `canvas` / `folder`，或插件注册文档类型的不带点扩展名。
    pub kind: String,
}

/// 新建结果：落定的仓库相对路径。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateEntryResult {
    pub path: String,
    /// 新建的是文件夹。
    #[serde(default)]
    pub is_dir: bool,
}

/// 取色器（`shell.pickColor`）：宿主的 HSV 取色器是系统单例 UI，插件只给初始色。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PickColorParams {
    /// 初始色（`#rrggbb`）；缺省 = 宿主强调色。
    pub initial: Option<String>,
}

/// 取色结果：用户取消时 `color` 缺省。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PickColorResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// 在文件管理器中显示一个条目（`shell.reveal`）：`path` 为仓库相对路径，空串 = 仓库根。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RevealParams {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// 目标是文件夹（文件夹用「打开」语义，文件用「选中」语义）。
    #[serde(default)]
    pub is_dir: bool,
}

/// 在外链打开 URL（`shell.openUrl`）：`url` 必须是 http(s) / mailto。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenUrlParams {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

/// 条目绝对路径（`shell.absolutePath` 回执）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbsolutePathResult {
    pub path: String,
}

/// 创建副本（`shell.duplicate`）：`path` 为源条目的仓库相对路径。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateParams {
    pub path: String,
    /// 源是文件夹（递归复制并重建内部 id）。
    #[serde(default)]
    pub is_dir: bool,
}

// ---------- fs ----------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateDirResult {
    pub path: String,
}

/// `path` 的口径由能力面决定：`fs` 用绝对路径，`vault` 用相对仓库根的路径。
/// 信任模型：插件与宿主同进程、持同等权限，声明式权限是透明度机制而非沙箱边界；
/// `fs` 服务的价值在统一出口与审计事实。
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

/// [`vault::FILES`] 的 result：全仓库可检索文件，按宿主文件树的遍历序（同层按名字排序，
/// 目录内容跟在该目录的位置）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesResult {
    pub files: Vec<VaultFileEntry>,
}

/// 可检索文件的一行。`icon` 是宿主注册表的图标键（插件注册的领域文档类型按注册图标，
/// 其余按内置扩展名映射）。`openable` = 行是否挂打开动作：图片附件不挂（由画布媒体卡承载），
/// 其余类型交给宿主按扩展名分流，没有对应视图时宿主提示。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultFileEntry {
    /// 仓库相对路径（`/` 分隔）。
    pub path: String,
    /// 文件名（路径末段）。
    pub name: String,
    pub icon: String,
    pub openable: bool,
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
    /// 允许多选（只对打开对话框有意义）：结果按选择顺序给全，`path` 仍是第一个。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiple: bool,
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
    /// 多选时的全部路径（顺序同用户选择；单选或取消为空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
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

// ---------- 文档实例通知 ----------

/// [`doc::HOST_OPEN`] 的 params：文档实例被打开。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInstanceOpenParams {
    /// 仓库容器内的相对路径（本地仓库）或服务端内容路径（协作空间），与 vault 口径一致。
    pub path: String,
    /// 宿主为本实例分配的面标识：插件据此为该面提交整面帧。
    pub surface_id: String,
}

/// [`doc::HOST_CLOSE`] 的 params：实例随标签关闭 / 换文件被关闭。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInstanceCloseParams {
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
    /// 推理等级（`low` / `medium` / `high`）；缺省 = 面板当前覆盖值，`off` = 不传该参数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// 仓库内图片相对路径（png / jpg / jpeg / webp / gif）；逐张校验，任一不可用 = 整条拒绝。
    /// 校验通过后到 run 读取之间文件可能消失：读不到的那张在该轮请求里跳过。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// 用户原始输入（未拼引用路径块）；缺省 = `text` 即原文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// 正文中登记的 # 引用；宿主按显示文本扫描命中后拼进消息开头。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<ChatRef>,
}

/// 一条 # 引用：仓库相对路径 + 显示标签 + 领域类型。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRef {
    pub path: String,
    pub label: String,
    /// 领域类型：`note` / `table` / `canvas` / `dir`。
    pub ref_kind: String,
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
    /// 直写模式的空占位移除（中断且没有产生任何内容）。
    Removed,
}

/// [`ai::TOOL_LIST`] 的 params：缺省 = 全部可用工具。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolListParams {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_ids: Vec<String>,
}

/// 一条工具声明（模型侧 schema）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    /// 分类：`web` / `file` / `task` / `plugin`。
    pub category: String,
    /// 可与其他工具并发执行（读写 / 未知工具串行屏障）。
    pub parallel_safe: bool,
    /// 勾选 id（内置工具 = 名称，插件工具 = `plugin:<插件 id>:<工具名>`）。
    pub selection_id: String,
}

/// [`ai::TOOL_LIST`] 的 result。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolListResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDecl>,
}

/// [`ai::TOOL_EXECUTE`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecParams {
    /// 工具名（内置工具或插件工具；插件工具用注册时的裸名称）。
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// [`ai::TOOL_EXECUTE`] 的 result：`content` 即模型可见结果文本。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecResult {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

// ---------- http 流式 ----------

/// [`http::STREAM_OPEN`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpStreamOpenParams {
    /// 调用方指定的流 id（可选）：指定后宿主按该 id 下发分片，插件可先登记接收端再开流。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// 空闲超时（毫秒）：超过该时长没有新数据即按失败结束；缺省由宿主设定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// [`http::STREAM_OPEN`] 的 result。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpStreamOpenResult {
    pub stream_id: String,
}

/// [`http::STREAM_CANCEL`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpStreamCancelParams {
    pub stream_id: String,
}

/// [`http::HOST_STREAM_CHUNK`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpStreamChunkParams {
    pub stream_id: String,
    /// 原始字节（base64）。
    pub base64: String,
}

/// [`http::HOST_STREAM_END`] 的 params：`error` 缺省 = 正常结束；
/// `cancelled` = 调用方主动取消（`http.stream.cancel` / 插件停用），不是流自然结束；
/// `status` = 响应状态码（建连成功时给出），供调用方区分鉴权 / 限流等非 2xx 失败。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpStreamEndParams {
    pub stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

// ---------- AI run（宿主 ↔ ai-core） ----------

/// 搜索配置（运行前由宿主从仓库 AI 设置解析）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchConfigWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavily_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searxng_url: Option<String>,
}

/// 一轮 run 的完整配置（供应商、模型、提示词、工具与仓库访问范围）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunConfigWire {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_ids: Vec<String>,
    #[serde(default)]
    pub search: SearchConfigWire,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// [`run::START`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartParams {
    /// 续聊会话；缺省 = 新建。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 新建会话时写入的 Agent。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 已构造好的 user 消息（含引用路径块、附件与 displayContent）。
    pub user_message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub config: RunConfigWire,
    /// 自动命名配置；缺省 = 不命名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub naming: Option<RunConfigWire>,
}

/// [`run::START`] 的 result。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartResult {
    pub session_id: String,
    pub message_id: String,
}

/// [`run::START_DIRECT`] 的 params：真源归调用方，插件只跑循环并发布事件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDirectParams {
    /// 事件归属的会话键（画布卡等合成 id）。
    pub session_id: String,
    pub history: Vec<Value>,
    pub user_message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub config: RunConfigWire,
}

/// [`run::STOP`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStopParams {
    pub session_id: String,
}

/// [`run::COMPACT`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCompactParams {
    pub session_id: String,
    pub config: RunConfigWire,
}

/// [`run::TITLE`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunTitleParams {
    pub session_id: String,
    pub config: RunConfigWire,
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

/// [`crate::event::name::TABLE_RESTORE`] 的 payload：历史回滚交回当前表格的全文快照。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableContentParams {
    /// 目标表格的仓库相对路径（订阅方按自己打开的文件匹配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `.atb` 全文（JSON 文本）。
    pub content: String,
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

// ---------- panel ----------

/// 头部状态胶囊的色调。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeaderTone {
    #[default]
    Normal,
    /// 有待落盘改动（琥珀色）。
    Pending,
    /// 失败（红色）。
    Danger,
}

/// 头部左侧的状态胶囊（未保存 / 保存失败这类）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderStatus {
    pub text: String,
    #[serde(default)]
    pub tone: HeaderTone,
}

/// 头部右侧的协作者胶囊（`color` 为 `#rrggbb` 身份色）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderPill {
    pub text: String,
    pub color: String,
}

/// 头部「⋯」菜单的一项：点击经 [`crate::event::name::PANEL_HEADER_ACTION`] 回发 `id`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderItem {
    pub id: String,
    pub label: String,
}

/// 一条面板头部状态：宿主按当前主题画成面板顶部那一行工具条。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelHeaderState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<HeaderStatus>,
    /// 错误文本（红色，过长由宿主截断）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pills: Vec<HeaderPill>,
    /// 右侧依次排列的文本（缩放百分比 / 行列统计这类）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub texts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<HeaderItem>,
    /// 显示「视图」下拉时给出贡献槽（当前只有 `tableView`）；缺省不显示下拉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_slot: Option<ContributionSlot>,
    /// 本面板有未落盘改动：宿主的脏拦截据此先要求插件 flush 再放行动作。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
}

/// [`panel::HEADER`] 的 params：`header` 缺省 = 收起头部。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelHeaderParams {
    /// 面板视图 id（壳侧视图 id，例如表格面板的 `table`）。
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<PanelHeaderState>,
}

/// [`crate::event::name::PANEL_FLUSH`] 的 payload：宿主请插件立即落盘，`token` 原样随
/// [`panel::FLUSH_DONE`] 回传。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelFlushParams {
    pub target: String,
    pub token: u64,
}

/// [`panel::FLUSH_DONE`] 的 params。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelFlushDoneParams {
    pub token: u64,
    /// 宿主发起请求时给的面板视图 id：回执原样带回，宿主据此确认是哪个面板。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// [`crate::event::name::PANEL_HEADER_ACTION`] 的 payload：用户点了头部菜单的某一项。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelHeaderActionParams {
    pub target: String,
    pub item: String,
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

/// 当前容器的种类（决定房间展示的是空间名册还是「未进入协作空间」）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpaceContainerKind {
    #[default]
    Local,
    Space,
}

/// [`space::FILE_PATCH`] 的 params：本端「上次落盘基线」与「冻结快照」两份全文，
/// 宿主按 `kind` 求实体差异交服务端仲裁。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceFilePatchParams {
    /// 仓库相对路径。
    pub path: String,
    /// 文件族：`table` / `canvas`——决定服务端补丁端点与历史类型。
    pub kind: String,
    /// 上次成功保存的全文（补丁合并的基线）。
    pub baseline: String,
    /// 本端当前全文（冻结快照）。
    pub frozen: String,
}

/// [`space::FILE_PATCH`] 的 result：服务端落地后的路径（补丁带标题时服务端按标题改名）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceFilePatchResult {
    pub path: String,
}

/// [`crate::event::name::SPACE_FILE_PATCH`] 的 payload：别的成员改动了本端打开的空间文件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceFilePatchPayload {
    pub path: String,
    /// 文件族（`table`），决定 `patch` 的形态。
    pub kind: String,
    pub patch: Value,
    /// 服务端是否已落盘（true = 已仲裁落地；false = 转发帧）。
    pub landed: bool,
}

/// [`space::ROOM`] 的 result：容器种类、连接态、本端身份与在线设备明细。
/// `peers` 不含自己；同一身份的多条连接由订阅方按「昵称 + 设备」合并。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceRoomResult {
    pub kind: SpaceContainerKind,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_peer_id: Option<String>,
    /// 本端身份（协作空间才有；本地仓库 = 缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_identity: Option<SpaceSelfIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<SpacePeer>,
}

/// 本端在线设备的展示资料（名字 / 成员色 / 平台 / 版本 / 打开的文件）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceSelfIdentity {
    pub name: String,
    /// 成员色（`#rrggbb`）。
    pub color: String,
    /// 平台标识（与 `std::env::consts::OS` 同词表）。
    pub platform: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SpacePeerFile>,
}

/// 同空间的一个在线设备。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacePeer {
    pub peer_id: String,
    pub nickname: String,
    /// 设备名（「昵称 + 设备」是合并键）。
    pub device: String,
    /// 成员色（`#rrggbb`）。
    pub color: String,
    /// 平台标识（旧端缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 客户端版本（旧端缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<SpacePeerFile>,
}

/// 一个打开中的文件：仓库相对路径 + 行图标键（宿主注册表口径）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacePeerFile {
    pub path: String,
    pub icon: String,
}

/// [`space::MEMBERS`] 的 result：名册缓存快照。`ready` 区分「缓存里有名册（可能为空）」
/// 与「宿主还没拉到」，两者都会给 `loading` 与 `error`。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceMembersResult {
    /// 缓存里已有名册（`members` 可信）；false = 尚未拉到，展示方按「加载中 / 暂无名册」处理。
    pub ready: bool,
    pub loading: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<SpaceMember>,
}

/// 空间内的一名成员。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceMember {
    pub username: String,
    pub display_name: String,
    pub role: SpaceMemberRole,
}

/// 成员角色（命名与 `SpaceRole` 的 wire 形态一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpaceMemberRole {
    Owner,
    Editor,
    Viewer,
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
    /// 空间名册里该成员的身份色（`#RRGGBB`）；由宿主填，插件不再自行哈希。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_color: Option<String>,
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
/// 改名 / 删除常量会直接编译失败，目录不会悄悄漂移。服务随归属插件启停。
pub const BUILTIN_SERVICES: &[(&str, &str, &[&str])] = &[
    ("fs", PLATFORM_PLUGIN, &[fs::PRIVATE_DIR, fs::READ_TEXT, fs::WRITE_TEXT, fs::READ_BINARY, fs::WRITE_BINARY, fs::REMOVE, fs::EXISTS, fs::LIST, fs::DOWNLOADS_DIR]),
    ("process", PLATFORM_PLUGIN, &[process::SPAWN, process::WRITE_STDIN, process::CLOSE_STDIN, process::KILL]),
    ("vault", PLATFORM_PLUGIN, &[vault::READ_TEXT, vault::WRITE_TEXT, vault::READ_BINARY, vault::WRITE_BINARY, vault::IMPORT_FILE, vault::IMAGE_INFO, vault::LIST, vault::EXISTS, vault::TAGS, vault::REMOVE, vault::RENAME, vault::BACKLINKS, vault::FILES,
        // 仓库界面状态（展开集 / 排序 / 目录颜色 / 提示词标记）与条目操作，随仓库走
        vault_ui::READ, vault_ui::WRITE, vault_ui::CREATE, vault_ui::SWITCH_CONTAINER, vault_ui::REMOVE_CONTAINER, vault_ui::LIST_DIRS, vault_ui::REFRESH_TREE]),
    ("drag", PLATFORM_PLUGIN, &[drag::START, drag::HOVER, drag::END]),
    ("menu", PLATFORM_PLUGIN, &[menu::OPEN]),
    ("shell", PLATFORM_PLUGIN, &[
        shell::PICK_COLOR,
        shell::PICK_VAULT,
        shell::REVEAL,
        shell::ABSOLUTE_PATH,
        shell::DUPLICATE,
        shell::OPEN_URL,
    ]),
    ("collab", PLATFORM_PLUGIN, &[collab::PEERS, collab::SEND, collab::META_GET, collab::META_PATCH]),
    // 空间名册与房间事实取自宿主会话与服务端，弹层 / 菜单也在宿主，归平台自身
    ("space", PLATFORM_PLUGIN, &[space::ROOM, space::MEMBERS, space::OPEN_MENU, space::OPEN_SETTINGS, space::FILE_PATCH]),
    // 面板头部状态与 flush 回执是宿主界面原语，随平台常驻
    ("panel", PLATFORM_PLUGIN, &[panel::HEADER, panel::FLUSH_DONE]),
    ("state", PLATFORM_PLUGIN, &[state::READ, state::WRITE]),
    ("notification", PLATFORM_PLUGIN, &[notification::SHOW]),
    ("tray", PLATFORM_PLUGIN, &[tray::SET_ITEMS]),
    ("shortcut", PLATFORM_PLUGIN, &[shortcut::REGISTER, shortcut::UNREGISTER, shortcut::QUERY]),
    ("clipboard", PLATFORM_PLUGIN, &[clipboard::WRITE_TEXT, clipboard::READ_TEXT, clipboard::READ_IMAGE, clipboard::WRITE_IMAGE]),
    ("dialog", PLATFORM_PLUGIN, &[dialog::PICK_FILE, dialog::PICK_DIRECTORY]),
    ("http", PLATFORM_PLUGIN, &[http::REQUEST, http::STREAM_OPEN, http::STREAM_CANCEL]),
    // 会话真源是平台数据：归属平台插件，外部提供者不得接管
    ("chat", PLATFORM_PLUGIN, &[chat::SESSION_READ, chat::SESSION_WRITE, chat::SESSION_SNAPSHOT]),
    // 工具名册与结果回报是宿主记账，不是可由外部提供者接管的能力，故不在目录里；
    // 对话门面由宿主编排（核心插件只是被调用的实现），服务归平台自身
    ("ai", PLATFORM_PLUGIN, &[ai::CHAT_SEND, ai::CHAT_SEND_DIRECT, ai::CHAT_STOP, ai::CHAT_HISTORY, ai::MODELS, ai::AGENTS, ai::CHAT_RENAME, ai::CHAT_DELETE, ai::CHAT_COMPACT, ai::CHAT_COMPACT_DIRECT, ai::CHAT_TITLE, ai::TOOL_LIST, ai::TOOL_EXECUTE, ai::CHAT_EMIT, ai::SETTINGS]),
    ("window", PLATFORM_PLUGIN, &[window::OPEN, window::CLOSE, window::UPDATE, window::SHOW, window::BEGIN_DRAG, window::MINIMIZE, window::TOGGLE_MAXIMIZE]),
    ("query", PLATFORM_PLUGIN, &[query::MEASURE, query::THEME, query::FONTS]),
    // 编辑器选区读写作用于宿主自持的当前文档会话，随平台常驻
    ("editor", PLATFORM_PLUGIN, &[editor::SELECTION, editor::REPLACE_SELECTION, editor::OPEN_FILES, editor::OPEN_FILE, editor::PREFS]),
    // 文档会话是宿主内容真源（保存链 / 协作增量 / 历史），归平台自身，不随插件停用下线
    (
        "doc",
        PLATFORM_PLUGIN,
        &[
            doc::OPEN,
            doc::EDIT,
            doc::SAVE,
            doc::CLOSE,
            doc::AWARENESS,
            doc::RENAME,
            doc::PROPS_READ,
            doc::PROPS_WRITE,
        ],
    ),
    // 历史读数取自宿主侧文件与服务端聚合，历史弹层也在宿主，与 doc 同理归平台自身
    ("history", PLATFORM_PLUGIN, &[history::ACTIVITY, history::DAY, history::GROUPS, history::OPEN, history::RECORD]),
    // 最近打开清单是宿主按容器身份记账的界面数据，记录与剔除都由宿主维护
    ("recent", PLATFORM_PLUGIN, &[recent::LIST]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_props_identity_fields_are_required() {
        let result = serde_json::json!({
            "path": "a.md",
            "fingerprint": "fp",
            "frontmatter": "---\nx: y\n---\n",
            "present": true,
            "parsed": true,
            "editable": true,
        });
        let parsed: DocPropsResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(parsed.fingerprint, "fp");
        let mut missing = result;
        missing.as_object_mut().unwrap().remove("fingerprint");
        assert!(
            serde_json::from_value::<DocPropsResult>(missing).is_err(),
            "读回执缺指纹必须反序列化失败"
        );
        let write = serde_json::json!({
            "frontmatter": "---\n---\n",
            "path": "a.md",
            "fingerprint": "fp",
        });
        assert!(serde_json::from_value::<DocPropsWriteParams>(write.clone()).is_ok());
        for field in ["path", "fingerprint"] {
            let mut missing = write.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<DocPropsWriteParams>(missing).is_err(),
                "写回缺 {field} 必须反序列化失败"
            );
        }
    }

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
    fn menu_items_keep_pane_and_submenu_target_apart() {
        let ok = MenuOpenParams {
            items: vec![
                MenuItem::Submenu {
                    label: "从列表移除".into(),
                    icon: None,
                    disabled: false,
                    pane: 0,
                    target: 1,
                },
                MenuItem::Text { text: "只从列表移除".into(), pane: 1 },
                MenuItem::Action {
                    id: "confirm".into(),
                    label: "确认移除".into(),
                    icon: None,
                    danger: true,
                    disabled: false,
                    note: None,
                    hint: None,
                    pane: 1,
                },
            ],
            width: None,
            search: None,
        };
        assert!(ok.validate().is_ok());
        // 子面板入口指向根面板会打不开；越界同样拒绝
        for target in [0, MAX_MENU_PANES] {
            let bad = MenuOpenParams {
                items: vec![MenuItem::Submenu {
                    label: "从列表移除".into(),
                    icon: None,
                    disabled: false,
                    pane: 0,
                    target,
                }],
                width: None,
                search: None,
            };
            assert!(bad.validate().is_err(), "应拒绝目标面板 {target}");
        }
    }

    #[test]
    fn menu_search_is_optional_and_validated() {
        let plain = MenuOpenParams {
            items: vec![MenuItem::Checked { id: "font".into(), label: "等宽".into(), pane: 0 }],
            width: None,
            search: None,
        };
        assert!(plain.validate().is_ok());
        let wire = serde_json::to_value(&plain).unwrap();
        assert!(wire.get("search").is_none(), "不声明搜索时载荷不变");

        let searched = MenuOpenParams {
            items: vec![MenuItem::Action {
                id: "font".into(),
                label: "等宽".into(),
                icon: None,
                danger: false,
                disabled: false,
                note: None,
                hint: None,
                pane: 0,
            }],
            width: None,
            search: Some(MenuSearch { placeholder: "搜索字体".into(), empty: Some("没有匹配项".into()) }),
        };
        assert!(searched.validate().is_ok());
        let bad = MenuOpenParams {
            search: Some(MenuSearch { placeholder: "x".repeat(MAX_UI_STATE_TEXT + 1), empty: None }),
            ..searched
        };
        assert!(bad.validate().unwrap_err().contains("菜单搜索提示"));
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
    fn recent_list_and_history_groups_shapes() {
        // 最近打开：类型取小写枚举名，缺省结果 = 容器未就绪
        let list = RecentListResult {
            ready: true,
            entries: vec![
                RecentEntry { file: "notes/a.md".into(), kind: RecentKind::Note, opened_at: 1_700_000_000 },
                RecentEntry { file: "b.atb".into(), kind: RecentKind::Table, opened_at: 1_700_000_001 },
                RecentEntry { file: "c.atlx".into(), kind: RecentKind::Canvas, opened_at: 1_700_000_002 },
            ],
        };
        let wire = serde_json::to_value(&list).unwrap();
        assert_eq!(wire["entries"][0]["kind"], "note");
        assert_eq!(wire["entries"][1]["kind"], "table");
        assert_eq!(wire["entries"][2]["kind"], "canvas");
        assert_eq!(wire["entries"][0]["openedAt"], 1_700_000_000_i64);
        assert_eq!(
            serde_json::to_value(RecentListResult::default()).unwrap(),
            serde_json::json!({ "ready": false, "entries": [] })
        );
        // 历史分组：缺省刷新标记不刷新（`{}` 可解析），刷新请求带 `refresh: true`
        assert_eq!(
            serde_json::to_value(HistoryGroupsParams::default()).unwrap(),
            serde_json::json!({ "refresh": false })
        );
        assert_eq!(
            serde_json::from_value::<HistoryGroupsParams>(serde_json::json!({})).unwrap(),
            HistoryGroupsParams::default()
        );
        assert_eq!(
            serde_json::to_value(HistoryGroupsParams { refresh: true }).unwrap(),
            serde_json::json!({ "refresh": true })
        );
        let groups = HistoryGroupsResult {
            ready: true,
            groups: vec![HistoryFileGroup {
                kind: "note".into(),
                file: "notes/a.md".into(),
                count: 3,
            }],
        };
        assert_eq!(
            serde_json::to_value(&groups).unwrap(),
            serde_json::json!({
                "ready": true,
                "groups": [{ "kind": "note", "file": "notes/a.md", "count": 3 }]
            })
        );
        // 服务名与方法名一致（能力总线按 `服务.方法` 切分）
        assert_eq!(recent::LIST, format!("{}.list", recent::SERVICE));
    }

    #[test]
    fn editor_open_file_reveal_defaults_to_true() {
        // 旧调用方只给 path：按「打开并让视图显示」处理
        let legacy: EditorOpenFileParams =
            serde_json::from_value(serde_json::json!({ "path": "notes/a.md" })).unwrap();
        assert!(legacy.reveal);
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            serde_json::json!({ "path": "notes/a.md", "reveal": true })
        );
        // 列表类入口显式要求不动布局
        let quiet: EditorOpenFileParams =
            serde_json::from_value(serde_json::json!({ "path": "notes/a.md", "reveal": false }))
                .unwrap();
        assert!(!quiet.reveal);
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

/// 历史回滚把某个版本交回当前画布（`canvas:restore` 的 payload）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasContentParams {
    /// 目标画布的仓库相对路径（订阅方按自己打开的文件匹配）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `.atlx` 全文（JSON 文本）。
    pub content: String,
}

/// 画布选中卡的引用加载状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CanvasRefStatus {
    /// 已加载（文本 / 图片 / 表格可见）。
    Loaded,
    /// 读到字节但无法按文本呈现。
    Binary,
    /// 引用的文件不存在或读取失败。
    Missing,
    /// 引用内容仍在读取。
    Loading,
}

/// 选中卡的一条连线。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasEdgeInfo {
    /// `association` / `reference-consumed` / `reference-unconsumed` / `lineage` / `flow`。
    pub role: String,
    /// `out` = 选中卡是源；`in` = 选中卡是目标。
    pub direction: String,
    /// 对端卡标题。
    pub peer_title: String,
}

/// 画布选中卡的属性快照（属性面板数据源）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSelectionInfo {
    pub title: String,
    /// 节点类型原文（`conversation` / `text` / `table` / `media` / `link` / 分组等）。
    pub node_type: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// 卡引用的仓库相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 引用内容状态；无引用文件 = 缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_status: Option<CanvasRefStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<CanvasEdgeInfo>,
}

/// 画布统计（无选中时属性面板展示）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasStats {
    pub cards: usize,
    pub groups: usize,
    pub edges: usize,
}

/// [`canvas::SNAPSHOT`] 的 result。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSnapshotResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<CanvasSelectionInfo>,
    #[serde(default)]
    pub stats: CanvasStats,
}

/// [`crate::event::name::CANVAS_COMMAND`] 的 payload：宿主按画布作用域拦截的快捷键命令。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasCommandParams {
    /// `undo` / `redo` / `copy` / `paste` / `select-all` / `delete` / `escape`。
    pub command: String,
}

/// [`crate::event::name::TABLE_COMMAND`] 的 payload：宿主按表格作用域拦截的快捷键命令。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableCommandParams {
    /// `undo` / `redo` / `copy` / `cut` / `paste` / `select-all` / `delete` / `save`。
    pub command: String,
}

/// [`ai::CHAT_SEND_DIRECT`] 的 params：宿主解析模型配置后把同一份负载交给 AI 核心插件，
/// 消息真源不进入会话语料。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDirectParams {
    /// 事件归属的会话键（画布卡等合成 id）。
    pub session_id: String,
    /// 本轮之前的消息（不含即将追加的用户消息）。
    pub history: Vec<Value>,
    /// 已构造好的用户消息（含引用路径块、附件与 displayContent）。
    pub user_message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    /// Agent id（当前仓库名册）；缺省 = 内置「对话」工具集。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 显式模型（可只给模型 id，在仓库供应商配置里唯一命中即采用；
    /// 指定 `providerId` 时必须同时给出模型 id）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 推理等级（`off` / `low` / `medium` / `high`）；缺省 = 面板当前覆盖值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// 空间模式下已取回的 Agent 提示词正文（本地模式缺省，宿主自取）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_prompt: Option<String>,
}

/// [`ai::CHAT_COMPACT_DIRECT`] 的 params：压缩调用方给出的消息，不读写会话语料。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDirectCompactParams {
    /// 事件归属的会话键（与直写 run 同口径）；缺省时仅按消息压缩。
    #[serde(default)]
    pub session_id: String,
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// [`ai::CHAT_COMPACT_DIRECT`] 的 result。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDirectCompactResult {
    pub summary: String,
}


/// [`run::COMPACT_DIRECT`] 的 params：消息由调用方给出，结果只回摘要。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCompactDirectParams {
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Value>,
    pub config: RunConfigWire,
}

/// [`run::COMPACT_DIRECT`] 的 result。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCompactDirectResult {
    pub summary: String,
}


/// [`ai::SETTINGS`] 的 result：与 AI 运行相关的仓库设置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsResult {
    /// 画布对话落卡开关；缺省 = 开启。
    #[serde(default = "default_auto_materialize")]
    pub auto_materialize: bool,
}

fn default_auto_materialize() -> bool {
    true
}

