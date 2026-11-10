//! Agent 工具协议与内置工具：名册（schema 声明）、参数校验、执行与结果回传文本。
//!
//! Agent 工具名册与执行：内置工具按分类归组，插件工具由宿主名册单列；文件写经 `WriteQueue` 与壳同出口。
//! 路径一律仓库相对，执行前做越界校验；结果一律以文本回传模型。

use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use serde_json::{json, Value};

use crate::space_client::SpaceClient;
use crate::vault;
use crate::write_queue::WriteQueue;

/// 单次工具执行的回传（text 即模型可见结果；is_error 供展示层标红）。
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
        }
    }

    fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
        }
    }
}

/// 内置工具分类（Agent 设置页按此分组，声明顺序即展示顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Web,
    Read,
    Edit,
    Task,
}

impl ToolCategory {
    /// 全部分类（展示顺序）。
    pub const ALL: [ToolCategory; 4] = [Self::Web, Self::Read, Self::Edit, Self::Task];

    /// 分组显示名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Web => "联网",
            Self::Read => "读取",
            Self::Edit => "编辑与整理",
            Self::Task => "任务清单",
        }
    }
}

/// 工具名册项：schema、展示元数据与执行语义的唯一真源。
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    /// 设置页与对话工具行的中文名。
    pub label: &'static str,
    /// 工具行图标（`ui::icon` 的形状名）。
    pub icon: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub category: ToolCategory,
    /// 只读工具：画布自动落卡按此分输入卡，只读 Agent 的工具校验按此排除写工具。
    pub read_only: bool,
    /// 与其他 parallel_safe 工具并发执行（读写/未知工具串行屏障）。
    pub parallel_safe: bool,
}

/// 内置名册（与仓库 agents.json 的工具 id 集一致）。
pub fn builtin_tools() -> &'static [ToolDef] {
    static ROSTER: OnceLock<Vec<ToolDef>> = OnceLock::new();
    ROSTER.get_or_init(|| {
        use ToolCategory::{Edit as E, Read as R, Task as T, Web as W};
        vec![
            ToolDef {
                name: "web_search",
                label: "联网搜索",
                icon: "globe",
                description: "联网搜索。返回标题、链接与摘要的结果列表。",
                parameters: json!({
                    "type": "object",
                    "properties": { "query": { "type": "string", "description": "搜索关键词" } },
                    "required": ["query"]
                }),
                category: W,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "web_fetch",
                label: "抓取网页",
                icon: "link",
                description: "抓取网页正文（纯文本）。用于读取搜索结果中的具体页面。",
                parameters: json!({
                    "type": "object",
                    "properties": { "url": { "type": "string", "description": "完整的 http(s) URL" } },
                    "required": ["url"]
                }),
                category: W,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "read_file",
                label: "读取文件",
                icon: "file-text",
                description: "读取仓库内文本文件，返回带行号的正文（分页：默认前 2000 行，按页脚提示翻页）。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径" },
                        "offset": { "type": "integer", "description": "起始行（1 起，缺省 1）" },
                        "limit": { "type": "integer", "description": "行数（缺省 2000）" }
                    },
                    "required": ["path"]
                }),
                category: R,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "glob",
                label: "查找文件",
                icon: "files",
                description: "按 glob 模式枚举仓库文件（**/ 任意深度），返回相对路径列表；结果超上限会截断，收窄 pattern 或限定 path 后再查。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "glob 模式，如 \"笔记/**/*.md\"" },
                        "path": { "type": "string", "description": "限定子目录（缺省仓库根）" }
                    },
                    "required": ["pattern"]
                }),
                category: R,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "grep",
                label: "搜索内容",
                icon: "search",
                description: "在仓库文件内容中检索正则，返回命中文件与行预览；结果超上限会截断，收窄 pattern 或 include 后再查。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "正则表达式" },
                        "path": { "type": "string", "description": "限定子目录（缺省仓库根）" },
                        "include": { "type": "string", "description": "文件名过滤 glob，如 \"*.md\"" }
                    },
                    "required": ["pattern"]
                }),
                category: R,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "list_dir",
                label: "列出目录",
                icon: "folder",
                description: "列出目录下的文件与子目录（单层，标注类型）。",
                parameters: json!({
                    "type": "object",
                    "properties": { "dir": { "type": "string", "description": "仓库相对目录（缺省根）" } }
                }),
                category: R,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "read_history",
                label: "读取历史",
                icon: "history",
                description: "读取文件的历史版本列表（时间、作者、摘要）。",
                parameters: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string", "description": "仓库相对路径" } },
                    "required": ["path"]
                }),
                category: R,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "write_file",
                label: "写入文件",
                icon: "file-plus",
                description: "整文件写入（覆盖原内容）。先 read_file 确认现状；表格（.atb）与画布（.atlx）文件不支持用文件工具写入。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径" },
                        "content": { "type": "string", "description": "完整文件内容" }
                    },
                    "required": ["path", "content"]
                }),
                category: E,
                read_only: false,
                parallel_safe: false,
            },
            ToolDef {
                name: "append_file",
                label: "追加内容",
                icon: "file-plus",
                description: "在文件末尾追加内容（文件不存在则创建）；表格与画布文件不支持用文件工具写入。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径" },
                        "content": { "type": "string", "description": "追加内容" }
                    },
                    "required": ["path", "content"]
                }),
                category: E,
                read_only: false,
                parallel_safe: false,
            },
            ToolDef {
                name: "edit_file",
                label: "编辑文件",
                icon: "pen-line",
                description: "对文件做精确替换：缺省要求 find 唯一命中（多处命中报错），replace_all=true 替换全部出现；找不到即报错不写入。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径" },
                        "find": { "type": "string", "description": "要替换的原文（精确匹配）" },
                        "replace": { "type": "string", "description": "替换后的文本" },
                        "replace_all": { "type": "boolean", "description": "替换全部出现（缺省 false：find 必须唯一）" }
                    },
                    "required": ["path", "find", "replace"]
                }),
                category: E,
                read_only: false,
                parallel_safe: false,
            },
            ToolDef {
                name: "move",
                label: "移动 / 重命名",
                icon: "arrow-right",
                description: "移动或重命名文件与目录：from → to 全路径（同目录改名即重命名）。改名会同步更新仓库内引用链接。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "from": { "type": "string", "description": "源相对路径" },
                        "to": { "type": "string", "description": "目标相对路径" }
                    },
                    "required": ["from", "to"]
                }),
                category: E,
                read_only: false,
                parallel_safe: false,
            },
            ToolDef {
                name: "delete",
                label: "删除",
                icon: "trash-2",
                description: "删除文件或目录（不可恢复）。目录非空时需 force=true；仅在用户明确要求时使用。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径（文件或目录）" },
                        "force": { "type": "boolean", "description": "目录非空也删（缺省仅删空目录）" }
                    },
                    "required": ["path"]
                }),
                category: E,
                read_only: false,
                parallel_safe: false,
            },
            ToolDef {
                name: "todo_write",
                label: "任务清单",
                icon: "list",
                description: "写入当前任务清单（全量覆盖）。任务进行中时应维护此清单让用户看到进展。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "任务条目全量列表",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string", "description": "稳定 id（更新时保持不变）" },
                                    "text": { "type": "string", "description": "任务内容" },
                                    "done": { "type": "boolean", "description": "是否完成" }
                                },
                                "required": ["id", "text"]
                            }
                        }
                    },
                    "required": ["todos"]
                }),
                category: T,
                read_only: false,
                parallel_safe: false,
            },
        ]
    })
}

