//! 对外 API 参考的生成：把协议 crate 里的公开项按原文（连同文档注释）贴进文档。
//! 只做文本级抽取——顶层项以列 0 的 `pub …` 起、列 0 的 `}` 收（或单行 `;`），
//! 抽取后校验括号配平；找不到项或边界不对都直接报错，不做猜测。

use std::collections::HashMap;
use std::path::Path;

/// 协议源码目录（相对仓库根）。
const SRC_DIR: &str = "atelyx-plugin-protocol/src";

/// 文档开头的说明。
const HEADER: &str = "\
# 插件 API 参考

Atelyx 插件协议的全部公开类型与方法名。概念、取舍与常见坑见 [插件开发](plugin-development.md)；这里只列契约本身。

> 本文件由 `cargo run -p xtask -- docs` 从 `atelyx-plugin-protocol/` 源码生成，请勿手改。
> 协议改动后重跑一次即可；忘记重跑时 `cargo test --workspace` 会失败。
";

/// 一个要贴进文档的顶层项（源码文件名 + 项名）。
struct Item {
    file: &'static str,
    name: &'static str,
}

/// 文档的一节：标题、导语、按顺序贴出的项。
struct Section {
    title: &'static str,
    intro: &'static str,
    items: &'static [Item],
}

macro_rules! items {
    ($( $file:literal : $name:literal ),* $(,)?) => {
        &[$(Item { file: $file, name: $name }),*]
    };
}

