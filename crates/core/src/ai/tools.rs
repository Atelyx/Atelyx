//! Agent 工具协议与内置工具：名册（schema 声明）、参数校验、执行与结果回传文本。
//!
//! 内置工具按分类归组，插件工具由宿主名册单列；内容写入经 `AiEditHost` 交壳的保存链落盘
//! （无宿主时才直写兜底），表格 / 画布操作由 `doc_ops` 纯函数应用到领域模型。
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
    Table,
    Canvas,
    Task,
}

impl ToolCategory {
    /// 全部分类（展示顺序）。
    pub const ALL: [ToolCategory; 6] =
        [Self::Web, Self::Read, Self::Edit, Self::Table, Self::Canvas, Self::Task];

    /// 分组显示名。
    pub fn label(self) -> &'static str {
        match self {
            Self::Web => "联网",
            Self::Read => "读取",
            Self::Edit => "编辑与整理",
            Self::Table => "表格",
            Self::Canvas => "画布",
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
        use ToolCategory::{Canvas as CV, Edit as E, Read as R, Table as TB, Task as T, Web as W};
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
                description: "在文件末尾追加内容（文件不存在则创建；原内容末尾无换行时自动补一个换行）；表格与画布文件不支持用文件工具写入。",
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
                description: "移动或重命名文件与目录：from → to 全路径（同目录改名即重命名）。本地仓库改名会同步更新引用链接。",
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
                name: "table_read",
                label: "读取表格",
                icon: "table",
                description: "读取表格（.atb）的表头与行：字段 id / 名称 / 类型、每行 id 与各字段值；分页缺省 50 行、最多 200 行。编辑前先用它取行 id 与字段 id。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径（.atb）" },
                        "offset": { "type": "integer", "description": "起始行（1 起，缺省 1）" },
                        "limit": { "type": "integer", "description": "行数（缺省 50，上限 200）" }
                    },
                    "required": ["path"]
                }),
                category: TB,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "table_edit",
                label: "编辑表格",
                icon: "pencil",
                description: "编辑表格（.atb）：一次调用提交一批操作，整批落成一个补丁。op = set_cell（按 rowId / fieldId 写单元格，value 传 null 清空）| add_row（cells 按字段 id 给初值）| delete_rows（rowIds）。表格文件不存在时按给定路径新建（标题取文件名）。表格不能用文件工具整文件写。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径（.atb）" },
                        "ops": {
                            "type": "array",
                            "description": "操作列表（按顺序应用）",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "op": { "type": "string", "enum": ["set_cell", "add_row", "delete_rows"], "description": "操作类型" },
                                    "rowId": { "type": "string", "description": "set_cell：行 id" },
                                    "fieldId": { "type": "string", "description": "set_cell：字段 id" },
                                    "value": { "description": "set_cell：值（文本 / 数字 / 单调选项；null 清空）" },
                                    "cells": { "type": "object", "description": "add_row：字段 id → 值" },
                                    "rowIds": { "type": "array", "items": { "type": "string" }, "description": "delete_rows：行 id 列表" }
                                },
                                "required": ["op"]
                            }
                        }
                    },
                    "required": ["path", "ops"]
                }),
                category: TB,
                read_only: false,
                parallel_safe: false,
            },
            ToolDef {
                name: "canvas_read",
                label: "读取画布",
                icon: "sticky-note",
                description: "读取画布（.atlx）：画布名、节点（id / 类型 / 标题或正文 / 坐标 / 引用文件）与连线（id / from / to / 类型）。",
                parameters: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string", "description": "仓库相对路径（.atlx）" } },
                    "required": ["path"]
                }),
                category: CV,
                read_only: true,
                parallel_safe: true,
            },
            ToolDef {
                name: "canvas_edit",
                label: "编辑画布",
                icon: "layers",
                description: "编辑画布（.atlx）：一次调用提交一批操作，整批落成一个补丁。op = add_text_card（text，x/y 缺省自动落位）| add_note_card（path 引用仓库笔记）| connect（from / to 节点 id，kind = data 数据流 / link 关联）| update_text（nodeId / text）| delete_node（nodeId）。画布文件不存在时按给定路径新建（标题取文件名）。画布不能用文件工具整文件写。",
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "仓库相对路径（.atlx）" },
                        "ops": {
                            "type": "array",
                            "description": "操作列表（按顺序应用）",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "op": { "type": "string", "enum": ["add_text_card", "add_note_card", "connect", "update_text", "delete_node"], "description": "操作类型" },
                                    "text": { "type": "string", "description": "add_text_card / update_text：正文（Markdown）" },
                                    "path": { "type": "string", "description": "add_note_card：要引用的仓库笔记路径（.md）" },
                                    "x": { "type": "number", "description": "新卡横坐标（缺省自动放在现有卡右侧）" },
                                    "y": { "type": "number", "description": "新卡纵坐标（缺省自动放在现有卡右侧）" },
                                    "from": { "type": "string", "description": "connect：起点节点 id" },
                                    "to": { "type": "string", "description": "connect：终点节点 id" },
                                    "kind": { "type": "string", "enum": ["data", "link"], "description": "connect：连线类型（缺省 data）" },
                                    "nodeId": { "type": "string", "description": "update_text / delete_node：节点 id" }
                                },
                                "required": ["op"]
                            }
                        }
                    },
                    "required": ["path", "ops"]
                }),
                category: CV,
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
        "table_read" => table_read(ctx, &args).await,
        "table_edit" => table_edit(ctx, &args).await,
        "canvas_read" => canvas_read(ctx, &args).await,
        "canvas_edit" => canvas_edit(ctx, &args).await,
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
    // 空文件与越界分开报：越界时无行可渲染，不能落进「空文件」分支骗模型判定内容为空
    if total == 0 {
        return ToolResult::ok(format!("（空文件：{rel}）"));
    }
    if offset >= total {
        return ToolResult::ok(format!(
            "（offset 超出范围：共 {total} 行，offset 从 1 起）"
        ));
    }
    let start = offset;
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
    let (kind, tool) = if lower.ends_with(".atb") {
        ("表格", "table_edit")
    } else if lower.ends_with(".atlx") {
        ("画布", "canvas_edit")
    } else {
        return Ok(());
    };
    Err(ToolResult::err(format!(
        "{rel} 是{kind}文件，不支持用文件工具写入（会绕过补丁合并与历史）；请改用 {tool} 按稳定 id 增量修改"
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
                    let next = crate::ai::edit_host::append_text(&existing, &content);
                    vault::write_note(&root, &target, &next)
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
            let next = crate::ai::edit_host::append_text(&existing, content);
            client
                .put_file_text(space_id, &rel, &next)
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

// ===== 表格 / 画布领域工具 =====

/// 表格读取缺省行数与上限（一次结果不撑爆上下文）。
const TABLE_READ_ROWS: usize = 50;
const TABLE_READ_ROWS_MAX: usize = 200;
/// 单元格值 / 正文在结果文本里的字符上限（长文本按省略号收口）。
const CELL_TEXT_CAP: usize = 80;
/// 画布读取的节点 / 连线显示上限（超出截断并提示）。
const CANVAS_READ_CAP: usize = 200;

/// 读表格模型：本地读盘（schema 校验）、空间取服务端文本后同口径校验。
async fn read_table_model(ctx: &ToolCtx, rel: &str) -> Result<vault::TableFile, String> {
    match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            let path = vault::safe_join(root, rel, false)?;
            vault::read_table_file(&path)
        }
        RepoAccess::Space { client, space_id } => {
            let text = client
                .get_file_text(space_id, rel)
                .await
                .map_err(|e| e.to_string())?;
            parse_table_model(&text)
        }
    }
}

/// 编辑源模型：文件不存在 = 本次新建（空模型起算）；存在但读不出 = 报错（坏文件不得当空表覆盖）。
async fn edit_source_table(ctx: &ToolCtx, rel: &str) -> Result<vault::TableFile, String> {
    match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            // 先查存在性：父目录缺失时 safe_join 会报错，不能把「不存在」当成「不可读」
            if !vault::file_exists(root, rel)? {
                return Ok(crate::table_ops::new_table_file(&file_label_of(rel)));
            }
            let path = vault::safe_join(root, rel, false)?;
            vault::read_table_file(&path)
        }
        RepoAccess::Space { client, space_id } => {
            match client.get_file_text(space_id, rel).await {
                Ok(text) => parse_table_model(&text),
                Err(crate::space_client::SpaceError::NotFound) => {
                    Ok(crate::table_ops::new_table_file(&file_label_of(rel)))
                }
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// 编辑源画布（口径同 [`edit_source_table`]）。
async fn edit_source_canvas(ctx: &ToolCtx, rel: &str) -> Result<vault::CanvasFile, String> {
    match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            // 先查存在性：父目录缺失时 safe_join 会报错，不能把「不存在」当成「不可读」
            if !vault::file_exists(root, rel)? {
                return Ok(crate::table_ops::new_canvas_file(&file_label_of(rel)));
            }
            let path = vault::safe_join(root, rel, false)?;
            vault::read_canvas_file(&path)
        }
        RepoAccess::Space { client, space_id } => {
            match client.get_file_text(space_id, rel).await {
                Ok(text) => parse_canvas_model(&text),
                Err(crate::space_client::SpaceError::NotFound) => {
                    Ok(crate::table_ops::new_canvas_file(&file_label_of(rel)))
                }
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// 文件显示标题：末段去扩展名（新建表格 / 画布的标题口径与壳一致）。
fn file_label_of(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    vault::file_stem(name).to_string()
}

/// 读画布模型（口径同 read_table_model）。
async fn read_canvas_model(ctx: &ToolCtx, rel: &str) -> Result<vault::CanvasFile, String> {
    match &ctx.repo {
        RepoAccess::Local { root, .. } => {
            let path = vault::safe_join(root, rel, false)?;
            vault::read_canvas_file(&path)
        }
        RepoAccess::Space { client, space_id } => {
            let text = client
                .get_file_text(space_id, rel)
                .await
                .map_err(|e| e.to_string())?;
            parse_canvas_model(&text)
        }
    }
}

fn parse_table_model(text: &str) -> Result<vault::TableFile, String> {
    let table: vault::TableFile =
        serde_json::from_str(text).map_err(|e| format!("表格解析失败：{e}"))?;
    if table.schema != vault::TABLE_SCHEMA {
        return Err(format!("表格 schema 不匹配：{}", table.schema));
    }
    Ok(table)
}

fn parse_canvas_model(text: &str) -> Result<vault::CanvasFile, String> {
    let canvas: vault::CanvasFile =
        serde_json::from_str(text).map_err(|e| format!("画布解析失败：{e}"))?;
    if canvas.schema != vault::CANVAS_SCHEMA {
        return Err(format!("画布 schema 不匹配：{}", canvas.schema));
    }
    Ok(canvas)
}

/// 领域工具的文件类型守卫（错误文本指向正确工具）。
fn require_ext(rel: &str, ext: &str, kind: &str, tool: &str) -> Option<ToolResult> {
    if rel.to_ascii_lowercase().ends_with(ext) {
        None
    } else {
        Some(ToolResult::err(format!(
            "{rel} 不是{kind}文件：{kind}工具只处理 {ext} 文件（.md 用文件工具，{ext} 用 {tool}）"
        )))
    }
}

async fn table_read(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Some(e) = require_ext(&rel, ".atb", "表格", "table_read") {
        return e;
    }
    match read_table_model(ctx, &rel).await {
        Ok(table) => ToolResult::ok(render_table(&table, args, &rel)),
        Err(e) => ToolResult::err(format!("表格不存在或不可读：{rel}（{e}）")),
    }
}

/// 表格结果文本：字段（id / 名称 / 类型）、行（id + 按字段 id 的值）、总行数与续读位。
fn render_table(table: &vault::TableFile, args: &Value, rel: &str) -> String {
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .map(|v| v.max(1) as usize - 1)
        .unwrap_or(0);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(TABLE_READ_ROWS)
        .clamp(1, TABLE_READ_ROWS_MAX);
    let mut out = format!(
        "表：{}（{} 字段 · {} 行）\n",
        table.title,
        table.fields.len(),
        table.rows.len()
    );
    if table.fields.is_empty() {
        out.push_str("字段：（无，表格没有列）\n");
    } else {
        out.push_str("字段：\n");
        for f in &table.fields {
            let options = f
                .options
                .as_ref()
                .filter(|o| !o.is_empty())
                .map(|o| format!("：{}", o.join(" / ")))
                .unwrap_or_default();
            out.push_str(&format!("- {} {}（{}{options}）\n", f.id, f.name, f.field_type));
        }
    }
    let total = table.rows.len();
    if total == 0 {
        out.push_str("行：（无）");
        return out;
    }
    let start = offset.min(total);
    if start >= total {
        out.push_str(&format!("（共 {total} 行，offset 超出范围：从 1 起）"));
        return out;
    }
    let end = (start + limit).min(total);
    out.push_str(&format!("行 {}-{}/共 {total}：\n", start + 1, end));
    let mut shown = start;
    for (idx, row) in table.rows[start..end].iter().enumerate() {
        let mut line = format!("- {}", row.id);
        for f in &table.fields {
            line.push_str(&format!(" | {}={}", f.id, cell_text(row.values.get(&f.id))));
        }
        out.push_str(&line);
        out.push('\n');
        shown = start + idx + 1;
        if out.len() > READ_CHARS_CAP {
            out.push_str("…（结果过长截断）\n");
            break;
        }
    }
    // 续读位按实际渲染到的行给（字符上限截断时不得按整页末尾给，否则被截掉的行永远读不到）
    if shown < total {
        out.push_str(&format!("（已显示到第 {shown} 行；续读传 offset={}）", shown + 1));
    } else {
        out.push_str("（已到末行）");
    }
    out.push_str(&format!("\n文件：{rel}"));
    out
}

/// 单元格值 → 单行文本（图片按占位、字符串带引号便于区分空值）。
fn cell_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "null".to_string(),
        Some(v) if v.is_array() || v.get("images").is_some() => "[图片]".to_string(),
        Some(v) => truncate_chars(&v.to_string(), CELL_TEXT_CAP),
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

async fn canvas_read(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Some(e) = require_ext(&rel, ".atlx", "画布", "canvas_read") {
        return e;
    }
    match read_canvas_model(ctx, &rel).await {
        Ok(canvas) => ToolResult::ok(render_canvas(&canvas, &rel)),
        Err(e) => ToolResult::err(format!("画布不存在或不可读：{rel}（{e}）")),
    }
}

/// 画布结果文本：画布名、节点（id / 类型 / 标题或正文 / 坐标 / 引用文件）、连线。
fn render_canvas(canvas: &vault::CanvasFile, rel: &str) -> String {
    let mut out = format!(
        "画布：{}（{} 节点 · {} 连线）\n",
        canvas.title,
        canvas.nodes.len(),
        canvas.edges.len()
    );
    if canvas.nodes.is_empty() {
        out.push_str("节点：（无）\n");
    } else {
        out.push_str("节点：\n");
        for n in canvas.nodes.iter().take(CANVAS_READ_CAP) {
            out.push_str(&canvas_node_line(n));
            out.push('\n');
            if out.len() > READ_CHARS_CAP {
                out.push_str("…（结果过长截断）\n");
                break;
            }
        }
        if canvas.nodes.len() > CANVAS_READ_CAP {
            out.push_str(&format!(
                "…（共 {} 节点，仅显示前 {CANVAS_READ_CAP} 个）\n",
                canvas.nodes.len()
            ));
        }
    }
    if canvas.edges.is_empty() {
        out.push_str("连线：（无）\n");
    } else {
        out.push_str("连线：\n");
        for e in canvas.edges.iter().take(CANVAS_READ_CAP) {
            let kind = if e.directed == Some(false) { "关联" } else { "数据流" };
            out.push_str(&format!("- {} {} → {}（{kind}）\n", e.id, e.source, e.target));
        }
        if canvas.edges.len() > CANVAS_READ_CAP {
            out.push_str(&format!(
                "…（共 {} 连线，仅显示前 {CANVAS_READ_CAP} 条）\n",
                canvas.edges.len()
            ));
        }
    }
    out.push_str(&format!("文件：{rel}"));
    out
}

/// 单节点摘要行：类型标签 + 标题 + 坐标 + 引用文件 / 正文预览 / 消息数。
fn canvas_node_line(n: &vault::CanvasFileNode) -> String {
    let file = n.data.get("file").and_then(Value::as_str);
    let title = n.data.get("title").and_then(Value::as_str).unwrap_or("");
    let kind = match n.node_type.as_str() {
        "text" if file.is_some() => "笔记卡",
        "text" => "文本卡",
        "table" => "表格卡",
        "media" => "媒体卡",
        "conversation" => "对话卡",
        "group" => "分组",
        other => other,
    };
    let mut line = format!("- {} {kind}「{title}」({}, {})", n.id, n.x, n.y);
    match file {
        Some(file) => line.push_str(&format!(" → {file}")),
        None if n.node_type == "text" => {
            let body = n.data.get("bodyMd").and_then(Value::as_str).unwrap_or("");
            let preview: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
            if !preview.is_empty() {
                line.push_str(&format!(" 正文：{}", truncate_chars(&preview, CELL_TEXT_CAP)));
            }
        }
        None if n.node_type == "conversation" => {
            let count = n
                .data
                .get("messages")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            line.push_str(&format!(" {count} 条消息"));
        }
        None => {}
    }
    line
}

/// 有宿主时把表格编辑交给宿主保存链（历史 / 协作 / 打开会话）；无宿主 = 返回 None 走直写兜底。
async fn apply_table_via_host(
    ctx: &ToolCtx,
    rel: &str,
    ops: Vec<crate::ai::edit_host::TableOp>,
) -> Option<ToolResult> {
    let host = ctx.host.as_ref()?;
    let Some(actor) = ctx.actor.as_ref() else {
        return Some(ToolResult::err("AI 编辑身份缺失，已拒绝写入"));
    };
    Some(match host.apply_table(rel, ops, actor).await {
        Ok(text) => ToolResult::ok(text),
        Err(e) => ToolResult::err(e),
    })
}

/// 有宿主时把画布编辑交给宿主保存链；无宿主 = 返回 None 走直写兜底。
async fn apply_canvas_via_host(
    ctx: &ToolCtx,
    rel: &str,
    ops: Vec<crate::ai::edit_host::CanvasOp>,
) -> Option<ToolResult> {
    let host = ctx.host.as_ref()?;
    let Some(actor) = ctx.actor.as_ref() else {
        return Some(ToolResult::err("AI 编辑身份缺失，已拒绝写入"));
    };
    Some(match host.apply_canvas(rel, ops, actor).await {
        Ok(text) => ToolResult::ok(text),
        Err(e) => ToolResult::err(e),
    })
}

async fn table_edit(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Some(e) = require_ext(&rel, ".atb", "表格", "table_edit") {
        return e;
    }
    let ops = match parse_table_ops(args) {
        Ok(ops) => ops,
        Err(e) => return ToolResult::err(e),
    };
    if let Some(result) = apply_table_via_host(ctx, &rel, ops.clone()).await {
        return result;
    }
    // 不存在 = 本次新建（从空模型起算）；坏文件报错，不当空表覆盖
    let base = match edit_source_table(ctx, &rel).await {
        Ok(table) => table,
        Err(e) => return ToolResult::err(format!("表格不存在或不可读：{rel}（{e}）")),
    };
    let mut next = base.clone();
    if let Err(e) = crate::ai::doc_ops::apply_table_ops(&mut next, &ops) {
        return ToolResult::err(e);
    }
    let Some(patch) = vault::compute_table_patch(&base, &next) else {
        return ToolResult::ok(format!("表格未变化：{rel}（操作与现值相同）"));
    };
    next.updated_at = now_secs();
    let summary = crate::history::version_summary(
        crate::history::HistoryKind::Table,
        &base_json(&base),
        &base_json(&next),
    );
    let receipt = format!("已编辑 {rel}（{} 项操作：{summary}）", ops.len());
    match &ctx.repo {
        RepoAccess::Local { root, queue } => {
            // 新建表格的父目录可能还不存在：写路径按建父目录取绝对路径
            let path = match vault::safe_join(root, &rel, true) {
                Ok(path) => path,
                Err(e) => return ToolResult::err(e),
            };
            let Ok(json) = serde_json::to_string_pretty(&next) else {
                return ToolResult::err("表格序列化失败".to_string());
            };
            queue
                .write(path, json)
                .await
                .map(|_| ToolResult::ok(receipt))
                .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
        }
        RepoAccess::Space { client, space_id } => client
            .table_patch(space_id, &rel, &patch)
            .await
            .map(|_| ToolResult::ok(receipt))
            .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}"))),
    }
}

async fn canvas_edit(ctx: &ToolCtx, args: &Value) -> ToolResult {
    let Ok(rel) = arg(args, "path") else {
        return ToolResult::err("工具参数错误：缺少 path");
    };
    let rel = match rel_path(rel) {
        Ok(rel) => rel,
        Err(e) => return e,
    };
    if let Some(e) = require_ext(&rel, ".atlx", "画布", "canvas_edit") {
        return e;
    }
    let ops = match parse_canvas_ops(args) {
        Ok(ops) => ops,
        Err(e) => return ToolResult::err(e),
    };
    if let Some(result) = apply_canvas_via_host(ctx, &rel, ops.clone()).await {
        return result;
    }
    // 不存在 = 本次新建（从空模型起算）；坏文件报错，不当空画布覆盖
    let base = match edit_source_canvas(ctx, &rel).await {
        Ok(canvas) => canvas,
        Err(e) => return ToolResult::err(format!("画布不存在或不可读：{rel}（{e}）")),
    };
    let mut next = base.clone();
    let touched = match crate::ai::doc_ops::apply_canvas_ops(&mut next, &ops) {
        Ok(touched) => touched,
        Err(e) => return ToolResult::err(e),
    };
    let Some(patch) = vault::compute_canvas_patch(&base, &next) else {
        return ToolResult::ok(format!("画布未变化：{rel}（操作与现值相同）"));
    };
    next.updated_at = now_secs();
    let summary = crate::history::version_summary(
        crate::history::HistoryKind::Canvas,
        &base_json(&base),
        &base_json(&next),
    );
    let receipt = format!(
        "已编辑 {rel}（{} 项操作：{summary}）；实体：{}",
        ops.len(),
        touched.join("、")
    );
    match &ctx.repo {
        RepoAccess::Local { root, queue } => {
            // 新建画布的父目录可能还不存在：写路径按建父目录取绝对路径
            let path = match vault::safe_join(root, &rel, true) {
                Ok(path) => path,
                Err(e) => return ToolResult::err(e),
            };
            let Ok(json) = serde_json::to_string_pretty(&next) else {
                return ToolResult::err("画布序列化失败".to_string());
            };
            queue
                .write(path, json)
                .await
                .map(|_| ToolResult::ok(receipt))
                .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}")))
        }
        RepoAccess::Space { client, space_id } => client
            .canvas_patch(space_id, &rel, &patch)
            .await
            .map(|_| ToolResult::ok(receipt))
            .unwrap_or_else(|e| ToolResult::err(format!("写入失败：{e}"))),
    }
}

fn base_json<T: serde::Serialize>(model: &T) -> String {
    serde_json::to_string(model).unwrap_or_default()
}

/// 表格 ops 参数解析：缺少必填字段 / 未知操作给可读错误（不 panic）。
fn parse_table_ops(args: &Value) -> Result<Vec<crate::ai::edit_host::TableOp>, String> {
    use crate::ai::edit_host::TableOp;
    let Some(items) = args.get("ops").and_then(Value::as_array) else {
        return Err("工具参数错误：缺少 ops".to_string());
    };
    if items.is_empty() {
        return Err("工具参数错误：ops 为空".to_string());
    }
    let mut ops = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let at = i + 1;
        let kind = item
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("第 {at} 项操作缺少 op"))?;
        match kind {
            "set_cell" => {
                let row_id = op_str(item, "rowId", at)?;
                let field_id = op_str(item, "fieldId", at)?;
                let value = item
                    .get("value")
                    .cloned()
                    .ok_or_else(|| format!("第 {at} 项操作缺少 value（清空传 null）"))?;
                ops.push(TableOp::SetCell { row_id, field_id, value });
            }
            "add_row" => {
                let cells = match item.get("cells") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Object(map)) => {
                        map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    }
                    Some(_) => return Err(format!("第 {at} 项操作：cells 需要对象（字段 id → 值）")),
                };
                ops.push(TableOp::AddRow { cells });
            }
            "delete_rows" => {
                let row_ids = strings_of(item.get("rowIds"))
                    .ok_or_else(|| format!("第 {at} 项操作缺少 rowIds（行 id 数组）"))?;
                if row_ids.is_empty() {
                    return Err(format!("第 {at} 项操作：rowIds 不能为空"));
                }
                ops.push(TableOp::DeleteRows { row_ids });
            }
            other => {
                return Err(format!(
                    "第 {at} 项操作：未知操作 {other}（set_cell / add_row / delete_rows）"
                ));
            }
        }
    }
    Ok(ops)
}