/// 按名取内置工具（插件工具不在名册内）。
pub fn builtin_tool(name: &str) -> Option<&'static ToolDef> {
    builtin_tools().iter().find(|t| t.name == name)
}

/// 工具显示名：内置取名册，未知（插件工具）原样返回 id。
pub fn tool_label(name: &str) -> &str {
    builtin_tool(name).map(|t| t.label).unwrap_or(name)
}

/// 工具行图标：内置取名册，未知（插件工具）用通用扳手。
pub fn tool_icon(name: &str) -> &str {
    builtin_tool(name).map(|t| t.icon).unwrap_or("wrench")
}

/// 联网搜索配置（运行前由调用方从仓库 AI 设置解析好；未配置 = web_search 降级报错文本）。
#[derive(Debug, Clone, Default)]
pub struct SearchConfig {
    pub provider: Option<String>,
    pub tavily_key: Option<String>,
    pub searxng_url: Option<String>,
}

/// 仓库访问后端：本地磁盘（写经写盘队列，与壳同出口）与协作空间（经服务端端点）。
#[derive(Clone)]
pub enum RepoAccess {
    Local { root: std::path::PathBuf, queue: WriteQueue },
    Space { client: Arc<SpaceClient>, space_id: String },
}

/// 工具执行环境：仓库访问后端 + 已解析的搜索配置 + 排除清单 + 插件工具。
#[derive(Clone)]
pub struct ToolCtx {
    pub repo: RepoAccess,
    pub search: SearchConfig,
    /// 仓库排除文件夹清单（检索工具跳过）。
    pub exclude: Vec<String>,
    /// 插件注册的 AI 工具（Agent 勾选按 `selection_id` 过滤，执行经运行时转发）。
    pub plugin_tools: Vec<crate::plugin_runtime::PluginToolSpec>,
    /// 插件运行时（`None` = 无插件工具可执行）。
    pub plugins: Option<Arc<crate::plugin_runtime::PluginRuntime>>,
    /// 内容写入出口（壳注入；`None` = 无宿主，写工具走直写兜底——单测与插件直调路径）。
    pub host: Option<Arc<dyn crate::ai::edit_host::AiEditHost>>,
    /// 本次 run 的操作者与 Agent 身份（历史署名 / 在场）。
    pub actor: Option<crate::ai::edit_host::EditActor>,
}

/// 把历史 user 消息里图片附件的路径引用读回 dataURL 填进 `payload`
/// （运行时字段，消息落盘时被序列化剥离）。单个附件读回失败 = payload 留空，
/// 该附件在请求组装时跳过，不拖垮整轮对话。非图片附件不读。
pub async fn resolve_image_attachments(messages: &mut [Value], repo: &RepoAccess) {
    for m in messages.iter_mut() {
        if m.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(atts) = m.get_mut("attachments").and_then(Value::as_array_mut) else {
            continue;
        };
        for a in atts.iter_mut() {
            if a.get("kind").and_then(Value::as_str) != Some("image") {
                continue;
            }
            if a.get("payload").and_then(Value::as_str).is_some_and(|p| !p.is_empty()) {
                continue;
            }
            let Some(rel) = a.get("file").and_then(Value::as_str) else {
                continue;
            };
            let mime = a
                .get("mime")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream");
            let bytes = match repo {
                RepoAccess::Local { root, .. } => vault::safe_join(root, rel, false)
                    .and_then(|path| std::fs::read(&path).map_err(|e| e.to_string())),
                RepoAccess::Space { client, space_id } => client
                    .get_file_bytes(space_id, rel)
                    .await
                    .map_err(|e| e.to_string()),
            };
            let Ok(bytes) = bytes else {
                // 失败可见性：stderr 留痕（该附件跳过，不拖垮整轮对话）
                eprintln!("图片附件读回失败，该附件不入本轮请求：{rel}");
                continue;
            };
            a["payload"] = json!(format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ));
        }
    }
}

/// 单行读取上限与结果文本上限（防一条结果撑爆上下文）。
const READ_LINE_CAP: usize = 2000;
const READ_CHARS_CAP: usize = 60_000;

/// 参数解析：缺失/类型不符即错误（错误文本回传模型，不 panic）。
fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolResult> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolResult::err(format!("工具参数错误：缺少 {key}")))
}

/// 执行内置工具与插件注册的工具。名称未知 = 错误文本；实现内部失败 = 错误文本
/// （不抛断对话循环）。插件工具只在当前上下文登记过时才可执行。
pub async fn execute(ctx: &ToolCtx, name: &str, args: Value) -> ToolResult {
    match name {
        "web_search" => web_search(ctx, &args).await,
        "web_fetch" => web_fetch(&args).await,
        "read_file" => read_file(ctx, &args).await,
        "glob" => glob(ctx, &args).await,
        "grep" => grep(ctx, &args).await,
        "list_dir" => list_dir(ctx, &args).await,
        "read_history" => read_history(ctx, &args).await,
        "write_file" => write_file(ctx, &args).await,
        "append_file" => append_file(ctx, &args).await,
        "edit_file" => edit_file(ctx, &args).await,
        "move" => move_tool(ctx, &args).await,
        "delete" => delete_tool(ctx, &args).await,
        "todo_write" => todo_write(ctx, &args).await,
        other => match ctx.plugin_tools.iter().find(|t| t.name == other) {
            Some(spec) => execute_plugin_tool(ctx, spec, args).await,
            None => ToolResult::err(format!("未知工具：{other}")),
        },
    }
}

