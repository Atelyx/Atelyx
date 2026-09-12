/**
 * 对话消息类型：画布对话节点（.atlx 内嵌）与 AI 对话面板会话共用。
 * role 遵循 OpenAI 兼容协议。
 */
export type Role = "system" | "user" | "assistant";

export interface Attachment {
  kind: "image" | "file";
  /**
   * 附件内容（运行时）：图片 = base64 dataURL，文件 = 解析出的文本。
   * **不随 `.atlx` 持久化**（有 `file` 引用时按引用读回，读到的内容缓存在这里）；
   * 缺省 = 尚未读回或读失败，消费方须自行按 `file` 补齐或跳过。
   */
  payload?: string;
  mime: string;
  filename?: string;
  /**
   * 附件内容引用（仓库相对路径）：`.atelyx/temp/<canvasKey>/…` = 未入库（仓库内隐藏临时区），
   * 其余 = 已入库的仓库附件（「保存到仓库」后）。有 `file` 时内容以它为准，`payload` 仅作运行时缓存。
   */
  file?: string;
  /** 来自画布媒体节点（@ 提及 / 连边）时标记源节点 id（用于「已注入」检测），临时附件无此字段 */
  sourceNodeId?: string;
}

/**
 * user 消息发送时固化的文本/搜索引用记录（一次性注入语义）。
 * 与 attachments 的固化语义对称：注入随该 user 消息进历史，未来消息不重复注入。
 */
export interface MessageRef {
  /** 源文本/搜索节点 id（消息气泡 @chip 点击定位目标）；纯路径引用（仓库文件/文件夹，画布无对应节点）= `file:<path>` */
  nodeId: string;
  /** @chip 显示名（内容前缀约 12 字） */
  label: string;
  /** 纯路径引用的仓库路径（有此字段 = 无画布节点，chip 点击按类型打开文件而非定位节点） */
  file?: string;
}

/**
 * 单次工具调用记录（Agent 模式可视化：消息气泡内展示调用过程与结果摘要）。
 * 仅展示性元数据：不进 API 历史（工具轮消息不落历史，重发仍无状态）。
 */
export interface ToolRun {
  /** tool call id（流式累积的 id，跨工具轮唯一） */
  id: string;
  /** 工具名（web_search / web_fetch / read_file / glob / grep / edit_file / write_file） */
  name: string;
  /** 参数摘要展示文本（如「搜索「xxx」」/「修改《x》2 处」） */
  argsSummary: string;
  status: "running" | "done" | "error";
  /** 结果摘要（如「修改《X》2 处」/ 错误信息；running 无） */
  resultSummary?: string;
  /** 完整调用参数（模型原始 JSON 字符串，展开详情展示；旧数据缺字段则展开仅显示可用部分）。 */
  args?: string;
  /** 完整结果文本（展开详情展示；旧数据缺字段则展开仅显示可用部分）。 */
  result?: string;
}

/**
 * Agent 步进的一个步骤（思考与工具调用按序交错，每步工具上方的叙述/思考可见）。
 * - `reasoning`：某一轮工具的思考（`reasoning_content` 流式累积，渲染为可折叠思考行）
 * - `text`：某一轮工具的叙述正文（模型在工具轮里说的普通文本，渲染为该步的叙述行、正文样式；
 *   最终回答轮的叙述由 `promoteTrailingNarration` 轮末提升进 content）
 * - `tool`：一次工具调用（含结果详情，可展开）
 * 仅展示性元数据，不进 API 历史（工具轮消息不落历史；叙述-only 消息的 API/复制正文经 `assistantReplyText` 回退）。
 */
export type AgentStep =
  | { kind: "reasoning"; text: string }
  | { kind: "text"; text: string }
  | { kind: "tool"; run: ToolRun };

/**
 * 对话输入框的待发送附件（临时附件通道）。
 * 生命周期：进托盘 → 随 user 消息发送（转为 Attachment 持久化）→ 无源附件生成影子节点。
 */
export interface PendingAttachment {
  id: string;
  kind: "image" | "file";
  /**
   * 附件内容（运行时）：图片 = dataURL，文件 = 读取的文本内容。
   * **不持久化**，有 `file` 引用时按引用读回；缺省 = 尚未读回或读失败。
   */
  payload?: string;
  mime: string;
  filename?: string;
  /**
   * 内容引用（仓库相对路径）：`.atelyx/temp/<canvasKey>/…` = 未入库（仓库内隐藏临时区），
   * 其余 = 已入库的仓库附件（媒体节点引用）。有 `file` 时内容以它为准。
   */
  file?: string;
  /** 来自画布媒体节点（@ 提及 / 连边）时标记源节点 id（用于「已注入」检测），临时附件无此字段 */
  sourceNodeId?: string;
  /** 文本类文件解析失败，仅作画布参考 */
  parseFailed?: boolean;
}

export interface Message {
  id: string;
  role: Role;
  content: string;
  /**
   * Agent 步进（assistant 消息展示用，思考与工具交错，每步思考可见）。
   * 画布随消息落 `.atlx`；旧数据退化为下方的 reasoningContent/toolRuns 遗留字段，
   * 经 `normalizeAgentSteps` 迁移后统一消费 `steps`（新写入不再写遗留字段）。
   */
  steps?: AgentStep[];
  /**
   * 思考过程（`delta.reasoning_content` 流式累积，思考型模型的推理阶段内容）。
   * 遗留：新数据思考并入 `steps`；仅旧数据读取用。仅作气泡展示，不进 API 历史上下文。
   */
  reasoningContent?: string;
  /**
   * user 消息气泡显示用：发送时的原始输入（含 @提及 标记），与 content 分离——
   * content 是 .md 笔记 @引用 的「引用文件」路径块 + 非文件节点（画布内文本/搜索等）的
   * 就地替换展开版（发给模型/历史重发），气泡避免展示一大篇注入内容。
   */
  displayContent?: string;
  /** user 消息可携带多模态附件（图片走 vision） */
  attachments?: Attachment[];
  /**
   * 该 user 消息发送时一次性注入的文本/搜索引用（固化进消息，未来不重复注入）。
   * 用于消息气泡显示只读 @chip + 点击定位到源节点。
   */
  refs?: MessageRef[];
  /** Agent 步进工具调用过程（assistant 消息展示用：调用了什么工具、结果摘要）。遗留：见 `steps`。 */
  toolRuns?: ToolRun[];
  createdAt: number;
}
