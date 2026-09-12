/**
 * 笔记编辑器运行时态：内容缓存 + 挂起输入 + 保存/冲突状态 + 外部修改序号 + 版本历史。
 *
 * 职责：仓库文件层（vaultStore）只留文件树与文件 CRUD，笔记编辑面的运行时态与写盘链归本 store——
 * 写盘链 = **缓存先行**（先于异步写盘更新内容缓存，防重挂载读到陈旧缓存闪回/回退）+ **按文件串行写盘队列**
 * （同一笔记的并发保存严格按调用序落盘，后调用者最后写）。
 *
 * 分层：组件不直连 service；笔记编辑会话（noteSessionStore）、属性面板、面板 header 经本 store 读写。
 * 模块环：builtins（组件层）静态引本模块、本模块引 vaultStore，环上的跨模块访问只能在函数体内延迟求值
 * （顶层 getState/useXxx 会在环上 TDZ 崩溃）。
 */
import { create } from "zustand";
import {
  readNote,
  writeNote,
} from "@/services/vault";
import {
  loadHistory as loadNoteHistory,
  recordHistoryVersion,
  versionContentAt,
  type HistoryAuthor,
  type HistoryVersion,
} from "@/services/history";
import { markSelfSave } from "@/utils/selfSave";
import { emitPluginEvent } from "@/services/cordis/events";
import { registerDomainLifecycle } from "@/utils/kernelLifecycle";
import { useVaultStore } from "@/stores/vaultStore";

/**
 * 按文件串行写盘队列：同一笔记的并发保存严格按调用序落盘，后调用者最后写。
 * 解决「卸载 flush 写盘在途 + 重挂载/新编辑又写盘」的同文件乱序覆盖（跨布局回退根因之一）：
 * 无论两个 `saveNoteContent` 的调用先后如何交织，磁盘最终 = 最后一次调用的内容。
 * 前序失败不阻断本序（prev.then(fn, fn)）。
 */
const noteWriteQueues = new Map<string, Promise<void>>();
function withNoteWriteQueue(file: string, fn: () => Promise<void>): Promise<void> {
  const prev = noteWriteQueues.get(file) ?? Promise.resolve();
  const next = prev.then(fn, fn);
  noteWriteQueues.set(file, next);
  // 队列项完成且仍是最新条目时自清理（Map 只留进行中的条目，防长会话无限增长）；
  // 成功/失败两条路径都要接住，否则写盘失败会留下未处理的 rejection
  const cleanup = () => {
    if (noteWriteQueues.get(file) === next) noteWriteQueues.delete(file);
  };
  void next.then(cleanup, cleanup);
  return next;
}

/** 笔记内容缓存上限（FIFO 淘汰最旧；防大笔记常驻内存无限膨胀，切仓库清空）。 */
const MAX_NOTE_CACHE = 30;

/** 冲突解决请求序号（全局单调，见 resolveNoteConflict）。 */
let conflictResolveSeq = 0;

/** 写入单文件笔记缓存并淘汰最旧（重复写入 = 移除旧条目再追加，FIFO 顺序近似最近使用）。 */
function cacheNoteContent(
  cache: Record<string, string>,
  file: string,
  content: string,
): Record<string, string> {
  const next: Record<string, string> = {};
  for (const [k, v] of Object.entries(cache)) if (k !== file) next[k] = v;
  next[file] = content;
  if (Object.keys(next).length > MAX_NOTE_CACHE) {
    delete next[Object.keys(next)[0]];
  }
  return next;
}

/** 笔记编辑器保存状态（面板 header 展示用；仅挂载中的编辑器写入、卸载清除）。 */
export type NoteSaveStatus = {
  state: "idle" | "edited" | "saving" | "saved" | "error";
  loadError: boolean;
};

