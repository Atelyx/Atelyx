//! AI 对话会话存储：消息 .jsonl 追加、元数据侧车、面板级覆盖与会话图片附件。

use std::path::{Path, PathBuf};

use nanoid::nanoid;
use serde::{Deserialize, Serialize};

use super::{atomic_write, image_ext_of, safe_join};

// 以对话历史文件夹为真相：每会话 = 一个消息 .jsonl（真追加式）+ 可选 .meta.json 元数据侧车，
// 会话清单 = 扫目录（无整文件索引）——多设备并发新建/删除/改名互不覆盖（各自追加自己的会话文件）。
// 元数据侧车只在改名/换 Agent 时写；消息正文文件为纯追加（截断场景除外）。
// 不含 API key（key 只进全局 keychain）。.atelyx/ 为隐藏目录（文件树不显示）。

/// 会话消息正文 .jsonl 目录（相对仓库根）。
pub const CHAT_HISTORY_DIR: &str = ".atelyx/对话历史";

/// 会话消息正文文件扩展名（JSON Lines：一行一条消息记录，追加式写）。
pub const CHAT_MESSAGE_EXT: &str = ".jsonl";

/// 会话元数据侧车扩展名（`<会话 id>.meta.json`）。
pub const CHAT_META_EXT: &str = ".meta.json";

/// 面板级覆盖文件（相对仓库根；设备偏好，不跨设备传播、不监听）。
pub const EDITOR_CHATS_META_FILE: &str = ".atelyx/editor-chats-meta.json";

/// editor-chats-meta.json 的 schema 版本。
pub const EDITOR_CHATS_META_SCHEMA: &str = "atelyx-editor-chats-meta/v1";

/// 会话压缩注解（用户手动触发；边界之前的消息不进模型请求，消息本体不删改）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatCompaction {
    /// 模型生成的结构化检查点摘要原文（不含框架包裹）。
    pub summary: String,
    /// 压缩覆盖到的最后一条消息 id（该条及其之前不进模型历史）。
    pub up_to_message_id: String,
    /// 被覆盖的消息条数。
    pub message_count: u32,
    /// 生成时间（毫秒时间戳）。
    pub created_at: i64,
    /// 生成摘要所用的供应商 id（展示溯源）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// 生成摘要所用的模型 id（展示溯源）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// 会话元数据侧车（.atelyx/对话历史/<会话 id>.meta.json）：仅可变会话级字段。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionMeta {
    pub id: String,
    /// 会话标题（首条 user 消息前缀 / LLM 自动命名 / 手动重命名；历史列表展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 引用的 Agent 配置 id（仓库级 `.atelyx/agents.json`；发送时实时解析系统提示词/工具）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// 会话压缩注解（缺省 = 未压缩）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<ChatCompaction>,
}

/// 面板级模型覆盖（优先于仓库默认模型）。
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct EditorChatModelOverride {
    pub provider_id: String,
    pub model: String,
}

/// .atelyx/editor-chats-meta.json 根结构（面板级覆盖）。
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatMetaFile {
    /// 缺失仍可解析：schema 只是演进标记，缺一段标记不足以否定一份结构完整的覆盖。
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub model_override: Option<EditorChatModelOverride>,
    /// 面板级推理等级覆盖（off/low/medium/high；null = 不指定/跟随默认；与模型覆盖正交）。
    #[serde(default)]
    pub effort_override: Option<String>,
    /// 面板级 Agent 选择（上次选择；新建会话继承；null = 未选过，走内置「对话」）。
    #[serde(default)]
    pub agent_override: Option<String>,
}

/// 读面板级覆盖（.atelyx/editor-chats-meta.json；不存在 = 默认，损坏先备份原文再按默认继续）。
pub fn read_editor_chats_meta_file(root: &Path) -> Result<ChatMetaFile, String> {
    let file: ChatMetaFile =
        crate::config::read_or_default(&root.join(EDITOR_CHATS_META_FILE), "editor-chats-meta")?;
    // schema 校验：只认 v1——旧版存量视为空覆盖（不报错，也不当损坏备份）
    if file.schema != EDITOR_CHATS_META_SCHEMA {
        return Ok(ChatMetaFile::default());
    }
    Ok(file)
}

