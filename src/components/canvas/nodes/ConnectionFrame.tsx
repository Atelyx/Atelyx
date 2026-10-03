import { useEffect, useRef, useState } from "react";
import { Handle, Position } from "@xyflow/react";

/**
 * 节点连接边框：四边透明长条拉线区 + 按需渐显的连接圆点与选中发光。
 * 四边各重叠两个透明长条 handle（source + target，上层可交互）——
 * 鼠标可从节点边缘任意位置拉线连接到另一个节点的边缘。
 *
 * 视觉按需渐显（CSS opacity 过渡），常态不显示任何指示、保持画布干净：
 * - 连接圆点 = 每边一个（React Flow 默认 handle 样式的圆点），外缘 = 条带外缘
 *   （= 连线锚点），圆心落在节点边框内缘（随边框宽 1~1.5px 内缩，视觉即贴在
 *   边缘上）；鼠标移到对应边的条带时该边圆点渐显、移开淡出；
 * - 选中反馈 = 节点边缘阴影发光（透明层贴合节点边缘，仅外阴影向外发光，
 *   圆角继承节点根元素），不改节点边框。
 *
 * 锚点几何（对齐 React Flow 规则，关键）：最终连线端点取 handle 外缘，
 * 拖拽预览线与落点吸附取 handle 中心——条带以节点边缘为轴、外 6/内 4
 * （padding 盒口径，外缘 = 圆点外缘），连线端点止于圆点外缘、
 * 预览线起点 ≈ 节点边缘；条带越厚锚点越远浮，故保持薄。
 *
 * 层序：topType 指定的类型渲染在上层（点击/释放优先命中）：
 * - 产出方（文本/媒体）：source 在上 → 从边缘拉出
 * - 消费方（对话）：target 在上 → 拉线接入（也从边缘拉出到产出方，方向仍为产出→消费）
 * connectionMode 为 Loose：任意 handle 组合可连接，连线语义由两端节点类型自动分类（isValidConnection 兜底）。
 */

/** 条带越出节点边缘的深度（px）：条带外缘 = 拉线锚点 = 圆点外缘 */
const STRIP_OUT = 6;
/** 条带越入节点边缘的深度（px）：与越出量共同构成连接命中区；
 * 越入段覆盖内容最外约 4px 窄带（带内点击会被拉线命中截获，主体交互不受影响） */
const STRIP_IN = 4;
/** 条带厚度 = 命中区总深度 */
const STRIP = STRIP_OUT + STRIP_IN;
/** 连接圆点直径（px）：默认 handle 圆点（6px）偏小，放大到两倍保证可见性 */
const DOT_SIZE = 12;
/** 条带离开延迟淡出（ms）：跨边移动/短暂抖动时不闪烁，重入即取消 */
const LEAVE_DELAY_MS = 120;

interface Props {
  /** 上层 handle 类型：产出方 source / 消费方 target */
  topType: "source" | "target";
  /** 节点选中时边缘阴影发光 */
  selected?: boolean;
}

/** 仅覆盖 React Flow 默认 handle 自带的居中 translate（外观重置由 .conn-strip 类承担） */
const STRIP_STYLE = { transform: "none" } as const;

/** 四边的条状定位（覆盖 React Flow 默认圆点样式，拉伸成整条边）。
 * 条带以节点边缘为轴跨内外两侧（外 STRIP_OUT / 内 STRIP_IN），外缘 = 拉线锚点。 */
function stripStyle(position: Position): React.CSSProperties {
  switch (position) {
    case Position.Top:
      return {
        ...STRIP_STYLE,
        top: -STRIP_OUT,
        left: 0,
        width: "100%",
        height: STRIP,
      };
    case Position.Bottom:
      return {
        ...STRIP_STYLE,
        bottom: -STRIP_OUT,
        left: 0,
        width: "100%",
        height: STRIP,
      };
    case Position.Left:
      return {
        ...STRIP_STYLE,
        left: -STRIP_OUT,
        top: 0,
        width: STRIP,
        height: "100%",
      };
    case Position.Right:
      return {
        ...STRIP_STYLE,
        right: -STRIP_OUT,
        top: 0,
        width: STRIP,
        height: "100%",
      };
  }
}

