/**
 * 笔记正文编辑会话契约（笔记正文编辑能力的定义面）。
 *
 * 一篇 `.md` 的全文、协作绑定与未落盘输入只有一份，会话是它唯一的持有者；
 * 笔记面板与画布文本节点都是该会话的编辑面，经 `utils/noteSurfaceHost` 取用实现。
 */

import type { Text as YText } from "yjs";
import type { Awareness } from "y-protocols/awareness";

/** 协作态正文绑定（下发给 MarkdownEditor 做 y-codemirror 绑定）。 */
export interface NoteEditorBinding {
  ytext: YText;
  awareness: Awareness;
}

/** 会话可订阅状态（引用稳定：仅在变化时替换，订阅方按引用比较决定重渲染）。 */
export interface NoteBodySessionView {
  /** 全文（frontmatter + 正文，保持文件原换行）。 */
  content: string;
  /** 有未落盘输入。 */
  dirty: boolean;
  /** 冲突未决：外部已修改且本地有改动，自动保存已暂停（解决入口见笔记面板冲突条）。 */
  conflict: boolean;
  /** 最近一次落盘失败（磁盘/权限等）：编辑面据此提示，成功或重开后复位。 */
  error: boolean;
  /** 非用户编辑的内容更新序号（加载完成/外部刷新/冲突重载/撤销回放/历史回滚），编辑面据此重建。 */
  syncSeq: number;
  /** 协作态正文绑定；非协作或未连接为 null。 */
  binding: NoteEditorBinding | null;
}

export interface NoteBodySession {
  getState: () => NoteBodySessionView;
  subscribe: (listener: () => void) => () => void;
  /** 当前全文（命令式读取，剪贴板等一次性取值用）。 */
  content: () => string;
  /** 实时预览编辑提交（入参为正文 LF；frontmatter 与文件换行由会话拼回）。 */
  applyBody: (bodyLF: string) => void;
  /** 全文提交（源码模式编辑、属性区合并）。 */
  commitContent: (content: string) => void;
  /** 协作挂载分歧（ytext 正文与本地正文相悖）：本地有未落盘编辑则写回 ytext，否则把 ytext 收作会话基准。 */
  handleCollabDivergence: (ytextText: string) => void;
  undo: () => void;
  redo: () => void;
  /** 冲突「重新加载」：丢弃本地改动，回到磁盘最新。 */
  reloadFromDisk: () => void;
  /** 冲突「保留本地并保存」：覆盖外部修改落盘。 */
  saveLocalOverExternal: () => void;
  /** 历史回滚后的内容登记（已落盘）：记入磁盘基准并让编辑面重建。 */
  applyRollback: (content: string) => void;
}

/** 笔记正文编辑能力提供者（笔记域实现，随笔记插件启停注册/注销）。 */
export interface NoteSurfaceProvider {
  /** 打开（或复用）某笔记的编辑会话；baselineContent 让编辑面先有内容可渲染，随后以磁盘为准。 */
  open: (file: string, baselineContent?: string) => NoteBodySession;
  /** 已打开的会话；null = 该文件当前无会话（不改变引用计数）。 */
  get: (file: string) => NoteBodySession | null;
  /** 该笔记是否有未决冲突（会话关掉后仍然为真，直到冲突被解决或作废）。 */
  isConflicted: (file: string) => boolean;
  /** 关闭会话（引用计数减一，归零时 flush 未落盘输入并解绑协作）。 */
  close: (file: string) => void;
}