/// 写面板级覆盖（原子写 .atelyx/editor-chats-meta.json）。
pub fn write_editor_chats_meta_file(root: &Path, file: &ChatMetaFile) -> Result<(), String> {
    crate::config::write(&root.join(EDITOR_CHATS_META_FILE), file)
}

/// 校验并定位会话消息正文路径：必须位于 `.atelyx/对话历史/` 下且以 .jsonl 结尾（防越权读写任意文件）。
fn chat_messages_path(root: &Path, file: &str) -> Result<PathBuf, String> {
    let prefix = format!("{}/", CHAT_HISTORY_DIR);
    if !file.starts_with(&prefix) || !file.ends_with(".jsonl") {
        return Err(format!("非法会话消息路径：{}", file));
    }
    safe_join(root, file, false)
}

/// 读会话消息正文 .jsonl（文件不存在报错，由调用方降级为空消息）。
pub fn read_chat_messages_file(root: &Path, file: &str) -> Result<String, String> {
    let path = chat_messages_path(root, file)?;
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

/// 写会话消息正文 .jsonl（自动建 `.atelyx/对话历史/` 目录 + 原子写；末尾补 `\n`，维持「文件以 `\n` 结尾」不变量）。
pub fn write_chat_messages_file(root: &Path, file: &str, content: &str) -> Result<(), String> {
    std::fs::create_dir_all(root.join(CHAT_HISTORY_DIR)).map_err(|e| e.to_string())?;
    let path = chat_messages_path(root, file)?;
    let normalized = if content.ends_with('\n') {
        content.to_string()
    } else {
        format!("{content}\n")
    };
    atomic_write(&path, &normalized)
}

/// 追加记录：消息 .jsonl 的增量（一行一条消息记录；
/// refs/steps/attachments 透传 JSON——Rust 侧不镜像嵌套类型，序列化时按字段名原样写出）。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageRecord {
    pub id: String,
    /// "user" | "assistant"
    pub role: String,
    pub content: String,
    /// user 消息气泡显示用：发送时的原始输入，与 content 分离（content 可能含注入的笔记全文）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_content: Option<String>,
    /// 发送时拖入的笔记引用（随记录持久化，重开会话恢复 @chip）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refs: Option<serde_json::Value>,
    /// Agent 步进（思考/工具交错；工具步含调用过程，随记录持久化恢复展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<serde_json::Value>,
    /// 附件 `file` 引用（payload 运行时缓存已在入库前剥离，不落盘）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<serde_json::Value>,
    pub created_at: i64,
}

/// 编码一次追加批：分隔符（如需）+ 每条记录 JSON + 换行，拼成单个缓冲区。
/// 一次 `write_all` 写完本批——共享盘/多设备并发追加时记录不与其它写入端在行中间交错，
/// 每行仍是完整 JSON 可独立解析（跨批交错只发生在行边界）。
fn build_chat_append_buffer(records: &[ChatMessageRecord], needs_sep: bool) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    if needs_sep {
        buf.push(b'\n');
    }
    for record in records {
        let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }
    Ok(buf)
}

