/**
 * 笔记撤销栈：按文件全文快照 + 时间合并，会话内内存驻留、**不落盘**。
 */
export interface NoteUndoEntry {
  content: string;
  ts: number;
}

/** 与 utils/undoStack.ts（画布/表格快照式栈）语义一致：push 清空 redo、undo/redo 弹栈互放。 */
export interface NoteUndoStack {
  /** 登记一次用户输入：before = 本次输入前的全文。连续输入合并为一个撤销步（撤销回到整段输入之前，而非逐键）。 */
  recordEdit(before: string, now?: number): void;
  /** 撤销：current = 当前全文；撤销成功时 current 进 redo；无可撤销返回 null。 */
  undo(current: string): string | null;
  /** 重做：current = 当前全文；重做成功时 current 进 undo；无可重做返回 null。 */
  redo(current: string): string | null;
  readonly canUndo: boolean;
  readonly canRedo: boolean;
  clear(): void;
}

/**
 * 创建某一文件的撤销栈：每文件独立实例，按 file 键严格隔离。
 * 只在内存——退出软件与切仓库（防跨仓库同路径串文件）由 store 层清空；预览↔编辑切换、源码切换、
 * 切笔记、布局/面板重挂载等会话内操作都不清栈。内存有界：depth + 快照字节双剪枝（超预算丢最旧保最新）。
 */
export function createNoteUndoStack(opts?: {
  /** 撤销深度（默认 50，与画布/表格 undo 栈对齐）。 */
  depth?: number;
  /** 单文件快照内存上限（默认 1MB，按 UTF-16 code unit 长度近似估算内存；
   *  超出丢最旧保最新。纯内存启发式，非磁盘字节口径）。 */
  byteCap?: number;
  /** 连续输入合并窗口（默认 1000ms：间隔更短的输入并入同一撤销步）。 */
  coalesceMs?: number;
}): NoteUndoStack {
  const depth = opts?.depth ?? 50;
  const byteCap = opts?.byteCap ?? 1_000_000;
  const coalesceMs = opts?.coalesceMs ?? 1000;
  let undoStack: NoteUndoEntry[] = [];
  let redoStack: NoteUndoEntry[] = [];
  /** 最近一次输入登记时刻（null = 尚无输入组）：组判定用（间隔 < coalesceMs = 同一输入组，不再新增条目）。 */
  let lastRecordTs: number | null = null;

  /** 快照内存估算（UTF-16 code unit 长度，非 UTF-8 字节）：仅作内存启发式。 */
  const totalUnits = (s: NoteUndoEntry[]) =>
    s.reduce((acc, e) => acc + e.content.length, 0);
  /** 字节剪枝：超预算从最旧丢起，至少保留 1 条（单条超预算只能尽力而为）。 */
  const pruneBytes = (s: NoteUndoEntry[]): NoteUndoEntry[] => {
    let out = s;
    while (out.length > 1 && totalUnits(out) > byteCap) out = out.slice(1);
    return out;
  };
  /** 入栈 undo（深度 + 字节双剪枝，保最新）；undo/redo/recordEdit 三路径共用，防 redo 链逃逸预算。 */
  const pushUndo = (entry: NoteUndoEntry): void => {
    undoStack = pruneBytes([...undoStack, entry].slice(-depth));
  };
  /** 入栈 redo（同 pushUndo 双剪枝）。 */
  const pushRedo = (entry: NoteUndoEntry): void => {
    redoStack = pruneBytes([...redoStack, entry].slice(-depth));
  };

  return {
    recordEdit(before, now = Date.now()) {
      if (lastRecordTs !== null && now - lastRecordTs < coalesceMs) {
        // 仍在同一输入组（打字成组）：组起点已入栈，不新增条目（否则退化成逐键撤销）
        lastRecordTs = now;
        return;
      }
      // 新输入组：以「组起点前的全文」入栈并清空 redo（撤销后产生新变更，redo 失效）
      lastRecordTs = now;
      pushUndo({ content: before, ts: now });
      redoStack = [];
    },
    undo(current) {
      const prev = undoStack[undoStack.length - 1];
      if (!prev) return null;
      undoStack = undoStack.slice(0, -1);
      pushRedo({ content: current, ts: Date.now() });
      // 撤销后输入另起新组：重置组计时，防上一次输入组的窗口吞掉新输入的撤销步
      lastRecordTs = null;
      return prev.content;
    },
    redo(current) {
      const next = redoStack[redoStack.length - 1];
      if (!next) return null;
      redoStack = redoStack.slice(0, -1);
      pushUndo({ content: current, ts: Date.now() });
      lastRecordTs = null;
      return next.content;
    },
    get canUndo() {
      return undoStack.length > 0;
    },
    get canRedo() {
      return redoStack.length > 0;
    },
    clear() {
      undoStack = [];
      redoStack = [];
    },
  };
}