/// 转发一次插件工具调用：结果文本与错误标记回传模型，超长结果截断。
async fn execute_plugin_tool(
    ctx: &ToolCtx,
    spec: &crate::plugin_runtime::PluginToolSpec,
    args: Value,
) -> ToolResult {
    let Some(runtime) = &ctx.plugins else {
        return ToolResult::err(format!("插件工具不可用：宿主运行时不可用（{}）", spec.plugin_id));
    };
    match runtime.call_tool(&spec.name, args).await {
        Ok(output) => {
            let mut text = output.content;
            if text.len() > READ_CHARS_CAP {
                let mut cut = READ_CHARS_CAP;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                text.push_str("…（结果过长截断）");
            }
            ToolResult {
                text,
                is_error: output.is_error,
            }
        }
        Err(e) => ToolResult::err(e),
    }
}

async fn web_search(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(query) = arg(args, "query") else {
        return ToolResult::err("工具参数错误：缺少 query");
    };
    let Some(provider) = ctx.search.provider.clone().filter(|p| !p.is_empty()) else {
        return ToolResult::err("未配置搜索源（设置 → AI 设置 → 联网搜索），无法联网搜索");
    };
    match crate::search::search_web(
        provider,
        query.to_string(),
        ctx.search.searxng_url.clone(),
        ctx.search.tavily_key.clone(),
    )
    .await
    {
        Ok(items) => {
            if items.is_empty() {
                return ToolResult::ok("无搜索结果");
            }
            let lines: Vec<String> = items
                .iter()
                .map(|i| format!("- [{}]({})：{}", i.title, i.url, i.snippet))
                .collect();
            ToolResult::ok(lines.join("\n"))
        }
        Err(e) => ToolResult::err(format!("联网搜索失败：{e}")),
    }
}

async fn web_fetch(args: &Value) -> ToolResult {
    let Ok(url) = arg(args, "url") else {
        return ToolResult::err("工具参数错误：缺少 url");
    };
    match crate::web::fetch_web(url.to_string()).await {
        Ok(page) => {
            let mut text = String::new();
            if let Some(title) = &page.title {
                text.push_str(&format!("标题：{title}\n\n"));
            }
            text.push_str(&page.content);
            if page.truncated {
                text.push_str("\n\n（内容过长已截断）");
            }
            ToolResult::ok(text)
        }
        Err(e) => ToolResult::err(format!("网页抓取失败：{e}")),
    }
}

/// 仓库相对路径校验：拒绝绝对路径 / `..` / 隐藏段（`.atelyx` 等不对模型开放）。
fn rel_path(rel: &str) -> Result<String, ToolResult> {
    vault::validate_tool_path(rel).map_err(ToolResult::err)
}

/// 单条检索命中（路径 / 行号 / 行文本）：本地与空间后端归一后的共用形状。
type GrepRow = (String, u64, String);

/// 行号分页渲染：本地与空间取到全文后共用同一口径（页脚续读位按实际渲染到的行给出）。
fn render_numbered(content: &str, args: &Value, rel: &str) -> ToolResult {
    let total = content.lines().count();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .map(|v| v.max(1) as usize - 1)
        .unwrap_or(0);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(READ_LINE_CAP)
        .clamp(1, READ_LINE_CAP);
    let start = offset.min(total);
    let end = (start + limit).min(total);
    let mut out = String::new();
    let mut shown_until = start;
    for (i, line) in content.lines().enumerate().skip(start).take(end - start) {
        out.push_str(&format!("{}| {line}\n", i + 1));
        shown_until = i + 1;
        if out.len() > READ_CHARS_CAP {
            out.push_str("…（结果过长截断）\n");
            break;
        }
    }
    if shown_until < total {
        out.push_str(&format!(
            "\n（共 {total} 行，显示 {}-{}；续读传 offset={}）",
            start + 1,
            shown_until,
            shown_until + 1
        ));
    } else if out.is_empty() {
        out.push_str(&format!("（空文件：{rel}）"));
    }
    ToolResult::ok(out)
}

async fn read_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    let content = match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            let path = match vault::safe_join(root, &rel, false) {
                Ok(path) => path,
                Err(e) => return ToolResult::err(e),
            };
            match std::fs::read(&path) {
                Ok(bytes) => {
                    if bytes[..bytes.len().min(4096)].contains(&0) {
                        return ToolResult::err(format!("二进制文件不支持读取：{rel}"));
                    }
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                Err(e) => return ToolResult::err(format!("文件不存在或不可读：{rel}（{e}）")),
            }
        }
        RepoAccess::Space { client, space_id } => match client.get_file_text(space_id, &rel).await {
            Ok(text) => text,
            Err(e) => return ToolResult::err(format!("文件不存在或不可读：{rel}（{e}）")),
        },
    };
    render_numbered(&content, args, &rel)
}

async fn glob(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(pattern) = arg(args, "pattern") else {
        return ToolResult::err("工具参数错误：缺少 pattern");
    };
    let sub = match args.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(path) => match rel_path(path) {
            Ok(path) => Some(path),
            Err(e) => return e,
        },
        None => None,
    };
    let result: Result<(Vec<String>, u64, bool), String> = match &ctx.repo {
        RepoAccess::Local { root, .. } => crate::filesearch::glob_vault_blocking(
            root.clone(),
            ctx.exclude.clone(),
            pattern.to_string(),
            sub.clone(),
        )
        .map(|r| (r.paths, r.total as u64, r.capped)),
        RepoAccess::Space { client, space_id } => client
            .glob(space_id, pattern, sub.as_deref())
            .await
            .map(|r| (r.paths, r.total, r.capped))
            .map_err(|e| e.to_string()),
    };
    match result {
        Ok((paths, total, capped)) => {
            if paths.is_empty() {
                return ToolResult::ok("无匹配文件");
            }
            let mut text = paths.join("\n");
            if capped {
                text.push_str(&format!("\n…（共 {total} 条，已截断）"));
            }
            ToolResult::ok(text)
        }
        Err(e) => ToolResult::err(format!("检索失败：{e}")),
    }
}

