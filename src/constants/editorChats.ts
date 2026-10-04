/**
 * AI 对话面板相关常量。
 */

/** `.atelyx/editor-chats-meta.json` 的 schema 版本号（面板级覆盖；Rust 侧 `vault.rs` 有同名常量，两端须保持一致）。 */
export const EDITOR_CHATS_META_SCHEMA = "atelyx-editor-chats-meta/v1" as const;

/** 会话消息正文 .jsonl 目录（相对仓库根；位于 `.atelyx/` 隐藏目录下，文件面板不显示）。 */
export const CHAT_HISTORY_DIR = ".atelyx/对话历史";

/** 会话消息正文文件扩展名（JSON Lines：一行一条消息记录，追加式写）。 */
export const CHAT_MESSAGE_EXT = ".jsonl";

/** 会话元数据侧车扩展名（`<会话 id>.meta.json`）。 */
export const CHAT_META_EXT = ".meta.json";

/** 协作空间内会话消息正文的 user meta 键前缀（`chat/messages/<会话 id>`；空间无本地副本，正文以 user meta 为真源）。 */
export const CHAT_MESSAGES_META_PREFIX = "chat/messages/";
