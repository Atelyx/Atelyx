/**
 * 笔记正文编辑会话（per-file）：`.md` 的全文、协作绑定、未落盘输入与冲突状态只有一份，
 * 笔记面板与画布文本节点都是它的编辑面（同一篇笔记的多个入口共用同一会话）。
 *
 * 落盘走 noteStore 的写盘链（缓存先行 + 按文件串行队列）与历史存档点；协作态正文模型在
 * noteCollabStore（per-file Y.Doc），本模块只做编排。引用计数归零即 flush 并解绑。
 */

import { create } from "zustand";
import type { HistoryAuthor } from "@/services/history";
import type { NoteBodySession, NoteBodySessionView, NoteSurfaceProvider } from "@/types/noteSurface";
import { parseFrontmatter } from "@/utils/frontmatter";
import { noteTitleFromFile } from "@/utils/filename";
import { notifyNoteSurfaceChange } from "@/utils/noteSurfaceHost";
import { isKnownNoteDiskContent, useNoteStore, type NoteSaveStatus } from "@/stores/noteStore";
import { useNoteUndoStore } from "@/stores/noteUndoStore";
import { useNoteCollabStore } from "@/stores/noteCollabStore";
import { republishPresence, useCollabStore } from "@/stores/collabStore";
import { useNotificationStore } from "@/stores/notificationStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { lastFolderRenameTarget, lastNoteRenameTarget, useVaultStore } from "@/stores/vaultStore";

/** 输入落盘防抖：连续输入合并为一次写盘。 */
const SAVE_DEBOUNCE_MS = 500;

/** 非用户编辑的内容更新序号：全局单调，跨会话切换时必然变化（编辑面据此判断需要重建）。 */
let syncSeqCounter = 0;

/** 已提示过的冲突文件（同一冲突只提示一次，解决后移除）。 */
const notifiedConflicts = new Set<string>();

/** 未决冲突的笔记（跨会话关闭保留：本地输入还挂在 pendingNoteContent 上，等用户在笔记面板决策）。 */
const conflictedFiles = new Set<string>();

/** 每文件会话运行时（非响应式：定时器、序号、作者归因）。 */
interface SessionRuntime {
  file: string;
  refcount: number;
  /** 冲突未决：暂停自动保存，防覆盖外部修改。 */
  conflict: boolean;
  /** 最后成功写盘的磁盘内容基准：外部修改感知据此区分自写回放与真实外部变化。 */
  lastSaved: string;
  /** 撤销回放标记：回放内容不再记入撤销栈。 */
  applyingUndo: boolean;
  /** 已处理的外部修改序号 / 冲突解决请求序号（只响应挂载后新产生的增量）。 */
  processedExternalSeq: number;
  processedResolveSeq: number;
  timer: ReturnType<typeof setTimeout> | null;
  /** 保存序号：写入完成时若已有更新输入，保持「保存中…」而非误报「已保存」。 */
  saveSeq: number;
  /** 在途写盘的正文（本端已发起、尚未确认落盘；null = 无）。内容已落盘的判定必须计入它：
   *  在途写盘落地后磁盘即为该正文，只看已确认基线会把「撤销回原样」误判为无改动而跳过写盘。 */
  pendingWrite: string | null;
  /** 协作绑定在途（已发起直读盘取基线，等待结果）：防同会话重复发起。 */
  bindingPending: boolean;
  /** 最近一次内容变化的来源作者（主作者）：协作远端合入 → 该协作者；本地编辑 → null。 */
  lastChangeAuthor: HistoryAuthor | null;
  /** 自上次落盘以来参与内容变化的协作者集合（历史 coAuthors）；落盘成功后清空。 */
  changeAuthors: Map<string, HistoryAuthor>;
  session: NoteBodySession;
}

const runtimeMap = new Map<string, SessionRuntime>();

interface NoteSessionState {
  sessions: Record<string, NoteBodySessionView>;
}

