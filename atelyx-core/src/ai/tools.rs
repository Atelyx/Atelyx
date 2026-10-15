//! Agent 工具协议与内置工具：名册（schema 声明）、参数校验、执行与结果回传文本。
//!
//! Agent 工具名册与执行：四分类归组；文件写经 `WriteQueue` 与壳同出口。
//! 路径一律仓库相对，执行前做越界校验；结果一律以文本回传模型。

use std::path::{Path, PathBuf};


use serde_json::{json, Value};

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

/// 工具执行环境：仓库根 + 与壳同出口的写盘队列 + 已解析的搜索配置 + 排除清单。
#[derive(Clone)]
pub struct ToolCtx {
    pub root: PathBuf,
    pub queue: WriteQueue,
    pub search: SearchConfig,
    /// 仓库排除文件夹清单（检索工具跳过）。
    pub exclude: Vec<String>,
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

/// 仓库相对路径校验与定位：拒绝绝对路径与 `..` 逃逸；返回绝对路径。
/// 不要求文件存在（写类工具要新建）。
fn resolve_rel(root: &Path, rel: &str) -> Result<PathBuf, ToolResult> {
    let p = Path::new(rel);
    // has_root 单独拦截：Windows 上 `/x`（有 root 无 prefix）不算绝对路径，
    // 但 join 会替换除 prefix 外的基路径——不拦即写穿到盘符根
    if p.is_absolute() || p.has_root() || rel.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(ToolResult::err(format!(
            "路径须为仓库相对路径（拒绝绝对路径与 ..）：{rel}"
        )));
    }
    Ok(root.join(p))
}

/// 执行内置工具。名称未知 = 错误文本；实现内部失败 = 错误文本（不抛断对话循环）。
pub async fn execute(ctx: &ToolCtx, name: &str, args: Value) -> ToolResult {
    match name {
        "web_search" => web_search(ctx, &args).await,
        "web_fetch" => web_fetch(&args).await,
        "read_file" => read_file(ctx, &args),
        "glob" => glob(ctx, &args),
        "grep" => grep(ctx, &args),
        "list_dir" => list_dir(ctx, &args),
        "read_history" => read_history(ctx, &args),
        "write_file" => write_file(ctx, &args).await,
        "append_file" => append_file(ctx, &args).await,
        "edit_file" => edit_file(ctx, &args).await,
        "rename_file" => rename_file(ctx, &args),
        "move_file" => move_file(ctx, &args),
        "delete_file" => delete_file(ctx, &args),
        "delete_dir" => delete_dir(ctx, &args),
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

fn read_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let path = match resolve_rel(&ctx.root, rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return ToolResult::err(format!("文件不存在或不可读：{rel}"));
    };
    if bytes[..bytes.len().min(4096)].contains(&0) {
        return ToolResult::err(format!("二进制文件不支持读取：{rel}"));
    }
    let content = String::from_utf8_lossy(&bytes);
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
    for (i, line) in content.lines().enumerate().skip(start).take(end - start) {
        out.push_str(&format!("{}| {line}\n", i + 1));
        if out.len() > READ_CHARS_CAP {
            out.push_str("…（结果过长截断）\n");
            break;
        }
    }
    if end < total {
        out.push_str(&format!(
            "\n（共 {total} 行，显示 {}-{}；续读传 offset={}）",
            start + 1,
            end,
            end + 1
        ));
    } else if out.is_empty() {
        out.push_str("（空文件）");
    }
    ToolResult::ok(out)
}

fn glob(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(pattern) = arg(args, "pattern") else {
        return ToolResult::err("工具参数错误：缺少 pattern");
    };
    let path = args.get("path").and_then(Value::as_str).map(str::to_string);
    let result = crate::filesearch::glob_vault_blocking(
        ctx.root.clone(),
        ctx.exclude.clone(),
        pattern.to_string(),
        path,
    );
    match result {
        Ok(r) => {
            if r.paths.is_empty() {
                return ToolResult::ok("无匹配文件");
            }
            let mut text = r.paths.join("\n");
            if r.capped {
                text.push_str(&format!("\n…（共 {} 条，已截断）", r.total));
            }
            ToolResult::ok(text)
        }
        Err(e) => ToolResult::err(format!("检索失败：{e}")),
    }
}

fn grep(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(pattern) = arg(args, "pattern") else {
        return ToolResult::err("工具参数错误：缺少 pattern");
    };
    let path = args.get("path").and_then(Value::as_str).map(str::to_string);
    let include = args
        .get("include")
        .and_then(Value::as_str)
        .map(str::to_string);
    let result = crate::filesearch::grep_vault_blocking(
        ctx.root.clone(),
        ctx.exclude.clone(),
        pattern.to_string(),
        path,
        include,
    );
    match result {
        Ok(r) => {
            if r.matches.is_empty() {
                return ToolResult::ok("无命中");
            }
            let mut text = String::new();
            for m in r.matches.iter() {
                text.push_str(&format!("{}:{}: {}\n", m.path, m.line_number, m.line));
                if text.len() > READ_CHARS_CAP {
                    text.push_str("…（结果过长截断）\n");
                    break;
                }
            }
            if r.capped {
                text.push_str(&format!("…（共 {} 条命中，已截断）", r.total));
            }
            ToolResult::ok(text)
        }
        Err(e) => ToolResult::err(format!("检索失败：{e}")),
    }
}

fn list_dir(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let rel = args
        .get("dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let path = match resolve_rel(&ctx.root, rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Ok(entries) = std::fs::read_dir(&path) else {
        return ToolResult::err(format!("目录不存在或不可读：{}", if rel.is_empty() { "." } else { rel }));
    };
    let mut rows: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        rows.push(if is_dir {
            format!("{name}/")
        } else {
            name
        });
    }
    rows.sort();
    if rows.is_empty() {
        ToolResult::ok("（空目录）")
    } else {
        ToolResult::ok(rows.join("\n"))
    }
}