interface NoteState {
  /** 笔记内容缓存（file → 正文；仅本会话内读过的笔记，FIFO 上限）。布局切换/重开笔记直接命中，
   *  避免每次 NoteEditor 挂载都读盘（与 tableStore 的「已加载不重读」同语义）；切仓库清空。 */
  noteContents: Record<string, string>;
  /** 笔记编辑器未落盘的最近输入（file → 正文；handleChange 登记、保存完成/flush 后清除）。
   *  供 flushPendingNotes（关窗守卫/AI 重命名移动删除前）把组件内 debounce timer 之外的
   *  挂起输入统一落盘 + 补历史存档点——组件卸载与关窗都不丢最后 500ms。 */
  pendingNoteContent: Record<string, string>;
  /** 登记/清除笔记未落盘输入（编辑会话维护；保存完成且无新输入时清除）。 */
  setPendingNoteContent: (file: string, content: string | null) => void;
  /** 立即落盘全部挂起笔记输入（关窗守卫/AI 文件操作前 flush 用），并补历史存档点（60s 合并）。
   *  期间又有新编辑（会话重新登记）则保留给下一轮，不误清。 */
  flushPendingNotes: () => Promise<void>;
  /** 读笔记正文（编辑会话加载用；组件不直调 service，走本 store）。 */
  readNoteContent: (file: string) => Promise<string>;
  /** 直读笔记磁盘全文（绕过内容缓存；外部修改感知/写前校验用真实磁盘）。 */
  readNoteFresh: (file: string) => Promise<string>;
  /** 写回笔记正文并落盘（缓存先行 + 按文件串行队列）。 */
  /** 写正文并返回是否真的落盘（`canWrite` 见实现：排队期间可取消）。 */
  saveNoteContent: (
    file: string,
    content: string,
    canWrite?: () => boolean,
  ) => Promise<boolean>;
  /** 作废单文件笔记内容缓存（真实外部修改/删除/改名时调用；下次读取走盘）。 */
  invalidateNoteCache: (file: string) => void;
  /** 作废某目录下的全部内容缓存（文件夹改名/移动后该前缀路径不再指代同一批文件）。 */
  invalidateNoteCacheUnder: (dir: string) => void;
  /** 仅更新单文件笔记内容缓存（不经磁盘/基线；写盘前预写，读取方即时拿到新内容）。 */
  stageNoteContent: (file: string, content: string) => void;
  /** 记录一条笔记历史版本（版本边界；连续编辑自动节流合并，不逐键记录）。 */
  noteHistoryRecord: (
    file: string,
    content: string,
    action: "edit" | "restore",
    opts?: { note?: string; authorOverride?: HistoryAuthor; coAuthors?: HistoryAuthor[] },
  ) => Promise<void>;
  /** 读取笔记历史版本列表（缺失/损坏 → 空数组，尽力而为）。 */
  noteHistoryLoad: (file: string) => Promise<HistoryVersion[]>;
  /** 回滚笔记到指定版本：写回磁盘 + 记一条 restore 版本；返回回滚后的全文（供编辑器重载），失败返回 null。 */
  noteHistoryRollback: (file: string, seq: number) => Promise<string | null>;
  /** 外部修改的笔记（file → 递增序号）。NoteEditor 订阅感知外部变更：无本地改动时实时刷新，有改动时提示冲突。 */
  externalNoteEdits: Record<string, number>;
  /** watcher 收到 `.md` 外部变化事件时 bump 序号（软件内重命名旧路径事件由调用方跳过）。 */
  markNoteExternallyEdited: (file: string) => void;
  /** 笔记编辑器保存状态（file → 状态；面板 header 读取，编辑器卸载/切文件时清除）。 */
  noteSaveStates: Record<string, NoteSaveStatus>;
  /** 更新笔记编辑器保存状态（null = 清除）。 */
  setNoteSaveState: (file: string, status: NoteSaveStatus | null) => void;
  /** 笔记编辑器冲突状态（file → 是否冲突；面板 header 读取，编辑器卸载/切文件时清除）。 */
  noteConflicts: Record<string, boolean>;
  /** 更新笔记编辑器冲突状态（false = 清除）。 */
  setNoteConflict: (file: string, conflict: boolean) => void;
  /** 笔记冲突解决请求（file → 递增序号 + 解决方式；面板 header 按钮发请求，NoteEditor 订阅执行）。 */
  noteConflictResolveReq: Record<string, { seq: number; keepLocal: boolean }>;
  /** 请求解决笔记冲突（keepLocal = 保留本地并保存；false = 重新加载丢弃本地）。 */
  resolveNoteConflict: (file: string, keepLocal: boolean) => void;
  /** 清除笔记冲突解决请求（编辑器卸载时调用，防残留）。 */
  clearNoteConflictResolveReq: (file: string) => void;
  /** 切仓库清态（openVault 后、下一个 await 前由领域生命周期钩子同步调用）：清空内容缓存与挂起输入。 */
  reset: () => void;
}