export const useNoteSessionStore = create<NoteSessionState>(() => ({ sessions: {} }));

function view(file: string): NoteBodySessionView | undefined {
  return useNoteSessionStore.getState().sessions[file];
}

function patchView(file: string, patch: Partial<NoteBodySessionView>): void {
  useNoteSessionStore.setState((s) => {
    const current = s.sessions[file];
    if (!current) return s;
    return { sessions: { ...s.sessions, [file]: { ...current, ...patch } } };
  });
}

function setSaveState(file: string, state: NoteSaveStatus["state"], loadError = false): void {
  useNoteStore.getState().setNoteSaveState(file, { state, loadError });
  // 编辑面（含独立成面板的属性面板）订阅会话状态，落盘失败须在这里可见
  patchView(file, { error: state === "error" });
}

function setConflict(rt: SessionRuntime, conflict: boolean): void {
  rt.conflict = conflict;
  useNoteStore.getState().setNoteConflict(rt.file, conflict);
  patchView(rt.file, { conflict });
  if (conflict) conflictedFiles.add(rt.file);
  else conflictedFiles.delete(rt.file);
  notifyNoteSurfaceChange();
  if (!conflict) {
    notifiedConflicts.delete(rt.file);
    return;
  }
  // 冲突不能在画布节点里解决（冲突条与「保留本地/重新加载」在笔记面板）——提示一次，避免静默停摆
  if (notifiedConflicts.has(rt.file)) return;
  notifiedConflicts.add(rt.file);
  useNotificationStore.getState().notify({
    level: "warning",
    message: `笔记「${noteTitleFromFile(rt.file)}」已被外部修改，自动保存暂停；请在笔记面板选择保留本地或重新加载`,
  });
}

function isCollabActive(): boolean {
  const settings = useSettingsStore.getState();
  return settings.collabEnabled && useCollabStore.getState().connected;
}

/** 协作态绑定（协作未激活或未绑定时 null）。 */
function currentBinding(file: string): NoteBodySessionView["binding"] {
  if (!isCollabActive()) return null;
  return useNoteCollabStore.getState().bindings[file] ?? null;
}

function localAuthor(): HistoryAuthor {
  const { collabNickname, deviceName } = useSettingsStore.getState();
  return {
    id: deviceName || collabNickname || "用户",
    name: collabNickname || deviceName || "用户",
    device: deviceName || "",
  };
}

/** 正文（LF 规范化，不含 frontmatter）。 */
function bodyLF(content: string): string {
  return parseFrontmatter(content).body.replace(/\r\n/g, "\n");
}

/**
 * 协作态绑定：以**磁盘最新正文**为基线（peer 把打开文本当磁盘权威并入外部改动，并登记为共同祖先），
 * 内容缓存可能滞后于磁盘（自写回波抑制窗口内的远端写入），故直读盘取基线；读失败退回会话正文（仍优于不绑定）。
 * 本端有未落盘输入时不特殊处理：绑定后由 `handleCollabDivergence` 把会话正文按差量写回 ytext（本地最新者胜），
 * 共同祖先仍是磁盘正文——未落盘内容不会被登记成已落盘。
 * 绑定完成即把会话对齐到 CRDT 真值。
 */
