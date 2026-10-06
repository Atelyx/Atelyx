/**
 * 图片单元格·轮播模式（ImageCell 分派壳的拆分件）：主图滑动翻页 + 底部缩略图队列长按排序。
 *
 * 两条手势管线的实现约束见各自锚点注释；视觉像素与布局像素的换算见 zoomOf。
 */
import { memo, useEffect, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";
import {
  IMAGE_LONG_PRESS_MS,
  IMAGE_PRESS_CANCEL_PX,
  IMAGE_QUEUE_GAP,
  IMAGE_QUEUE_STRIP_HEIGHT,
  IMAGE_QUEUE_THUMB_SIZE,
  IMAGE_SWIPE_EDGE_DAMPING,
  IMAGE_SWIPE_THRESHOLD,
} from "@/constants/table";
import { useTableStore } from "@/stores/tableStore";
import { IconButton } from "@/components/common/Button";

/** 队列槽位步进 / 水平内衬（布局 px，与队列条样式保持一致）。 */
const QUEUE_STEP = IMAGE_QUEUE_THUMB_SIZE + IMAGE_QUEUE_GAP;
const QUEUE_PAD_X = 4;

/** 元素实际缩放（表格 Ctrl+滚轮 CSS zoom）：指针位移是视觉像素，transform/布局是布局像素。 */
function zoomOf(el: HTMLElement): number {
  const z = parseFloat(getComputedStyle(el).zoom);
  return Number.isFinite(z) && z > 0 ? z : 1;
}

interface Props {
  images: string[];
  /** 当前图下标（ImageCell 持有）。 */
  cur: number;
  /** 当前图切换（翻页提交 / 拖拽排序后跟随自身新位置）。 */
  onCurChange: (i: number) => void;
  /** 打开放大预览（ImageCell 持有解析与弹层）。 */
  onOpenAt: (i: number) => void;
  /** 预载结果：条目路径 → dataURL（以路径为键，重排后零重解析零错帧）。 */
  srcMap: Map<string, string>;
  /** 当前图即时解析（未及预载时兜底）。 */
  currentSrc: string | null;
  rowId: string;
  fieldId: string;
}

export const ImageCarouselMode = memo(function ImageCarouselMode({
  images,
  cur,
  onCurChange,
  onOpenAt,
  srcMap,
  currentSrc,
  rowId,
  fieldId,
}: Props) {
  const multi = images.length > 1;
  // 轮播视图下标（浮点：跟手期间介于两图之间；静止时恒为整数）
  const [view, setView] = useState(cur);
  const [dragging, setDragging] = useState(false);
  const suppressClickRef = useRef(false);

  // 当前图被外部变更（协作增删/撤销/lightbox 联动）时，视图下标跟随回正（跟手期间不打断）
  useEffect(() => {
    if (!dragging) setView((v) => (Math.round(v) === cur ? v : cur));
  }, [cur, dragging]);

  // 队列当前项保持可见（自算 scrollLeft，不惊动外层表格滚动容器）
  const queueRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = queueRef.current;
    const child = el?.children[cur];
    if (!el || !(child instanceof HTMLElement)) return;
    const start = child.offsetLeft;
    const end = start + child.offsetWidth;
    if (start < el.scrollLeft) el.scrollLeft = start;
    else if (end > el.scrollLeft + el.clientWidth) el.scrollLeft = end - el.clientWidth;
  }, [cur]);

  // ===== 主图滑动手势 =====
  const dragRef = useRef<{ startX: number; startY: number; active: boolean; dx: number; visualW: number } | null>(null);

  const onViewportPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    suppressClickRef.current = false;
    if (e.button !== 0 || !multi) return;
    dragRef.current = { startX: e.clientX, startY: e.clientY, active: false, dx: 0, visualW: 0 };
  };
  const onViewportPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const dx = e.clientX - d.startX;
    const dy = e.clientY - d.startY;
    if (!d.active) {
      if (Math.abs(dx) <= 5 && Math.abs(dy) <= 5) return;
      // 纵向占优手势不进入滑动（放弃本次按压，让 td 层语义照常）
      if (Math.abs(dx) <= Math.abs(dy)) {
        dragRef.current = null;
        return;
      }
      d.active = true;
      setDragging(true);
      // 视觉宽度激活时读一次缓存（zoom/宽度手势期间恒定）
      d.visualW = e.currentTarget.clientWidth * zoomOf(e.currentTarget);
      e.currentTarget.setPointerCapture(e.pointerId);
    }
    d.dx = dx;
    const atEdge = (dx > 0 && cur === 0) || (dx < 0 && cur === images.length - 1);
    const eff = atEdge ? dx * IMAGE_SWIPE_EDGE_DAMPING : dx;
    // 钳制单次手势最多位移一页：渲染层只画 cur±1，超范围会让当前图离场出现瞬间空白
    const target = cur - eff / d.visualW;
    setView(Math.max(cur - 1, Math.min(cur + 1, target)));
  };
  const onViewportPointerUp = () => {
    const d = dragRef.current;
    dragRef.current = null;
    if (!d?.active) return;
    setDragging(false);
    suppressClickRef.current = true;
    const passed = Math.abs(d.dx) >= Math.max(IMAGE_SWIPE_THRESHOLD, d.visualW / 5);
    if (passed && d.dx < 0 && cur < images.length - 1) {
      onCurChange(cur + 1);
      setView(cur + 1);
    } else if (passed && d.dx > 0 && cur > 0) {
      onCurChange(cur - 1);
      setView(cur - 1);
    } else {
      setView(cur);
    }
  };
  /** 系统打断手势（滚动手势接管等）：不提交翻页，直接回弹。 */
  const onViewportPointerCancel = () => {
    const d = dragRef.current;
    dragRef.current = null;
    if (!d?.active) return;
    setDragging(false);
    setView(cur);
  };

  // ===== 队列长按拖动排序 =====
  const [qDrag, setQDrag] = useState<{ orig: number; order: number[] } | null>(null);
  const qDxRef = useRef(0);
  const pressRef = useRef<{ orig: number; timer: number; startX: number; startY: number; pointerId: number; el: HTMLButtonElement } | null>(null);
  const qDragRef = useRef<{
    orig: number;
    order: number[];
    startX: number;
    zoom: number;
    pointerId: number;
    el: HTMLButtonElement;
    curAtStart: number;
  } | null>(null);

  const onThumbPointerDown = (e: ReactPointerEvent<HTMLButtonElement>, orig: number) => {
    // 手势互斥：拖拽/按压进行中忽略新按压（他指/第二输入源）
    if (e.button !== 0 || images.length < 2 || pressRef.current || qDragRef.current) return;
    suppressClickRef.current = false;
    e.preventDefault(); // 阻断文本选区与原生拖拽意图（同表格行拖拽惯例）
    const timer = window.setTimeout(() => {
      const p = pressRef.current;
      const strip = queueRef.current;
      if (!p || !strip) return;
      pressRef.current = null; // 激活即清按压（防他指事件走取消分支）
      qDragRef.current = {
        orig,
        order: images.map((_, i) => i),
        startX: p.startX,
        zoom: zoomOf(strip),
        pointerId: p.pointerId,
        el: p.el,
        curAtStart: cur,
      };
      qDxRef.current = 0;
      setQDrag({ orig, order: images.map((_, i) => i) });
    }, IMAGE_LONG_PRESS_MS);
    pressRef.current = { orig, timer, startX: e.clientX, startY: e.clientY, pointerId: e.pointerId, el: e.currentTarget };
  };

  // document 级 move/up/cancel：拖拽全程、按压取消与「松手早于长按」的清理都在此路由
  // （onCurChange 经 useCallback 稳定，监听器不重挂）。
  // 走 document 而非元素级监听：被拖元素激活后会被 React 原地改写、事件属性剥离，元素级监听会断流。
  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      // —— 队列拖拽（按 pointerId 过滤他指事件）——
      const q = qDragRef.current;
      if (q) {
        if (e.pointerId !== q.pointerId) return;
        // 跟手位移直接写 DOM（零 React 渲染）；ref 供槽位切换重渲染读取，位置不回跳
        qDxRef.current = (e.clientX - q.startX) / q.zoom;
        q.el.style.transform = `translateX(${qDxRef.current}px) scale(1.12)`;
        // 命中测试统一布局像素口径：clientX-rect.left 是视觉像素须除回 zoom，scrollLeft/内衬本就是布局像素
        const strip = queueRef.current;
        if (!strip) return;
        const contentX = (e.clientX - strip.getBoundingClientRect().left) / q.zoom - QUEUE_PAD_X + strip.scrollLeft;
        const slot = Math.max(0, Math.min(q.order.length - 1, Math.floor(contentX / QUEUE_STEP)));
        const from = q.order.indexOf(q.orig);
        if (slot !== from) {
          const order = [...q.order];
          order.splice(from, 1);
          order.splice(slot, 0, q.orig);
          qDragRef.current = { ...q, order };
          setQDrag({ orig: q.orig, order });
        }
        return;
      }
      // —— 长按按压中：超位移取消（且松手不触发跳转 click）——
      const p = pressRef.current;
      if (p && e.pointerId === p.pointerId) {
        if (Math.hypot(e.clientX - p.startX, e.clientY - p.startY) > IMAGE_PRESS_CANCEL_PX) {
          clearTimeout(p.timer);
          pressRef.current = null;
          suppressClickRef.current = true;
        }
      }
    };
    const onUp = (e: PointerEvent) => {
      // 松手早于长按定时器：清按压（否则定时器会在无按键状态下幽灵激活拖拽）
      const p = pressRef.current;
      if (p && e.pointerId === p.pointerId) {
        clearTimeout(p.timer);
        pressRef.current = null;
      }
      const q = qDragRef.current;
      if (!q || e.pointerId !== q.pointerId) return;
      qDragRef.current = null;
      setQDrag(null);
      if (q.order.some((v, i) => v !== i)) {
        useTableStore.getState().reorderImages(rowId, fieldId, q.order);
        onCurChange(q.order.indexOf(q.curAtStart)); // 当前图跟随自身新位置（slide 以条目为 key，无内容跳变）
      }
    };
    const onCancel = (e: PointerEvent) => {
      const p = pressRef.current;
      if (p && e.pointerId === p.pointerId) {
        clearTimeout(p.timer);
        pressRef.current = null;
      }
      const q = qDragRef.current;
      if (!q || e.pointerId !== q.pointerId) return;
      qDragRef.current = null;
      setQDrag(null);
    };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onCancel);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      qDragRef.current = null; // 卸载兜底：拖拽态遗留会让位预览卡住
      if (pressRef.current) clearTimeout(pressRef.current.timer);
      pressRef.current = null;
    };
  }, [onCurChange, rowId, fieldId]);

  const slideTransition = dragging
    ? "none"
    : "transform var(--dur-base) var(--ease)";
  // 只渲染 cur±1（配合上面的一页钳制）；key 取图片条目，重排/切换内容时复用同一 slide 无跳变
  const slides: ReactNode[] = [];
  {
    const center = Math.round(view);
    for (let i = Math.max(0, center - 1); i <= Math.min(images.length - 1, center + 1); i++) {
      const src = i === cur ? (srcMap.get(images[i]) ?? currentSrc) : (srcMap.get(images[i]) ?? null);
      slides.push(
        <div
          key={images[i]}
          className="absolute inset-0 flex items-center justify-center p-1"
          style={{ transform: `translateX(${(i - view) * 100}%)`, transition: slideTransition }}
        >
          {src ? (
            <img
              src={src}
              alt={`${i + 1}`}
              className="max-h-full max-w-full object-contain rounded select-none"
              draggable={false}
            />
          ) : (
            // 路径条目首次读取中/失败：占位（不显示破图图标）
            <div className="h-full w-full rounded bg-[var(--hover)]" />
          )}
        </div>,
      );
    }
  }

  return (
    <>
      <div
        className="relative flex-1 min-h-0 overflow-hidden cursor-pointer"
        style={{ touchAction: "none" }}
        onPointerDown={onViewportPointerDown}
        onPointerMove={onViewportPointerMove}
        onPointerUp={onViewportPointerUp}
        onPointerCancel={onViewportPointerCancel}
        onClick={() => {
          if (suppressClickRef.current) {
            suppressClickRef.current = false;
            return;
          }
          onOpenAt(cur);
        }}
      >
        {slides}
      </div>
      {/* 底部迷你缩略图条：当前图强调色描边、点击跳转、溢出横向滚动、当前项跟随可见；
          长按小图拖动排序见 onThumbPointerDown。拖拽期间队列渲染顺序恒等、只由 transform 让位——
          DOM 重排叠加 transform 会双重位移并让被拖项脱手 */}
      {multi && (
        <div
          ref={queueRef}
          className="relative flex-shrink-0 flex items-center overflow-x-auto no-horizontal-scrollbar px-1"
          style={{ height: IMAGE_QUEUE_STRIP_HEIGHT, gap: IMAGE_QUEUE_GAP }}
        >
          {images.map((entry, j) => {
            const isDragged = qDrag?.orig === j;
            const slot = qDrag ? qDrag.order.indexOf(j) : j;
            const shift = qDrag && !isDragged ? (slot - j) * QUEUE_STEP : 0;
            return (
              <IconButton
                key={entry}
                onPointerDown={(e) => onThumbPointerDown(e, j)}
                onClick={(e) => {
                  e.stopPropagation();
                  if (suppressClickRef.current) {
                    suppressClickRef.current = false;
                    return;
                  }
                  onCurChange(j);
                }}
                // 缩略图本体即图片（`IconButton` 不接受 children，图片走 icon 通道）
                icon={
                  srcMap.get(entry) ? (
                    <img
                      src={srcMap.get(entry)}
                      alt=""
                      className="w-full h-full object-cover pointer-events-none"
                      draggable={false}
                    />
                  ) : (
                    <div className="w-full h-full bg-[var(--hover)]" />
                  )
                }
                label={`第 ${slot + 1} 张`}
                size="md"
                // 选 subtle 而非 ghost：原按钮无 hover 底色，subtle 的 hover 只改文字色（此处无文字）
                variant="subtle"
                // 圆角保留：`IconButton` 不带圆角档，这里的圆角是裁图片四角（形状，非控件尺寸）
                className="rounded-[var(--radius-sm)] overflow-hidden border-2 cursor-pointer"
                style={{
                  width: IMAGE_QUEUE_THUMB_SIZE,
                  height: IMAGE_QUEUE_THUMB_SIZE,
                  borderColor: j === cur ? "var(--accent)" : "transparent",
                  opacity: isDragged ? 1 : qDrag ? 0.45 : j === cur ? 1 : 0.55,
                  transform: isDragged
                    ? `translateX(${qDxRef.current}px) scale(1.12)`
                    : shift
                      ? `translateX(${shift}px)`
                      : undefined,
                  transition: isDragged
                    ? "none"
                    : "transform var(--dur-base) var(--ease)",
                  position: "relative",
                  zIndex: isDragged ? 10 : undefined,
                  boxShadow: isDragged ? "var(--shadow-pop)" : undefined,
                }}
              />
            );
          })}
        </div>
      )}
    </>
  );
});
