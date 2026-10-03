/**
 * 工作区面板网格：递归多叉树渲染。
 *
 * Split → 自研 flex 分割容器（子项按占比分配，相邻子项间插拖拽手柄，拖拽回写占比）；
 * Panel → PanelFrame（标签组头部 + 视图承载）。布局操作经 uiStateStore（走既有 debounce 持久化链路）。
 *
 * 分割/合并入口：分割在面板 ≡ 菜单，面板清理走「关闭标签 → 空面板 → 删除面板」；
 * 边 = 纯 resize 手柄（指针拖拽 + 方向键）。
 *
 * 跨窗口拖拽：面板 DOM 标注 data-drop-panel，panelStore 拖拽会话按 getBoundingClientRect 命中；
 * 本组件渲染 drop 指示器 overlay（中部 = 加标签 / 四边缘 = 分割）。
 *
 * 性能：拖拽只改本 Split 的本地占比，不在拖拽帧内重建布局树；占比经防抖 IPC 回写（Rust 为最终权威）。
 * PanelFrame/PanelTabBar 以 memo 收敛无关重渲染。
 */
import {
  Fragment,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { usePanelStore } from "@/stores/panelStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { collectPanels, collectAllViews, findPanel } from "@/utils/workspaceLayout";
import { notifyViewRemoved } from "@/utils/kernelLifecycle";
import { PanelFrame } from "@/components/layout/PanelFrame";
import { DragGhost } from "@/components/layout/DragGhost";
import type { LayoutNode, SplitNode } from "@/types";

/** 分割子项最小占比（%）：拖拽与方向键钳制用，两侧仍守恒。 */
const MIN_SPLIT_SIZE = 12;

/** 方向键单次调整步长（%）。 */
const KEYBOARD_STEP = 2;

/**
 * 拖拽重分配：手柄左侧（index-1）与右侧（index）按 delta 平移，各自不小于 minSize，
 * 两侧之和守恒，其余子项不变。返回新数组（不改原数组）。
 * 两侧之和不足以容纳双下限时对半分（持久化占比来自外部，可能不含下限约束）。
 */
export function applySplitDrag(
  sizes: number[],
  index: number,
  delta: number,
  minSize: number = MIN_SPLIT_SIZE,
): number[] {
  const next = [...sizes];
  const pairSum = sizes[index - 1] + sizes[index];
  if (pairSum <= minSize * 2) {
    next[index - 1] = pairSum / 2;
    next[index] = pairSum / 2;
    return next;
  }
  let left = sizes[index - 1] + delta;
  if (left < minSize) left = minSize;
  else if (left > pairSum - minSize) left = pairSum - minSize;
  next[index - 1] = left;
  next[index] = pairSum - left;
  return next;
}

/** 递归渲染节点。结构签名变化（children 增删）即换 key 重挂分割容器，占比随之重建。 */
function GridNode({
  node,
  onFocus,
  usedKey,
  panelCount,
}: {
  node: LayoutNode;
  onFocus: (id: string) => void;
  usedKey: string;
  panelCount: number;
}) {
  if (node.kind === "panel") {
    return <PanelFrame node={node} onFocus={onFocus} usedKey={usedKey} panelCount={panelCount} />;
  }
  const splitKey = `${node.id}|${node.children.map((c) => c.id).join(",")}`;
  return (
    <SplitView key={splitKey} node={node} onFocus={onFocus} usedKey={usedKey} panelCount={panelCount} />
  );
}

function SplitView({
  node,
  onFocus,
  usedKey,
  panelCount,
}: {
  node: SplitNode;
  onFocus: (id: string) => void;
  usedKey: string;
  panelCount: number;
}) {
  const setLayoutSizes = useUiStateStore((s) => s.setLayoutSizes);
  const containerRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ index: number; startPos: number; total: number; startSizes: number[] } | null>(null);
  // 尺寸非受控：以挂载时的树上占比为初值，拖拽期间只改本地（不回读树防抖动）；
  // 结构变化由 GridNode 换 key 重挂，占比按新的 children 重建。
  const [sizes, setSizes] = useState(node.sizes);
  const horizontal = node.direction === "horizontal";

  const commit = (next: number[]) => {
    setSizes(next);
    // 与既有链路一致：拖拽期间经防抖提交，Rust 仍是最终权威
    setLayoutSizes(node.id, next);
  };

  const handlePointerDown = (e: ReactPointerEvent<HTMLDivElement>, index: number) => {
    const container = containerRef.current;
    if (!container) return;
    const total = horizontal ? container.clientWidth : container.clientHeight;
    if (total <= 0) return;
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = {
      index,
      startPos: horizontal ? e.clientX : e.clientY,
      total,
      startSizes: sizes,
    };
  };

  const handlePointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const delta = (((horizontal ? e.clientX : e.clientY) - drag.startPos) / drag.total) * 100;
    commit(applySplitDrag(drag.startSizes, drag.index, delta));
  };

  const handlePointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    if (e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
  };

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>, index: number) => {
    let delta = 0;
    if (horizontal) {
      if (e.key === "ArrowLeft") delta = -KEYBOARD_STEP;
      else if (e.key === "ArrowRight") delta = KEYBOARD_STEP;
    } else if (e.key === "ArrowUp") delta = -KEYBOARD_STEP;
    else if (e.key === "ArrowDown") delta = KEYBOARD_STEP;
    if (delta === 0) return;
    e.preventDefault();
    commit(applySplitDrag(sizes, index, delta));
  };

  return (
    <div
      ref={containerRef}
      className="h-full w-full"
      style={{ display: "flex", flexDirection: horizontal ? "row" : "column" }}
    >
      {node.children.map((child, i) => {
        // 手柄位置 = 其前所有子项占比之和（多叉时不等同于左邻居占比）
        const beforeSum = sizes.slice(0, i).reduce((sum, s) => sum + s, 0);
        return (
          <Fragment key={child.id}>
            {/* 边 = 纯 resize 手柄（分割在面板 ≡ 菜单）；方向化样式（竖边 w / 横边 h + hover/focus 强调）见 styles/index.css */}
            {i > 0 && (
              <div
                role="separator"
                tabIndex={0}
                aria-orientation={horizontal ? "vertical" : "horizontal"}
                aria-label="调整面板大小"
                aria-valuenow={Math.round(beforeSum)}
                aria-valuemin={0}
                aria-valuemax={100}
                className="split-handle"
                data-direction={node.direction}
                onPointerDown={(e) => handlePointerDown(e, i)}
                onPointerMove={handlePointerMove}
                onPointerUp={handlePointerUp}
                onPointerCancel={handlePointerUp}
                onKeyDown={(e) => handleKeyDown(e, i)}
              />
            )}
            <div
              className="min-w-0 min-h-0 overflow-hidden"
              style={{ flexGrow: sizes[i], flexShrink: 0, flexBasis: 0 }}
            >
              <GridNode node={child} onFocus={onFocus} usedKey={usedKey} panelCount={panelCount} />
            </div>
          </Fragment>
        );
      })}
    </div>
  );
}