/// 画布 ops 参数解析（口径同 parse_table_ops）。
fn parse_canvas_ops(args: &Value) -> Result<Vec<crate::ai::edit_host::CanvasOp>, String> {
    use crate::ai::edit_host::CanvasOp;
    let Some(items) = args.get("ops").and_then(Value::as_array) else {
        return Err("工具参数错误：缺少 ops".to_string());
    };
    if items.is_empty() {
        return Err("工具参数错误：ops 为空".to_string());
    }
    let mut ops = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let at = i + 1;
        let kind = item
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("第 {at} 项操作缺少 op"))?;
        match kind {
            "add_text_card" => {
                let text = op_str(item, "text", at)?;
                ops.push(CanvasOp::AddTextCard {
                    text,
                    x: item.get("x").and_then(Value::as_f64),
                    y: item.get("y").and_then(Value::as_f64),
                });
            }
            "add_note_card" => {
                let path = op_str(item, "path", at)?;
                let path = match rel_path(&path) {
                    Ok(path) => path,
                    Err(e) => return Err(e.text),
                };
                ops.push(CanvasOp::AddNoteCard {
                    path,
                    x: item.get("x").and_then(Value::as_f64),
                    y: item.get("y").and_then(Value::as_f64),
                });
            }
            "connect" => {
                let from = op_str(item, "from", at)?;
                let to = op_str(item, "to", at)?;
                ops.push(CanvasOp::Connect {
                    from,
                    to,
                    kind: item.get("kind").and_then(Value::as_str).map(str::to_string),
                });
            }
            "update_text" => {
                let node_id = op_str(item, "nodeId", at)?;
                let text = op_str(item, "text", at)?;
                ops.push(CanvasOp::UpdateText { node_id, text });
            }
            "delete_node" => {
                let node_id = op_str(item, "nodeId", at)?;
                ops.push(CanvasOp::DeleteNode { node_id });
            }
            other => {
                return Err(format!(
                    "第 {at} 项操作：未知操作 {other}（add_text_card / add_note_card / connect / update_text / delete_node）"
                ));
            }
        }
    }
    Ok(ops)
}