/// 追加式写会话消息正文 .jsonl（消息增长场景：调用方只传新增记录，每记录一行紧凑 JSON，省全量重拼与 IPC 载荷）。
/// 真 OS 追加（open append + fsync）而非读-改-写全量重写：多设备并发追加同一会话互不覆盖
/// （两写皆落、行交错，每行完整 JSON 可独立解析）。写前校验文件以 `\n` 结尾，缺则补 `\n`——
/// 崩溃残尾（不完整行）与新记录保持分隔，残尾由调用方解析时跳过（降级不丢已提交记录）。
/// 文件缺失直接报错（调用方回落全量重写重建历史——新建会话首次落盘走全量写路径，追加永不该建新文件）。
/// 截断场景（回到此处/重新生成删消息）仍走 write_chat_messages_file 全量重写。
pub fn append_chat_messages_file(
    root: &Path,
    file: &str,
    records: &[ChatMessageRecord],
) -> Result<(), String> {
    use std::io::{Read, Seek, Write};
    let path = chat_messages_path(root, file)?;
    if !path.exists() {
        return Err("会话消息文件缺失，请重写".to_string());
    }
    // 崩溃残尾分隔：末尾非 `\n` 时先补一个，避免残尾与新记录粘成一行。
    // 用独立只读句柄查尾（O_APPEND 写句柄在 Windows 下不可读）。
    let needs_sep = {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size == 0 {
            false
        } else {
            let mut r = std::fs::File::open(&path).map_err(|e| e.to_string())?;
            r.seek(std::io::SeekFrom::End(-1)).map_err(|e| e.to_string())?;
            let mut last = [0u8; 1];
            r.read_exact(&mut last).map_err(|e| e.to_string())?;
            last[0] != b'\n'
        }
    };
    let buf = build_chat_append_buffer(records, needs_sep)?;
    let mut handle = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    // 本批一次写入：共享盘/多设备并发追加时，本批记录不会与其它写入端在行中间交错
    handle.write_all(&buf).map_err(|e| e.to_string())?;
    handle.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

/// 消息 .jsonl 追加原始行（会话写链专用）：tail 为确定性序列化、以 `\n` 结尾的多行
/// 文本，原样落盘。文件缺失报错——调用方（ai::session 写链）先核对磁盘内容与内存
/// 基线一致才追加，不一致或缺失时回落全量重写。
pub fn append_chat_messages_raw(root: &Path, file: &str, tail: &str) -> Result<(), String> {
    use std::io::Write;
    let path = chat_messages_path(root, file)?;
    let mut handle = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    handle.write_all(tail.as_bytes()).map_err(|e| e.to_string())?;
    handle.sync_all().map_err(|e| e.to_string())?;
    Ok(())
}

/// 删会话消息正文 .jsonl（不存在视为成功——幂等，删除会话时调用）。
pub fn delete_chat_messages_file(root: &Path, file: &str) -> Result<(), String> {
    let path = chat_messages_path(root, file)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 校验并定位会话元数据侧车路径：必须位于 `.atelyx/对话历史/` 下且以 .meta.json 结尾（防越权）。
fn chat_meta_path(root: &Path, file: &str) -> Result<PathBuf, String> {
    let prefix = format!("{}/", CHAT_HISTORY_DIR);
    if !file.starts_with(&prefix) || !file.ends_with(CHAT_META_EXT) {
        return Err(format!("非法会话元数据路径：{}", file));
    }
    safe_join(root, file, false)
}

/// 读会话元数据侧车（不存在 = None；损坏先备份原文再按 None 继续——缺省标题/Agent）。
pub fn read_chat_session_meta_file(
    root: &Path,
    file: &str,
) -> Result<Option<ChatSessionMeta>, String> {
    let path = chat_meta_path(root, file)?;
    if !path.exists() {
        return Ok(None);
    }
    let (meta, backup) = crate::config::read::<ChatSessionMeta>(
        &path,
        "chat-meta",
        crate::config::OnMissing::Default,
        crate::config::OnCorrupt::BackupDefault,
    )?;
    // 损坏（已留档）与缺失同视：会话照常可读，只是回到无标题/无 Agent 的状态
    Ok(backup.is_none().then_some(meta))
}

/// 写会话元数据侧车（自动建目录 + 原子写）。
pub fn write_chat_session_meta_file(
    root: &Path,
    file: &str,
    meta: &ChatSessionMeta,
) -> Result<(), String> {
    crate::config::write(&chat_meta_path(root, file)?, meta)
}

/// 删会话元数据侧车（不存在视为成功——幂等）。
pub fn delete_chat_session_meta_file(root: &Path, file: &str) -> Result<(), String> {
    let path = chat_meta_path(root, file)?;
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 会话清单行（扫目录结果：一个会话 = 一个消息 .jsonl + 可选元数据侧车）。
#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionRow {
    pub id: String,
    /// 消息正文 .jsonl 相对仓库根路径（`.atelyx/对话历史/<会话 id>.jsonl`）
    pub file: String,
    /// 元数据侧车（`.atelyx/对话历史/<会话 id>.meta.json`；缺失/损坏 = 调用方按未命名/缺省 Agent 展示）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<ChatSessionMeta>,
}

/// 扫 `.atelyx/对话历史/` 列出全部会话（以 `.jsonl` 为存在性真相；孤儿 `.meta.json` 忽略）。
pub fn list_chat_sessions_file(root: &Path) -> Result<Vec<ChatSessionRow>, String> {
    let dir = root.join(CHAT_HISTORY_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(CHAT_MESSAGE_EXT) {
            continue;
        }
        let id = name
            .strip_suffix(CHAT_MESSAGE_EXT)
            .unwrap_or(&name)
            .to_string();
        let file = format!("{CHAT_HISTORY_DIR}/{name}");
        // 读元数据侧车须传 `<id>.meta.json` 路径（误传 `.jsonl` 会被 chat_meta_path 校验拒绝 → meta 恒 None，标题/Agent 加载不出）
        let meta_file = format!("{CHAT_HISTORY_DIR}/{id}{CHAT_META_EXT}");
        let meta = read_chat_session_meta_file(root, &meta_file).ok().flatten();
        rows.push(ChatSessionRow { id, file, meta });
    }
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}

/// 会话图片附件相对路径：`img-<nanoid>.<ext>`（每次导入新文件名，重发不覆盖旧文件）。
/// 会话 id 与消息 .jsonl 同源（uuid 十六进制 + 连字符），只接受该字母表，拒绝路径语义与空串。
pub fn chat_image_rel_path(session_id: &str, file_name: &str) -> Result<String, String> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(format!("非法的会话 id：{session_id}"));
    }
    let ext = image_ext_of(file_name).ok_or_else(|| format!("非图片文件：{file_name}"))?;
    let name = format!("img-{}", nanoid!());
    Ok(format!(".atelyx/temp/chats/{session_id}/{name}.{ext}"))
}