fn read_history(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let versions = crate::history::load_history(
        &ctx.root,
        crate::history::HistoryKind::Note,
        rel,
    );
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
    let path = match resolve_rel(&ctx.root, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return ToolResult::err(format!("无法创建父目录：{rel}"));
        }
    }
    ctx.queue.write(path, content.to_string()).await
        .map(|_| ToolResult::ok(format!("已写入 {rel}")))
        .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
}

async fn append_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let (Ok(rel), Some(content)) = (
        arg(args, "path").map(str::to_string),
        args.get("content").and_then(Value::as_str),
    ) else {
        return ToolResult::err("工具参数错误：需要 path 与 content");
    };
    let path = match resolve_rel(&ctx.root, &rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let next = format!("{existing}{content}");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    ctx.queue.write(path, next).await
        .map(|_| ToolResult::ok(format!("已追加到 {rel}")))
        .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
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
    let path = match resolve_rel(&ctx.root, rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return ToolResult::err(format!("文件不存在或不可读：{rel}"));
    };
    if find.is_empty() || !existing.contains(find) {
        return ToolResult::err(format!("未找到要替换的原文（find 精确匹配失败）：{rel}"));
    }
    let next = existing.replace(find, replace);
    ctx.queue.write(path, next).await
        .map(|_| ToolResult::ok(format!("已编辑 {rel}")))
        .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
}

fn rename_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let Ok(new_name) = arg(args, "newName") else {
        return ToolResult::err("工具参数错误：缺少 newName");
    };
    if new_name.contains(['/', '\\']) || new_name == "." || new_name == ".." {
        return ToolResult::err("newName 只能是名称，不含路径分隔符");
    }
    let path = match resolve_rel(&ctx.root, rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let target = match path.parent() {
        Some(parent) => parent.join(new_name),
        None => return ToolResult::err(format!("路径异常：{rel}")),
    };
    std::fs::rename(&path, &target)
        .map(|_| ToolResult::ok(format!("已重命名 {rel} → {new_name}")))
        .unwrap_or_else(|e| ToolResult::err(format!("重命名失败：{e}")))
}

fn move_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(from) = arg(args, "from") else {
        return ToolResult::err("工具参数错误：缺少 from");
    };
    let Ok(to) = arg(args, "to") else {
        return ToolResult::err("工具参数错误：缺少 to");
    };
    let src = match resolve_rel(&ctx.root, from) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let dst = match resolve_rel(&ctx.root, to) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::rename(&src, &dst)
        .map(|_| ToolResult::ok(format!("已移动 {from} → {to}")))
        .unwrap_or_else(|e| ToolResult::err(format!("移动失败：{e}")))
}

fn delete_file(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let path = match resolve_rel(&ctx.root, rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if path.is_dir() {
        return ToolResult::err(format!("目标是目录，请用 delete_dir：{rel}"));
    }
    std::fs::remove_file(&path)
        .map(|_| ToolResult::ok(format!("已删除 {rel}")))
        .unwrap_or_else(|e| ToolResult::err(format!("删除失败：{e}")))
}

fn delete_dir(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "dir") else {
        return ToolResult::err("工具参数错误：缺少 dir");
    };
    let path = match resolve_rel(&ctx.root, rel) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !path.is_dir() {
        return ToolResult::err(format!("目录不存在：{rel}"));
    }
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let result = if force {
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_dir(&path)
    };
    match result {
        Ok(()) => ToolResult::ok(format!("已删除目录 {rel}")),
        Err(_) if !force => ToolResult::err(format!("目录非空（force 可强制删除）：{rel}")),
        Err(e) => ToolResult::err(format!("删除失败：{e}")),
    }
}

/// 任务清单落点（仓库相对）。
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
    let content = serde_json::to_string_pretty(&json!({
        "schema": 1,
        "updatedAt": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
        "todos": cleaned,
    }))
    .unwrap_or_else(|_| "[]".to_string());
    let path = ctx.root.join(TODO_FILE);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    ctx.queue.write(path, content).await
        .map(|_| ToolResult::ok(format!("任务清单已更新（{} 项）", todos.len())))
        .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::test_support::TempDir;

    fn ctx(dir: &TempDir) -> ToolCtx {
        ToolCtx {
            root: dir.to_path_buf(),
            queue: WriteQueue::new(),
            search: SearchConfig::default(),
            exclude: Vec::new(),
        }
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
        ctx.queue.drain().await.unwrap();
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
