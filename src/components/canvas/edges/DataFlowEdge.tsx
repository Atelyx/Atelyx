import {
  BaseEdge,
  EdgeLabelRenderer,
  getBezierPath,
  type EdgeProps,
} from "@xyflow/react";
import { ArrowRight, ArrowRightLeft, Minus } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useCanvasStore } from "@/stores/canvasStore";
import { IconButton } from "@/components/common/Button";
import { isAssetConsumed } from "@/utils/consumed";
import type { LinkMode } from "@/types";

/**
 * 画布边（按类型自动分类，见 2.4）：
 * - **数据流边**（有向）：金色实线/虚线 + 终点金色箭头 + 中点圆点；
 *   虚实反映消费状态（资产引用边 text/media/search → conversation）
 * - **关联边**（directed: false）：灰色自由线，无中点圆点；中点常显模式切换圆钮
 *   （无向 / 单向 / 双向，`linkMode` 循环切换）；可选中 Delete 单独删除
 *
 * 箭头用组件内 `<defs>` + url 引用（BaseEdge 的 markerEnd 只接受字符串引用），
 * id 取边 id 保证唯一。
 */

/** 数据流箭头色（金色，跟随主题 --accent）与关联箭头色（灰，与关联边描边同源变量）。 */
const GOLD = "var(--accent)";
const GRAY = "var(--xy-edge-stroke-default)";

/** 箭头 defs（orient auto-start-reverse：marker-start 端点自动反向，供双向箭头复用）。 */
function ArrowDef({ id, color }: { id: string; color: string }) {
  return (
    <defs>
      <marker
        id={id}
        viewBox="0 0 10 10"
        refX="8"
        refY="5"
        markerWidth="7"
        markerHeight="7"
        orient="auto-start-reverse"
      >
        <path d="M 0 0 L 10 5 L 0 10 z" fill={color} />
      </marker>
    </defs>
  );
}

/** 关联边模式循环：无向 → 单向 → 双向 → 无向。 */
function cycleLinkMode(m: LinkMode): LinkMode {
  return m === "none" ? "single" : m === "single" ? "double" : "none";
}

function linkModeIcon(m: LinkMode) {
  return m === "single" ? <ArrowRight size={10} /> : m === "double" ? <ArrowRightLeft size={10} /> : <Minus size={10} />;
}

export function DataFlowEdge({
  id,
  source,
  target,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  selected,
}: EdgeProps) {
  // 只订阅「本边的虚实判定」派生（useShallow 逐字段比较：节点移动/流式增量不触发重渲染）
  const { dashed, isLink, linkMode } = useCanvasStore(
    useShallow((s) => {
      const edge = s.edges.find((e) => e.id === id);
      // 关联边（directed:false）：恒实线、无消费语义
      if (edge?.directed === false) {
        return { dashed: false, isLink: true, linkMode: edge.linkMode ?? "none" };
      }
      const src = s.nodes.find((n) => n.id === source);
      const tgt = s.nodes.find((n) => n.id === target);
      if (!src || !tgt) return { dashed: false, isLink: false, linkMode: "none" as LinkMode };
      // 数据流资产引用边：虚线 = 未消费（待注入）；已消费（历史 refs/attachments 含 source）→ 实线
      const isAssetConsumption =
        (src.type === "text" || src.type === "media" || src.type === "search") &&
        tgt.type === "conversation";
      if (isAssetConsumption) {
        const injected = isAssetConsumed(s.messagesByConv[target] ?? [], source);
        return { dashed: !injected, isLink: false, linkMode: "none" as LinkMode };
      }
      // 数据流产出边（对话 → 资产）恒实线
      return { dashed: false, isLink: false, linkMode: "none" as LinkMode };
    })
  );
  const readOnly = useCanvasStore((s) => s.readOnly);
  const setEdgeLinkMode = useCanvasStore((s) => s.setEdgeLinkMode);

  const [edgePath, labelX, labelY] = getBezierPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
  });
  // 数据流边恒金色（选中加粗）；关联边灰（选中金色）
  const stroke = isLink ? (selected ? GOLD : GRAY) : GOLD;
  // 箭头 url 引用（选中一律金色）：数据流 = 金色终点箭头；关联按 linkMode（无向无箭头 / 单向终点 / 双向两端）
  const arrowId = selected || !isLink ? `gold-${id}` : `gray-${id}`;
  const markerEnd =
    !isLink || linkMode === "single" || linkMode === "double" ? `url(#${arrowId})` : undefined;
  const markerStart = isLink && linkMode === "double" ? `url(#${arrowId})` : undefined;

  return (
    <>
      <ArrowDef id={`gold-${id}`} color={GOLD} />
      {isLink && <ArrowDef id={`gray-${id}`} color={GRAY} />}
      <BaseEdge
        id={id}
        path={edgePath}
        markerEnd={markerEnd}
        markerStart={markerStart}
        style={{
          stroke,
          strokeWidth: selected ? 2 : 1.5,
          // 虚线 = 未消费的资产引用边（5 5）；其余实线不设 strokeDasharray
          strokeDasharray: dashed ? "5 5" : undefined,
        }}
      />
      <EdgeLabelRenderer>
        <div
          className="nodrag nopan"
          style={{
            position: "absolute",
            transform: `translate(-50%, -50%) translate(${labelX}px,${labelY}px)`,
            pointerEvents: "none",
          }}
        >
          {isLink ? (
            /* 关联边中点：模式切换圆钮常显（低透明度，hover 高亮；无向/单向/双向循环），只读白板隐藏 */
            !readOnly && (
              <IconButton
                variant="secondary"
                size="2xs"
                // 关联边中点是直径 18px 的正圆，超出 2xs 档的 16px 方形，故尺寸与圆角
                // 由本处覆写（`size` 的 w/h/圆角产出均被盖掉，仅取其变体与基元行为）；
                // 基元提供焦点环、悬停提示与透传的事件。
                className="!w-[18px] !h-[18px] !rounded-full !border !border-[var(--border)] opacity-50 hover:opacity-100"
                style={{ pointerEvents: "auto", color: "var(--text-secondary)" }}
                icon={linkModeIcon(linkMode)}
                label={`边模式：${linkMode === "none" ? "无向" : linkMode === "single" ? "单向" : "双向"}（点击切换）`}
                onClick={(e) => {
                  e.stopPropagation();
                  setEdgeLinkMode(id, cycleLinkMode(linkMode));
                }}
              />
            )
          ) : (
            /* 数据流边中点圆点（随边色） */
            <svg width="12" height="12" viewBox="0 0 12 12">
              <circle cx="6" cy="6" r="3" fill={stroke} />
            </svg>
          )}
        </div>
      </EdgeLabelRenderer>
    </>
  );
}
