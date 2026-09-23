/**
 * 防抖持久化控制器：统一各 store 的「变更 → debounce 写盘」样板（timer 管理 + 代数防吞 + 写盘串行）。
 * 语义对齐画布既有实现：写盘期间若又有新变更（schedule 再次被调），persist 回调对比
 * 「开始写盘时的 version」判断本轮是否生效——变了 = 保留脏标记，由下一轮 timer 再写，
 * 防写盘成功回调吞掉新编辑。
 *
 * 写盘串行：同一 store 的两次写不得同时在途——两次写各自在开始时读取「当前状态与上次落盘基线」，
 * 并发时后到者会基于前一次写盘之前的旧内容重算增量，丢掉前一次已落盘的内容。排队后每次写读到的
 * 都是前一次落盘之后的最新状态与基线。
 *
 * 各 store 保留自有语义（dirty 判定 / 写盘基线 / watcher 回环抑制 / 仓库归属校验），写在 persist 回调里。
 */
export interface PersistController {
  /** 变更后调度：代数 +1 并重置 debounce timer（timer 到点调 persist()）。 */
  schedule(): void;
  /** 清除未到期的调度（load/clear/外部刷新前调用，防旧 timer 重写新状态）。 */
  cancel(): void;
  /** 立即持久化（timer 到点与外部 flush 共用）：清 timer 后调 persist()，排在在途写盘之后。 */
  flush(): Promise<void>;
  /** 变更代数：schedule 每次 +1；persist 回调写盘前捕获、完成后对比。 */
  readonly version: number;
}

export function createPersistController(opts: {
  /** 实际写盘（各 store 实现）。 */
  persist: () => Promise<void>;
  /** debounce 间隔毫秒（缺省 500）。 */
  delay?: number;
  /** schedule 时同步执行（置 saving/dirty 等瞬时状态）。 */
  beforeSchedule?: () => void;
}): PersistController {
  const delay = opts.delay ?? 500;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let version = 0;
  /** 在途写盘链尾（成功或失败都推进，单次失败不阻断后续保存）。 */
  let chain: Promise<void> = Promise.resolve();
  const run = (): Promise<void> => {
    const next = chain.then(() => opts.persist());
    chain = next.catch(() => {});
    return next;
  };
  return {
    schedule() {
      version++;
      opts.beforeSchedule?.();
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        void run();
      }, delay);
    },
    cancel() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    },
    flush() {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      return run();
    },
    get version() {
      return version;
    },
  };
}