export const useNoteStore = create<NoteState>((set, get) => ({
  noteContents: {},
  pendingNoteContent: {},

  readNoteContent: async (file) => {
    // 命中缓存（本会话已读过）：布局切换/重开笔记直接返回，不再读盘
    const cached = get().noteContents[file];
    if (cached !== undefined) return cached;
    const content = await readNote(file);
    set((s) => ({ noteContents: cacheNoteContent(s.noteContents, file, content) }));
    return content;
  },
  /** 直读笔记磁盘全文（绕过内容缓存）：外部修改感知/写前校验需要真实磁盘而非可能滞后的缓存。 */
  readNoteFresh: (file) => readNote(file),

  /**
   * 写笔记正文（缓存先行 + 按文件串行队列）。返回是否真的落盘。
   *
   * `canWrite`：可在排队期间被取消的落盘许可（笔记会话在写盘排队期间被外部修改打断转冲突时，
   * 这次尚未执行的写盘必须作废——否则它落地时会覆盖刚被识别出来的外部内容，而用户看到的是冲突条）。
   * 取消时同时作废刚写入的内容缓存：缓存若停在未落盘正文上，重开会话会把它当磁盘基线。
   */
  saveNoteContent: async (file, content, canWrite) => {
    if (canWrite && !canWrite()) return false;
    // 缓存先行（先于异步写盘）：重挂载/跨编辑面读取立即拿到最新内容，消灭「卸载 flush
    // 写盘在途 → 重挂载读陈旧缓存」的闪回/回退窗口（跨布局回退根因之一）。写盘失败时
    // 缓存与编辑器显示一致（均为最新内容），失败由调用方置 error 状态，下次保存重试。
    get().stageNoteContent(file, content);
    let written = false;
    await withNoteWriteQueue(file, async () => {
      if (canWrite && !canWrite()) {
        get().invalidateNoteCache(file);
        return;
      }
      await writeNote(file, content);
      // 标记路径级自写回波：watcher 收到同路径事件后跳过无关的全树重扫（内容编辑不改文件树）
      markSelfSave(file);
      written = true;
    });
    if (!written) return false;
    // 笔记内容落盘：通知订阅方（note:changed 轻量信号，按需再调 note 服务读内容）。
    emitPluginEvent("note:changed", { file });
    return true;
  },

  invalidateNoteCache: (file) =>
    set((s) => {
      if (!(file in s.noteContents)) return s; // 无缓存条目：返回原引用，不触发订阅
      const next = { ...s.noteContents };
      delete next[file];
      return { noteContents: next };
    }),

  invalidateNoteCacheUnder: (dir) =>
    set((s) => {
      const prefix = `${dir}/`;
      const next: Record<string, string> = {};
      let dropped = false;
      for (const [file, content] of Object.entries(s.noteContents)) {
        if (file.startsWith(prefix)) dropped = true;
        else next[file] = content;
      }
      return dropped ? { noteContents: next } : s; // 无命中：返回原引用，不触发订阅
    }),

  stageNoteContent: (file, content) =>
    set((s) => ({ noteContents: cacheNoteContent(s.noteContents, file, content) })),

  setPendingNoteContent: (file, content) =>
    set((s) => {
      const next = { ...s.pendingNoteContent };
      if (content === null) delete next[file];
      else next[file] = content;
      return { pendingNoteContent: next };
    }),

  flushPendingNotes: async () => {
    const pending = get().pendingNoteContent;
    // 失败/冲突未决/文件已删的条目不清除（保留给下一轮或用户决策），其余落盘后清除
    const keep = new Set<string>();
    for (const [file, content] of Object.entries(pending)) {
      // 冲突未决（外部已修改、用户未选择「重新加载/保留本地」）：不覆盖外部修改，保留待决策
      if (get().noteConflicts[file]) {
        keep.add(file);
        continue;
      }
      // 文件已从列表消失（已被删除，cleanup 同款 stillExists 守卫）：不重建已删除文件
      if (!useVaultStore.getState().noteList.some((n) => n.file === file)) {
        keep.add(file);
        continue;
      }
      try {
        // 走统一写盘链（缓存先行 + 按文件串行队列），落盘后补历史存档点（60s 合并；
        // 与 debounce 路径同内容时 recordHistoryVersion 按内容去重跳过，不产生重复版本）
        await get().saveNoteContent(file, content);
        await get().noteHistoryRecord(file, content, "edit");
      } catch (e) {
        // 写盘失败：保留条目待重试（防关窗/切仓库场景下未落盘输入永久丢失）
        keep.add(file);
        console.error("笔记挂起输入落盘失败", e);
      }
    }
    // 只清「落盘成功且未被新编辑替换」的条目（期间 handleChange 重新登记的保留给下一轮）
    set((s) => {
      const next = { ...s.pendingNoteContent };
      for (const [file, content] of Object.entries(pending)) {
        if (!keep.has(file) && next[file] === content) delete next[file];
      }
      return { pendingNoteContent: next };
    });
  },

  noteHistoryRecord: (file, content, action, opts) =>
    recordHistoryVersion("note", file, {
      content,
      action,
      ...(opts?.authorOverride ? { authorOverride: opts.authorOverride } : {}),
      ...(opts?.coAuthors && opts.coAuthors.length ? { coAuthors: opts.coAuthors } : {}),
      ...(opts?.note ? { note: opts.note } : {}),
      // 连续编辑节流：60s 内合并为一个存档点（版本粒度，不逐键），显式边界（外部/回滚）不受限
      coalesceEditMs: action === "edit" ? 60_000 : 0,
    }),

  noteHistoryLoad: (file) => loadNoteHistory("note", file),

  noteHistoryRollback: async (file, seq) => {
    const versions = await loadNoteHistory("note", file);
    const content = versionContentAt(versions, seq);
    if (content == null) return null;
    await get().saveNoteContent(file, content);
    // 回滚记一条 restore 版本（滚动恢复点 + 审计「何时回滚到哪」）
    await recordHistoryVersion("note", file, { content, action: "restore" });
    return content;
  },

  externalNoteEdits: {},

  markNoteExternallyEdited: (file) =>
    set((s) => ({
      externalNoteEdits: { ...s.externalNoteEdits, [file]: (s.externalNoteEdits[file] ?? 0) + 1 },
    })),

  noteSaveStates: {},

  setNoteSaveState: (file, status) =>
    set((s) => {
      if (status === null) {
        const next = { ...s.noteSaveStates };
        delete next[file];
        return { noteSaveStates: next };
      }
      return { noteSaveStates: { ...s.noteSaveStates, [file]: status } };
    }),

  noteConflicts: {},

  setNoteConflict: (file, conflict) =>
    set((s) => {
      const next = { ...s.noteConflicts };
      if (conflict) next[file] = true;
      else delete next[file];
      return { noteConflicts: next };
    }),

  noteConflictResolveReq: {},

  // 请求序号全局单调（不随条目清除归零）：会话按「已处理的最大序号」判断新请求，
  // 清除条目后从头计数会让会话误判为已处理过而吞掉按钮点击
  resolveNoteConflict: (file, keepLocal) =>
    set((s) => ({
      noteConflictResolveReq: {
        ...s.noteConflictResolveReq,
        [file]: { seq: ++conflictResolveSeq, keepLocal },
      },
    })),

  clearNoteConflictResolveReq: (file) =>
    set((s) => {
      const next = { ...s.noteConflictResolveReq };
      delete next[file];
      return { noteConflictResolveReq: next };
    }),

  reset: () => {
    // 挂起输入已在切仓库前 flush 落盘，这里只清残留（含 flush 后、切仓库前的新输入）：
    // 必须与调用方同步完成，防旧仓库内容经已切换的仓库根写进新仓库同路径文件。
    // 冲突/保存状态同属按文件的旧仓库运行时态，一并清（残留会让新仓库同路径误显冲突条、
    // 并让 flushPendingNotes 永久跳过该文件）
    set({ noteContents: {}, pendingNoteContent: {}, noteConflicts: {}, noteSaveStates: {} });
  },
}));

/**
 * 切仓库清空笔记运行时态：在模块加载时注册，不挂笔记插件的生命周期钩子——
 * 笔记插件停用期间内核照常切仓库，残留的内容缓存/挂起输入会把旧仓库正文串给新仓库同路径笔记。
 * 这是本 store 自身的数据边界（非领域事件反应），故不随插件启停撤销。
 */
registerDomainLifecycle({
  id: "noteStore",
  onVaultLeaving: () => useNoteStore.getState().reset(),
});