function ensureCollabBinding(rt: SessionRuntime): void {
  if (!isCollabActive() || rt.conflict || rt.bindingPending) return;
  const file = rt.file;
  if (useNoteCollabStore.getState().bindings[file]) return;
  const content = view(file)?.content ?? "";
  // 空正文不参与协作（无可合并内容）：用户输入后经 commit 再次进入本函数完成绑定
  if (!content) return;

  const bindWith = (baselineContent: string, diskContent: string | undefined): void => {
    const { collabNickname, collabColor, deviceName } = useSettingsStore.getState();
    const binding = useNoteCollabStore.getState().bind(file, bodyLF(baselineContent), {
      name: collabNickname || deviceName || "用户",
      color: collabColor || "#30bced",
    });
    // 直读盘正文交给会话做采纳判定：与磁盘逐字节一致（无对端合入）时只对齐视图，不重复写盘
    rt.session.handleCollabDivergence(binding.ytext.toString(), diskContent);
  };

  rt.bindingPending = true;
  void useNoteStore
    .getState()
    .readNoteFresh(file)
    .catch(() => null)
    .then((disk) => {
      rt.bindingPending = false;
      // 读盘窗口内会话可能已关闭/转冲突/协作失活/已由他处绑定：这些分支都是「无需绑定」的合法条件，
      // 不是失败——后续输入（commit）或协作连接变化会再次触发本函数完成绑定
      if (runtimeMap.get(file) !== rt || rt.conflict) return;
      if (!isCollabActive() || useNoteCollabStore.getState().bindings[file]) return;
      const latest = view(file)?.content ?? "";
      if (!latest) return;
      // 读盘成功则磁盘正文即事实（空文件也是事实，按形态与逐字节比对用）；读失败退回会话正文，且不提供磁盘事实
      if (disk === null) bindWith(latest, undefined);
      else bindWith(disk, disk);
    });
}

/**
 * 程序化替换全文（加载完成/外部刷新/冲突重载/历史回滚）：
 * 协作态写回 ytext（对端即时可见，编辑面随 yCollab 更新）；非协作态 bump syncSeq 让编辑面重建。
 */
function replaceContent(rt: SessionRuntime, content: string): void {
  const binding = currentBinding(rt.file);
  const current = view(rt.file);
  if (!current) return;
  if (current.content !== content) {
    patchView(rt.file, binding ? { content } : { content, syncSeq: ++syncSeqCounter });
  } else if (!binding) {
    patchView(rt.file, { syncSeq: ++syncSeqCounter });
  }
  if (binding) useNoteCollabStore.getState().syncLocalBody(rt.file, bodyLF(content));
}

/** 用户侧内容提交（编辑面 → 会话）：登记撤销栈与挂起输入，置脏并按防抖落盘。 */
function commit(rt: SessionRuntime, content: string, syncDoc: boolean): void {
  const file = rt.file;
  const current = view(file);
  if (!current || current.content === content) return;
  const collab = isCollabActive();
  const remoteAuthor =
    collab && useNoteCollabStore.getState().isRemoteApplying()
      ? useNoteCollabStore.getState().lastRemoteAuthor(file)
      : null;
  rt.lastChangeAuthor = remoteAuthor;
  if (collab) {
    const local = localAuthor();
    rt.changeAuthors.set(remoteAuthor?.id ?? local.id, remoteAuthor ?? local);
  }
  if (!rt.applyingUndo) {
    // 撤销栈记「本次输入前全文」（连续输入合并为一步）；挂起输入供关窗/切仓库 flush
    useNoteUndoStore.getState().recordEdit(file, current.content);
    useNoteStore.getState().setPendingNoteContent(file, content);
  }
  patchView(file, { content, dirty: true });
  ensureCollabBinding(rt);
  // 源码模式/属性区等整篇入口：正文须写回 ytext（实时预览编辑已由 yCollab 同步）
  if (syncDoc && currentBinding(file)) {
    useNoteCollabStore.getState().syncLocalBody(file, bodyLF(content));
  }
  if (rt.conflict) return;
  setSaveState(file, "edited");
  if (rt.timer) clearTimeout(rt.timer);
  const seq = ++rt.saveSeq;
  rt.timer = setTimeout(() => {
    rt.timer = null;
    void save(rt, seq);
  }, SAVE_DEBOUNCE_MS);
}

