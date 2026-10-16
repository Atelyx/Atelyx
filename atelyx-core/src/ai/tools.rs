//! Agent 工具协议与内置工具：名册（schema 声明）、参数校验、执行与结果回传文本。
//!
//! Agent 工具名册与执行：四分类归组；文件写经 `WriteQueue` 与壳同出口。
//! 路径一律仓库相对，执行前做越界校验；结果一律以文本回传模型。

use std::sync::Arc;

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

/// 工具四分类（与 Agent 设置勾选分组一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    Web,
    File,
    Task,
    Plugin,
}

/// 工具名册项：schema、归属分类、是否可并行执行。
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    pub category: ToolCategory,
    /// 与其他 parallel_safe 工具并发执行（读写/未知工具串行屏障）。
    pub parallel_safe: bool,
}

/// 内置名册（与仓库 agents.json 的工具 id 集一致）。
pub fn builtin_tools() -> Vec<ToolDef> {
    use ToolCategory::{File as F, Task as T, Web as W};
    vec![
        ToolDef {
            name: "web_search",
            description: "联网搜索。返回标题、链接与摘要的结果列表。",
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "搜索关键词" } },
                "required": ["query"]
            }),
            category: W,
            parallel_safe: true,
        },
        ToolDef {
            name: "web_fetch",
            description: "抓取网页正文（纯文本）。用于读取搜索结果中的具体页面。",
            parameters: json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "完整的 http(s) URL" } },
                "required": ["url"]
            }),
            category: W,
            parallel_safe: true,
        },
        ToolDef {
            name: "read_file",
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
            category: F,
            parallel_safe: true,
        },
        ToolDef {
            name: "glob",
            description: "按 glob 模式枚举仓库文件（`**/` 任意深度），返回相对路径列表。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "glob 模式，如 \"笔记/**/*.md\"" },
                    "path": { "type": "string", "description": "限定子目录（缺省仓库根）" }
                },
                "required": ["pattern"]
            }),
            category: F,
            parallel_safe: true,
        },
        ToolDef {
            name: "grep",
            description: "在仓库文件内容中检索正则，返回命中文件与行预览。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "正则表达式" },
                    "path": { "type": "string", "description": "限定子目录（缺省仓库根）" },
                    "include": { "type": "string", "description": "文件名过滤 glob，如 \"*.md\"" }
                },
                "required": ["pattern"]
            }),
            category: F,
            parallel_safe: true,
        },
        ToolDef {
            name: "list_dir",
            description: "列出目录下的文件与子目录（单层，标注类型）。",
            parameters: json!({
                "type": "object",
                "properties": { "dir": { "type": "string", "description": "仓库相对目录（缺省根）" } }
            }),
            category: F,
            parallel_safe: true,
        },
        ToolDef {
            name: "read_history",
            description: "读取文件的历史版本列表（时间、作者、摘要）。",
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "仓库相对路径" } },
                "required": ["path"]
            }),
            category: F,
            parallel_safe: true,
        },
        ToolDef {
            name: "write_file",
            description: "整文件写入（覆盖原内容）。先读后写：覆盖前应已 read_file 确认现状。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "仓库相对路径" },
                    "content": { "type": "string", "description": "完整文件内容" }
                },
                "required": ["path", "content"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "append_file",
            description: "在文件末尾追加内容（文件不存在则创建）。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "仓库相对路径" },
                    "content": { "type": "string", "description": "追加内容" }
                },
                "required": ["path", "content"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "edit_file",
            description: "对文件做精确替换（find 全部出现替换为 replace；找不到即报错不写入）。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "仓库相对路径" },
                    "find": { "type": "string", "description": "要替换的原文（精确匹配）" },
                    "replace": { "type": "string", "description": "替换后的文本" }
                },
                "required": ["path", "find", "replace"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "rename_file",
            description: "重命名文件或目录。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "仓库相对路径" },
                    "newName": { "type": "string", "description": "新名称（不含路径）" }
                },
                "required": ["path", "newName"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "move_file",
            description: "移动文件或目录到新位置。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "from": { "type": "string", "description": "源相对路径" },
                    "to": { "type": "string", "description": "目标相对路径" }
                },
                "required": ["from", "to"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "delete_file",
            description: "删除文件（不可恢复，谨慎使用）。",
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "仓库相对路径" } },
                "required": ["path"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "delete_dir",
            description: "删除目录及其全部内容（不可恢复，仅在用户明确要求清理时使用）。",
            parameters: json!({
                "type": "object",
                "properties": {
                    "dir": { "type": "string", "description": "仓库相对目录" },
                    "force": { "type": "boolean", "description": "非空也删（缺省仅删空目录）" }
                },
                "required": ["dir"]
            }),
            category: F,
            parallel_safe: false,
        },
        ToolDef {
            name: "todo_write",
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
            parallel_safe: false,
        },
    ]
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

