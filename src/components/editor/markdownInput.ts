/**
 * 编辑输入面：整篇纯文本由一个隐藏 textarea 承载。
 *
 * 职责边界——浏览器托管「输入语义」，本层只做搬运与绘制：
 * - textarea 的 value 就是正文源码（纯文本，零 HTML 往返），因此输入法组合、方向键、
 *   Home/End、跨块选区、剪贴板复制（得到源码）全部沿用浏览器原生行为；
 * - 自绘光标与选区：textarea 不可见，视觉由本层按源偏移在渲染 DOM 上测量后绘制；
 * - 撤销/重做不归本层（面板内由窗口级按文件撤销栈接管），故显式屏蔽原生历史按键，
 *   避免浏览器自身撤销与本应用撤销栈各撤一次。
 */

export interface EditSinkCallbacks {
  /** 文本变更（含输入法上屏）；来自用户输入。 */
  onTextChange(text: string): void;
  /** 选区变更（源偏移）。 */
  onSelectionChange(from: number, to: number): void;
  /** 输入法组合开关（组合期抑制外部回灌与候选浮层）。 */
  onCompositionChange(composing: boolean): void;
}

/**
 * 列表行 Enter 的文本变换：光标所在行是列表标记行时延续该项——
 * 无序沿用同字符、有序序号 +1（保留定界符）、任务项带空任务框，缩进保持同级；
 * 空项（标记后无内容）Enter 清掉整行标记退出列表。
 * 非列表标记行返回 null（走浏览器默认换行）。
 */
export function listEnterEdit(text: string, from: number): { text: string; cursor: number } | null {
  const lineStart = text.lastIndexOf("\n", Math.max(0, from - 1)) + 1;
  const lineEnd = text.indexOf("\n", from) === -1 ? text.length : text.indexOf("\n", from);
  const m = /^(\s*)([-+*]|(\d+)([.)]))[ \t]+(\[[ xX]\][ \t]+)?/.exec(text.slice(lineStart, lineEnd));
  if (!m) return null;
  const indent = m[1] ?? "";
  const marker = m[2]!;
  const num = m[3];
  const delim = m[4];
  const task = m[5];
  if (text.slice(lineStart + m[0].length, lineEnd).trim() === "") {
    // 空项 Enter 退出列表：整行标记清掉，光标到行首
    return { text: text.slice(0, lineStart) + text.slice(lineEnd), cursor: lineStart };
  }
  const nextMarker = num !== undefined && delim !== undefined ? `${Number(num) + 1}${delim}` : marker;
  const insert = `\n${indent}${nextMarker} ${task ? "[ ] " : ""}`;
  return { text: text.slice(0, from) + insert + text.slice(from), cursor: from + insert.length };
}

/** 按元素当前字体以 pre 布局测量文本宽度（与 textarea 内部行内布局同构）。 */
function measurePreWidth(el: HTMLTextAreaElement, text: string): number {
  const style = getComputedStyle(el);
  const probe = document.createElement("span");
  probe.style.cssText = "position:absolute;top:-9999px;white-space:pre;";
  probe.style.fontFamily = style.fontFamily;
  probe.style.fontSize = style.fontSize;
  probe.style.fontWeight = style.fontWeight;
  probe.style.fontStyle = style.fontStyle;
  probe.style.letterSpacing = style.letterSpacing;
  probe.textContent = text;
  document.body.appendChild(probe);
  const width = probe.offsetWidth;
  probe.remove();
  return width;
}

const SINK_STYLE = [
  "position:absolute",
  "top:0",
  "left:0",
  "width:1px",
  "height:1em",
  "padding:0",
  "margin:0",
  "border:0",
  "outline:none",
  "resize:none",
  "opacity:0",
  "overflow:hidden",
  "white-space:pre",
  "z-index:-1",
].join(";");

export class MarkdownEditSink {
  readonly el: HTMLTextAreaElement;
  #callbacks: EditSinkCallbacks;
  #composing = false;
  #applyingExternal = false;
  /** 原生历史开关：按文件撤销栈接管时关（画布草稿等无宿主撤销链的场景保留原生撤销）。 */
  #interceptHistory: boolean;
  /** 内部行高缓存（white-space:pre 下每行等高，字体不变则恒定）。 */
  #lineH: number | null = null;