/** 跨窗口拖拽 drop 指示器：命中面板中部 = 加标签高亮整块；四边缘 = 分割带。 */
function DropIndicatorOverlay() {
  const dropTarget = usePanelStore((s) => s.dropTarget);
  if (!dropTarget || dropTarget.window !== "main" || !dropTarget.panelId) {
    return null;
  }
  const el = document.querySelector(`[data-drop-panel="${dropTarget.panelId}"]`);
  if (!el) return null;
  const r = el.getBoundingClientRect();
  const zone = dropTarget.zone;
  const base: CSSProperties = {
    position: "fixed",
    pointerEvents: "none",
    zIndex: 40,
    borderRadius: 4,
    background: "color-mix(in srgb, var(--accent) 22%, transparent)",
    outline: "1px solid color-mix(in srgb, var(--accent) 60%, transparent)",
  };
  // tab 区 = 标签排序：指示由 PanelTabBar 的插入条承担，整块高亮会盖住它（只显示面板块的假象）
  if (zone === "tab") return null;
  let style: CSSProperties;
  if (zone === "center") {
    style = { ...base, left: r.left, top: r.top, width: r.width, height: r.height };
  } else if (zone === "left") {
    style = { ...base, left: r.left, top: r.top, width: r.width * 0.12, height: r.height };
  } else if (zone === "right") {
    style = { ...base, left: r.right - r.width * 0.12, top: r.top, width: r.width * 0.12, height: r.height };
  } else if (zone === "top") {
    style = { ...base, left: r.left, top: r.top, width: r.width, height: r.height * 0.12 };
  } else {
    style = { ...base, left: r.left, top: r.bottom - r.height * 0.12, width: r.width, height: r.height * 0.12 };
  }
  return <div style={style} />;
}

/** 工作区面板网格根：聚焦兜底 + 派发渲染。 */
export function WorkspaceGrid({ tree }: { tree: LayoutNode }) {
  const focusedPanelId = useUiStateStore((s) => s.focusedPanelId);
  const setFocusedPanel = useUiStateStore((s) => s.setFocusedPanel);
  const detachedWindows = useUiStateStore((s) => s.detachedWindows);

  // 聚焦兜底：聚焦面板被关闭/布局切换后失效 → 聚焦第一个面板
  useEffect(() => {
    if (!focusedPanelId || !findPanel(tree, focusedPanelId)) {
      const first = collectPanels(tree)[0];
      if (first) setFocusedPanel(first.id);
    }
  }, [focusedPanelId, tree, setFocusedPanel]);

  const panels = collectPanels(tree);
  // 稳定键（全局已占用视图集合的排序拼接，含撕裂窗口）：resize 拖拽时不变，PanelFrame memo 可跳过
  const usedKey = [...new Set(collectAllViews(tree, detachedWindows))].sort().join(",");

  // 布局中无画布面板时清属性面板选中（画布未渲染，InspectorPanel 的 setCenter 定位无实例）；
  // 经生命周期注册表分发（builtin.canvas 的 onViewRemoved 钩子）
  const hasCanvas = panels.some((p) => p.tabs.some((t) => t.view === "canvas"));
  useEffect(() => {
    if (!hasCanvas) notifyViewRemoved("canvas");
  }, [hasCanvas]);

  return (
    // 不画底色：与 html/body 的底色同值（面板间隙透出的就是同一色），重画会挡住半透明皮肤的氛围底
    <div className="h-full w-full">
      <GridNode
        node={tree}
        onFocus={setFocusedPanel}
        usedKey={usedKey}
        panelCount={panels.length}
      />
      <DropIndicatorOverlay />
      <DragGhost />
    </div>
  );
}