async fn grep(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(pattern) = arg(args, "pattern") else {
        return ToolResult::err("工具参数错误：缺少 pattern");
    };
    let sub = match args.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(path) => match rel_path(path) {
            Ok(path) => Some(path),
            Err(e) => return e,
        },
        None => None,
    };
    let include = args
        .get("include")
        .and_then(Value::as_str)
        .map(str::to_string);
    let result: Result<(Vec<GrepRow>, u64, bool), String> = match &ctx.repo {
        RepoAccess::Local { root, .. } => crate::filesearch::grep_vault_blocking(
            root.clone(),
            ctx.exclude.clone(),
            pattern.to_string(),
            sub.clone(),
            include.clone(),
        )
        .map(|r| {
            let rows = r
                .matches
                .into_iter()
                .map(|m| (m.path, m.line_number as u64, m.line))
                .collect();
            (rows, r.total as u64, r.capped)
        }),
        RepoAccess::Space { client, space_id } => client
            .grep(space_id, pattern, sub.as_deref(), include.as_deref())
            .await
            .map(|r| {
                let rows = r
                    .matches
                    .into_iter()
                    .map(|m| (m.path, m.line_number, m.line))
                    .collect();
                (rows, r.total, r.capped)
            })
            .map_err(|e| e.to_string()),
    };
    match result {
        Ok((matches, total, capped)) => {
            if matches.is_empty() {
                return ToolResult::ok("无命中");
            }
            let mut text = String::new();
            for (path, line_number, line) in matches.iter() {
                text.push_str(&format!("{path}:{line_number}: {line}\n"));
                if text.len() > READ_CHARS_CAP {
                    text.push_str("…（结果过长截断）\n");
                    break;
                }
            }
            if capped {
                text.push_str(&format!("…（共 {total} 条命中，已截断）"));
            }
            ToolResult::ok(text)
        }
        Err(e) => ToolResult::err(format!("检索失败：{e}")),
    }
}

async fn list_dir(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let dir = args
        .get("dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let dir = if dir.is_empty() {
        String::new()
    } else {
        match rel_path(dir) {
            Ok(dir) => dir,
            Err(e) => return e,
        }
    };
    let mut rows: Vec<String> = match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            let path = if dir.is_empty() {
                root.clone()
            } else {
                match vault::safe_join(root, &dir, false) {
                    Ok(path) => path,
                    Err(e) => return ToolResult::err(e),
                }
            };
            let Ok(entries) = std::fs::read_dir(&path) else {
                return ToolResult::err(format!(
                    "目录不存在或不可读：{}",
                    if dir.is_empty() { "." } else { dir.as_str() }
                ));
            };
            let mut rows = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue; // 隐藏项不对模型开放
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                rows.push(if is_dir { format!("{name}/") } else { name });
            }
            rows
        }
        RepoAccess::Space { client, space_id } => match client.tree(space_id).await {
            Ok(tree) => space_dir_rows(&tree, &dir),
            Err(e) => return ToolResult::err(format!("目录不存在或不可读：{e}")),
        },
    };
    rows.sort();
    if rows.is_empty() {
        ToolResult::ok("（空目录）")
    } else {
        ToolResult::ok(rows.join("\n"))
    }
}

/// 空间整树 → 目标目录的直接子项（隐藏项过滤；目录带尾 `/`）。
fn space_dir_rows(tree: &[crate::space_client::TreeNode], dir: &str) -> Vec<String> {
    let children: &[crate::space_client::TreeNode] = if dir.is_empty() {
        tree
    } else {
        match find_tree_node(tree, dir) {
            Some(node) => &node.children,
            None => return Vec::new(),
        }
    };
    children
        .iter()
        .filter(|node| !node.name.starts_with('.'))
        .map(|node| {
            if node.is_dir {
                format!("{}/", node.name)
            } else {
                node.name.clone()
            }
        })
        .collect()
}

fn find_tree_node<'a>(
    nodes: &'a [crate::space_client::TreeNode],
    path: &str,
) -> Option<&'a crate::space_client::TreeNode> {
    for node in nodes {
        if node.path == path {
            return Some(node);
        }
        if let Some(hit) = find_tree_node(&node.children, path) {
            return Some(hit);
        }
    }
    None
}

async fn read_history(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    let versions = match &ctx.repo {
        RepoAccess::Local { root, .. } => Ok(crate::history::load_history(
            root,
            crate::history::HistoryKind::Note,
            &rel,
        )),
        RepoAccess::Space { client, space_id } => client
            .history_versions(space_id, crate::history::HistoryKind::Note, &rel)
            .await
            .map_err(|e| e.to_string()),
    };
    let versions = match versions {
        Ok(versions) => versions,
        Err(e) => return ToolResult::err(format!("历史读取失败：{e}")),
    };
    if versions.is_empty() {
        return ToolResult::ok("无历史版本");
    }
    let lines: Vec<String> = versions
        .iter()
        .map(|v| {
            let editor = v
                .editor
                .as_ref()
                .map(|e| format!("（AI：{}）", e.agent_name))
                .unwrap_or_default();
            format!(
                "- seq {} {} {}{editor}: {}",
                v.seq,
                v.ts,
                v.author.name,
                v.summary.as_deref().unwrap_or("-")
            )
        })
        .collect();
    ToolResult::ok(lines.join("\n"))
}

/// 表格 / 画布文件禁止整文件写：绕过补丁仲裁、历史与协作广播。
fn structured_write_guard(rel: &str) -> Result<(), ToolResult> {
    let lower = rel.to_ascii_lowercase();
    let kind = if lower.ends_with(".atb") {
        "表格"
    } else if lower.ends_with(".atlx") {
        "画布"
    } else {
        return Ok(());
    };
    Err(ToolResult::err(format!(
        "{rel} 是{kind}文件，不支持用文件工具写入（会绕过补丁合并与历史）；请在{kind}视图中编辑"
    )))
}