/** 圆点定位：外缘 = 条带外缘（= 连线锚点），圆心落在节点边框内缘。
 * top/left 以元素顶/左边缘为基准，负向偏移半个直径即圆心落在边缘基准上，
 * 只需单轴 translate 居中；bottom/right 以底/右边缘为基准，同理只偏移水平/垂直单轴
 * （不可用 translate(-50%, -50%)——它恒向左上偏，会把下方/右侧圆点推进节点内部）。 */
function dotStyle(position: Position): React.CSSProperties {
  const half = DOT_SIZE / 2;
  switch (position) {
    case Position.Top:
      return { top: -half, left: "50%", transform: "translateX(-50%)" };
    case Position.Bottom:
      return { bottom: -half, left: "50%", transform: "translateX(-50%)" };
    case Position.Left:
      return { left: -half, top: "50%", transform: "translateY(-50%)" };
    case Position.Right:
      return { right: -half, top: "50%", transform: "translateY(-50%)" };
  }
}

/** 圆点外观 = React Flow 默认 handle（accent 圆点 + 主题描边色） */
const DOT_BASE = {
  width: DOT_SIZE,
  height: DOT_SIZE,
  borderRadius: "50%",
  background: "var(--accent)",
  border: "1px solid var(--xy-handle-border-color-default)",
} as const;

/** 四边遍历顺序 */
const POSITIONS = [
  Position.Top,
  Position.Bottom,
  Position.Left,
  Position.Right,
];

export function ConnectionFrame({ topType, selected }: Props) {
  const bottomType = topType === "source" ? "target" : "source";

  const [hoverSide, setHoverSide] = useState<Position | null>(null);
  const leaveTimerRef = useRef(0);
  useEffect(() => () => window.clearTimeout(leaveTimerRef.current), []);

  const handleEnter = (side: Position) => {
    window.clearTimeout(leaveTimerRef.current);
    setHoverSide(side);
  };
  const handleLeave = () => {
    window.clearTimeout(leaveTimerRef.current);
    leaveTimerRef.current = window.setTimeout(
      () => setHoverSide(null),
      LEAVE_DELAY_MS,
    );
  };

  return (
    <>
      {/* 选中发光：透明层贴合节点边缘（圆角继承节点根元素），仅外阴影向外发光 */}
      <div
        aria-hidden
        className="pointer-events-none absolute"
        style={{
          inset: 0,
          borderRadius: "inherit",
          boxShadow: selected
            ? "0 0 10px color-mix(in srgb, var(--accent) 55%, transparent)"
            : "0 0 0 transparent",
          transition: `box-shadow var(--dur-base) var(--ease)`,
        }}
      />
      {/* 连接圆点（外缘 = 连线锚点，圆心贴节点边缘）：鼠标移到对应边条带时渐显 */}
      {POSITIONS.map((p) => (
        <div
          key={p}
          aria-hidden
          className="pointer-events-none absolute"
          style={{
            ...DOT_BASE,
            ...dotStyle(p),
            opacity: hoverSide === p ? 1 : 0,
            transition: `opacity var(--dur-base) var(--ease)`,
          }}
        />
      ))}
      {/* 下层 handle：与上层几何重合、DOM 在前恒被覆盖（事件恒由上层接），
          仅作为另一种 handle 类型存在——两类 handle 的 bounds 都是连线锚点查找所需 */}
      {POSITIONS.map((p) => (
        <Handle
          key={`${p}-${bottomType}`}
          type={bottomType}
          position={p}
          id={`${p}-${bottomType}`}
          className="conn-strip"
          style={stripStyle(p)}
        />
      ))}
      {/* 上层 handle：点击/释放优先命中 */}
      {POSITIONS.map((p) => (
        <Handle
          key={`${p}-${topType}`}
          type={topType}
          position={p}
          id={`${p}-${topType}`}
          className="conn-strip"
          style={stripStyle(p)}
          onMouseEnter={() => handleEnter(p)}
          onMouseLeave={handleLeave}
        />
      ))}
    </>
  );
}