  constructor(host: HTMLElement, callbacks: EditSinkCallbacks, options: { interceptHistory?: boolean } = {}) {
    this.#callbacks = callbacks;
    this.#interceptHistory = options.interceptHistory ?? true;
    const el = document.createElement("textarea");
    el.setAttribute("style", SINK_STYLE);
    el.setAttribute("autocapitalize", "off");
    el.setAttribute("autocorrect", "off");
    el.spellcheck = false;
    el.setAttribute("aria-hidden", "true");
    el.tabIndex = -1;
    host.appendChild(el);
    this.el = el;

    el.addEventListener("input", this.#onInput);
    el.addEventListener("compositionstart", this.#onCompositionStart);
    el.addEventListener("compositionend", this.#onCompositionEnd);
    el.addEventListener("keydown", this.#onKeyDown, true);
    document.addEventListener("selectionchange", this.#onSelectionChange);
  }

  get composing(): boolean {
    return this.#composing;
  }

  get selection(): { from: number; to: number } {
    return { from: this.el.selectionStart ?? 0, to: this.el.selectionEnd ?? 0 };
  }

  /** 写入权威文本（外部同步/程序化替换）：不触发 onTextChange，尽量保留选区。 */
  setText(text: string): void {
    if (this.el.value === text) return;
    const { from, to } = this.selection;
    this.#applyingExternal = true;
    this.el.value = text;
    const max = text.length;
    this.el.setSelectionRange(Math.min(from, max), Math.min(to, max));
    this.#applyingExternal = false;
    this.#callbacks.onSelectionChange(this.el.selectionStart ?? 0, this.el.selectionEnd ?? 0);
    this.syncScroll();
  }

  /** 程序化设置选区（点击落点/外部定位）；focus=true 时把焦点交给输入面。 */
  setSelection(from: number, to: number, focus = true): void {
    const max = this.el.value.length;
    const start = Math.max(0, Math.min(from, max));
    const end = Math.max(start, Math.min(to, max));
    // 先聚焦再设选区：对未聚焦的输入面先设范围会被聚焦动作重置
    if (focus && document.activeElement !== this.el) this.el.focus({ preventScroll: true });
    this.el.setSelectionRange(start, end);
    this.#callbacks.onSelectionChange(start, end);
    this.syncScroll();
  }

  /** 把输入面挪到光标矩形处，让输入法候选框跟随真实光标位置（矩形为编辑面宿主坐标）。
   *  高度由 syncScroll 按内部行高管理（滚动窗口须容纳整行，末行才能对齐）。 */
  moveTo(rect: { left: number; top: number } | null): void {
    if (!rect) return;
    this.el.style.left = `${Math.round(rect.left)}px`;
    this.el.style.top = `${Math.round(rect.top)}px`;
  }

  /**
   * 内部滚动对齐：输入法候选窗锚定 textarea 内组合光标的视口坐标。textarea
   * （white-space:pre）内部按换行符分行、行内横向展开——光标的内部坐标 =
   * 前置行数 × 行高 + 行内前置文本宽，两者都会让候选窗偏离 moveTo 定位的光标处。
   * 把内部滚动滚到光标行列，其视口坐标即贴回原点；行高按内容总高 / 总行数实测缓存，
   * 行内宽度按输入面当前字体测量（与内部布局同构的 pre 布局）。
   */
  syncScroll(): void {
    const el = this.el;
    const value = el.value;
    if (!value) return;
    if (this.#lineH === null) {
      const totalLines = (value.match(/\n/g) ?? []).length + 1;
      this.#lineH = el.scrollHeight / totalLines;
      // 滚动窗口至少容纳一行：文档末行也能滚到光标视口顶（height 过小会被 maxScroll 钳制）
      el.style.height = `${this.#lineH}px`;
    }
    const at = Math.max(el.selectionStart ?? 0, el.selectionEnd ?? 0);
    const before = value.slice(0, at);
    const targetTop = (before.match(/\n/g) ?? []).length * this.#lineH;
    const lineStart = before.lastIndexOf("\n") + 1;
    const targetLeft = lineStart < at ? measurePreWidth(el, value.slice(lineStart, at)) : 0;
    if (Math.abs(el.scrollTop - targetTop) >= 1) el.scrollTop = targetTop;
    if (Math.abs(el.scrollLeft - targetLeft) >= 1) el.scrollLeft = targetLeft;
  }

  focus(): void {
    this.el.focus({ preventScroll: true });
  }

  destroy(): void {
    this.el.removeEventListener("input", this.#onInput);
    this.el.removeEventListener("compositionstart", this.#onCompositionStart);
    this.el.removeEventListener("compositionend", this.#onCompositionEnd);
    this.el.removeEventListener("keydown", this.#onKeyDown, true);
    document.removeEventListener("selectionchange", this.#onSelectionChange);
    this.el.remove();
  }

  #onInput = (): void => {
    // 组合期不上报：未上屏的组合文本不得进入正文链（防半截拼音落盘/广播/全文重渲染），
    // 最终内容由 compositionend 统一提交
    if (this.#applyingExternal || this.#composing) return;
    this.#callbacks.onTextChange(this.el.value);
  };

  #onCompositionStart = (): void => {
    this.#composing = true;
    this.#callbacks.onCompositionChange(true);
    this.syncScroll();
  };

  #onCompositionEnd = (): void => {
    this.#composing = false;
    this.#callbacks.onCompositionChange(false);
    this.#callbacks.onTextChange(this.el.value);
  };

  /** 面板内撤销归按文件撤销栈：屏蔽原生历史，避免双撤销（其余按键全部交浏览器原生处理）。 */
  #onKeyDown = (event: KeyboardEvent): void => {
    if (!this.#interceptHistory) return;
    if (!(event.ctrlKey || event.metaKey)) return;
    const key = event.key.toLowerCase();
    if (key === "z" || key === "y") event.preventDefault();
  };

  #onSelectionChange = (): void => {
    if (document.activeElement !== this.el) return;
    this.#callbacks.onSelectionChange(this.el.selectionStart ?? 0, this.el.selectionEnd ?? 0);
    this.syncScroll();
  };
}

/** 自绘光标与选区的绘制层：按源区间在渲染 DOM 上测量矩形并绘制。 */
/** 远端光标（协作者）：位置 + 昵称与用户色。 */
export interface RemoteCursor {
  readonly rect: DOMRect | null;
  readonly label: string;
  readonly color: string;
}

export class CaretOverlay {
  readonly el: HTMLElement;
  #caret: HTMLElement;
  #selectionLayer: HTMLElement;
  #remoteLayer: HTMLElement;

  constructor(host: HTMLElement) {
    const el = document.createElement("div");
    el.className = "md-caret-layer";
    el.setAttribute("aria-hidden", "true");
    const selectionLayer = document.createElement("div");
    selectionLayer.className = "md-selection-layer";
    const remoteLayer = document.createElement("div");
    remoteLayer.className = "md-remote-layer";
    const caret = document.createElement("div");
    caret.className = "md-caret";
    el.append(selectionLayer, remoteLayer, caret);
    host.appendChild(el);
    this.el = el;
    this.#selectionLayer = selectionLayer;
    this.#remoteLayer = remoteLayer;
    this.#caret = caret;
  }

  /** 绘制协作者光标（含昵称标签）。 */
  setRemoteCursors(cursors: readonly RemoteCursor[], origin: DOMRect): void {
    const fragment = document.createDocumentFragment();
    for (const cursor of cursors) {
      if (!cursor.rect) continue;
      const box = document.createElement("div");
      box.className = "md-remote-caret";
      box.style.left = `${cursor.rect.left - origin.left}px`;
      box.style.top = `${cursor.rect.top - origin.top}px`;
      box.style.height = `${cursor.rect.height || 18}px`;
      box.style.background = cursor.color;
      if (cursor.label) {
        const tag = document.createElement("span");
        tag.className = "md-remote-caret-label";
        tag.style.background = cursor.color;
        tag.textContent = cursor.label;
        box.appendChild(tag);
      }
      fragment.appendChild(box);
    }
    this.#remoteLayer.replaceChildren(fragment);
  }

  /** 绘制选区（多段矩形，内容坐标 = 相对绘制层已定位的父元素）。 */
  setSelectionRects(rects: DOMRect[], origin: DOMRect): void {
    const fragment = document.createDocumentFragment();
    for (const rect of rects) {
      const box = document.createElement("div");
      box.className = "md-selection-rect";
      box.style.left = `${rect.left - origin.left}px`;
      box.style.top = `${rect.top - origin.top}px`;
      box.style.width = `${rect.width}px`;
      box.style.height = `${rect.height}px`;
      fragment.appendChild(box);
    }
    this.#selectionLayer.replaceChildren(fragment);
  }

  /** 绘制光标；rect 为 null 表示不显示（如焦点不在编辑面）。 */
  setCaretRect(rect: DOMRect | null, origin: DOMRect): void {
    if (!rect) {
      this.#caret.style.display = "none";
      return;
    }
    this.#caret.style.display = "";
    this.#caret.style.left = `${rect.left - origin.left}px`;
    this.#caret.style.top = `${rect.top - origin.top}px`;
    this.#caret.style.height = `${rect.height || 18}px`;
  }

  destroy(): void {
    this.el.remove();
  }
}