/**
 * 防抖持久化控制器：统一各 store 的「变更 → debounce 写盘」样板（timer 管理 + 代数防吞 + 写盘串行）。
 * 写盘期间又有新变更（schedule 再次被调）时，persist 回调对比「开始写盘时的 version」——变了
 * = 保留脏标记由下一轮 timer 再写，防写盘成功回调吞掉新编辑。
 * 写盘串行：同一 store 的两次写不得同时在途——并发时后到者会基于前一次写盘之前的旧内容重算
 * 增量、丢掉已落盘内容；排队后每次写读到的都是前一次落盘之后的最新状态与基线。
 * 各 store 的 dirty 判定 / 写盘基线 / 仓库归属校验写在 persist 回调里。
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

/**
 * 把落盘基线数组按补丁 upsert 的 id 推进：同 id 覆盖为「应用补丁后的内存引用」，新 id 追加置尾
 * （与补丁应用同序）。基线实体与内存同引用，按引用 diff 才能判定「已落盘、无需重发」。
 * 协作补丁回放防重发专用：补丁到达即服务端已落地（服务端落地后才广播），基线只吃补丁内实体，
 * 本端未保存改动不在补丁内、保留旧引用，下一次保存仍会按引用 diff 发出。
 */
export function advanceBaselineRefs<T extends { id: string }>(
  list: T[],
  upsertIds: string[],
  refById: Map<string, T>,
): void {
  const indexById = new Map(list.map((item, i) => [item.id, i] as const));
  for (const id of upsertIds) {
    const ref = refById.get(id);
    // 防御：补丁已应用则内存必含该 id；缺引用时跳过，该实体下一轮重发一次幂等收敛
    if (!ref) continue;
    const i = indexById.get(id);
    if (i === undefined) {
      indexById.set(id, list.length);
      list.push(ref);
    } else {
      list[i] = ref;
    }
  }
}

/**
 * 单文件持久化一轮的统一收尾（canvas/table 共用骨架）：
 * 归属守卫（旧目标的写盘结果不得覆盖新目标的脏标记/路径）→ 漂移路径同步（先于并发判断，
 * 改名落点是既成事实）→ 记历史（只绑定「本轮真实落盘」这一事实）→ 按本轮是否被新变更
 * 接续分流：有新变更保留 dirty 由下一轮 timer 再写，无新变更清脏并推进落盘基线。
 */
export function finishPersistRound(args: {
  /** 本轮 persist 开始时捕获的 persistCtl.version。 */
  versionAtStart: number;
  /** persistCtl.version 读取器（收尾时对比，变了 = 写盘期间有新编辑）。 */
  versionNow: () => number;
  /** 归属守卫：写盘目标仍是当前激活目标（false = 已切换画布/表格，本轮结果丢弃）。 */
  ownerUnchanged: () => boolean;
  /** 本轮写盘的起始文件路径。 */
  file: string;
  /** 服务端回传的漂移落点（title 改名）；缺省或与起始相同 = 无漂移。 */
  newFile?: string;
  /** 本轮是否真实落盘（false = 空补丁，磁盘未动，不记历史）。 */
  written: boolean;
  /** 漂移路径同步副作用（本 store 状态与同源引用）。 */
  onRelocated: (newFile: string, oldFile: string) => void;
  /** 记历史（入参 = 落盘后的最终路径）。 */
  onWritten: (file: string) => void;
  /** 有新变更接续：只收 saving（dirty 保留，基线不推进）。 */
  onSuperseded: () => void;
  /** 无新变更：清 dirty/error 并推进落盘基线。 */
  onClean: () => void;
}): void {
  if (!args.ownerUnchanged()) return;
  if (args.newFile && args.newFile !== args.file) {
    args.onRelocated(args.newFile, args.file);
  }
  if (args.written) args.onWritten(args.newFile ?? args.file);
  if (args.versionNow() !== args.versionAtStart) {
    args.onSuperseded();
    return;
  }
  args.onClean();
}

/**
 * 补丁持久化失败的统一分流：文件已被外部删除（补丁只含变化实体，重建会丢未变化部分）→
 * 走全量回退；其余错误交给上报。
 */
export async function routePersistError(
  e: unknown,
  opts: {
    /** 服务端「文件已从磁盘删除」错误的判定文本。 */
    deletedMarker: string;
    /** 全量回退（调用方自行 finish）。 */
    rewriteFull: () => Promise<void>;
    /** 其余错误上报。 */
    reportError: (e: unknown) => void;
  },
): Promise<void> {
  if (typeof e === "string" && e.includes(opts.deletedMarker)) {
    await opts.rewriteFull();
    return;
  }
  opts.reportError(e);
}
