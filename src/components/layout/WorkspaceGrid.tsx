/**
 * 工作区面板网格：递归二分树渲染。
 *
 * Split → react-resizable-panels PanelGroup（非受控 + defaultSize，拖拽 onLayout 回写 sizes）；
 * Panel → PanelFrame（标签组头部 + 视图承载）。布局操作经 uiStateStore（走既有 debounce 持久化链路）。
 *
 * 分割/合并入口：分割在面板 ≡ 菜单，面板清理走「关闭标签 → 空面板 → 删除面板」；
 * 边 = 纯 resize 手柄。
 *
 * 跨窗口拖拽：面板 DOM 标注 data-drop-panel，panelStore 拖拽会话按 getBoundingClientRect 命中；
 * 本组件渲染 drop 指示器 overlay（中部 = 加标签 / 四边缘 = 分割）。
 *
 * 性能：resize 拖拽每帧更新树 → 本组件重渲染，但 Panel 叶子节点引用稳定
 * （utils 纯函数重建 Split 时保留叶子原引用）+ PanelFrame/PanelTabBar memo，
 * 画布/编辑器在拖边期间不重渲染。
 */
import { Fragment, useEffect, type CSSProperties } from "react";
import { Panel, PanelGroup, PanelResizeHandle } from "react-resizable-panels";
import { usePanelStore } from "@/stores/panelStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { collectPanels, collectAllViews, findPanel } from "@/utils/workspaceLayout";
import { notifyViewRemoved } from "@/utils/kernelLifecycle";
import { PanelFrame } from "@/components/layout/PanelFrame";
import { DragGhost } from "@/components/layout/DragGhost";
import type { LayoutNode, SplitNode } from "@/types";

/** 递归渲染节点。 */
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
  return <SplitView node={node} onFocus={onFocus} usedKey={usedKey} panelCount={panelCount} />;
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
  // 非受控 + defaultSize：sizes 只作为挂载初值与持久化记录（拖拽后内部布局保持，onLayout 回写树）。
  // key=split.id：布局操作（分割/删除面板/切换布局）产生新节点结构时强制重挂，defaultSize 重新生效。
  // id + order 必须稳定且唯一：react-resizable-panels 对动态渲染的面板要求同时提供 id 与 order
  // 才按 id 关联布局状态——缺一会在面板增删重挂后状态错位（拖边方向反/跨面板联动）。
  return (
    <PanelGroup
      key={node.id}
      id={`group-${node.id}`}
      direction={node.direction}
      onLayout={(sizes) => setLayoutSizes(node.id, sizes)}
      className="h-full w-full"
    >
      {node.children.map((child, i) => (
        <Fragment key={child.id}>
          {i > 0 && <SplitHandle key={`handle-${node.id}-${i}`} />}
          <Panel
            id={`panel-${child.id}`}
            order={i}
            defaultSize={node.sizes[i]}
            minSize={12}
            className="min-w-0 min-h-0"
          >
            <GridNode node={child} onFocus={onFocus} usedKey={usedKey} panelCount={panelCount} />
          </Panel>
        </Fragment>
      ))}
    </PanelGroup>
  );
}

/** 边 = 纯 resize 手柄（分割在面板 ≡ 菜单）。
 * 方向化样式（竖边 w / 横边 h + hover 强调）在 styles/index.css 按 data 属性统一处理。 */
function SplitHandle() {
  return <PanelResizeHandle className="flex-shrink-0" />;
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
    <div className="h-full w-full" style={{ background: "var(--bg-primary)" }}>
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