/** 落盘收尾（仅当本次保存仍是最后一次、且会话仍是同一实例：期间又有输入则保持「保存中…」）。 */
function finishSave(rt: SessionRuntime, seq: number): void {
  // 会话身份守卫：落盘在途期间会话可能已关闭并重开（新实例有自己的挂起输入与状态），
  // 旧实例的收尾不得清新会话的脏标记与挂起登记
  if (runtimeMap.get(rt.file) !== rt || seq !== rt.saveSeq) return;
  useNoteStore.getState().setPendingNoteContent(rt.file, null);
  rt.changeAuthors.clear();
  patchView(rt.file, { dirty: false });
  setSaveState(rt.file, "saved");
}

/** 内容已落盘的判定目标：在途写盘优先（其完成后磁盘即为它），否则已确认落盘基线。 */
function diskContentOf(rt: SessionRuntime): string {
  return rt.pendingWrite ?? rt.lastSaved;
}

/** 写盘并登记在途内容（`diskContentOf` 判定用）；结束后仍等于自身才注销——已被更新输入接管则保留。 */
async function writeContent(rt: SessionRuntime, content: string): Promise<void> {
  rt.pendingWrite = content;
  try {
    await useNoteStore.getState().saveNoteContent(rt.file, content);
  } finally {
    if (rt.pendingWrite === content) rt.pendingWrite = null;
  }
}

/** 写入当前全文：协作态直接收敛落盘；非协作态先校验磁盘基准（外部已改动则转冲突）。 */
async function save(rt: SessionRuntime, seq: number): Promise<void> {
  const file = rt.file;
  const current = view(file);
  if (!current) return;
  const content = current.content;
  const local = localAuthor();
  const mainAuthor = rt.lastChangeAuthor;
  const coAuthors = [...rt.changeAuthors.values()].filter(
    (a) => a.id !== (mainAuthor?.id ?? local.id),
  );
  const recordHistory = () => {
    if (!view(file)) return;
    void useNoteStore.getState().noteHistoryRecord(file, content, "edit", {
      ...(mainAuthor ? { authorOverride: mainAuthor } : {}),
      ...(coAuthors.length ? { coAuthors } : {}),
    });
  };
  try {
    if (isCollabActive()) {
      if (content === diskContentOf(rt)) {
        finishSave(rt, seq);
        return;
      }
      setSaveState(file, "saving");
      await writeContent(rt, content);
      if (runtimeMap.get(file) !== rt) return;
      rt.lastSaved = content;
      useNoteCollabStore.getState().notifyNoteDiskWrite(file, bodyLF(content));
      finishSave(rt, seq);
      recordHistory();
      return;
    }
    const disk = await useNoteStore.getState().readNoteFresh(file);
    if (runtimeMap.get(file) !== rt || seq !== rt.saveSeq) return;
    if (disk !== rt.lastSaved) {
      setConflict(rt, true);
      return;
    }
    if (content === diskContentOf(rt)) {
      finishSave(rt, seq);
      return;
    }
    setSaveState(file, "saving");
    await writeContent(rt, content);
    if (runtimeMap.get(file) !== rt) return;
    rt.lastSaved = content;
    finishSave(rt, seq);
    recordHistory();
  } catch (e) {
    // 会话身份守卫：失败状态只写自己的会话（同路径可能已被新会话接管）
    if (runtimeMap.get(file) !== rt) return;
    console.error("笔记保存失败", e);
    // 内容已被后续输入取代（更新的一次保存会给出自己的结论）：不覆盖当前状态
    if (view(file)?.content !== content) return;
    // 当前正文确实还在磁盘之外：如实置错，并补回未落盘登记（关窗 flush / 下次编辑重试）
    patchView(file, { dirty: true });
    useNoteStore.getState().setPendingNoteContent(file, content);
    setSaveState(file, "error");
  }
}