const SECTIONS: &[Section] = &[
    Section {
        title: "协议版本与清单",
        intro: "`plugin.json` 的结构、能力声明与宿主保留的清单文件名。`apiVersion` 必须与宿主相等，\
不等即拒绝装载。",
        items: items!(
            "lib.rs": "API_VERSION",
            "manifest.rs": "MANIFEST_FILE",
            "manifest.rs": "Manifest",
            "manifest.rs": "Entry",
            "manifest.rs": "Permission",
            "manifest.rs": "DocTypeRegistration",
        ),
    },
    Section {
        title: "握手与信封",
        intro: "插件启动后的第一个请求是 `initialize`；此后双向都是一行一个 JSON-RPC 2.0 信封（NDJSON）。",
        items: items!(
            "handshake.rs": "INITIALIZE",
            "handshake.rs": "InitializeParams",
            "handshake.rs": "PluginInfo",
            "handshake.rs": "InitializeResult",
            "handshake.rs": "HostInfo",
            "envelope.rs": "Envelope",
            "envelope.rs": "Request",
            "envelope.rs": "Notification",
            "envelope.rs": "Response",
            "envelope.rs": "RpcError",
        ),
    },
    Section {
        title: "能力总线",
        intro: "能力按「服务 + 方法」登记：方法名本身就是 `服务.方法`，所以目录按第一个 `.` 切分即得。内置实现是出厂的活动提供者；外部提供者登记后只成为候选，切换由用户决定。`service.call` 是名字要到运行时才知道时的信封形态，与直接调用同名方法解析到同一条目、同一个实现。",
        items: items!(
            "service.rs": "method",
            "service.rs": "SERVICE_VERSION",
            "service.rs": "MAX_METHOD_NAME",
            "service.rs": "MAX_METHODS_PER_SERVICE",
            "service.rs": "RegisterParams",
            "service.rs": "UnregisterParams",
            "service.rs": "ServiceInfo",
            "service.rs": "ListResult",
            "service.rs": "CallParams",
            "service.rs": "HostCallParams",
            "service.rs": "ResultParams",
            "service.rs": "ChangedParams",
            "service.rs": "error",
            "method.rs": "BUILTIN_SERVICES",
        ),
    },
    Section {
        title: "绘制指令",
        intro: "插件把界面画成指令：一条指令一个形状或一段文字，坐标是面本地逻辑像素、左上原点。\
指令集封闭，`fill` 与 `stroke` 至少声明其一；命中由命中区（见下一节）表达，绘制指令本身不带交互语义。",
        items: items!(
            "paint.rs": "DrawCommand",
            "paint.rs": "PaintColor",
            "paint.rs": "PaintShadow",
            "paint.rs": "PaintPoint",
            "paint.rs": "TextAlign",
            "paint.rs": "TextFamily",
            "paint.rs": "TextWeight",
            "paint.rs": "ColorToken",
            "paint.rs": "ImageFit",
            "paint.rs": "ImageSource",
            "paint.rs": "MAX_FPS",
            "method.rs": "query",
            "method.rs": "markdown",
            "method.rs": "MAX_MEASURE_ITEMS",
            "method.rs": "MeasureItem",
            "method.rs": "MeasureBatchParams",
            "method.rs": "MeasureBatchResult",
            "method.rs": "MeasureTextResult",
            "method.rs": "ThemeResult",
            "method.rs": "ThemeColors",
            "method.rs": "FontSizes",
            "method.rs": "Radii",
            "method.rs": "PaddingSpec",
            "method.rs": "AxesPadding",
            "method.rs": "EdgesPadding",
        ),
    },
    Section {
        title: "整面帧与输入事件",
        intro: "`frame.submitShared` 一帧即该面的完整界面（指令 + 命中区 + 输入声明），帧内容写进共享内存\
槽位，信封只带槽位坐标；宿主只按几何绘制并把原始输入经 `host:frame.input` 原样回发，插件按帧自行维护\
悬停 / 按下 / 焦点 / 滚动 / 文本编辑状态。",
        items: items!(
            "method.rs": "frame",
            "frame.rs": "SurfaceFrame",
            "frame.rs": "HitRegion",
            "frame.rs": "HitCursor",
            "frame.rs": "FrameRect",
            "frame.rs": "FrameInput",
            "frame.rs": "MAX_HITS",
            "frame.rs": "MAX_HIT_ID_LEN",
            "frame.rs": "FrameSubmitSharedParams",
            "frame.rs": "FrameRegionRequest",
            "frame.rs": "FrameInputParams",
            "frame.rs": "FrameTickParams",
            "frame.rs": "MouseButton",
            "frame.rs": "Key",
            "frame.rs": "NamedKey",
            "frame.rs": "KeyAction",
            "frame.rs": "Modifiers",
            "method.rs": "MAX_MARKDOWN_BYTES",
            "method.rs": "MarkdownParams",
            "method.rs": "MarkdownResult",
            "method.rs": "MarkdownBlock",
            "method.rs": "MarkdownSpan",
        ),
    },
    Section {
        title: "帧共享内存",
        intro: "握手应答给出区域描述（路径 / 代次 / 槽数 / 槽容量）：插件把编码后的帧写进一个槽位，再以\
`frame.submitShared` 报告槽位坐标。区域文件全部小端：64 字节区域头（魔数 `ATLXFRM\\0` / 版本 / 槽数 /\
槽容量 / 代次 / 头长 / 槽步长），每槽 16 字节槽头（序号 u64、长度 u32）+ 槽容量。写入顺序是「序号置奇 →\
写长度与内容 → 序号置偶」，序号取非零偶数、每次 +2；宿主首尾序号一致才认这一帧，撕裂或错位的整帧丢弃。\
帧超出槽容量时用 `frame.region` 请求扩容，宿主换新文件并递增代次，旧代次的提交一律拒绝。",
        items: items!(
            "frame_region.rs": "FrameRegionInfo",
            "frame_region.rs": "REGION_MAGIC",
            "frame_region.rs": "REGION_VERSION",
            "frame_region.rs": "REGION_HEADER_SIZE",
            "frame_region.rs": "SLOT_HEADER_SIZE",
            "frame_region.rs": "SLOT_COUNT",
            "frame_region.rs": "SLOT_CAPACITY",
            "frame_region.rs": "MAX_SLOT_CAPACITY",
            "frame_region.rs": "init_region",
            "frame_region.rs": "validate_region",
            "frame_region.rs": "write_slot",
            "frame_region.rs": "read_slot",
            "frame_codec.rs": "encode",
            "frame_codec.rs": "decode",
        ),
    },
    Section {
        title: "贡献点与窗口面",
        intro: "贡献点决定插件界面出现在哪里；`surfaceId` 即贡献 id（文档实例另由宿主分配 id，插件窗口面由 `window.open` 的 `id` 指定）。",
        items: items!(
            "method.rs": "ContributionSlot",
            "method.rs": "SurfaceKind",
            "method.rs": "SurfaceAttachedParams",
            "method.rs": "SurfaceDetachedParams",
            "method.rs": "SubmitResult",
            "method.rs": "RegisterParams",
            "method.rs": "UnregisterParams",
            "method.rs": "TakeoverScope",
            "method.rs": "TakeoverParams",
            "method.rs": "WindowOpenParams",
            "method.rs": "WindowCloseParams",
            "method.rs": "WindowUpdateParams",
            "method.rs": "WindowIdParams",
            "method.rs": "WindowClosedParams",
            "method.rs": "WindowDragParams",
        ),
    },
    Section {
        title: "默认布局",
        intro: "插件声明自己期望的默认布局：宿主在插件首次启用时摆进当前场景，之后改名 / 删除都不再补回。\
`views` 填视图 kind（本插件贡献为 `plugin:<插件 id>:<贡献 id>`）。",
        items: items!(
            "method.rs": "layout",
            "method.rs": "DeclareDefaultLayoutParams",
            "layout.rs": "LayoutSpecNode",
            "layout.rs": "Axis",
            "layout.rs": "MAX_NAME_BYTES",
            "layout.rs": "MAX_DEPTH",
            "layout.rs": "MAX_PANELS",
            "layout.rs": "MAX_VIEWS_PER_PANEL",
        ),
    },
    Section {
        title: "能力面方法名",
        intro: "每个模块是一类能力面的方法名常量，与 `permissions` 里的能力声明一一对应；\
声明用于公示与审计，不是拦截：插件是原生进程，能力面上的限制只作便利层语义。",
        items: items!(
            "method.rs": "view",
            "method.rs": "window",
            "method.rs": "fs",
            "method.rs": "process",
            "method.rs": "vault",
            "method.rs": "table",
            "method.rs": "state",
            "method.rs": "notification",
            "method.rs": "shortcut",
            "method.rs": "clipboard",
            "method.rs": "dialog",
            "method.rs": "http",
            "method.rs": "doc",
            "method.rs": "editor",
            "method.rs": "history",
            "method.rs": "ai",
            "method.rs": "tray",
            "method.rs": "collab",
        ),
    },
    Section {
        title: "能力面负载：文件、进程与系统服务",
        intro: "上表方法的 params / result 形态（`path` 在 `fs` 面是绝对路径，在 `vault` 面是仓库相对路径）。",
        items: items!(
            "method.rs": "PrivateDirResult",
            "method.rs": "PathParams",
            "method.rs": "TextContent",
            "method.rs": "BinaryContent",
            "method.rs": "WriteTextParams",
            "method.rs": "WriteBinaryParams",
            "method.rs": "ImportFileParams",
            "method.rs": "ExistsResult",
            "method.rs": "ImageInfoResult",
            "method.rs": "VaultRenameParams",
            "method.rs": "ListResult",
            "method.rs": "FsEntry",
            "method.rs": "FsEntryKind",
            "method.rs": "SpawnParams",
            "method.rs": "SpawnResult",
            "method.rs": "PidParams",
            "method.rs": "WriteStdinParams",
            "method.rs": "ProcessEvent",
            "method.rs": "ProcessEventParams",
            "method.rs": "StateReadResult",
            "method.rs": "StateWriteParams",
            "method.rs": "ShowNotificationParams",
            "method.rs": "NotificationLevel",
            "method.rs": "RegisterShortcutParams",
            "method.rs": "AcceleratorParams",
            "method.rs": "ShortcutTriggeredParams",
            "method.rs": "ShortcutQueryParams",
            "method.rs": "ShortcutQueryResult",
            "method.rs": "TextPayload",
            "method.rs": "ClipboardImage",
            "method.rs": "ClipboardImageResult",
            "method.rs": "ClipboardWriteImageParams",
            "method.rs": "PickFileParams",
            "method.rs": "FileFilter",
            "method.rs": "PickDirectoryParams",
            "method.rs": "PickedPath",
            "method.rs": "HttpRequestParams",
            "method.rs": "HttpResult",
        ),
    },
    Section {
        title: "能力面负载：文档与对话",
        intro: "领域文档实例、AI 对话与工具桥的负载形态。",
        items: items!(
            "method.rs": "DocOpenParams",
            "method.rs": "DocCloseParams",
            "method.rs": "MAX_EDITOR_TEXT_BYTES",
            "method.rs": "EditorSelection",
            "method.rs": "EditorReplaceParams",
            "method.rs": "ChatSendParams",
            "method.rs": "ChatSendResult",
            "method.rs": "ChatStopParams",
            "method.rs": "ChatHistoryParams",
            "method.rs": "ChatRenameParams",
            "method.rs": "ChatSessionParams",
            "method.rs": "ChatSessionSummary",
            "method.rs": "ChatSessionWire",
            "method.rs": "ChatHistoryResult",
            "method.rs": "AiModel",
            "method.rs": "AiModelProvider",
            "method.rs": "AiModelPick",
            "method.rs": "AiModelsResult",
            "method.rs": "AiAgent",
            "method.rs": "AiAgentsResult",
            "method.rs": "ChatEventParams",
            "method.rs": "ChatEvent",
            "method.rs": "RegisterToolParams",
            "method.rs": "UnregisterToolParams",
            "method.rs": "ToolCallParams",
            "method.rs": "ToolResultParams",
        ),
    },
    Section {
        title: "能力面负载：表格",
        intro: "表格面读写当前打开的表格；真源在宿主会话，插件不得旁路读写 `.atb`。",
        items: items!(
            "method.rs": "TableSnapshot",
            "method.rs": "TableSelection",
            "method.rs": "TableRemoteSelection",
            "method.rs": "TableField",
            "method.rs": "TableRow",
            "method.rs": "TableUpdateCellParams",
            "method.rs": "TableAddRowResult",
            "method.rs": "TableRowIdParams",
            "method.rs": "TableSetCellImageParams",
        ),
    },
    Section {
        title: "日历服务（插件提供）",
        intro: "日程读写由预置插件 `com.atelyx.calendar` 以 `calendar` 服务开放，宿主不内置实现：\
调用方经 `service.call` 传 `service = calendar` 与下列方法名，或直接调 `calendar.<方法>`——两条路径落到\
同一登记。插件停用时该服务随之消失，调用方收到「服务不可用」。日程真源随容器走（本地仓库 \
`.atelyx/calendar.json` / 协作空间团队 meta）。",
        items: items!(
            "method.rs": "calendar",
            "method.rs": "CalendarItem",
            "method.rs": "CalendarListResult",
            "method.rs": "CalendarAddParams",
            "method.rs": "CalendarUpdateParams",
            "method.rs": "CalendarIdParams",
            "method.rs": "MAX_CALENDAR_TITLE_CHARS",
        ),
    },
    Section {
        title: "能力面负载：仓库活动",
        intro: "按天聚合的文件活动计数与当日版本记录；取自当前仓库（本地历史侧文件 / 空间聚合），\
按本地日界归日。",
        items: items!(
            "method.rs": "DayActivity",
            "method.rs": "HistoryActivityResult",
            "method.rs": "HistoryDayParams",
            "method.rs": "HistoryDayEntry",
            "method.rs": "HistoryDayResult",
            "method.rs": "HISTORY_KINDS",
            "method.rs": "HistoryOpenParams",
        ),
    },
    Section {
        title: "能力面负载：托盘与协作",
        intro: "系统托盘条目与空间内插件间消息的负载形态；两类能力的可用性都随宿主平台与容器类型而变。",
        items: items!(
            "method.rs": "TrayItemSpec",
            "method.rs": "SetTrayItemsParams",
            "method.rs": "TrayClickParams",
            "method.rs": "CollabPeer",
            "method.rs": "CollabPeersResult",
            "method.rs": "CollabSendQos",
            "method.rs": "CollabSendParams",
            "method.rs": "CollabSendResult",
            "method.rs": "CollabMessageParams",
            "method.rs": "MAX_META_KEY_BYTES",
            "method.rs": "MAX_META_VALUE_BYTES",
            "method.rs": "CollabMetaGetParams",
            "method.rs": "CollabMetaResult",
            "method.rs": "CollabMetaEntry",
            "method.rs": "CollabMetaPatchParams",
        ),
    },
    Section {
        title: "宿主广播事件",
        intro: "插件经 `event.subscribe` 声明订阅的事件名与载荷；未上表的名字宿主拒绝订阅。",
        items: items!(
            "event.rs": "SUBSCRIBE",
            "event.rs": "HOST_PUBLISH",
            "event.rs": "name",
            "event.rs": "VaultSwitchPayload",
            "event.rs": "TableChangedPayload",
            "event.rs": "TableSelectionChangedPayload",
            "event.rs": "ChatChangedPayload",
            "event.rs": "ChatChangeKind",
            "event.rs": "FileChangedPayload",
            "event.rs": "FileChangeKind",
            "event.rs": "SpaceMetaChangedPayload",
            "event.rs": "PublishParams",
            "event.rs": "SubscribeParams",
            "event.rs": "SubscribeResult",
        ),
    },
];