/// 工具执行环境：仓库访问后端 + 已解析的搜索配置 + 排除清单。
#[derive(Clone)]
pub struct ToolCtx {
    pub repo: RepoAccess,
    pub search: SearchConfig,
    /// 仓库排除文件夹清单（检索工具跳过）。
    pub exclude: Vec<String>,
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

/// 执行内置工具。名称未知 = 错误文本；实现内部失败 = 错误文本（不抛断对话循环）。
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
        "rename_file" => rename_file(ctx, &args).await,
        "move_file" => move_file(ctx, &args).await,
        "delete_file" => delete_file(ctx, &args).await,
        "delete_dir" => delete_dir(ctx, &args).await,
        "todo_write" => todo_write(ctx, &args).await,
        other => ToolResult::err(format!("未知工具：{other}")),
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
    vault::validate_ai_path(rel).map_err(ToolResult::err)
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
            format!(
                "- seq {} {} {}: {}",
                v.seq,
                v.ts,
                v.author.name,
                v.summary.as_deref().unwrap_or("-")
            )
        })
        .collect();
    ToolResult::ok(lines.join("\n"))
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
    let find = find.to_string();
    let replace = replace.to_string();
    match &ctx.repo {
        RepoAccess::Local { root, queue } => {
            let root = root.clone();
            let target = rel.clone();
            queue
                .bulk(move || {
                    let path = vault::safe_join(&root, &target, false)?;
                    let existing = read_existing_text(&path, &target)?;
                    if find.is_empty() || !existing.contains(&find) {
                        return Err(format!("未找到要替换的原文（find 精确匹配失败）：{target}"));
                    }
                    vault::write_note(&root, &target, &existing.replace(&find, &replace))
                })
                .await
                .map(|_| ToolResult::ok(format!("已编辑 {rel}")))
                .unwrap_or_else(ToolResult::err)
        }
        RepoAccess::Space { client, space_id } => {
            let existing = match client.get_file_text(space_id, &rel).await {
                Ok(text) => text,
                Err(e) => return ToolResult::err(format!("文件不存在或不可读：{rel}（{e}）")),
            };
            if find.is_empty() || !existing.contains(&find) {
                return ToolResult::err(format!("未找到要替换的原文（find 精确匹配失败）：{rel}"));
            }
            client
                .put_file_text(space_id, &rel, &existing.replace(&find, &replace))
                .await
                .map(|_| ToolResult::ok(format!("已编辑 {rel}")))
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

async fn rename_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let Ok(new_name) = arg(args, "newName") else {
        return ToolResult::err("工具参数错误：缺少 newName");
    };
    if new_name.contains(['/', '\\']) || new_name == "." || new_name == ".." {
        return ToolResult::err("newName 只能是名称，不含路径分隔符");
    }
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    let parent = rel.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or_default();
    let target = if parent.is_empty() {
        new_name.to_string()
    } else {
        format!("{parent}/{new_name}")
    };
    let target = match rel_path(&target) {
        Ok(target) => target,
        Err(e) => return e,
    };
    move_entry(ctx, &rel, &target, &format!("已重命名 {rel} → {new_name}")).await
}

async fn move_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
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
            synced.map(|_| ToolResult::ok(ok.to_string())).unwrap_or_else(|e| {
                ToolResult::err(format!("操作失败：{e}"))
            })
        }
        RepoAccess::Space { client, space_id } => client
            .rename_file(space_id, from, to)
            .await
            .map(|_| ToolResult::ok(ok.to_string()))
            .unwrap_or_else(|e| ToolResult::err(format!("操作失败：{e}"))),
    }
}