/** 从磁盘载入全文（会话打开时一次）。 */
async function load(rt: SessionRuntime): Promise<void> {
  const file = rt.file;
  const seqAtLoad = useNoteStore.getState().externalNoteEdits[file] ?? 0;
  rt.processedExternalSeq = seqAtLoad;
  rt.processedResolveSeq = useNoteStore.getState().noteConflictResolveReq[file]?.seq ?? 0;
  try {
    const content = await useNoteStore.getState().readNoteContent(file);
    if (runtimeMap.get(file) !== rt) return;
    // 加载期间外部已修改：放弃本次结果（外部感知会刷新），防旧内容覆盖新磁盘
    if ((useNoteStore.getState().externalNoteEdits[file] ?? 0) !== seqAtLoad) return;
    rt.lastSaved = content;
    const current = view(file);
    if (!current) return;
    // 上一轮会话遗留的未决冲突：恢复本地内容并保持冲突态，等用户在冲突条选择保留本地/重新加载
    const sticky = useNoteStore.getState().noteConflicts[file]
      ? useNoteStore.getState().pendingNoteContent[file]
      : undefined;
    if (sticky !== undefined) {
      setConflict(rt, true);
      patchView(file, { content: sticky, dirty: true, syncSeq: ++syncSeqCounter });
      setSaveState(file, "edited");
      return;
    }
    // 加载完成前用户已输入：输入优先，不覆盖正在打的字
    if (!current.dirty) {
      replaceContent(rt, content);
      setSaveState(file, "idle");
    }
    ensureCollabBinding(rt);
  } catch {
    // 会话身份守卫：读盘失败（文件已删/权限）只标记自己的会话
    if (runtimeMap.get(file) !== rt) return;
    if (view(file)?.dirty) return;
    setSaveState(file, "idle", true);
  }
}

/** 外部修改感知：磁盘内容 ≠ 自上次落盘基准时刷新或转冲突。 */
async function handleExternalChange(rt: SessionRuntime): Promise<void> {
  const file = rt.file;
  try {
    const disk = await useNoteStore.getState().readNoteFresh(file);
    if (runtimeMap.get(file) !== rt) return;
    const current = view(file);
    if (!current || disk === rt.lastSaved) return;
    if (isCollabActive()) {
      // 多写者协作：对端/本端正收敛写盘是常态，不走单写者冲突模型
      if (disk === current.content) {
        rt.lastSaved = disk;
        useNoteCollabStore.getState().notifyNoteDiskWrite(file, bodyLF(disk));
        return;
      }
      if (current.dirty) return; // 本地有未落盘编辑：保留本地，等防抖写盘收敛
      if (rt.timer) {
        clearTimeout(rt.timer);
        rt.timer = null;
      }
      rt.lastSaved = disk;
      // 磁盘已持有该正文（对端写入）：三方合并的共同祖先随之对齐
      useNoteCollabStore.getState().notifyNoteDiskWrite(file, bodyLF(disk));
      replaceContent(rt, disk);
      patchView(file, { dirty: false });
      setSaveState(file, "idle");
      return;
    }
    if (current.dirty) {
      // 应用内其他编辑面写入（画布文本节点/AI 工具登记基线）：静默保留本地输入；
      // 真实外部修改则暂停自动保存并提示冲突，防覆盖
      if (isKnownNoteDiskContent(file, disk)) {
        rt.lastSaved = disk;
        return;
      }
      if (rt.timer) {
        clearTimeout(rt.timer);
        rt.timer = null;
      }
      setConflict(rt, true);
      return;
    }
    rt.lastSaved = disk;
    replaceContent(rt, disk);
    patchView(file, { dirty: false });
    setSaveState(file, "idle");
  } catch {
    // 外部删除等情况由窗口联动处理
  }
}

