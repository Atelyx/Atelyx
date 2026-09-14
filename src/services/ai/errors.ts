/**
 * AI 请求错误：LlmError 错误载体 + 错误文本判定纯函数。
 *
 * - `LlmError`：AI 请求错误载体，附 `status` 与 `retryAfterMs`（重试策略优先尊重服务端 retry-after）。
 * - 错误判定只有两个出口：传输级可重试（`isTransportRetryable` → `isRetryableError`，供重试策略）
 *   与上下文溢出（`isContextOverflowError` → `withOverflowHint` 追加友好提示；调用方据此先折叠超长
 *   历史工具结果重试一次）；
 *   其余错误（配额耗尽/鉴权失败/参数错误/其余未知）不区分类别，统一不重试、原始文案直出。
 * - 配额/鉴权等终态特征在 `isTransportRetryable` 内优先否决，防止 HTTP 429/5xx 状态兜底误重试。
 *
 * 判定规则只在这一个文件维护，client 与各调用方共用。
 */

/** 上下文溢出错误的友好提示（追加到原始错误消息后展示）。 */
const OVERFLOW_HINT = "上下文过长：建议精简对话、去掉多余引用，或新建分支继续";

/** 结构化 AI 错误载体（附 HTTP 状态与服务端要求的重试等待，供重试策略消费）。 */
export class LlmError extends Error {
  readonly status?: number;
  readonly retryAfterMs?: number;

  constructor(
    message: string,
    opts?: { status?: number; retryAfterMs?: number; cause?: unknown },
  ) {
    super(message, opts?.cause !== undefined ? { cause: opts.cause } : undefined);
    this.status = opts?.status;
    this.retryAfterMs = opts?.retryAfterMs;
    this.name = "LlmError";
  }
}

/** 可重试的传输级失败特征（网络抖动 / 限流 / 服务端临时故障）。 */
const RETRYABLE_PATTERNS = [
  /overloaded/i,
  /rate\s*limit/i,
  /too\s*many\s*requests/i,
  /temporarily\s*unavailable/i,
  /service\s*unavailable/i,
  /connection\s*refused/i,
  /fetch\s*failed/i,
  /enotfound|eai_again|econnreset|econnrefused|socket\s*hang\s*up/i,
  /stream\s*ended\s*before/i,
  /http2\s*request\s*did\s*not\s*get\s*a\s*response/i,
];

/** 快速失败（不重试）的鉴权 / 配额特征。 */
const NON_RETRYABLE_PATTERNS = [
  /quota|insufficient_quota|out\s*of\s*budget|billing/i,
  /invalid\s*(api\s*)?key|unauthorized|unauthorised|forbidden/i,
];

/** 账号配额/余额耗尽特征（终态，区别于瞬时限流）。 */
const QUOTA_PATTERNS = [
  /insufficient[\s_-]+(?:quota|balance|credits?)/i,
  /quota[\s_-]+(?:exceeded|exhausted|reached)|usage[\s_-]+limit[\s_-]+(?:exceeded|reached)/i,
  /(?:balance|credits?)[\s_-]+(?:exhausted|depleted)/i,
  /out\s*of\s*(?:credits?|budget)/i,
];

/** 上下文溢出特征（请求超过模型 context window）。 */
const CONTEXT_OVERFLOW_PATTERNS = [
  /prompt\s+is\s+too\s+long/i,
  /context\s+length\s+exceeded/i,
  /exceed(s|ed)?\s+the\s+(model\s*'?s\s*)?context\s+window/i,
  /maximum\s+context\s+length/i,
  /context\s+too\s+large/i,
  /reduce\s+the\s+length\s+of\s+the\s+messages/i,
];

/** 提取错误文本中的 HTTP 状态码（无则 null）。 */
function matchStatus(msg: string): number | null {
  const m = msg.match(/HTTP\s+(\d{3})/);
  return m ? Number(m[1]) : null;
}

/**
 * 根据错误文本判定是否传输级可重试（网络抖动 / 限流 / 服务端临时故障，HTTP 429/5xx 状态兜底）。
 * 顺序敏感：溢出与终态特征（配额/鉴权）优先否决——即便 HTTP 状态是 429/5xx 也不重试
 * （配额耗尽是账号终态，重试无法恢复）。
 */
function isTransportRetryable(message: string): boolean {
  if (CONTEXT_OVERFLOW_PATTERNS.some((re) => re.test(message))) return false;
  if (QUOTA_PATTERNS.some((re) => re.test(message))) return false;
  if (NON_RETRYABLE_PATTERNS.some((re) => re.test(message))) return false;
  if (RETRYABLE_PATTERNS.some((re) => re.test(message))) return true;
  const status = matchStatus(message);
  return status !== null && (status === 429 || status >= 500);
}

/** 该错误是否可重试（传输级 / 5xx / 429；quota/auth/其余快速失败）。 */
export function isRetryableError(err: Error): boolean {
  return isTransportRetryable(err.message);
}

/** 该错误是否表示上下文溢出（超长请求不再裸报错，降级为友好提示；调用方据此先折叠超长工具结果重试）。 */
export function isContextOverflowError(err: Error): boolean {
  return CONTEXT_OVERFLOW_PATTERNS.some((re) => re.test(err.message));
}

/**
 * 上下文溢出错误追加友好提示（防用户看到裸 API 报错不知所措）。
 * 仅命中溢出判定才包装一次，其余原样返回。
 */
export function withOverflowHint(err: Error): Error {
  if (isContextOverflowError(err)) {
    return new Error(`${err.message}（${OVERFLOW_HINT}）`);
  }
  return err;
}

/**
 * 从 HTTP 状态 / 错误文本 / retry-after 组装 LlmError。
 * 透传 `status` 与 `retryAfterMs`（供 retry 策略优先尊重服务端 retry-after）。
 */
export function toLlmError(
  message: string,
  opts?: { status?: number; retryAfterMs?: number; cause?: unknown },
): LlmError {
  return new LlmError(message, {
    status: opts?.status,
    retryAfterMs: opts?.retryAfterMs,
    cause: opts?.cause,
  });
}
