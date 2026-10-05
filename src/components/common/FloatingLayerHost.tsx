/**
 * 插件浮层宿主（ctx.ui.showFloatingLayer 的渲染层）。
 *
 * 每个窗口各挂一个（App.tsx，与 NotificationHost 同位；撕裂窗口是独立 webview，浮层各自独立）。
 * 浮层按登记顺序叠放；fixed 定位脱离 transform 祖先（同 PopupLayer 的 portal 语义），
 * z 层级高于全部既有弹层、低于通知堆叠。收起语义宿主代管：Esc 恒收起（后开先关），
 * 外点收起按登记选项；center 居中或坐标定位（实测尺寸后钳制视口）。
 */
import { useEffect, useRef, type CSSProperties, type RefObject } from "react";
import { useFloatingLayerStore, type FloatingLayerEntry } from "@/stores/floatingLayerStore";
import { useDismissOnOutside } from "@/hooks/useDismissOnOutside";
import { useBackHandler } from "@/hooks/useBackHandler";
import { useClampedMenuPosition } from "@/hooks/useClampedMenuPosition";

export function FloatingLayerHost() {
  const layers = useFloatingLayerStore((s) => s.layers);
  return (
    <>
      {layers.map((layer) =>
        layer.placement === "center" ? (
          <CenteredLayer key={layer.id} layer={layer} />
        ) : (
          <PositionedLayer key={layer.id} layer={layer} />
        ),
      )}
    </>
  );
}

/** 单个浮层的共享交互与容器样式：Esc/外点只关最上层（多层叠放时一次按键不连坐），
 *  外点收起按登记选项；外观与宿主弹层同语汇。返回键栈（移动端）本身后进先出，天然只达最上层。 */
function LayerFrame({
  layer,
  containerRef,
  className,
  style,
}: {
  layer: FloatingLayerEntry;
  containerRef: RefObject<HTMLDivElement>;
  className: string;
  style: CSSProperties;
}) {
  // 只有关闭动作到达 store 的那一层才真正收起：非最上层时 no-op
  const closeIfTop = () => {
    const layers = useFloatingLayerStore.getState().layers;
    if (layers[layers.length - 1]?.id !== layer.id) return;
    useFloatingLayerStore.getState().close(layer.id);
  };
  // escape: false——Esc 归本组件的「只关最上层」判定（hook 自带监听无栈语义，多层会全触发）
  useDismissOnOutside(layer.closeOnOutsideClick ? closeIfTop : () => {}, containerRef, undefined, {
    escape: false,
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const layers = useFloatingLayerStore.getState().layers;
      if (layers[layers.length - 1]?.id !== layer.id) return;
      useFloatingLayerStore.getState().close(layer.id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [layer.id]);
  useBackHandler(true, () => {
    closeIfTop();
    return true;
  });
  const Content = layer.component;
  return (
    <div
      ref={containerRef}
      // data-popup-layer：宿主全局外点监听（如 NoteEditor 的「点外退出编辑」）按此标记排除浮层内操作
      data-popup-layer
      className={`fixed border rounded-[var(--radius-md)] shadow-[var(--shadow-pop)] z-[1150] ${className}`}
      style={{
        ...style,
        ...(layer.width !== undefined ? { width: layer.width } : {}),
        maxHeight: "calc(100vh - 24px)",
        background: "var(--bg-overlay)",
        borderColor: "var(--border)",
        // 浮层压在内容之上：背景模糊由皮肤决定（默认 none = 不模糊）
        backdropFilter: "var(--glass-filter)",
        WebkitBackdropFilter: "var(--glass-filter)",
      }}
    >
      <Content />
    </div>
  );
}

function CenteredLayer({ layer }: { layer: FloatingLayerEntry }) {
  const ref = useRef<HTMLDivElement>(null);
  return (
    <LayerFrame
      layer={layer}
      containerRef={ref}
      className="left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 overflow-y-auto"
      style={{}}
    />
  );
}

function PositionedLayer({ layer }: { layer: FloatingLayerEntry }) {
  const placement = layer.placement as { x: number; y: number };
  const { ref, pos } = useClampedMenuPosition(placement.x, placement.y);
  return (
    <LayerFrame
      layer={layer}
      containerRef={ref}
      className="overflow-y-auto"
      style={{ left: pos.x, top: pos.y }}
    />
  );
}
