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
            "envelope.rs": "RequestId",
            "envelope.rs": "RpcError",
        ),
    },
    Section {
        title: "界面控件树",
        intro: "插件提交的声明式控件树：容器带 `children`，叶子不带；`id` 在同一棵树内唯一，\
事件按节点 `events` 声明回发。",
        items: items!(
            "view.rs": "ViewNode",
            "view.rs": "NodeBody",
            "view.rs": "Align",
            "view.rs": "Justify",
            "view.rs": "Axis",
            "view.rs": "BoxSize",
            "view.rs": "SizeSpec",
            "view.rs": "RowHeight",
            "view.rs": "Interaction",
            "view.rs": "InteractionState",
            "view.rs": "Surface",
            "view.rs": "ColorSpec",
            "view.rs": "Rgba",
            "view.rs": "TextFamily",
            "view.rs": "TextWeight",
            "view.rs": "ColorToken",
            "view.rs": "ButtonVariant",
            "view.rs": "CardTone",
            "view.rs": "ControlSize",
            "view.rs": "ImageFit",
            "view.rs": "ImageSource",
            "view.rs": "MenuItem",
            "view.rs": "PaddingSpec",
            "view.rs": "AxesPadding",
            "view.rs": "EdgesPadding",
        ),
    },
    Section {
        title: "视图事件与指针",
        intro: "宿主回发给插件的事件种类与取值形态；画布指针事件单列在下一节。",
        items: items!(
            "view.rs": "ReportMode",
            "view.rs": "EventKind",
            "view.rs": "ViewEvent",
            "view.rs": "LinkForm",
            "view.rs": "EventValue",
            "method.rs": "ViewEventParams",
        ),
    },
    Section {
        title: "自绘画布",
        intro: "画布是树里的叶子控件（占位 + 指针事件），内容经 `canvas.submit` 整发整换；\
坐标一律画布本地逻辑像素、左上原点。",
        items: items!(
            "view.rs": "CanvasEventKind",
            "view.rs": "MouseButton",
            "view.rs": "CanvasPointer",
            "canvas.rs": "CanvasColor",
            "canvas.rs": "CanvasPoint",
            "canvas.rs": "DrawCommand",
            "method.rs": "CanvasSubmitParams",
            "method.rs": "CanvasResizedParams",
        ),
    },
    Section {
        title: "贡献点与窗口面",
        intro: "贡献点决定插件界面出现在哪里；`surfaceId` 即贡献 id（插件窗口与文档实例另有宿主分配的 id）。",
        items: items!(
            "method.rs": "ContributionSlot",
            "method.rs": "SurfaceKind",
            "method.rs": "SurfaceAttachedParams",
            "method.rs": "SurfaceDetachedParams",
            "method.rs": "SubmitParams",
            "method.rs": "SubmitResult",
            "method.rs": "RegisterParams",
            "method.rs": "UnregisterParams",
            "method.rs": "DefineIconParams",
            "method.rs": "ScrollEdge",
            "method.rs": "ScrollToParams",
            "method.rs": "TakeoverScope",
            "method.rs": "TakeoverParams",
            "method.rs": "WindowOpenParams",
            "method.rs": "WindowCloseParams",
            "method.rs": "WindowUpdateParams",
            "method.rs": "WindowIdParams",
            "method.rs": "WindowClosedParams",
        ),
    },
    Section {
        title: "能力面方法名",
        intro: "每个模块是一类能力面的方法名常量，与 `permissions` 里的能力声明一一对应；\
未声明能力的调用会被宿主拒绝并记入审计。",
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
            "method.rs": "canvas",
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
            "method.rs": "ExistsResult",
            "method.rs": "ImageInfoResult",
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
            "method.rs": "ChatSendParams",
            "method.rs": "ChatSendResult",
            "method.rs": "ChatStopParams",
            "method.rs": "ChatHistoryParams",
            "method.rs": "ChatRenameParams",
            "method.rs": "ChatSessionParams",
            "method.rs": "ChatSessionSummary",
            "method.rs": "ChatSessionWire",
            "method.rs": "ChatHistoryResult",
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
            "method.rs": "TableField",
            "method.rs": "TableRow",
            "method.rs": "TableUpdateCellParams",
            "method.rs": "TableAddRowResult",
            "method.rs": "TableRowIdParams",
            "method.rs": "TableSetCellImageParams",
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
            "method.rs": "CollabSendParams",
            "method.rs": "CollabSendResult",
            "method.rs": "CollabMessageParams",
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
            "event.rs": "ChatChangedPayload",
            "event.rs": "ChatChangeKind",
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
    ["struct ", "enum ", "mod ", "const "].iter().any(|kind| {
        rest.strip_prefix(kind).is_some_and(|tail| {
            let ident: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            ident == name
        })
    })
}

/// 项的收尾行：单行项到 `;` 为止，其余到列 0 的 `}`。
fn item_end(lines: &[&str], header: usize) -> Option<usize> {
    if lines[header].trim_end().ends_with(';') {
        return Some(header);
    }
    (header + 1..lines.len()).find(|&index| lines[index] == "}")
}