/** 冲突「重新加载」：丢弃本地改动回到磁盘最新（挂起输入一并作废，防旧内容随后被 flush 写回）。 */
async function reloadFromDisk(rt: SessionRuntime): Promise<void> {
  const file = rt.file;
  try {
    const disk = await useNoteStore.getState().readNoteFresh(file);
    if (runtimeMap.get(file) !== rt) return;
    rt.lastSaved = disk;
    setConflict(rt, false);
    useNoteStore.getState().setPendingNoteContent(file, null);
    useNoteCollabStore.getState().notifyNoteDiskWrite(file, bodyLF(disk));
    replaceContent(rt, disk);
    patchView(file, { dirty: false });
    setSaveState(file, "idle");
  } catch {
    // 读盘失败（文件已删）：保持现状，由窗口联动关闭
  }
}

/** 冲突「保留本地并保存」：立即覆盖外部修改落盘。 */
async function saveLocalOverExternal(rt: SessionRuntime): Promise<void> {
  const file = rt.file;
  const current = view(file);
  if (!current) return;
  const content = current.content;
  setConflict(rt, false);
  const seq = ++rt.saveSeq;
  setSaveState(file, "saving");
  try {
    await writeContent(rt, content);
    if (runtimeMap.get(file) !== rt || seq !== rt.saveSeq) return;
    rt.lastSaved = content;
    useNoteCollabStore.getState().notifyNoteDiskWrite(file, bodyLF(content));
    useNoteStore.getState().setPendingNoteContent(file, null);
    patchView(file, { dirty: false });
    setSaveState(file, "saved");
  } catch (e) {
    if (runtimeMap.get(file) !== rt) return;
    console.error("笔记保存失败", e);
    // 内容已被后续输入取代：不给更新的保存窗口贴旧结论
    if (view(file)?.content === content) setSaveState(file, "error");
  }
}

/** 撤销/重做回放：目标全文走提交链（保存/协作/冲突门控复用），不再记入撤销栈。 */
function replay(rt: SessionRuntime, dir: "undo" | "redo"): void {
  const file = rt.file;
  const current = view(file);
  if (!current) return;
  const stack = useNoteUndoStore.getState();
  const target =
    dir === "undo" ? stack.undo(file, current.content) : stack.redo(file, current.content);
  if (target === null) return;
  rt.applyingUndo = true;
  commit(rt, target, false);
  rt.applyingUndo = false;
  // 撤销/重做是应持久化的编辑：补登记挂起输入（提交链在回放标记下跳过了登记）
  useNoteStore.getState().setPendingNoteContent(file, target);
  if (currentBinding(file)) {
    useNoteCollabStore.getState().syncLocalBody(file, bodyLF(target));
  } else {
    patchView(file, { syncSeq: ++syncSeqCounter });
  }
}

/** 关闭会话：引用计数归零时 flush 未落盘输入、解绑协作并清运行时。 */
function closeSession(file: string): void {
  const rt = runtimeMap.get(file);
  if (!rt) return;
  rt.refcount -= 1;
  if (rt.refcount > 0) return;
  flushPending(rt);
  if (useNoteCollabStore.getState().bindings[file]) useNoteCollabStore.getState().unbind(file);
  runtimeMap.delete(file);
  useNoteSessionStore.setState((s) => {
    const sessions = { ...s.sessions };
    delete sessions[file];
    return { sessions };
  });
  // 编辑面集合变化：presence 的「谁在这篇笔记上」需立即反映（广播只在显式 publish 时重算）
  republishPresence();
}

