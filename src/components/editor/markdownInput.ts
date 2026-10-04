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

/** 输入法组合中的未上屏文本：`at` 为起始偏移，`remove` 为被它替换掉的正文长度。 */
export interface CompositionText {
  at: number;
  text: string;
  remove: number;
}

export interface EditSinkCallbacks {
  /** 文本变更（含输入法上屏）；来自用户输入。 */
  onTextChange(text: string): void;
  /** 选区变更（源偏移，组合期已折算回正文坐标）。 */
  onSelectionChange(from: number, to: number): void;
  /** 输入法组合态：`preedit` 为未上屏文本（组合结束为 null）；组合期为真时调用方不应回灌正文。 */
  onCompositionChange(composing: boolean, preedit: CompositionText | null): void;
}

/**
 * 列表行 Enter 的文本变换：光标所在行是列表标记行时延续该项——
 * 无序沿用同字符、有序序号 +1（保留定界符）、任务项带空任务框，缩进保持同级；
 * 空项亦然（继续列表），退出列表靠退格删掉标记。
 * 光标停在标记本身之前（行首）时返回 null：那是「在标记前换行」，再续一个标记会拆出嵌套列表。
 */
export function listEnterEdit(text: string, from: number): { text: string; cursor: number } | null {
  const lineStart = text.lastIndexOf("\n", Math.max(0, from - 1)) + 1;
  const lineEnd = text.indexOf("\n", from) === -1 ? text.length : text.indexOf("\n", from);
  // 标记后的空格按「一个以上或行尾」判定：`-` 独占一行是合法项，`-foo` 不是
  const m = /^(\s*)([-+*]|(\d+)([.)]))(?:[ \t]+|$)(\[[ xX]\][ \t]*)?/.exec(text.slice(lineStart, lineEnd));
  if (!m) return null;
  if (from < lineStart + m[0].length) return null;
  const indent = m[1] ?? "";
  const marker = m[2]!;
  const num = m[3];
  const delim = m[4];
  const task = m[5];
  const nextMarker = num !== undefined && delim !== undefined ? `${Number(num) + 1}${delim}` : marker;
  const insert = `\n${indent}${nextMarker} ${task ? "[ ] " : ""}`;
  return { text: text.slice(0, from) + insert + text.slice(from), cursor: from + insert.length };
}

/**
 * 空列表项行上的退格变换：行内只有标记（无内容）且光标落在标记之内或之后时，一次删掉整段标记
 * （缩进 + 标记 + 标记后空格 + 任务框），留下一行空行、光标到行首——光标已经在行首时再退格
 * 走浏览器的「并进上一行」，于是「回车新建空项 → 退格」不会在源码里残留标记，两次退格又能
 * 把这一行收掉。其余情况返回 null（逐字符退格交浏览器）。
 */
export function listBackspaceEdit(text: string, from: number): { text: string; cursor: number } | null {
  const lineStart = text.lastIndexOf("\n", Math.max(0, from - 1)) + 1;
  const lineEnd = text.indexOf("\n", from) === -1 ? text.length : text.indexOf("\n", from);
  const m = /^(\s*)([-+*]|(\d+)[.)])(?:[ \t]+|$)(\[[ xX]\][ \t]*)?$/.exec(text.slice(lineStart, lineEnd));
  if (!m) return null;
  const markerEnd = lineStart + m[0].length;
  // 光标已在行首：这一下该并进上一行（交浏览器）；光标在标记之内或标记之后都算「删这个空项」
  if (from <= lineStart || from > markerEnd) return null;
  return { text: text.slice(0, lineStart) + text.slice(markerEnd), cursor: lineStart };
}

/** 文本变换结果：新正文 + 变换后的选区（源偏移）。 */
export interface LineEdit {
  text: string;
  from: number;
  to: number;
}

/** 行级缩进单位：两个空格（列表嵌套的通行写法；CommonMark 下 `- ` 项缩 2 即构成子项）。 */
const INDENT_UNIT = "  ";

/** offset 所在行的行首偏移。 */
function lineStartAt(text: string, offset: number): number {
  return text.lastIndexOf("\n", Math.max(0, offset - 1)) + 1;
}

/** offset 所在行文本（不含换行符）。 */
function lineTextAt(text: string, offset: number): string {
  const start = lineStartAt(text, offset);
  const end = text.indexOf("\n", start);
  return text.slice(start, end === -1 ? text.length : end);
}

