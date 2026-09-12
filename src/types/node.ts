import type { Edge } from "@xyflow/react";
import type { ReasoningEffort } from "./provider";

/** 单条搜索结果（AI 自主搜索产物）。 */
export interface SearchResultItem {
  title: string;
  url: string;
  snippet: string;
}

/** 搜索结果节点 data（结构嵌 .atlx，不单独文件化；4.6）。 */
export interface SearchResultData {
  /** 搜索词（节点标题） */
  query: string;
  results: SearchResultItem[];
  /** 搜索失败时错误信息（节点显示错误 + 重试，4.6 失败降级） */
  error?: string;
  /** 勾选的条目下标（「仅将勾选条目注入上下文」；缺省 = 全部注入） */
  checked?: number[];
}

/**
 * 各节点类型的专属 data 结构。
 * data 由各节点组件消费，持久化为 `.atlx` 文件内嵌 JSON。
 */
export interface ConversationData {
  providerId: string;
  model: string;
  /**
   * 引用的 Agent 配置 id（settingsStore.agents，仓库级 `.atelyx/agents.json`）：
   * 发送时实时解析——系统提示词（引用已注册提示词笔记）注入 system 消息、勾选工具可用；
   * 缺省（未设置）= 按预置「对话」Agent 处理。
   */
  agentId?: string;
  /** LLM 自动生成的话题标题（首轮对话完成后命名，InspectorPanel 来源/血缘显示名用；缺省 = 未命名）。 */
  title?: string;
  /** 推理等级（本节点独立，随 .atlx 持久化；下发 `reasoning_effort`。缺省 = 不指定/跟随默认 与 providerId/model 正交独立）。 */
  reasoningEffort?: ReasoningEffort;
}

export interface TextData {
  /** 标题（显示用） */
  title: string;
  /**
   * 相对仓库根的 `.md` 路径，如 `笔记/提示词.md`。
   * - 有 file = 笔记节点：正文存独立 `.md`（外部可编辑、可跨画布共享），bodyMd 运行时从文件填充、持久化时剥离
   * - 无 file = 画布内文本节点：仅存在于画布，bodyMd 随 `.atlx` 内嵌保存；右键「保存为笔记」后生成 `.md` 转为笔记节点
   */
  file?: string;
  /**
   * 正文。
   * - 笔记节点：运行时从 `.md` 填充，持久化时写回 `.md` 并剥离
   * - 画布内文本节点：随 `.atlx` 内嵌持久化（唯一存储）
   */
  bodyMd?: string;
  /** 引用文件被外部删除/重命名时降级标记（仅笔记节点，运行时态不持久化） */
  fileMissing?: boolean;
}

export interface MediaData {
  /**
   * 附件引用（仓库相对路径）：`.atelyx/temp/<canvasKey>/…` = 未入库（仓库内临时区），其余 = 已入库附件。
   * 内容不在节点里内嵌——图片字节/文本正文按引用读盘（与文本节点 `bodyMd` 剥离同模式）。
   */
  file?: string;
  mime: string;
  kind: "image" | "file";
  /** 图片：dataURL 预览（运行时缓存，按 `file` 读回后填充；**不持久化**——否则画布文件重又内嵌 base64） */
  thumb?: string;
  /** 文件名（画布显示用） */
  name?: string;
  /** 二进制类解析失败时标注，仅作画布参考、不注入模型 */
  parseFailed?: boolean;
  /** 文本类文件解析出的内容（@ 引用/连边时注入用，运行时填充；不持久化） */
  body?: string;
  /**
   * 按图片真实比例计算的展示宽度（px），首次加载时推导并持久化。
   * 用户 resize 后此字段不再生效。
   */
  displayWidth?: number;
  /** 用户是否手动 resize 过此节点，用于区分 auto 高度与固定高度两种渲染分支。 */
  userResized?: boolean;
  /** 引用文件被外部删除/重命名时降级标记（对称 TextData.fileMissing，运行时态不持久化） */
  fileMissing?: boolean;
}

export interface TableData {
  /** 相对仓库根的 .atb 路径（表格节点 = 仓库表格文件引用，如 `项目A/分镜.atb`） */
  file: string;
  title: string;
  /**
   * 表格内容快照文本（运行时从 .atb 填充/外部修改刷新，注入对话上下文用；持久化时剥离——
   * 与笔记节点 bodyMd 同模式，内容在独立文件）。
   */
  snapshot?: string;
  /** 引用文件被外部删除/重命名时降级标记（运行时态不持久化） */
  fileMissing?: boolean;
}

/** 关联边（directed: false）的箭头模式：无向 / 单向 / 双向。缺省 = 无向。 */
export type LinkMode = "none" | "single" | "double";

/**
 * 运行时边（React Flow Edge + 画布扩展字段）。
 * `directed: false` = 关联自由线（无消费语义、可删除、箭头模式由 linkMode 决定）；
 * 缺省（undefined）= 数据流边（金色箭头，引用/产出/血缘语义）。
 */
export type CanvasEdge = Edge & { directed?: boolean; linkMode?: LinkMode };