/// 有宿主时把笔记写入交给宿主保存链（历史 / 协作 / 打开会话）；无宿主 = 返回 None 走直写兜底。
async fn apply_via_host(
    ctx: &ToolCtx,
    rel: &str,
    op: crate::ai::edit_host::NoteOp,
) -> Option<ToolResult> {
    let host = ctx.host.as_ref()?;
    let Some(actor) = ctx.actor.as_ref() else {
        return Some(ToolResult::err("AI 编辑身份缺失，已拒绝写入"));
    };
    Some(match host.apply_note(rel, op, actor).await {
        Ok(text) => ToolResult::ok(text),
        Err(e) => ToolResult::err(e),
    })
}

async fn write_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let (Ok(rel), Some(content)) = (
        arg(args, "path").map(str::to_string),
        args.get("content").and_then(Value::as_str),
    ) else {
        return ToolResult::err("工具参数错误：需要 path 与 content");
    };
    let rel = match rel_path(&rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Err(e) = structured_write_guard(&rel) {
        return e;
    }
    if let Some(result) =
        apply_via_host(ctx, &rel, crate::ai::edit_host::NoteOp::Write(content.to_string())).await
    {
        return result;
    }
    match &ctx.repo {
        RepoAccess::Local { root, queue } => {
            let path = match vault::safe_join(root, &rel, true) {
                Ok(path) => path,
                Err(e) => return ToolResult::err(e),
            };
            queue
                .write(path, content.to_string())
                .await
                .map(|_| ToolResult::ok(format!("已写入 {rel}")))
                .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
        }
        RepoAccess::Space { client, space_id } => client
            .put_file_text(space_id, &rel, content)
            .await
            .map(|_| ToolResult::ok(format!("已写入 {rel}")))
            .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}"))),
    }
}

async fn append_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let (Ok(rel), Some(content)) = (
        arg(args, "path").map(str::to_string),
        args.get("content").and_then(Value::as_str),
    ) else {
        return ToolResult::err("工具参数错误：需要 path 与 content");
    };
    let rel = match rel_path(&rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Err(e) = structured_write_guard(&rel) {
        return e;
    }
    if let Some(result) =
        apply_via_host(ctx, &rel, crate::ai::edit_host::NoteOp::Append(content.to_string())).await
    {
        return result;
    }
    match &ctx.repo {
        RepoAccess::Local { root, queue } => {
            let root = root.clone();
            let target = rel.clone();
            let content = content.to_string();
            // 读改写整段落在写盘队列内：与壳的笔记保存同一条串行链，避免两侧互相覆盖
            queue
                .bulk(move || {
                    let path = vault::safe_join(&root, &target, true)?;
                    let existing = read_existing_text(&path, &target)?;
                    vault::write_note(&root, &target, &format!("{existing}{content}"))
                })
                .await
                .map(|_| ToolResult::ok(format!("已追加到 {rel}")))
                .unwrap_or_else(|e| ToolResult::err(format!("追加失败：{e}")))
        }
        RepoAccess::Space { client, space_id } => {
            // 空间端点无原子读改写：读与写之间他方（其他设备/成员）的写入会被本次整体覆盖，
            // 这是服务端契约约束下的已知窗口（本地分支经写盘队列串行化，两侧出口语义不同源）
            let existing = match client.get_file_text(space_id, &rel).await {
                Ok(text) => text,
                Err(crate::space_client::SpaceError::NotFound) => String::new(),
                Err(e) => return ToolResult::err(format!("文件不可读：{rel}（{e}）")),
            };
            client
                .put_file_text(space_id, &rel, &format!("{existing}{content}"))
                .await
                .map(|_| ToolResult::ok(format!("已追加到 {rel}")))
                .unwrap_or_else(|e| ToolResult::err(format!("追加失败：{e}")))
        }
    }
}

async fn edit_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let Ok(find) = arg(args, "find") else {
        return ToolResult::err("工具参数错误：缺少 find");
    };
    let Some(replace) = args.get("replace").and_then(Value::as_str) else {
        return ToolResult::err("工具参数错误：缺少 replace");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Err(e) = structured_write_guard(&rel) {
        return e;
    }
    let all = args.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    let find = find.to_string();
    let replace = replace.to_string();
    if let Some(result) = apply_via_host(
        ctx,
        &rel,
        crate::ai::edit_host::NoteOp::Replace {
            find: find.clone(),
            replace: replace.clone(),
            all,
        },
    )
    .await
    {
        return result;
    }
    match &ctx.repo {
        RepoAccess::Local { root, queue } => {
            let root = root.clone();
            let target = rel.clone();
            let outcome = queue
                .bulk(move || {
                    let path = vault::safe_join(&root, &target, false)?;
                    let existing = read_existing_text(&path, &target)?;
                    let (next, count) = crate::ai::edit_host::replace_exact(&existing, &find, &replace, all)?;
                    vault::write_note(&root, &target, &next).map(|_| count)
                })
                .await;
            match outcome {
                Ok(count) => ToolResult::ok(format!("已编辑 {rel}（替换 {count} 处）")),
                Err(e) => ToolResult::err(e),
            }
        }
        RepoAccess::Space { client, space_id } => {
            let existing = match client.get_file_text(space_id, &rel).await {
                Ok(text) => text,
                Err(e) => return ToolResult::err(format!("文件不存在或不可读：{rel}（{e}）")),
            };
            let (next, count) = match crate::ai::edit_host::replace_exact(&existing, &find, &replace, all) {
                Ok(value) => value,
                Err(e) => return ToolResult::err(e),
            };
            client
                .put_file_text(space_id, &rel, &next)
                .await
                .map(|_| ToolResult::ok(format!("已编辑 {rel}（替换 {count} 处）")))
                .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
        }
    }
}
/// 读既有文本：不存在 = 空（新建），不可读 = 错误（不得当空内容覆盖）。
fn read_existing_text(path: &std::path::Path, rel: &str) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    std::fs::read_to_string(path).map_err(|e| format!("文件不可读：{rel}（{e}）"))
}