/// 会话图片附件临时目录（`.atelyx/temp/chats/{id}`；删除会话时整体清理）。
pub fn chat_attachment_dir(root: &Path, session_id: &str) -> Result<PathBuf, String> {
    safe_join(root, &format!(".atelyx/temp/chats/{session_id}"), false)
}

#[cfg(test)]
mod chat_append_tests {
    use super::*;

    fn record(id: &str, content: &str) -> ChatMessageRecord {
        ChatMessageRecord {
            id: id.to_string(),
            role: "user".to_string(),
            content: content.to_string(),
            display_content: None,
            refs: None,
            steps: None,
            attachments: None,
            created_at: 1,
        }
    }

    #[test]
    fn buffer_is_one_json_line_per_record() {
        // 无残尾：每记录独立一行，无前导分隔符
        let buf = build_chat_append_buffer(&[record("a", "你好"), record("b", "x\ny")], false).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        // 行首是 JSON（`{`），无分隔符前缀
        assert!(lines[0].starts_with('{'));
        // 每行都是完整可解析的记录（round-trip 校验字段）
        let parsed: ChatMessageRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.id, "a");
        let parsed: ChatMessageRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(parsed.id, "b");
    }

    #[test]
    fn append_line_keeps_attachments_field() {
        // 追加路径按记录序列化：附件引用必须随行保留（serde 不得静默丢字段；
        // 全量重写走原文透传不受此影响，唯独强类型追加曾是丢字段的重灾区）
        let mut with_atts = record("a", "看这个");
        with_atts.attachments = Some(serde_json::json!([
            { "kind": "image", "mime": "image/png", "file": ".atelyx/附件/a.png" }
        ]));
        let buf = build_chat_append_buffer(&[with_atts], false).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let parsed: ChatMessageRecord = serde_json::from_str(text.trim()).unwrap();
        let atts = parsed
            .attachments
            .expect("attachments 必须随追加行持久化");
        assert_eq!(atts[0]["kind"], "image");
        assert_eq!(atts[0]["file"], ".atelyx/附件/a.png");
    }

    #[test]
    fn buffer_prepends_separator_when_trailing_line_missing() {
        // 崩溃残尾（末尾非 `\n`）：先补分隔符，残尾与新记录不粘行
        let buf = build_chat_append_buffer(&[record("a", "x")], true).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with('\n'));
        // 分隔符后的第一条记录独立成行且完整可解析
        let non_empty: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(non_empty.len(), 1);
        let parsed: ChatMessageRecord = serde_json::from_str(non_empty[0]).unwrap();
        assert_eq!(parsed.id, "a");
    }

    #[test]
    fn concurrent_appends_keep_each_line_parseable() {
        // 并发追加不变量：多端各自 O_APPEND 追加后，每行仍是完整 JSON（无行内交错）、
        // 消息不丢失。单机并发进程测试（本环境无 SMB/NAS，无法做跨设备双端压测）。
        let dir = std::env::temp_dir().join(format!("atelyx-chat-append-{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = &dir;
        let rel = ".atelyx/对话历史/s.md.jsonl";
        let path = root.join(".atelyx/对话历史").join("s.md.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 先全量写一个空文件（追加路径要求文件已存在）
        write_chat_messages_file(root, rel, "").unwrap();

        let total_threads = 8;
        let per_thread = 20;
        std::thread::scope(|scope| {
            for t in 0..total_threads {
                let rel = rel.to_string();
                let root = root.clone();
                scope.spawn(move || {
                    let records: Vec<ChatMessageRecord> =
                        (0..per_thread).map(|i| record(&format!("t{t}-{i}"), "并发")).collect();
                    append_chat_messages_file(&root, &rel, &records).unwrap();
                });
            }
        });

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), total_threads * per_thread, "消息不得因交错而丢失/破碎");
        let mut ids = std::collections::HashSet::new();
        for line in lines {
            let parsed: ChatMessageRecord = serde_json::from_str(line).unwrap();
            ids.insert(parsed.id);
        }
        assert_eq!(ids.len(), total_threads * per_thread, "每条记录须完整可解析");
    }
}

