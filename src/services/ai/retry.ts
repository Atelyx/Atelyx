/**
 * LLM 请求重试策略（独立于核心，可独立复用）：传输级失败（网络抖动 / 5xx / 429）按指数退避重试，尊重服务端 `retry-after`。
 * 可重试判定见 errors.ts 的 `isRetryableError`；流已开始后的中断与 abort 后均不重试（防重复输出）。
 */

/** 服务端要求的重试延迟超过此值即放弃重试（服务器都觉得自己要挂 60s+ 不值得等）。 */
const MAX_RETRY_DELAY_MS = 60_000;

/**
 * 放弃重试的哨兵值：`computeRetryDelay` 在服务端要求等待超限时返回；调用方遇之直接判负不再重试，
 * 避免把「服务器要求等 120s」错当短退避连打（此前 null 放弃信号会被 `??` 吞掉）。
 */
export const GIVE_UP_RETRY_MS = -1;

/** 重试上限初始退避基准（指数步长）。 */
const BACKOFF_BASE_MS = 500;

/** 重建一个可中断睡眠：abort 时立即 resolve(false)。 */
export function sleep(ms: number, signal?: AbortSignal): Promise<boolean> {
  return new Promise((resolve) => {
    if (signal?.aborted) {
      resolve(false);
      return;
    }
    let settled = false;
    const settle = (value: boolean) => {
      if (settled) return;
      settled = true;
      resolve(value);
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      settle(true);
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      settle(false);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

/**
 * 重试延迟：优先服务端 retry-after 头（秒或 HTTP 日期），否则指数退避 0.5*2^n 秒 + 25% 抖动。
 * 延迟超出 MAX_RETRY_DELAY_MS 返回 GIVE_UP_RETRY_MS（调用方放弃本次重试）。
 */
export function computeRetryDelay(attempt: number, res?: Response): number {
  if (res) {
    const raw = res.headers.get("retry-after");
    if (raw) {
      const secs = /^\d+$/.test(raw) ? Number(raw) : NaN;
      const ms = Number.isFinite(secs)
        ? secs * 1000
        : Number.isFinite(Date.parse(raw))
          ? Math.max(0, Date.parse(raw) - Date.now())
          : NaN;
      if (Number.isFinite(ms)) return ms > MAX_RETRY_DELAY_MS ? GIVE_UP_RETRY_MS : ms;
    }
  }
  const base = Math.min(BACKOFF_BASE_MS * 2 ** attempt, 8000);
  return base + Math.random() * base * 0.25;
}

/**
 * 是否应发起下一次重试。
 * @param retryable 是否传输级失败。
 * @param attempt 已完成尝试数（0 起）。
 * @param maxRetries 最大重试次数。
 * @param receivedAnyEvent 流是否已产生过有效事件（true = 不重试，防重复输出）。
 */
export function shouldRetry(
  retryable: boolean,
  attempt: number,
  maxRetries: number,
  receivedAnyEvent: boolean,
): boolean {
  if (!retryable || receivedAnyEvent) return false;
  return attempt < maxRetries;
}