/// 渲染整份文档。
pub fn render(root: &Path) -> Result<String, String> {
    let mut sources: HashMap<&str, String> = HashMap::new();
    let mut out = String::from(HEADER);
    for (index, section) in SECTIONS.iter().enumerate() {
        out.push_str(&format!(
            "\n## {}. {}\n\n{}\n",
            index + 1,
            section.title,
            section.intro
        ));
        for item in section.items {
            if !sources.contains_key(item.file) {
                let path = root.join(SRC_DIR).join(item.file);
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
                sources.insert(item.file, text);
            }
            let source = &sources[item.file];
            let pasted = extract_item(source, item.name)
                .map_err(|e| format!("{}：{e}", item.file))?;
            out.push_str(&format!(
                "\n```rust\n// {SRC_DIR}/{}\n{pasted}\n```\n",
                item.file
            ));
        }
    }
    Ok(out)
}

/// 抽取一个顶层项的原文：含紧邻的 `///` 文档与 `#[serde…]` 属性，去掉 `#[derive…]` 噪音。
fn extract_item(source: &str, name: &str) -> Result<String, String> {
    let lines: Vec<&str> = source.lines().collect();
    let header = lines
        .iter()
        .position(|line| is_item_header(line, name))
        .ok_or_else(|| format!("源码里找不到 {name}"))?;
    let mut start = header;
    while start > 0 {
        let prev = lines[start - 1];
        // 只带紧邻的文档与属性；空行或 `//!` 模块文档即断开
        if prev.starts_with("///") || prev.starts_with("#[derive(") || prev.starts_with("#[serde(") {
            start -= 1;
        } else {
            break;
        }
    }
    let end = item_end(&lines, header).ok_or_else(|| format!("{name} 的定义没有闭合"))?;
    let body: Vec<&str> = lines[start..=end]
        .iter()
        .copied()
        .filter(|line| !line.starts_with("#[derive("))
        .collect();
    // 抽取边界必须停在项本身：落进测试模块说明收尾符没认出（`];` / `}`），会把它整段贴进对外文档
    if body.iter().any(|line| line.trim_start().starts_with("#[cfg(test)]")) {
        return Err(format!("{name} 的抽取结果越过了项边界（落进了测试模块），抽取收尾符需修正"));
    }
    // 括号配平作为抽取边界的完整性校验：文档注释里的花括号不参与统计
    let code: Vec<&&str> = body
        .iter()
        .filter(|line| !line.trim_start().starts_with("///"))
        .collect();
    let opens: usize = code.iter().map(|line| line.matches('{').count()).sum();
    let closes: usize = code.iter().map(|line| line.matches('}').count()).sum();
    if opens != closes {
        return Err(format!(
            "{name} 的抽取结果括号不配平（{opens} 开 / {closes} 闭），抽取边界需修正"
        ));
    }
    Ok(body.join("\n"))
}

/// 是否是目标项的头部行（顶层项，列 0 起头）。
fn is_item_header(line: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix("pub ") else {
        return false;
    };
    ["struct ", "enum ", "mod ", "const ", "fn "].iter().any(|kind| {
        rest.strip_prefix(kind).is_some_and(|tail| {
            let ident: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            ident == name
        })
    })
}

/// 项的收尾行：单行项到 `;` 为止；数组 / 元组这类常量到列 0 的 `];` 为止；
/// 其余（结构体 / 枚举 / 模块）到列 0 的 `}`。
fn item_end(lines: &[&str], header: usize) -> Option<usize> {
    let first = lines[header].trim_end();
    if first.ends_with(';') {
        return Some(header);
    }
    if first.ends_with("= &[") || first.ends_with("= [") {
        return (header + 1..lines.len()).find(|&index| lines[index] == "];");
    }
    (header + 1..lines.len()).find(|&index| lines[index] == "}")
}