/// 移动 / 重命名工具：改名 = 同目录 move（引用同步与空间端点共用一套实现）。
async fn move_tool(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(from) = arg(args, "from") else {
        return ToolResult::err("工具参数错误：缺少 from");
    };
    let Ok(to) = arg(args, "to") else {
        return ToolResult::err("工具参数错误：缺少 to");
    };
    let from = match rel_path(from) {
        Ok(from) => from,
        Err(e) => return e,
    };
    let to = match rel_path(to) {
        Ok(to) => to,
        Err(e) => return e,
    };
    if from == to {
        return ToolResult::err("from 与 to 相同，无需移动");
    }
    move_entry(ctx, &from, &to, &format!("已移动 {from} → {to}")).await
}

/// 改名 / 移动共用实现：本地经引用同步（全库 `.md` 链接、元数据路径一并改写），
/// 空间走服务端 rename（客户端引用改写由壳负责，与文件树操作同链）。
async fn move_entry(ctx: &ToolCtx, from: &str, to: &str, ok: &str) -> ToolResult {
    match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            let is_dir = vault::safe_join(root, from, false)
                .map(|path| path.is_dir())
                .unwrap_or(false);
            let synced = if is_dir {
                crate::vault_refs::rename_folder_synced(root, &ctx.exclude, from, to)
            } else {
                crate::vault_refs::rename_note_synced(root, &ctx.exclude, from, to)
            };
            synced
                .map(|_| ToolResult::ok(ok.to_string()))
                .unwrap_or_else(|e| ToolResult::err(format!("操作失败：{e}")))
        }
        RepoAccess::Space { client, space_id } => client
            .rename_file(space_id, from, to)
            .await
            .map(|_| ToolResult::ok(ok.to_string()))
            .unwrap_or_else(|e| ToolResult::err(format!("操作失败：{e}"))),
    }
}

/// 删除工具：按路径类型分派文件 / 目录（目录非空需 force），空间侧按树节点类型判定。
async fn delete_tool(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            let path = match vault::safe_join(root, &rel, false) {
                Ok(path) => path,
                Err(e) => return ToolResult::err(e),
            };
            if path.is_dir() {
                delete_dir_entry(root, &rel, force)
            } else if path.is_file() {
                vault::delete_vault_file(root, &rel)
                    .map(|_| ToolResult::ok(format!("已删除 {rel}")))
                    .unwrap_or_else(|e| ToolResult::err(format!("删除失败：{e}")))
            } else {
                ToolResult::err(format!("路径不存在：{rel}"))
            }
        }
        RepoAccess::Space { client, space_id } => {
            let tree = match client.tree(space_id).await {
                Ok(tree) => tree,
                Err(e) => return ToolResult::err(format!("路径不存在或不可读：{e}")),
            };
            let Some(node) = find_tree_node(&tree, &rel) else {
                return ToolResult::err(format!("路径不存在：{rel}"));
            };
            if node.is_dir {
                match client.delete_folder(space_id, &rel, force).await {
                    Ok(ack) if ack.deleted => ToolResult::ok(format!("已删除目录 {rel}")),
                    Ok(_) => ToolResult::err(format!("目录非空（force 可强制删除）：{rel}")),
                    Err(e) => ToolResult::err(format!("删除失败：{e}")),
                }
            } else {
                client
                    .delete_file(space_id, &rel)
                    .await
                    .map(|_| ToolResult::ok(format!("已删除 {rel}")))
                    .unwrap_or_else(|e| ToolResult::err(format!("删除失败：{e}")))
            }
        }
    }
}

/// 本地目录删除：空目录 / force 两种结果同一口径（`deleted = false` 即非空未删）。
fn delete_dir_entry(root: &std::path::Path, rel: &str, force: bool) -> ToolResult {
    match vault::delete_folder(root, rel, force) {
        Ok(result) if result.deleted => ToolResult::ok(format!("已删除目录 {rel}")),
        Ok(_) => ToolResult::err(format!("目录非空（force 可强制删除）：{rel}")),
        Err(e) => ToolResult::err(format!("删除失败：{e}")),
    }
}
/// 任务清单落点（仓库相对；空间落用户元数据同键）。
pub const TODO_FILE: &str = ".atelyx/todos/agent.json";