#[cfg(test)]
mod chat_session_meta_tests {
    use super::{ChatCompaction, ChatSessionMeta};

    /// 侧车是按字段白名单序列化的强类型结构（非整对象透传）：新增字段若不同步此结构，
    /// 调用方写入会被 serde 静默丢弃。此用例锁住压缩注解的落盘往返。
    #[test]
    fn compaction_survives_roundtrip() {
        let meta = ChatSessionMeta {
            id: "s1".to_string(),
            title: Some("标题".to_string()),
            agent_id: None,
            compaction: Some(ChatCompaction {
                summary: "## 会话主题\n- 摘要".to_string(),
                up_to_message_id: "m4".to_string(),
                message_count: 5,
                created_at: 1_700_000_000_000,
                provider_id: Some("openai".to_string()),
                model: Some("gpt-4o".to_string()),
            }),
        };
        let json = serde_json::to_string(&meta).expect("serialize");
        let back: ChatSessionMeta = serde_json::from_str(&json).expect("deserialize");
        let compaction = back.compaction.expect("compaction 不得被丢弃");
        assert_eq!(compaction.summary, "## 会话主题\n- 摘要");
        assert_eq!(compaction.up_to_message_id, "m4");
        assert_eq!(compaction.message_count, 5);
        assert_eq!(back.agent_id, None);
        // camelCase 线名即契约
        assert!(json.contains("\"upToMessageId\""));
        assert!(json.contains("\"messageCount\""));
    }

    #[test]
    fn absent_compaction_is_omitted() {
        let json = serde_json::to_string(&ChatSessionMeta {
            id: "s1".to_string(),
            ..Default::default()
        })
        .expect("serialize");
        assert!(!json.contains("compaction"));
    }
}
