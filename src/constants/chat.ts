/**
 * AI 对话共享常量（画布对话节点 / AI 对话面板共用，单一维护点）。
 */

/** 错误占位前缀：写入消息 content 并随画布/会话持久化；发送前按此前缀过滤，防污染 API 历史。 */
export const ERROR_PREFIX = "[错误]";

/** 流式空闲超时的友好降级文案（超时且回答未产出时写入占位，拼接在 ERROR_PREFIX 后）。 */
export const TIMEOUT_ERROR_TEXT = "响应超时（长时间无输出，已自动停止）";

/** 回复被截断（达到输出上限）的友好降级文案（有正文则拼在其后，正文为空则拼在 ERROR_PREFIX 后作占位）。 */
export const TRUNCATED_TEXT = "回复被截断（达到输出上限，已保留已生成内容）";

/**
 * 工具参数生成中的合成工具行 id 前缀：参数分片缺 id 时由流式引擎以此造临时 id，
 * 完整调用到达后同 id 被真实工具行原位替换；running 态残留由 mergeToolRuns 剪除，
 * 已结算的终态合成行（如「已中断」）作为历史记录保留。
 */
export const PENDING_RUN_ID_PREFIX = "pending:";

/** 对话能力不可用时的提示（对话核心插件停用时，面板占位与发送守卫共用同一文案）。 */
export const CHAT_UNAVAILABLE_TEXT = "AI 对话能力未启用：请到 设置 → 插件 启用「AI 对话核心」";