async fn delete_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    match &ctx.repo {
        RepoAccess::Local { root, .. } => vault::delete_vault_file(root, &rel)
            .map(|_| ToolResult::ok(format!("已删除 {rel}")))
            .unwrap_or_else(ToolResult::err),
        RepoAccess::Space { client, space_id } => client
            .delete_file(space_id, &rel)
            .await
            .map(|_| ToolResult::ok(format!("已删除 {rel}")))
            .unwrap_or_else(|e| ToolResult::err(format!("删除失败：{e}"))),
    }
}

async fn delete_dir(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "dir") else {
        return ToolResult::err("工具参数错误：缺少 dir");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    match &ctx.repo {
        RepoAccess::Local { root, .. } => match vault::delete_folder(root, &rel, force) {
            Ok(result) if result.deleted => ToolResult::ok(format!("已删除目录 {rel}")),
            Ok(_) => ToolResult::err(format!("目录非空（force 可强制删除）：{rel}")),
            Err(e) => ToolResult::err(format!("删除失败：{e}")),
        },
        RepoAccess::Space { client, space_id } => {
            match client.delete_folder(space_id, &rel, force).await {
                Ok(ack) if ack.deleted => ToolResult::ok(format!("已删除目录 {rel}")),
                Ok(_) => ToolResult::err(format!("目录非空（force 可强制删除）：{rel}")),
                Err(e) => ToolResult::err(format!("删除失败：{e}")),
            }
        }
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
    fn ai_path_rules_reject_hidden_segments_and_escapes() {
        // 隐藏目录（仓库配置与密钥所在处）不对模型开放
        assert!(vault::validate_ai_path(".atelyx/config.json").is_err());
        assert!(vault::validate_ai_path("a/.git/y").is_err());
        assert!(vault::validate_ai_path("../x").is_err());
        assert!(vault::validate_ai_path("/x").is_err());
        // 普通仓库相对路径原样通过（分隔符归一为 `/`，空间端点直接可用）
        assert_eq!(vault::validate_ai_path("笔记/待办.md").unwrap(), "笔记/待办.md");
        assert_eq!(vault::validate_ai_path("./笔记/a.md").unwrap(), "笔记/a.md");
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
    async fn path_guards_reject_escape_and_absolute() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        for bad in ["../逃逸.md", "C:\\abs.md", "/abs.md", "a/../../b.md"] {
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
    async fn list_dir_delete_and_rename() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        std::fs::create_dir_all(dir.join("子目录")).unwrap();
        std::fs::write(dir.join("子目录/a.md"), "A").unwrap();
        let r = execute(&ctx, "list_dir", json!({ "dir": "子目录" })).await;
        assert!(r.text.contains("a.md"));
        let r = execute(
            &ctx,
            "rename_file",
            json!({ "path": "子目录/a.md", "newName": "b.md" }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert!(!dir.join("子目录/a.md").exists());
        assert!(dir.join("子目录/b.md").exists());
        // 非空目录：无 force 报错，force 删除
        let r = execute(&ctx, "delete_dir", json!({ "dir": "子目录" })).await;
        assert!(r.is_error);
        let r = execute(
            &ctx,
            "delete_dir",
            json!({ "dir": "子目录", "force": true }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert!(!dir.join("子目录").exists());
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

    #[test]
    fn registry_covers_four_categories() {
        let tools = builtin_tools();
        assert_eq!(tools.iter().filter(|t| t.name == "web_search").count(), 1);
        let by_cat = |c: ToolCategory| tools.iter().filter(|t| t.category == c).count();
        assert!(by_cat(ToolCategory::Web) >= 2);
        assert!(by_cat(ToolCategory::File) >= 10);
        assert_eq!(by_cat(ToolCategory::Task), 1);
        // 读写屏障：write 类不可并行
        assert!(!tools.iter().any(|t| t.name == "write_file" && t.parallel_safe));
        assert!(tools.iter().any(|t| t.name == "read_file" && t.parallel_safe));
    }
}
