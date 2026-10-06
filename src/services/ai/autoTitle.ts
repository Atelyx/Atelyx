/**
 * LLM 话题自动命名：把首轮对话摘要为简短标题，复用当前对话的 provider/model（调用方传入），不新增配置。
 * 失败/空结果返回 null——调用方保留现有占位标题，不阻塞、不重试轰炸。
 */
import { chatOnce, type ChatParams } from "@/services/ai/client";

/** 标题最大长度（中文字符，超出截断）。 */
const TITLE_MAX_LEN = 16;

/** 命名输入截断（防超长首轮对话浪费 token）。 */
const DIALOGUE_MAX_CHARS = 400;

/** 命名响应 token 上限（含思考预算）：短任务小值，防无限生成占住后端槽位。 */
const TITLE_MAX_TOKENS = 512;

/** 命名请求超时：重新命名（全量会话）时模型可能很慢，但也不能无限挂起等待。 */
const AUTO_NAMING_TIMEOUT_MS = 60_000;

/** 命名请求延迟发出时长：避开与主对话请求背靠背发送，防触发端点速率限制。 */
export const AUTO_NAMING_DELAY_MS = 3_000;

/** 各目标（会话 id/对话节点 id）的自动命名中止句柄：发新消息/手动接管时按目标 abort，
 * 防全局单句柄误中止其他目标的命名请求（并发命名场景：A 命名在途时 B 触发命名会覆盖旧句柄）。 */
const titleControllers = new Map<string, AbortController>();

/** 中止进行中的自动命名请求。key = 目标标识（会话 id/对话节点 id）：
 * 指定 = 只中止该目标（发送新消息/手动接管）；省略 = 全部中止（切仓库/切画布）。 */
export function abortAutoTitle(key?: string) {
  if (key !== undefined) {
    titleControllers.get(key)?.abort();
    titleControllers.delete(key);
    return;
  }
  for (const c of titleControllers.values()) c.abort();
  titleControllers.clear();
}

/** 清洗模型输出：去引号/换行/「标题：」前缀/编号，超长截断；空结果 null。 */
function cleanTitle(raw: string): string | null {
  let t = raw.trim();
  t = t.replace(/^["'「『]/u, "").replace(/["'」』]$/u, "").trim();
  t = t.replace(/^标题[:：]\s*/u, "").trim();
  // 多行输出只取首行（部分模型会追加解释）
  t = t.split("\n")[0].trim();
  // 去掉开头编号（"1. " / "1、"）
  t = t.replace(/^\d+[.、]\s*/u, "").trim();
  if (!t) return null;
  return t.slice(0, TITLE_MAX_LEN);
}

interface AutoTitleParams {
  baseUrl: string;
  apiKey: string;
  model: string;
  signal?: AbortSignal;
}

interface AutoTitleResult {
  /** 生成的标题（失败/空结果 = null，调用方保留占位）。 */
  title: string | null;
  /** 请求是否被主动中止（发送新消息/切仓库/手动接管）——调用方按「取消」处理，不视为失败。 */
  aborted: boolean;
}

/** 用 LLM 为对话生成简短标题。 */
export async function autoTitle(
  params: AutoTitleParams,
  dialogue: string,
  opts?: { maxChars?: number; key?: string },
): Promise<AutoTitleResult> {
  // maxChars：自动命名按首轮摘要语义截断防浪费 token；重新命名传 Infinity = 全量会话记录
  const excerpt = dialogue.slice(0, opts?.maxChars ?? DIALOGUE_MAX_CHARS);
  const messages: ChatParams["messages"] = [
    {
      role: "system",
      text: `你负责为对话生成标题。只输出标题本身（不超过 ${TITLE_MAX_LEN} 字），不要引号、解释、标点或编号。`,
    },
    { role: "user", text: `为下面的对话起一个简洁的标题：\n\n${excerpt}` },
  ];
  const controller = new AbortController();
  const key = opts?.key;
  if (key) titleControllers.set(key, controller);
  // 超时与主动取消都以 AbortError 呈现，必须区分：超时是真实失败（调用方按 failed 提示可重试），
  // 主动取消（abortAutoTitle：发新消息/切仓库/手动接管）静默按 skipped 处理
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, AUTO_NAMING_TIMEOUT_MS);
  try {
    return {
      aborted: false,
      title: cleanTitle(
        await chatOnce({ ...params, messages, maxTokens: TITLE_MAX_TOKENS, signal: controller.signal })
      ),
    };
  } catch (e) {
    if ((e as Error).name === "AbortError" && !timedOut) {
      return { aborted: true, title: null };
    }
    return { aborted: false, title: null };
  } finally {
    clearTimeout(timer);
    if (key && titleControllers.get(key) === controller) titleControllers.delete(key);
  }
}