/** 列表项行判定：行首缩进 + 列表标记 + 标记后空格或行尾（与 {@link listEnterEdit} 同口径）。 */
function isListLine(line: string): boolean {
  return /^\s*(?:[-+*]|\d+[.)])(?:[ \t]|$)/.test(line);
}

/** 选区覆盖的行首偏移：折叠光标即当前行；有选区时取最后一个被选中字符所在行（行首处的终点不计入）。 */
function coveredLineStarts(text: string, from: number, to: number): number[] {
  const lastStart = lineStartAt(text, to > from ? to - 1 : from);
  const starts: number[] = [];
  let start = lineStartAt(text, from);
  for (;;) {
    starts.push(start);
    if (start >= lastStart) return starts;
    start = text.indexOf("\n", start) + 1;
  }
}

/**
 * Tab 缩进变换。有选区、或光标停在列表项行时整行缩进——列表靠行首缩进判定嵌套，整行右移
 * 才能把一行变成上一项的子项；其余情况（代码块、普通段落）只在光标处插入一级缩进，
 * 与代码编辑器一致。
 */
export function indentEdit(text: string, from: number, to: number): LineEdit {
  if (from === to && !isListLine(lineTextAt(text, from))) {
    return { text: text.slice(0, from) + INDENT_UNIT + text.slice(from), from: from + INDENT_UNIT.length, to: to + INDENT_UNIT.length };
  }
  const starts = coveredLineStarts(text, from, to);
  const shift = (offset: number) => offset + INDENT_UNIT.length * starts.filter((s) => s <= offset).length;
  let out = "";
  let prev = 0;
  for (const start of starts) {
    out += text.slice(prev, start) + INDENT_UNIT;
    prev = start;
  }
  return { text: out + text.slice(prev), from: shift(from), to: shift(to) };
}

/**
 * Shift+Tab 反缩进变换：选区覆盖的每一行（折叠光标即当前行）去掉一级前导缩进——行首制表符
 * 整体删一个，否则删至多一个缩进单位的空格；没有任何可删的行返回 null（不改动、由调用方决定是否吞键）。
 */
