/**
 * AI 对话面板（右侧边栏）的消息与会话类型。
 * 持久化：以 `.atelyx/对话历史/` 文件夹为真相——每会话一个消息 `.jsonl`（追加式写）+ 可选
 * `.meta.json` 元数据侧车（title/agentId/compaction）；会话清单 = 扫目录（无整文件索引，多设备共享文件夹
 * 实时互见）；面板级覆盖存 `.atelyx/editor-chats-meta.json`。单一全局历史，不按笔记归属。
 *
 * 与画布对话（types/message.ts）的差异：面板是纯文本对话，
 * 无 attachments/refs/system 消息——错误占位用 content 的 `[错误]` 前缀标记（同 runStream 约定）。
 */
import { EDITOR_CHATS_META_SCHEMA } from "@/constants/editorChats";
import type { AgentStep } from "./message";
import type { ConversationCompaction } from "./compaction";
import type { ReasoningEffort } from "./provider";

export type EditorChatRole = "user" | "assistant";

/**
 * 用户消息发送时固化的笔记引用（拖入输入框的 @引用，运行时有效）。
 * 与画布对话的 MessageRef 对称：仅「显示 @chip + 点击打开笔记」用，不参与模型上下文
 * （@引用 只发文件路径，路径块随 user 消息 content 固化，见 chatPanelStore.send）。
 * 随消息 .jsonl 记录持久化（refs 字段），重开会话恢复 @chip。
 */
export interface EditorChatMessageRef {
  /** 被引用笔记的相对仓库根路径 */
  file: string;
  /** @chip 显示名（笔记名去 .md 后缀） */
  label: string;
}

/**
 * 笔记划词 AI 改写请求（NoteEditor 划词右键确认 → 面板输入框插入指令文本）。
 * 不改动 Agent 开关：agent 开着（勾了 edit_file）AI 直接改文件，没开则输出修改建议。
 */
export interface NoteRewriteRequest {
  /** 被划词笔记的相对仓库根路径 */
  noteFile: string;
  /** 笔记显示名（去 .md 后缀） */
  label: string;
  /** 划词选中的原文片段 */
  selectedText: string;
  /** 用户追加的评论/要求（可为空 = 仅询问/泛化改写） */
  comment: string;
}

export interface EditorChatMessage {
  /** 稳定 id：创建时生成，随 .jsonl 记录持久化，恢复不重新生成（画布/面板均需稳定 id 语义）。 */
  id: string;
  role: EditorChatRole;
  content: string;
  /**
   * Agent 步进（assistant 消息展示用，思考与工具交错，每步思考可见）。
   * 随消息 .jsonl 记录结构化持久化，重开会话恢复展示（含工具步）；
   * 同时是下一轮请求历史重建的工具消息来源（见 `utils/agentHistory`）。
   */
  steps?: AgentStep[];
  /**
   * user 消息气泡显示用：发送时的原始输入，与 content 分离——content 含「引用文件」
   * 路径块（@引用），气泡避免展示注入内容。
   * 随消息 .jsonl 记录持久化（displayContent 字段）。
   */
  displayContent?: string;
  /** 该 user 消息发送时拖入的笔记引用（气泡显示只读 @chip，点击打开笔记）；随 .jsonl 记录持久化。 */
  refs?: EditorChatMessageRef[];
  /** 真实创建时间（发送时生成，随 .jsonl 记录持久化，恢复不重排）。 */
  createdAt: number;
}

/** 会话元数据侧车（`.atelyx/对话历史/<会话 id>.meta.json`）：仅可变会话级字段。 */
export interface ChatSessionMeta {
  id: string;
  /** 会话标题（首条 user 消息前缀 / LLM 自动命名 / 手动重命名；历史会话列表展示）。 */
  title?: string;
  /** 引用的 Agent 配置 id（仓库级 `.atelyx/agents.json`；发送时实时解析系统提示词/工具；缺省 = 按预置「对话」Agent 处理）。 */
  agentId?: string;
  /**
   * 会话压缩注解（用户手动触发）：锚点及其之前的消息不进模型请求。
   * 消息本体不动，仅重建历史时按注解裁剪；锚点缺失即注解失效（见 `splitByCompaction`）。
   */
  compaction?: ConversationCompaction;
}

/** list_chat_sessions 返回行（Rust 扫 `.atelyx/对话历史/` 目录结果）。 */
export interface ChatSessionRow {
  id: string;
  /** 消息正文 .jsonl 相对仓库根路径（`.atelyx/对话历史/<会话 id>.jsonl`）。 */
  file: string;
  /** 元数据侧车（缺省 = 缺省标题/Agent，由前端按首条消息派生标题）。 */
  meta: ChatSessionMeta | null;
}

/** 运行时会话（内存态：会话元数据 + 消息正文；createdAt/updatedAt 由消息派生，不落盘）。 */
export interface EditorChatSession {
  id: string;
  /** 会话标题（首条 user 消息前缀，历史会话列表展示）。 */
  title?: string;
  /** 引用的 Agent 配置 id（同 ChatSessionMeta.agentId）。 */
  agentId?: string;
  /** 会话压缩注解（同 ChatSessionMeta.compaction；运行时态 + 侧车持久化）。 */
  compaction?: ConversationCompaction;
  /** 消息正文 .jsonl 相对仓库根路径（`.atelyx/对话历史/<会话 id>.jsonl`）。 */
  file: string;
  /** 首条消息创建时间（运行时派生，不持久化）。 */
  createdAt: number;
  /** 末条消息创建时间（最近对话排序，运行时派生，不持久化）。 */
  updatedAt: number;
  messages: EditorChatMessage[];
}

/** 面板级模型覆盖（优先于仓库默认模型；缺省 = 跟随仓库默认）。 */
export interface EditorChatModelOverride {
  providerId: string;
  model: string;
}

/** `.atelyx/editor-chats-meta.json` 文件结构（面板级覆盖，设备偏好）。 */
export interface ChatMetaFile {
  schema: typeof EDITOR_CHATS_META_SCHEMA;
  modelOverride: EditorChatModelOverride | null;
  /** 面板级推理等级覆盖（null = 不指定/跟随默认；与模型覆盖正交，跟随仓库默认时也可单独设置）。 */
  effortOverride: ReasoningEffort | null;
}