/** 会话关闭前落盘挂起输入（冲突未决除外——不覆盖外部修改，提示条已告知）。 */
function flushPending(rt: SessionRuntime): void {
  if (rt.conflict) {
    if (rt.timer) {
      clearTimeout(rt.timer);
      rt.timer = null;
    }
    return;
  }
  if (!rt.timer) return;
  clearTimeout(rt.timer);
  rt.timer = null;
  const file = rt.file;
  const pending = view(file)?.content ?? rt.lastSaved;
  // 文件已从仓库列表消失 = 软件内改名（写到新路径）或真删除（跳过，防重建已删文件）
  const stillExists = useVaultStore.getState().noteList.some((n) => n.file === file);
  const target =
    stillExists ? file : lastNoteRenameTarget(file) ?? lastFolderRenameTarget(file);
  if (!target) return;
  void useNoteStore
    .getState()
    .saveNoteContent(target, pending)
    .then(() => {
      if (useNoteStore.getState().pendingNoteContent[file] === pending) {
        useNoteStore.getState().setPendingNoteContent(file, null);
      }
      // 不在此登记磁盘基线：本函数只由 closeSession 调用，编辑器绑定随之下线；保留文档（refcount=0）
      // 仍会接收帧并用 lastFlushed 当共同祖先，其偏旧由下次打开时的直读盘正文校正——
      // 偏旧是保守方向（不会把未落盘内容当已落盘），代价只是合并精度
    })
    .catch((e) => console.error("笔记保存失败", e));
}

/** 打开（或复用）会话：首次打开时载入磁盘内容、按需绑定协作。 */
function openSession(file: string, baselineContent?: string): NoteBodySession {
  const existing = runtimeMap.get(file);
  if (existing) {
    existing.refcount += 1;
    return existing.session;
  }
  const initialView: NoteBodySessionView = {
    content: baselineContent ?? "",
    dirty: false,
    conflict: false,
    error: false,
    syncSeq: ++syncSeqCounter,
    binding: currentBinding(file),
  };
  const rt: SessionRuntime = {
    file,
    refcount: 1,
    conflict: false,
    lastSaved: "",
    applyingUndo: false,
    processedExternalSeq: useNoteStore.getState().externalNoteEdits[file] ?? 0,
    processedResolveSeq: useNoteStore.getState().noteConflictResolveReq[file]?.seq ?? 0,
    timer: null,
    saveSeq: 0,
    pendingWrite: null,
    bindingPending: false,
    lastChangeAuthor: null,
    changeAuthors: new Map(),
    session: {
      getState: () => useNoteSessionStore.getState().sessions[file] ?? initialView,
      subscribe: (listener) => useNoteSessionStore.subscribe(listener),
      content: () => view(file)?.content ?? "",
      applyBody: (body) => {
        const current = view(file);
        if (!current) return;
        const parsed = parseFrontmatter(current.content);
        // 编辑器统一输出 LF：拼回 frontmatter 前转回文件原有换行，防文件内换行混用
        const restored = parsed.body.includes("\r\n") ? body.replace(/\n/g, "\r\n") : body;
        commit(rt, parsed.fmPrefix + restored, false);
      },
      commitContent: (content) => commit(rt, content, true),
      undo: () => replay(rt, "undo"),
      redo: () => replay(rt, "redo"),
      reloadFromDisk: () => void reloadFromDisk(rt),
      saveLocalOverExternal: () => void saveLocalOverExternal(rt),
      applyRollback: (content) => {
        rt.lastSaved = content;
        setConflict(rt, false);
        // 回滚内容已落盘：回滚前的本地输入作废，防关窗 flush 把它写回覆盖回滚结果
        useNoteStore.getState().setPendingNoteContent(file, null);
        useNoteCollabStore.getState().notifyNoteDiskWrite(file, bodyLF(content));
        replaceContent(rt, content);
        patchView(file, { dirty: false });
        setSaveState(file, "saved");
      },
      handleCollabDivergence: (ytextText, diskContent) => {
        const current = view(file);
        if (!current || !currentBinding(file)) return;
        // 本地有未落盘编辑：本地正文写回 ytext（本地最新者胜）
        if (current.dirty) {
          useNoteCollabStore.getState().syncLocalBody(file, bodyLF(current.content));
          return;
        }
        // 正文只存在于 CRDT，frontmatter/换行风格只存在于磁盘：直读盘正文可用时采纳形态以磁盘为准
        const shape = parseFrontmatter(diskContent ?? current.content);
        const restored = shape.body.includes("\r\n")
          ? ytextText.replace(/\n/g, "\r\n")
          : ytextText;
        const content = shape.fmPrefix + restored;
        if (content === current.content) return;
        // 采纳结果与磁盘逐字节一致：磁盘已持有，只对齐会话视图
        if (content === diskContent) {
          patchView(file, { content, dirty: false });
          setSaveState(file, "idle");
          return;
        }
        // 采纳对端/保留文档已前进的正文：按内容变更走保存链落盘。不得登记为已落盘基线——
        // 协作态编辑面以 ytext 为文档源，登记会让磁盘永久落后于编辑面，且随后的「改回该内容」
        // 会被判无改动而漏写；进撤销栈与远端合入同语义。
        commit(rt, content, false);
      },
    },
  };
  runtimeMap.set(file, rt);
  useNoteSessionStore.setState((s) => ({
    sessions: { ...s.sessions, [file]: initialView },
  }));
  // 编辑面集合变化：presence 的「谁在这篇笔记上」需立即反映（广播只在显式 publish 时重算）
  republishPresence();
  void load(rt);
  return rt.session;
}