fn op_str(item: &Value, key: &str, at: usize) -> Result<String, String> {
    item.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("第 {at} 项操作缺少 {key}"))
}

fn strings_of(value: Option<&Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(item.as_str()?.to_string());
    }
    Some(out)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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

/// 改名 / 移动共用实现：本地经引用同步（全库 `.md` 链接、元数据路径一并改写）；
/// 空间只走服务端 rename（引用改写是壳的文件树改名链的第二步，本路径不经过它）。
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
/// 任务清单落点（仓库相对路径）。
pub const TODO_FILE: &str = ".atelyx/todos/agent.json";
/// 任务清单在空间用户元数据里的键：元数据键不接受隐藏段（首段以 `.` 开头即被服务端拒绝），
/// 与本地路径分开命名；同一份清单在两种容器各存一份，内容形状一致。
pub const TODO_META_KEY: &str = "todos/agent";

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
            .my_meta_patch(space_id, &[(TODO_META_KEY.to_string(), content)])
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
        assert_eq!(tools.len(), 17, "名册长度");
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
        // 追加：原内容末尾无换行时自动补一个，新内容自成一行而不是接在上一行尾
        let r = execute(
            &ctx,
            "append_file",
            json!({ "path": "笔记/待办.md", "content": "- [ ] 二" }),
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
    async fn read_file_offset_out_of_range_reports_range_not_empty() {
        let dir = TempDir::new("ai-read-offset");
        let ctx = ctx(&dir);
        execute(&ctx, "write_file", json!({ "path": "a.md", "content": "一\n二\n" })).await;
        queue_of(&ctx).drain().await.unwrap();
        // offset 越过末行：非空文件不得报成空文件（模型会据此判定内容为空）
        let r = execute(&ctx, "read_file", json!({ "path": "a.md", "offset": 9 })).await;
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("空文件"), "{}", r.text);
        assert!(r.text.contains("offset 超出范围") && r.text.contains("共 2 行"), "{}", r.text);
        // 恰为末行的 offset 照常渲染
        let r = execute(&ctx, "read_file", json!({ "path": "a.md", "offset": 2 })).await;
        assert!(r.text.contains("2| 二"), "{}", r.text);
        // 真空文件仍报空文件
        execute(&ctx, "write_file", json!({ "path": "empty.md", "content": "" })).await;
        queue_of(&ctx).drain().await.unwrap();
        let r = execute(&ctx, "read_file", json!({ "path": "empty.md" })).await;
        assert!(r.text.contains("空文件"), "{}", r.text);
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
        for (tool, args, want) in [
            ("write_file", json!({ "path": "表.atb", "content": "{}" }), "table_edit"),
            ("append_file", json!({ "path": "画布.atlx", "content": "{}" }), "canvas_edit"),
            ("edit_file", json!({ "path": "表.ATB", "find": "a", "replace": "b" }), "table_edit"),
        ] {
            let r = execute(&ctx, tool, args).await;
            assert!(r.is_error && r.text.contains("补丁合并"), "{tool}：{}", r.text);
            assert!(r.text.contains(want), "{tool} 未指向 {want}：{}", r.text);
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
    async fn table_read_and_edit_roundtrip() {
        let dir = TempDir::new("ai-table");
        let ctx = ctx(&dir);
        let table = crate::vault::TableFile {
            schema: crate::vault::TABLE_SCHEMA.into(),
            id: "t1".into(),
            title: "任务".into(),
            fields: vec![
                crate::vault::TableField {
                    id: "f1".into(),
                    name: "名称".into(),
                    field_type: "text".into(),
                    options: None,
                    width: None,
                    calc_type: None,
                },
                crate::vault::TableField {
                    id: "f2".into(),
                    name: "状态".into(),
                    field_type: "singleSelect".into(),
                    options: Some(vec!["待办".into(), "完成".into()]),
                    width: None,
                    calc_type: None,
                },
            ],
            rows: vec![crate::vault::TableRow {
                id: "r1".into(),
                values: Default::default(),
                height: None,
                styles: None,
            }],
            created_at: 0,
            updated_at: 0,
        };
        std::fs::write(
            dir.join("表.atb"),
            serde_json::to_string_pretty(&table).unwrap(),
        )
        .unwrap();
        let r = execute(&ctx, "table_read", json!({ "path": "表.atb" })).await;
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("f1 名称（text）"), "{}", r.text);
        assert!(r.text.contains("f2 状态（singleSelect：待办 / 完成）"), "{}", r.text);
        assert!(r.text.contains("r1 | f1=null | f2=null"), "{}", r.text);
        assert!(r.text.contains("已到末行"), "{}", r.text);
        // 编辑：写单元格 + 新增行，落盘后行数变化
        let r = execute(
            &ctx,
            "table_edit",
            json!({ "path": "表.atb", "ops": [
                { "op": "set_cell", "rowId": "r1", "fieldId": "f1", "value": "写文档" },
                { "op": "add_row", "cells": { "f2": "完成" } }
            ] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 项操作"), "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        let saved: crate::vault::TableFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join("表.atb")).unwrap()).unwrap();
        assert_eq!(saved.rows.len(), 2);
        assert_eq!(saved.rows[0].values["f1"], json!("写文档"));
        assert_eq!(saved.rows[1].values["f2"], json!("完成"));
        // 错误路径：行 / 字段 id 不存在、类型不符、扩展名不符
        for (args, want) in [
            (json!({ "path": "表.atb", "ops": [{ "op": "set_cell", "rowId": "nope", "fieldId": "f1", "value": "x" }] }), "行 id 不存在"),
            (json!({ "path": "表.atb", "ops": [{ "op": "set_cell", "rowId": "r1", "fieldId": "nope", "value": "x" }] }), "字段 id 不存在"),
            (json!({ "path": "表.atb", "ops": [{ "op": "set_cell", "rowId": "r1", "fieldId": "f2", "value": "未定义" }] }), "可选值"),
            (json!({ "path": "表.atb", "ops": [{ "op": "boom" }] }), "未知操作"),
            (json!({ "path": "表.atb" }), "缺少 ops"),
            (json!({ "path": "笔记.md" }), "不是表格文件"),
        ] {
            let r = execute(&ctx, "table_edit", args).await;
            assert!(r.is_error && r.text.contains(want), "期望「{want}」，实际：{}", r.text);
        }
        // 编辑不存在的表格 = 按路径新建（读仍然要求文件存在）
        let r = execute(&ctx, "table_edit", json!({ "path": "新表.atb", "ops": [{ "op": "add_row" }] })).await;
        assert!(!r.is_error, "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        assert!(dir.join("新表.atb").is_file());
        let r = execute(&ctx, "table_read", json!({ "path": "无此表.atb" })).await;
        assert!(r.is_error && r.text.contains("不存在或不可读"), "{}", r.text);
    }

    #[tokio::test]
    async fn canvas_read_and_edit_roundtrip() {
        let dir = TempDir::new("ai-canvas");
        let ctx = ctx(&dir);
        std::fs::create_dir_all(dir.join("笔记")).unwrap();
        let canvas = crate::vault::CanvasFile {
            schema: crate::vault::CANVAS_SCHEMA.into(),
            id: "c1".into(),
            title: "画布".into(),
            nodes: vec![],
            edges: vec![],
            created_at: 0,
            updated_at: 0,
        };
        std::fs::write(
            dir.join("画布.atlx"),
            serde_json::to_string_pretty(&canvas).unwrap(),
        )
        .unwrap();
        let r = execute(
            &ctx,
            "canvas_edit",
            json!({ "path": "画布.atlx", "ops": [
                { "op": "add_text_card", "text": "第一行\n正文" },
                { "op": "add_note_card", "path": "笔记/方案.md" }
            ] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 项操作"), "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        let r = execute(&ctx, "canvas_read", json!({ "path": "画布.atlx" })).await;
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("文本卡「第一行」"), "{}", r.text);
        assert!(r.text.contains("笔记卡「方案」") && r.text.contains("→ 笔记/方案.md"), "{}", r.text);
        let saved: crate::vault::CanvasFile =
            serde_json::from_str(&std::fs::read_to_string(dir.join("画布.atlx")).unwrap()).unwrap();
        let (a, b) = (saved.nodes[0].id.clone(), saved.nodes[1].id.clone());
        // 连线 + 改正文 + 删节点
        let r = execute(
            &ctx,
            "canvas_edit",
            json!({ "path": "画布.atlx", "ops": [
                { "op": "connect", "from": a.clone(), "to": b.clone(), "kind": "link" },
                { "op": "update_text", "nodeId": a.clone(), "text": "改后" }
            ] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        let r = execute(&ctx, "canvas_read", json!({ "path": "画布.atlx" })).await;
        assert!(r.text.contains("（关联）"), "{}", r.text);
        assert!(r.text.contains("正文：改后"), "{}", r.text);
        let r = execute(
            &ctx,
            "canvas_edit",
            json!({ "path": "画布.atlx", "ops": [{ "op": "delete_node", "nodeId": b.clone() }] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        // 错误路径：节点不存在、非画布扩展名
        let r = execute(
            &ctx,
            "canvas_edit",
            json!({ "path": "画布.atlx", "ops": [{ "op": "update_text", "nodeId": b, "text": "x" }] }),
        )
        .await;
        assert!(r.is_error && r.text.contains("节点 id 不存在"), "{}", r.text);
        let r = execute(&ctx, "canvas_edit", json!({ "path": "笔记.md", "ops": [] })).await;
        assert!(r.is_error && r.text.contains("不是画布文件"), "{}", r.text);
        let r = execute(&ctx, "canvas_read", json!({ "path": "无此画布.atlx" })).await;
        assert!(r.is_error && r.text.contains("不存在或不可读"), "{}", r.text);
    }

    /// fake 宿主：记录编辑调用（工具层只经 host 写，不落盘）。
    struct FakeHost {
        calls: Arc<std::sync::Mutex<Vec<String>>>,
        fail: bool,
    }

    impl crate::ai::edit_host::AiEditHost for FakeHost {
        fn apply_note(
            &self,
            path: &str,
            _op: crate::ai::edit_host::NoteOp,
            _actor: &crate::ai::edit_host::EditActor,
        ) -> crate::ai::edit_host::EditFuture<'_> {
            let calls = self.calls.clone();
            let path = path.to_string();
            Box::pin(async move {
                calls.lock().unwrap().push(format!("note:{path}"));
                Ok("已写入".to_string())
            })
        }

        fn apply_table(
            &self,
            path: &str,
            ops: Vec<crate::ai::edit_host::TableOp>,
            actor: &crate::ai::edit_host::EditActor,
        ) -> crate::ai::edit_host::EditFuture<'_> {
            let calls = self.calls.clone();
            let path = path.to_string();
            let count = ops.len();
            let agent = actor.agent.agent_name.clone();
            let fail = self.fail;
            Box::pin(async move {
                calls.lock().unwrap().push(format!("table:{path}:{count}:{agent}"));
                if fail {
                    Err("宿主拒绝写入".to_string())
                } else {
                    Ok("已编辑表".to_string())
                }
            })
        }

        fn apply_canvas(
            &self,
            path: &str,
            ops: Vec<crate::ai::edit_host::CanvasOp>,
            actor: &crate::ai::edit_host::EditActor,
        ) -> crate::ai::edit_host::EditFuture<'_> {
            let calls = self.calls.clone();
            let path = path.to_string();
            let count = ops.len();
            let agent = actor.agent.agent_name.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(format!("canvas:{path}:{count}:{agent}"));
                Ok("已编辑画布".to_string())
            })
        }
    }

    fn actor() -> crate::ai::edit_host::EditActor {
        crate::ai::edit_host::EditActor {
            author: crate::history::HistoryAuthor {
                id: "u1".into(),
                name: "用户".into(),
                device: "PC".into(),
            },
            agent: crate::ai::edit_host::AiIdentity {
                agent_id: "builtin-agent".into(),
                agent_name: "Agent".into(),
            },
        }
    }

    #[tokio::test]
    async fn domain_edits_route_through_host() {
        let dir = TempDir::new("ai-table-host");
        let mut ctx = ctx(&dir);
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        ctx.host = Some(Arc::new(FakeHost { calls: calls.clone(), fail: false }));
        ctx.actor = Some(actor());
        let r = execute(
            &ctx,
            "table_edit",
            json!({ "path": "表.atb", "ops": [{ "op": "add_row" }] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.text, "已编辑表");
        let r = execute(
            &ctx,
            "canvas_edit",
            json!({ "path": "画布.atlx", "ops": [{ "op": "add_text_card", "text": "x" }] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.text, "已编辑画布");
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec!["table:表.atb:1:Agent".to_string(), "canvas:画布.atlx:1:Agent".to_string()]
        );
        // 无宿主身份：拒绝写入（不静默）
        let mut no_actor = ctx.clone();
        no_actor.actor = None;
        let r = execute(
            &no_actor,
            "table_edit",
            json!({ "path": "表.atb", "ops": [{ "op": "add_row" }] }),
        )
        .await;
        assert!(r.is_error && r.text.contains("身份缺失"), "{}", r.text);
        // 宿主失败：错误文本原样回传模型
        let mut failing = ctx.clone();
        failing.host = Some(Arc::new(FakeHost { calls: calls.clone(), fail: true }));
        let r = execute(
            &failing,
            "table_edit",
            json!({ "path": "表.atb", "ops": [{ "op": "add_row" }] }),
        )
        .await;
        assert!(r.is_error && r.text.contains("宿主拒绝写入"), "{}", r.text);
    }

    #[tokio::test]
    async fn domain_edit_creates_missing_file() {
        let dir = TempDir::new("ai-domain-create");
        let ctx = ctx(&dir);
        // 文件不存在：按路径新建空模型再应用 ops（标题取文件名）
        let r = execute(
            &ctx,
            "table_edit",
            json!({ "path": "计划/排期.atb", "ops": [{ "op": "add_row", "cells": {} }] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        let table = vault::read_table_file(&dir.join("计划/排期.atb")).unwrap();
        assert_eq!(table.title, "排期");
        assert_eq!(table.rows.len(), 1, "ops 应作用在新建的空表上");
        let r = execute(
            &ctx,
            "canvas_edit",
            json!({ "path": "方案.atlx", "ops": [{ "op": "add_text_card", "text": "首卡" }] }),
        )
        .await;
        assert!(!r.is_error, "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        let canvas = vault::read_canvas_file(&dir.join("方案.atlx")).unwrap();
        assert_eq!(canvas.title, "方案");
        assert_eq!(canvas.nodes.len(), 1);
        // 坏文件不得当空模型覆盖（解析失败即报错，磁盘原样）
        std::fs::write(dir.join("坏.atb"), "{不是 JSON").unwrap();
        let r = execute(&ctx, "table_edit", json!({ "path": "坏.atb", "ops": [{ "op": "add_row" }] })).await;
        assert!(r.is_error, "{}", r.text);
        queue_of(&ctx).drain().await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("坏.atb")).unwrap(), "{不是 JSON");
    }

    #[test]
    fn domain_reads_reject_non_domain_files() {
        let dir = TempDir::new("ai-domain-ext");
        let ctx = ctx(&dir);
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let r = rt.block_on(execute(&ctx, "table_read", json!({ "path": "a.md" })));
        assert!(r.is_error && r.text.contains("不是表格文件"), "{}", r.text);
        let r = rt.block_on(execute(&ctx, "canvas_read", json!({ "path": "a.atb" })));
        assert!(r.is_error && r.text.contains("不是画布文件"), "{}", r.text);
    }
    #[tokio::test]
    async fn web_search_without_provider_degrades() {
        let dir = TempDir::new("ai-tools");
        let ctx = ctx(&dir);
        let r = execute(&ctx, "web_search", json!({ "query": "rust" })).await;
        assert!(r.is_error && r.text.contains("未配置搜索源"));
    }

    #[test]
    fn legacy_builtin_agent_tools_migrate_to_current_defaults() {
        // 存量内置 Agent 若仍是旧默认集（用户未改过），装载迁移补上领域工具
        let mut agents = crate::vault::builtin_agents();
        agents[0].tools = ["web_search", "web_fetch", "read_file", "glob", "grep", "list_dir", "read_history"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        agents[1].tools = [
            "web_search", "web_fetch", "read_file", "glob", "grep", "list_dir", "read_history",
            "write_file", "append_file", "edit_file", "move", "delete", "todo_write",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        crate::vault::migrate_agent_configs(&mut agents);
        let defaults = crate::vault::builtin_agents();
        assert_eq!(agents[0].tools, defaults[0].tools, "只读 Agent 应补上表格/画布读工具");
        assert_eq!(agents[1].tools, defaults[1].tools, "全量 Agent 应补上领域工具");
        // 用户改过的工具集不动
        let mut custom = crate::vault::builtin_agents();
        custom[1].tools = vec!["read_file".to_string()];
        crate::vault::migrate_agent_configs(&mut custom);
        assert_eq!(custom[1].tools, vec!["read_file"]);
    }
}
