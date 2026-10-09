/**
 * 有界退避重试定时器（会话容器提交、协作重连、会话写盘共用骨架）：
 * 按退避序列逐次取延迟调度；序列走完缺省按末位封顶持续重试，给 onGiveUp 则放弃。
 */

export interface BackoffRetry {
  /** 安排下一次重试（重入安全：未触发的旧 timer 先清除）。 */
  schedule(): void;
  /** 归零退避进度（重试成功后调用）；在途 timer 保留——其触发的是更早失败批次的补交，不得吞掉。 */
  resetProgress(): void;
  /** 归零进度并清除在途 timer（切换目标/取消链路时调用：在途重试对新目标已无意义）。 */
  reset(): void;
  /** 仅清除在途 timer（进度保留）。 */
  cancel(): void;
}

export function createBackoffRetry(opts: {
  /** 退避延迟序列（毫秒，严格递增；末位即封顶延迟）。 */
  delaysMs: number[];
  /** 延迟到点触发（重试动作）。 */
  onRetry: () => void;
  /** 序列耗尽后再次 schedule 的处置：缺省按末位延迟持续重试；给出时放弃（不再触发）。 */
  onGiveUp?: () => void;
  /** 定时器实现（缺省全局 setTimeout/clearTimeout；浏览器上下文的帧泵传 window 版本，测试可桩）。 */
  timers?: {
    set: (fn: () => void, ms: number) => ReturnType<typeof setTimeout>;
    clear: (t: ReturnType<typeof setTimeout>) => void;
  };
}): BackoffRetry {
  let attempt = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const set = opts.timers?.set ?? setTimeout;
  const clear = opts.timers?.clear ?? clearTimeout;
  return {
    schedule() {
      if (timer) clear(timer);
      if (attempt >= opts.delaysMs.length && opts.onGiveUp) {
        opts.onGiveUp();
        return;
      }
      const delay = opts.delaysMs[Math.min(attempt, opts.delaysMs.length - 1)];
      attempt += 1;
      timer = set(() => {
        timer = null;
        opts.onRetry();
      }, delay);
    },
    resetProgress() {
      attempt = 0;
    },
    reset() {
      attempt = 0;
      if (timer) {
        clear(timer);
        timer = null;
      }
    },
    cancel() {
      if (timer) {
        clear(timer);
        timer = null;
      }
    },
  };
}