/** 关闭全部会话（切仓库/笔记插件停用）：挂起输入落盘后清运行时。 */
export function closeAllNoteSessions(): void {
  for (const file of [...runtimeMap.keys()]) {
    const rt = runtimeMap.get(file);
    if (!rt) continue;
    rt.refcount = 1;
    closeSession(file);
  }
  // 冲突态随仓库/能力边界失效（noteStore 侧的单文件状态由各自的重置路径清），去重集合一并复位
  notifiedConflicts.clear();
  conflictedFiles.clear();
  notifyNoteSurfaceChange();
}

/** 本端已打开编辑面的笔记（协作 presence 上报用）。 */
export function openNoteSessionFiles(): string[] {
  return [...runtimeMap.keys()];
}

export const noteSurfaceProvider: NoteSurfaceProvider = {
  open: openSession,
  get: (file) => runtimeMap.get(file)?.session ?? null,
  isConflicted: (file) => conflictedFiles.has(file),
  close: closeSession,
};

// ===== 订阅：外部修改/冲突解决请求、协作文档与协作开关变化 =====

useNoteStore.subscribe((s, prev) => {
  if (
    s.externalNoteEdits === prev.externalNoteEdits &&
    s.noteConflictResolveReq === prev.noteConflictResolveReq
  ) {
    return;
  }
  for (const rt of [...runtimeMap.values()]) {
    const externalSeq = s.externalNoteEdits[rt.file] ?? 0;
    if (externalSeq > rt.processedExternalSeq) {
      rt.processedExternalSeq = externalSeq;
      void handleExternalChange(rt);
    }
    const request = s.noteConflictResolveReq[rt.file];
    const resolveSeq = request?.seq ?? 0;
    if (resolveSeq > rt.processedResolveSeq) {
      rt.processedResolveSeq = resolveSeq;
      if (request.keepLocal) void saveLocalOverExternal(rt);
      else void reloadFromDisk(rt);
    }
  }
});

// doc 整体重建后 noteCollabStore 会换上新的 ytext/awareness：同步到会话视图，编辑面随之重绑
useNoteCollabStore.subscribe((s, prev) => {
  if (s.bindings === prev.bindings) return;
  for (const rt of runtimeMap.values()) patchView(rt.file, { binding: currentBinding(rt.file) });
});

// 协作连接/开关变化：补绑定（正文非空时）并同步绑定状态
function syncCollabBindings(): void {
  for (const rt of runtimeMap.values()) {
    ensureCollabBinding(rt);
    patchView(rt.file, { binding: currentBinding(rt.file) });
  }
}

useCollabStore.subscribe((s, prev) => {
  if (s.connected !== prev.connected) syncCollabBindings();
});

useSettingsStore.subscribe((s, prev) => {
  if (s.collabEnabled !== prev.collabEnabled) syncCollabBindings();
});
