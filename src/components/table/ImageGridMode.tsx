/**
 * 图片单元格·九宫格模式（ImageCell 分派壳的拆分件）。
 *
 * - 多图平铺方块（object-cover 裁切，gridAutoRows 1fr 填满展示层）：点击开预览对应图；单图限宽方块。
 * - **长按格子拖动排序**：影子 portal 到 body 跟手（CSS zoom 会缩放 fixed 后代，与放大预览
 *   同避法），原位虚线占位，落点格实时让位，松手 reorderImages 写回。
 *
 * 拖拽事件管线与表格行拖拽同款（document 级监听 + ref 路由）：pointerdown 只登记按压 + 长按
 * 定时器，move/up/cancel 由 document 监听处理——被拖元素激活后会被 React 原地改写成占位符
 * （事件属性剥离），元素级监听会事件断流。影子位置 ref + 直写 DOM（pointermove 零 React
 * 渲染，仅槽位切换重渲染）；拖拽期间按 pointerId 过滤他指事件。槽位矩形激活时快照（视觉
 * 像素，槽位位置固定，命中测试全程复用，与 clientX 同口径无需 zoom 换算）。
 */
import { memo, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { PointerEvent as ReactPointerEvent } from "react";
import { IMAGE_LONG_PRESS_MS, IMAGE_PRESS_CANCEL_PX, IMAGE_GRID_GAP, IMAGE_GRID_SINGLE_MAX } from "@/constants/table";
import { useTableStore } from "@/stores/tableStore";

interface Props {
  images: string[];
  /** 当前图下标（ImageCell 持有；拖拽排序提交后跟随自身新位置）。 */
  cur: number;
  onCurChange: (i: number) => void;
  onOpenAt: (i: number) => void;
  /** 预载结果：条目路径 → dataURL（以路径为键，重排后零重解析零错帧）。 */
  srcMap: Map<string, string>;
  /** 列数（ImageCell 按列宽/图数决定，与撑高层估算共用单一来源）。 */
  gridCols: number;
  rowId: string;
  fieldId: string;
}

export const ImageGridMode = memo(function ImageGridMode({
  images,
  cur,
  onCurChange,
  onOpenAt,
  srcMap,
  gridCols,
  rowId,
  fieldId,
}: Props) {
  const [gDrag, setGDrag] = useState<{ orig: number; order: number[]; w: number; h: number; src: string | null } | null>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const shadowRef = useRef<HTMLDivElement>(null);
  const gPosRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  /** 长按按压登记（激活前；超位移取消）。 */
  const pressRef = useRef<{ orig: number; timer: number; startX: number; startY: number; pointerId: number } | null>(null);
  const gDragRef = useRef<{ orig: number; order: number[]; pointerId: number; curAtStart: number } | null>(null);
  // 拖拽激活时各槽位矩形（视觉像素，槽位位置固定不变，命中测试全程复用）
  const gridSlotsRef = useRef<DOMRect[]>([]);
  const suppressClickRef = useRef(false);

  const onTilePointerDown = (e: ReactPointerEvent<HTMLDivElement>, orig: number) => {
    // 手势互斥：拖拽/按压进行中忽略新按压（他指/第二输入源）
    if (e.button !== 0 || images.length < 2 || pressRef.current || gDragRef.current) return;
    suppressClickRef.current = false;
    e.preventDefault(); // 阻断文本选区（同表格行拖拽惯例）
    const timer = window.setTimeout(() => {
      const p = pressRef.current;
      if (!p || !gridRef.current) return;
      pressRef.current = null; // 激活即清按压（防他指事件走取消分支）
      gridSlotsRef.current = Array.from(gridRef.current.children).map((c) =>
        (c as HTMLElement).getBoundingClientRect(),
      );
      const r = gridSlotsRef.current[orig];
      if (!r) return;
      gPosRef.current = { x: p.startX, y: p.startY };
      gDragRef.current = { orig, order: images.map((_, i) => i), pointerId: p.pointerId, curAtStart: cur };
      setGDrag({
        orig,
        order: images.map((_, i) => i),
        w: r.width,
        h: r.height,
        src: srcMap.get(images[orig]) ?? null,
      });
    }, IMAGE_LONG_PRESS_MS);
    pressRef.current = { orig, timer, startX: e.clientX, startY: e.clientY, pointerId: e.pointerId };
  };

  // document 级 move/up/cancel：拖拽全程、按压取消与「松手早于长按」的清理都在此路由
  // （onCurChange 经 useCallback 稳定，监听器不重挂）
  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const g = gDragRef.current;
      if (g) {
        if (e.pointerId !== g.pointerId) return;
        // 影子位置直接写 DOM（零 React 渲染）；ref 供槽位切换重渲染读取，位置不回跳
        gPosRef.current = { x: e.clientX, y: e.clientY };
        const shadow = shadowRef.current;
        if (shadow) {
          shadow.style.left = `${e.clientX}px`;
          shadow.style.top = `${e.clientY}px`;
        }
        // 命中测试（视觉像素）：指针落在哪个槽位格子，被拖图就实时挪到哪（仅切换槽位才重渲染）
        const px = e.clientX;
        const py = e.clientY;
        const slot = gridSlotsRef.current.findIndex(
          (r) => px >= r.left && px <= r.right && py >= r.top && py <= r.bottom,
        );
        const from = g.order.indexOf(g.orig);
        if (slot >= 0 && slot !== from) {
          const order = [...g.order];
          order.splice(from, 1);
          order.splice(slot, 0, g.orig);
          gDragRef.current = { ...g, order };
          setGDrag((prev) => (prev ? { ...prev, order } : prev));
        }
        return;
      }
      // —— 长按按压中：超位移取消（且松手不触发「打开预览」click）——
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
      const g = gDragRef.current;
      if (!g || e.pointerId !== g.pointerId) return;
      gDragRef.current = null;
      gridSlotsRef.current = [];
      setGDrag(null);
      if (g.order.some((v, i) => v !== i)) {
        useTableStore.getState().reorderImages(rowId, fieldId, g.order);
        onCurChange(g.order.indexOf(g.curAtStart)); // 当前图跟随自身新位置
      }
    };
    const onCancel = (e: PointerEvent) => {
      const p = pressRef.current;
      if (p && e.pointerId === p.pointerId) {
        clearTimeout(p.timer);
        pressRef.current = null;
      }
      const g = gDragRef.current;
      if (!g || e.pointerId !== g.pointerId) return;
      gDragRef.current = null;
      gridSlotsRef.current = [];
      setGDrag(null);
    };
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onCancel);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
      gDragRef.current = null; // 卸载兜底：拖拽态遗留会让影子/占位符卡住
      if (pressRef.current) clearTimeout(pressRef.current.timer);
      pressRef.current = null;
    };
  }, [onCurChange, rowId, fieldId]);

  // 拖拽中的展示顺序（长度守卫：拖拽期间协作增删图片则回退恒等顺序，store 侧另有排列校验兜底）
  const displayOrder = gDrag && gDrag.order.length === images.length ? gDrag.order : images.map((_, i) => i);

  return (
    <>
      <div className="flex-1 min-h-0 overflow-hidden p-1">
        <div
          ref={gridRef}
          className="grid h-full"
          style={{
            gap: IMAGE_GRID_GAP,
            gridTemplateColumns: `repeat(${gridCols}, minmax(0, 1fr))`,
            gridAutoRows: "1fr",
          }}
        >
          {displayOrder.map((orig) => {
            const isDragged = gDrag?.orig === orig;
            const src = srcMap.get(images[orig]);
            if (isDragged) {
              return (
                <div
                  key={images[orig]}
                  className="rounded bg-[var(--hover)]"
                  style={{ border: "1px dashed var(--border)" }}
                />
              );
            }
            return (
              <div
                key={images[orig]}
                className="relative rounded overflow-hidden cursor-pointer"
                style={{ maxWidth: images.length === 1 ? IMAGE_GRID_SINGLE_MAX : undefined }}
                onPointerDown={(e) => onTilePointerDown(e, orig)}
                onClick={() => {
                  if (suppressClickRef.current) {
                    suppressClickRef.current = false;
                    return;
                  }
                  onOpenAt(orig);
                }}
              >
                {src ? (
                  <img
                    src={src}
                    alt={`${orig + 1}`}
                    className="absolute inset-0 w-full h-full object-cover select-none"
                    draggable={false}
                  />
                ) : (
                  <div className="absolute inset-0 bg-[var(--hover)]" />
                )}
              </div>
            );
          })}
        </div>
      </div>
      {/* 拖拽影子：portal 到 body（CSS zoom 会缩放 fixed 后代，与放大预览同避法）；位置走 ref + 直接写 DOM */}
      {gDrag &&
        createPortal(
          <div
            ref={shadowRef}
            className="pointer-events-none rounded overflow-hidden shadow-2xl"
            style={{
              position: "fixed",
              left: gPosRef.current.x,
              top: gPosRef.current.y,
              width: gDrag.w,
              height: gDrag.h,
              transform: "translate(-50%, -50%)",
              opacity: 0.92,
              zIndex: 90,
            }}
          >
            {gDrag.src ? (
              <img src={gDrag.src} alt="" className="w-full h-full object-cover" draggable={false} />
            ) : (
              <div className="w-full h-full bg-[var(--hover)]" />
            )}
          </div>,
          document.body,
        )}
    </>
  );
});
