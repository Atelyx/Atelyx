import { useCallback, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { useReactFlow } from "@xyflow/react";
import { useCanvasStore } from "@/stores/canvasStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { baseName, noteTitleFromFile, tableTitleFromFile } from "@/utils/filename";
import { useHandleMoveFile, type DragSession } from "./actions";
import type { FileTreeNode } from "@/types";

/** 触屏起拖需长按达到的时长（按住不动才进入拖拽，否则让位于列表滚动）。 */
const TOUCH_ARM_MS = 400;
/** 触屏长按期间的容许抖动，超过视为滚动意图。 */
const TOUCH_MOVE_TOLERANCE_PX = 8;

/** 拖拽幽灵（pointer 模拟拖拽时跟随鼠标；下方追加悬停目标的动作提示）。 */
export interface DragGhost {
  label: string;
  x: number;
  y: number;
}

/** pointer 模拟拖拽（HTML5 DnD 在 WebView2 不可靠，弃用）：记录潜在拖拽会话、
 * 全局 pointermove/up 判定位移 → 显示幽灵 → 松手落到文件夹移动 / 画布建节点。 */
export function useVaultDrag(onNotice: (message: string) => void) {
  const { screenToFlowPosition } = useReactFlow();
  const addTextNoteFromVault = useCanvasStore((s) => s.addTextNoteFromVault);
  const addMediaFromVault = useCanvasStore((s) => s.addMediaFromVault);
  const addTableFromVault = useCanvasStore((s) => s.addTableFromVault);
  const handleMoveFile = useHandleMoveFile(onNotice);

  const dragRef = useRef<DragSession | null>(null);
  const [dragGhost, setDragGhost] = useState<DragGhost | null>(null);
  /** 拖拽悬停的目标文件夹（data-dir 命中），高亮提示可放入；null = 无目标。 */
  const [dropDir, setDropDir] = useState<string | null>(null);
  const dropDirRef = useRef<string | null>(null);
  /** 拖拽悬停可交互目标的提示文本（幽灵下方显示）；null = 无目标。 */
  const [dragHint, setDragHint] = useState<string | null>(null);
  const dragHintRef = useRef<string | null>(null);
  /** 触屏长按计时器（未成立前移动即作废）。 */
  const armTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  /** 触屏会话是否仍在等待长按成立。 */
  const pendingTouchRef = useRef(false);

  // 触屏拖拽期间阻止列表滚动：touch-action 在手势开始后不可改，只能拦截 touchmove；
  // 仅在长按成立后挂载，避免常驻非被动监听拖慢全局滚动
  const touchMoveGuard = useCallback((e: TouchEvent) => {
    if (dragRef.current?.active) e.preventDefault();
  }, []);

  /** 清空拖拽会话与长按计时（松手/取消/放弃起拖共用）。 */
  const resetDrag = useCallback(() => {
    document.removeEventListener("touchmove", touchMoveGuard);
    if (armTimerRef.current !== undefined) {
      clearTimeout(armTimerRef.current);
      armTimerRef.current = undefined;
    }
    pendingTouchRef.current = false;
    dragRef.current = null;
    setDragGhost(null);
    dropDirRef.current = null;
    setDropDir(null);
    dragHintRef.current = null;
    setDragHint(null);
    document.body.style.cursor = "";
  }, [touchMoveGuard]);

  // 全局 pointermove/up：位移超 5px 进入拖拽（显示幽灵）；松手在文件夹行 = 移动文件，落点在 .react-flow 内 = 建节点
  useEffect(() => {
    /** 触屏长按成立后松手仍会派生一次 click（打开文件/切换目录），抑制这一次抬起。 */
    let suppressClickUntil = 0;
    const onMove = (e: PointerEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const dist = Math.hypot(e.clientX - d.startX, e.clientY - d.startY);
      // 触屏长按未成立前的移动 = 滚动意图，放弃起拖
      if (pendingTouchRef.current) {
        if (dist > TOUCH_MOVE_TOLERANCE_PX) resetDrag();
        return;
      }
      if (d.active || dist > 5) {
        // 位置变化超 2px 才 setState（pointermove 高频触发，节流避免每帧重渲染整个面板）
        const moved = !d.active || Math.hypot(e.clientX - d.x, e.clientY - d.y) > 2;
        dragRef.current = { ...d, active: true, x: e.clientX, y: e.clientY };
        if (moved) setDragGhost({ label: d.title ?? d.name, x: e.clientX, y: e.clientY });
        // 拖拽悬停目标文件夹：高亮可放入（变化才 setState）；文件行（data-file）内部不视为目标（防误判根目录）
        const hit = document.elementFromPoint(e.clientX, e.clientY);
        const dirEl = hit?.closest<HTMLElement>("[data-dir]");
        let dir = dirEl && !hit?.closest<HTMLElement>("[data-file]") ? (dirEl.dataset.dir ?? "") : null;
        // 拖文件夹：自身、自身后代、自身祖先均不可放入（非法嵌套/原地 no-op），不高亮
        if (
          d.kind === "folder" &&
          dir !== null &&
          (dir === d.file || dir.startsWith(`${d.file}/`) || d.file.startsWith(`${dir}/`))
        ) {
          dir = null;
        }
        if (dir !== dropDirRef.current) {
          dropDirRef.current = dir;
          setDropDir(dir);
        }
        // 悬停可交互目标的提示文本（幽灵下方显示；变化才 setState）：分支顺序与 onUp 落点判定严格一致
        let hint: string | null = null;
        if (hit?.closest<HTMLElement>("[data-chat-input]")) {
          hint = "作为引用";
        } else if (dir !== null) {
          hint = dir === "" ? "移到根目录" : `移动到「${baseName(dir)}」`;
        } else if (hit?.closest(".react-flow")) {
          if (d.kind === "note") hint = "创建文本节点";
          else if (d.kind === "table") hint = "创建表格节点";
          else if (d.kind === "attachment") hint = "创建媒体节点";
        }
        if (hint !== dragHintRef.current) {
          dragHintRef.current = hint;
          setDragHint(hint);
        }
        // 平时行上不显示 grab 光标（避免误以为可点），拖拽激活时才显示「抓住」
        document.body.style.cursor = "grabbing";
      }
    };
    const onUp = (e: PointerEvent) => {
      const d = dragRef.current;
      const wasActive = !!d?.active;
      if (wasActive && e.pointerType === "touch") suppressClickUntil = Date.now() + 400;
      resetDrag();
      if (!d || !wasActive) return;
      const target = document.elementFromPoint(e.clientX, e.clientY);
      // 拖入右侧 AI 对话面板输入框（data-chat-input）：任意文件/文件夹 → @引用 入队（AiChatPanel 消费后显示 @标签；
      // 发送只并路径进「引用文件」块，模型 read_file/glob 按需读取）
      if (target?.closest<HTMLElement>("[data-chat-input]")) {
        useChatPanelStore
          .getState()
          .queueMention({ file: d.file, label: d.title ?? d.name });
        return;
      }
      // 优先：落到文件夹行/树空白（data-dir，含根目录 data-dir=""）→ 移动文件；文件行内部不是目标
      const dirEl = target?.closest<HTMLElement>("[data-dir]");
      const inFileRow = !!target?.closest<HTMLElement>("[data-file]");
      if (dirEl && !inFileRow) {
        void handleMoveFile(d, dirEl.dataset.dir ?? "");
        return;
      }
      if (target?.closest(".react-flow")) {
        // 画布行（kind="canvas"）与文件夹行只支持拖到文件夹移动，不支持拖到画布建节点（media 节点会按附件误读 JSON）
        if (d.kind === "canvas" || d.kind === "folder") return;
        const pos = screenToFlowPosition({ x: e.clientX, y: e.clientY });
        if (d.kind === "note") void addTextNoteFromVault(d.file, d.title ?? d.name, pos, true);
        else if (d.kind === "table") void addTableFromVault(d.file, d.title ?? d.name, pos, true);
        else void addMediaFromVault(d.file, d.name, pos, true);
      }
    };
    // 抑制长按拖拽抬起派生的 click（捕获阶段拦在 React 根容器之前）
    const onClickCapture = (e: MouseEvent) => {
      if (Date.now() >= suppressClickUntil) return;
      suppressClickUntil = 0;
      e.stopPropagation();
      e.preventDefault();
    };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", resetDrag);
    document.addEventListener("click", onClickCapture, true);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", resetDrag);
      document.removeEventListener("click", onClickCapture, true);
      resetDrag();
    };
  }, [screenToFlowPosition, addTextNoteFromVault, addMediaFromVault, addTableFromVault, handleMoveFile, resetDrag]);

  /** 行按下（左键）记录潜在拖拽会话（文件夹/画布/笔记/附件均可拖：移动到文件夹；文件还可拖到画布建节点）。
   *  鼠标：位移超阈值即拖拽；触屏：长按成立后才进入拖拽态（否则让位于列表滚动）。 */
  const startPotentialDrag = (e: ReactPointerEvent, node: FileTreeNode) => {
    if (e.button !== 0) return;
    e.preventDefault(); // 阻止文本选择干扰

    let session: DragSession;
    if (node.isDir) {
      session = {
        kind: "folder",
        file: node.path,
        name: node.name,
        startX: e.clientX,
        startY: e.clientY,
        active: false,
        x: e.clientX,
        y: e.clientY,
      };
    } else {
      // 外部白板（.canvas）与 .atlx 同归 canvas 类：拖到画布不建节点（只支持移动到文件夹）
      const lower = node.name.toLowerCase();
      const isCanvasFile = lower.endsWith(".atlx") || lower.endsWith(".canvas");
      const kind: DragSession["kind"] = isCanvasFile
        ? "canvas"
        : lower.endsWith(".md")
          ? "note"
          : lower.endsWith(".atb")
            ? "table"
            : "attachment";
      session = {
        kind,
        file: node.path,
        name: node.name,
        title: kind === "note" ? noteTitleFromFile(node.path) : kind === "table" ? tableTitleFromFile(node.path) : undefined,
        startX: e.clientX,
        startY: e.clientY,
        active: false,
        x: e.clientX,
        y: e.clientY,
      };
    }

    dragRef.current = session;
    if (e.pointerType !== "touch") {
      pendingTouchRef.current = false;
      return;
    }
    // 触屏：长按成立才起拖，成立前移动（滚动）由 onMove 作废
    pendingTouchRef.current = true;
    if (armTimerRef.current !== undefined) clearTimeout(armTimerRef.current);
    armTimerRef.current = setTimeout(() => {
      armTimerRef.current = undefined;
      const d = dragRef.current;
      if (!d || !pendingTouchRef.current) return;
      pendingTouchRef.current = false;
      dragRef.current = { ...d, active: true };
      document.addEventListener("touchmove", touchMoveGuard, { passive: false });
      setDragGhost({ label: d.title ?? d.name, x: d.startX, y: d.startY });
    }, TOUCH_ARM_MS);
  };

  return { dragGhost, dropDir, dragHint, startPotentialDrag };
}