async fn todo_write(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Some(todos) = args.get("todos").and_then(Value::as_array) else {
        return ToolResult::err("工具参数错误：缺少 todos");
    };
    let cleaned: Vec<Value> = todos
        .iter()
        .filter_map(|t| {
            let id = t.get("id").and_then(Value::as_str)?;
            let text = t.get("text").and_then(Value::as_str)?;
            Some(json!({
                "id": id,
                "text": text,
                "done": t.get("done").and_then(Value::as_bool).unwrap_or(false),
            }))
        })
        .collect();
    let payload = json!({
        "schema": 1,
        "updatedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        "todos": cleaned,
    });
    let Ok(content) = serde_json::to_string_pretty(&payload) else {
        return ToolResult::err("任务清单序列化失败".to_string());
    };
    match &ctx.repo {
        // 落点是工具私有路径（`.atelyx/`），不走 AI 路径校验
        RepoAccess::Local { root, queue } => {
            let path = root.join(TODO_FILE);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            queue
                .write(path, content)
                .await
                .map(|_| ToolResult::ok(format!("任务清单已更新（{} 项）", todos.len())))
                .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
        }
        RepoAccess::Space { client, space_id } => client
            .my_meta_patch(space_id, &[(TODO_FILE.to_string(), content)])
            .await
            .map(|_| ToolResult::ok(format!("任务清单已更新（{} 项）", todos.len())))
            .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn ctx(dir: &TempDir) -> ToolCtx {
        ToolCtx {
            repo: RepoAccess::Local {
                root: dir.to_path_buf(),
                queue: WriteQueue::new(),
            },
            search: SearchConfig::default(),
            exclude: Vec::new(),
            plugin_tools: Vec::new(),
            plugins: None,
            host: None,
            actor: None,
        }
    }

    /// 本地写盘队列（测试等待落盘用）。
    fn queue_of(ctx: &ToolCtx) -> &WriteQueue {
        match &ctx.repo {
            RepoAccess::Local { queue, .. } => queue,
            RepoAccess::Space { .. } => panic!("测试上下文恒为本地后端"),
        }
    }

    #[test]
    fn roster_is_unique_and_fully_described() {
        let tools = builtin_tools();
        assert_eq!(tools.len(), 13, "名册长度");
        for (i, tool) in tools.iter().enumerate() {
            assert!(!tool.label.is_empty(), "{} 缺显示名", tool.name);
            assert!(!tool.icon.is_empty(), "{} 缺图标", tool.name);
            assert!(!tool.description.is_empty(), "{} 缺描述", tool.name);
            assert!(tool.parameters.get("type").is_some(), "{} 缺参数 schema", tool.name);
            assert!(
                tools[i + 1..].iter().all(|other| other.name != tool.name),
                "{} 重名",
                tool.name
            );
        }
        // 每组至少一项（设置页分组不出现空组）
        for cat in ToolCategory::ALL {
            assert!(tools.iter().any(|t| t.category == cat), "{} 分组为空", cat.label());
        }
        // 只读工具可并行、写工具串行屏障
        assert!(tools.iter().filter(|t| t.read_only).all(|t| t.parallel_safe));
        assert!(tools.iter().filter(|t| !t.read_only).all(|t| !t.parallel_safe));
        // 旧 id 已合并：按名取新工具、旧名不再在册
        assert!(builtin_tool("move").is_some() && builtin_tool("rename_file").is_none());
        assert!(builtin_tool("delete").is_some() && builtin_tool("delete_dir").is_none());
        assert_eq!(tool_label("write_file"), "写入文件");
        assert_eq!(tool_label("plugin:x:y"), "plugin:x:y");
        assert_eq!(tool_icon("plugin:x:y"), "wrench");
    }

    #[test]
    fn builtin_agents_track_roster_and_chat_stays_read_only() {
        let agents = crate::vault::builtin_agents();
        let roster: Vec<&str> = builtin_tools().iter().map(|t| t.name).collect();
        for agent in &agents {
            for id in &agent.tools {
                assert!(roster.contains(&id.as_str()), "{} 引用了名册外工具 {id}", agent.name);
            }
        }
        // 只读 Agent 不得含写工具（新增写工具漏进缺省对话集即失败）
        let chat = &agents[0];
        assert!(
            chat.tools
                .iter()
                .all(|id| builtin_tool(id).is_some_and(|t| t.read_only)),
            "只读 Agent 含写工具：{:?}",
            chat.tools
        );
        // 全量 Agent 覆盖整个名册
        let full = &agents[1];
        for name in roster {
            assert!(full.tools.iter().any(|t| t == name), "全量 Agent 缺 {name}");
        }
    }

    #[test]
    fn legacy_agent_tool_ids_migrate_on_load() {
        let dir = TempDir::new("ai-agents");
        std::fs::create_dir_all(dir.join(".atelyx")).unwrap();
        std::fs::write(
            dir.join(".atelyx/agents.json"),
            r#"{"schema":"1","agents":[{"id":"a","name":"A","tools":["rename_file","move_file","delete_file","delete_dir","read_file"]}]}"#,
        )
        .unwrap();
        let agents = crate::vault::read_agents_file(&dir.to_path_buf()).unwrap();
        assert_eq!(agents[0].tools, vec!["move", "delete", "read_file"]);
    }

    #[test]
    fn ai_path_rules_reject_hidden_segments_and_escapes() {
        // 隐藏目录（仓库配置与密钥所在处）不对模型开放
        assert!(vault::validate_tool_path(".atelyx/config.json").is_err());
        assert!(vault::validate_tool_path("a/.git/y").is_err());
        assert!(vault::validate_tool_path("../x").is_err());
        assert!(vault::validate_tool_path("/x").is_err());
        // 普通仓库相对路径原样通过（分隔符归一为 `/`，空间端点直接可用）
        assert_eq!(vault::validate_tool_path("笔记/待办.md").unwrap(), "笔记/待办.md");
        assert_eq!(vault::validate_tool_path("./笔记/a.md").unwrap(), "笔记/a.md");
    }

    #[tokio::test]
    async fn image_attachments_resolve_to_data_urls_and_skip_failures() {
        let dir = TempDir::new("ai-attach");
        let root = dir.to_path_buf();
        let good = root.join(".atelyx/temp/chats/s1/img-ok.png");
        std::fs::create_dir_all(good.parent().unwrap()).unwrap();
        std::fs::write(&good, b"\x89PNG").unwrap();
        let mut messages = vec![
            json!({
                "id": "u1", "role": "user", "content": "看图", "createdAt": 1,
                "attachments": [
                    { "kind": "image", "mime": "image/png", "file": ".atelyx/temp/chats/s1/img-ok.png" },
                    { "kind": "image", "mime": "image/png", "file": ".atelyx/temp/chats/s1/img-missing.png" },
                    { "kind": "image", "mime": "image/png" }
                ]
            }),
            json!({ "id": "u2", "role": "user", "content": "无附件", "createdAt": 2 }),
        ];
        let repo = RepoAccess::Local { root, queue: WriteQueue::new() };
        resolve_image_attachments(&mut messages, &repo).await;
        let atts = messages[0]["attachments"].as_array().unwrap();
        assert!(atts[0]["payload"].as_str().unwrap().starts_with("data:image/png;base64,"));
        // 读回失败与缺引用：payload 不出现
        assert!(atts[1].get("payload").is_none());
        assert!(atts[2].get("payload").is_none());
        // 无附件消息不产生 attachments 键
        assert!(messages[1].get("attachments").is_none());
    }

    #[test]
    fn space_dir_rows_lists_direct_children_and_hides_dot_entries() {
        fn node(path: &str, is_dir: bool, children: Vec<crate::space_client::TreeNode>) -> crate::space_client::TreeNode {
            crate::space_client::TreeNode {
                name: path.rsplit('/').next().unwrap_or(path).to_string(),
                path: path.to_string(),
                is_dir,
                updated_at: 0,
                children,
            }
        }
        let tree = vec![
            node("笔记", true, vec![
                node("笔记/a.md", false, vec![]),
                node("笔记/子目录", true, vec![]),
            ]),
            node(".atelyx", true, vec![]),
            node("根文件.md", false, vec![]),
        ];
        // 根目录：隐藏项过滤
        assert_eq!(space_dir_rows(&tree, ""), vec!["笔记/", "根文件.md"]);
        // 子目录：列出直接子项，目录带尾斜杠
        assert_eq!(space_dir_rows(&tree, "笔记"), vec!["a.md", "子目录/"]);
        // 目录不存在 = 空（调用方按空目录呈现）
        assert!(space_dir_rows(&tree, "无此目录").is_empty());
    }

    #[tokio::test]
    async fn write_read_edit_roundtrip_and_guards() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        let r = execute(
            &ctx,
            "write_file",
            json!({ "path": "笔记/待办.md", "content": "# 待办\n\n- [x] 一" }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        let r = execute(&ctx, "read_file", json!({ "path": "笔记/待办.md" })).await;
        assert!(r.text.contains("1| # 待办"));
        // 追加
        let r = execute(
            &ctx,
            "append_file",
            json!({ "path": "笔记/待办.md", "content": "\n- [ ] 二" }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        let r = execute(&ctx, "read_file", json!({ "path": "笔记/待办.md" })).await;
        assert!(r.text.contains("3| - [x] 一") && r.text.contains("4| - [ ] 二"), "{}", r.text);
        // 编辑：找不到即报错不写
        let r = execute(
            &ctx,
            "edit_file",
            json!({ "path": "笔记/待办.md", "find": "不存在", "replace": "x" }),
        )
        .await;
        assert!(r.is_error);
        let r = execute(
            &ctx,
            "edit_file",
            json!({ "path": "笔记/待办.md", "find": "- [ ] 二", "replace": "- [x] 二" }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        let content =
            std::fs::read_to_string(dir.join("笔记/待办.md")).unwrap();
        assert!(content.contains("- [x] 二"));
        // 写盘队列异步落盘（等队列排空后校验磁盘）
        queue_of(&ctx).drain().await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("笔记/待办.md")).unwrap(),
            content
        );
    }

    #[tokio::test]
    async fn edit_file_requires_unique_match_unless_replace_all() {
        let dir = TempDir::new("ai-edit");
        let ctx = ctx(&dir);
        execute(&ctx, "write_file", json!({ "path": "a.md", "content": "项\n项\n" })).await;
        // 多处命中且未开 replace_all：报错并给出命中次数，磁盘不变
        let r = execute(&ctx, "edit_file", json!({ "path": "a.md", "find": "项", "replace": "条" })).await;
        assert!(r.is_error && r.text.contains("2 次"), "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "项\n项\n");
        // 显式 replace_all：全部替换
        let r = execute(
            &ctx,
            "edit_file",
            json!({ "path": "a.md", "find": "项", "replace": "条", "replace_all": true }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("替换 2 处"), "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.md")).unwrap(), "条\n条\n");
    }

    #[tokio::test]
    async fn structured_files_reject_file_tool_writes() {
        let dir = TempDir::new("ai-structured");
        let ctx = ctx(&dir);
        for (tool, args) in [
            ("write_file", json!({ "path": "表.atb", "content": "{}" })),
            ("append_file", json!({ "path": "画布.atlx", "content": "{}" })),
            ("edit_file", json!({ "path": "表.ATB", "find": "a", "replace": "b" })),
        ] {
            let r = execute(&ctx, tool, args).await;
            assert!(r.is_error && r.text.contains("补丁合并"), "{tool}：{}", r.text);
        }
        assert!(!dir.join("表.atb").exists() && !dir.join("画布.atlx").exists());
    }

    #[tokio::test]
    async fn path_guards_reject_escape_and_absolute() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        for bad in ["../逃逸.md", "C:\\abs.md", "/abs.md", "a/../../b.md", "..\\逃逸.md", "C:abs.md"] {
            let r = execute(&ctx, "write_file", json!({ "path": bad, "content": "x" })).await;
            assert!(r.is_error, "{bad} 应被拒绝");
        }
    }

    #[tokio::test]
    async fn unknown_tool_and_missing_args_are_errors() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        let r = execute(&ctx, "nope", json!({})).await;
        assert!(r.is_error && r.text.contains("未知工具"));
        let r = execute(&ctx, "read_file", json!({})).await;
        assert!(r.is_error && r.text.contains("缺少 path"));
    }

    #[tokio::test]
    async fn list_dir_move_and_delete() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        std::fs::create_dir_all(dir.join("子目录")).unwrap();
        std::fs::write(dir.join("子目录/a.md"), "A").unwrap();
        let r = execute(&ctx, "list_dir", json!({ "dir": "子目录" })).await;
        assert!(r.text.contains("a.md"));
        // 同目录改名 = move
        let r = execute(&ctx, "move", json!({ "from": "子目录/a.md", "to": "子目录/b.md" })).await;
        assert!(!r.is_error, "{}", r.text);
        assert!(!dir.join("子目录/a.md").exists());
        assert!(dir.join("子目录/b.md").exists());
        // 目录：无 force 报错，force 删除
        let r = execute(&ctx, "delete", json!({ "path": "子目录" })).await;
        assert!(r.is_error, "{}", r.text);
        let r = execute(&ctx, "delete", json!({ "path": "子目录", "force": true })).await;
        assert!(!r.is_error, "{}", r.text);
        assert!(!dir.join("子目录").exists());
        // 不存在的路径：可读报错
        let r = execute(&ctx, "delete", json!({ "path": "无此文件.md" })).await;
        assert!(r.is_error && r.text.contains("不存在"), "{}", r.text);
    }

    #[tokio::test]
    async fn todo_write_roundtrip() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        let r = execute(
            &ctx,
            "todo_write",
            json!({ "todos": [
                { "id": "t1", "text": "调研", "done": true },
                { "id": "t2", "text": "实现" }
            ] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        let on_disk = std::fs::read_to_string(dir.join(TODO_FILE)).unwrap();
        let v: Value = serde_json::from_str(&on_disk).unwrap();
        assert_eq!(v["todos"].as_array().unwrap().len(), 2);
        assert_eq!(v["todos"][0]["done"], json!(true));
    }

    #[tokio::test]
    async fn web_search_without_provider_degrades() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        let r = execute(&ctx, "web_search", json!({ "query": "rust" })).await;
        assert!(r.is_error && r.text.contains("未配置搜索源"));
    }
}