export function outdentEdit(text: string, from: number, to: number): LineEdit | null {
  const removals: { start: number; count: number }[] = [];
  for (const start of coveredLineStarts(text, from, to)) {
    let count = 0;
    if (text[start] === "\t") count = 1;
    else while (count < INDENT_UNIT.length && text[start + count] === " ") count++;
    if (count > 0) removals.push({ start, count });
  }
  if (removals.length === 0) return null;
  const map = (offset: number): number => {
    let removed = 0;
    for (const r of removals) {
      if (r.start >= offset) break;
      if (offset < r.start + r.count) return r.start - removed;
      removed += r.count;
    }
    return offset - removed;
  };
  let out = "";
  let prev = 0;
  for (const r of removals) {
    out += text.slice(prev, r.start);
    prev = r.start + r.count;
  }
  return { text: out + text.slice(prev), from: map(from), to: map(to) };
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

/** 隐藏输入面类名：基态样式在 styles/index.css 的 .md-edit-sink（走类不走 style 属性，见该处说明）。 */
const SINK_CLASS = "md-edit-sink";

export class MarkdownEditSink {
  readonly el: HTMLTextAreaElement;
  #callbacks: EditSinkCallbacks;
  #composing = false;
  /** 组合起点（正文坐标）：输入面 value 组合期已含未上屏文本，用它折回正文偏移。 */
  #composeAt = 0;
  /** 组合替换掉的正文终点（通常等于起点；有选区时大于起点）。 */
  #composeTo = 0;
  #preedit = "";
  #applyingExternal = false;
  /** 原生历史开关：按文件撤销栈接管时关（画布草稿等无宿主撤销链的场景保留原生撤销）。 */
  #interceptHistory: boolean;
  /** 内部行高缓存（white-space:pre 下每行等高，字体不变则恒定）。 */
  #lineH: number | null = null;

  constructor(host: HTMLElement, callbacks: EditSinkCallbacks, options: { interceptHistory?: boolean } = {}) {
    this.#callbacks = callbacks;
    this.#interceptHistory = options.interceptHistory ?? true;
    const el = document.createElement("textarea");
    el.className = SINK_CLASS;
    el.setAttribute("autocapitalize", "off");
    el.setAttribute("autocorrect", "off");
    el.spellcheck = false;
    el.setAttribute("aria-hidden", "true");
    el.tabIndex = -1;
    host.appendChild(el);
    this.el = el;

    el.addEventListener("input", this.#onInput);
    el.addEventListener("compositionstart", this.#onCompositionStart);
    el.addEventListener("compositionupdate", this.#onCompositionUpdate);
    el.addEventListener("compositionend", this.#onCompositionEnd);
    el.addEventListener("keydown", this.#onKeyDown, true);
    document.addEventListener("selectionchange", this.#onSelectionChange);
  }

  get composing(): boolean {
    return this.#composing;
  }

  get preedit(): CompositionText | null {
    return this.#composing
      ? { at: this.#composeAt, text: this.#preedit, remove: Math.max(0, this.#composeTo - this.#composeAt) }
      : null;
  }

  /** 选区（正文坐标）：组合期输入面选区落在未上屏文本上，折算回组合起点。 */
  get selection(): { from: number; to: number } {
    const raw = { from: this.el.selectionStart ?? 0, to: this.el.selectionEnd ?? 0 };
    if (!this.#composing) return raw;
    return { from: this.#toCommitted(raw.from), to: this.#toCommitted(raw.to) };
  }

  /**
   * 输入面坐标 → 正文坐标。组合期 value = 正文前段 + 未上屏文本 + 正文后段，
   * 未上屏文本占据的区间整体折叠到组合起点，被替换的正文区间按长度补回。
   */
  #toCommitted(valueOffset: number): number {
    const composed = this.#composeAt + this.#preedit.length;
    if (valueOffset <= this.#composeAt) return valueOffset;
    if (valueOffset < composed) return this.#composeAt;
    return valueOffset - composed + this.#composeTo;
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
    const { from: restFrom, to: restTo } = this.selection;
    this.#callbacks.onSelectionChange(restFrom, restTo);
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
    this.el.removeEventListener("compositionupdate", this.#onCompositionUpdate);
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

  /** 组合开始：此刻 value 仍是正文，选区即被组合替换的正文区间。 */
  #onCompositionStart = (event: CompositionEvent): void => {
    this.#composing = true;
    this.#composeAt = Math.min(this.el.selectionStart ?? 0, this.el.selectionEnd ?? 0);
    this.#composeTo = Math.max(this.el.selectionStart ?? 0, this.el.selectionEnd ?? 0);
    this.#preedit = event.data ?? "";
    this.#callbacks.onCompositionChange(true, this.preedit);
    this.syncScroll();
  };

  /** 组合串增长/变化：上报未上屏文本，编辑面据此就地显示（此时不落正文）。 */
  #onCompositionUpdate = (event: CompositionEvent): void => {
    if (!this.#composing) return;
    this.#preedit = event.data ?? "";
    this.#callbacks.onCompositionChange(true, this.preedit);
  };

  #onCompositionEnd = (): void => {
    this.#composing = false;
    this.#preedit = "";
    this.#callbacks.onCompositionChange(false, null);
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
    const { from, to } = this.selection;
    this.#callbacks.onSelectionChange(from, to);
    this.syncScroll();
  };
}

/** 自绘光标与选区的绘制层：按源区间在渲染 DOM 上测量矩形并绘制。 */
/** 远端光标（协作者）：光标位置 + 选区矩形 + 昵称与用户色。 */
export interface RemoteCursor {
  readonly rect: DOMRect | null;
  /** 选区覆盖的矩形（视口坐标，跨块多段）；折叠光标为空数组。 */
  readonly rects: readonly DOMRect[];
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

  /** 绘制协作者光标与选区：选区矩形按协作者色半透明铺底（色浓度与本地选区一致），
   *  光标竖线带昵称标签；rect 为 null 时只画选区。 */
  setRemoteCursors(cursors: readonly RemoteCursor[], origin: DOMRect): void {
    const fragment = document.createDocumentFragment();
    for (const cursor of cursors) {
      for (const rect of cursor.rects) {
        const box = document.createElement("div");
        box.className = "md-remote-selection-rect";
        box.style.left = `${rect.left - origin.left}px`;
        box.style.top = `${rect.top - origin.top}px`;
        box.style.width = `${rect.width}px`;
        box.style.height = `${rect.height}px`;
        box.style.background = `color-mix(in srgb, ${cursor.color} 25%, transparent)`;
        fragment.appendChild(box);
      }
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