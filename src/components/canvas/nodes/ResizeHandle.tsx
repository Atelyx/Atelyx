/**
 * 节点右下角缩放柄（NodeResizeControl 统一样式）。
 * 浮层色底 + accent 描边 + 斜向光标，7 个可缩放节点共用（样式收敛，改外观只动此处）。
 */
import { NodeResizeControl, type OnResizeStart } from "@xyflow/react";

export function ResizeHandle({ onResizeStart }: { onResizeStart?: OnResizeStart }) {
  return (
    <NodeResizeControl
      position="bottom-right"
      onResizeStart={onResizeStart}
      style={{
        width: 10,
        height: 10,
        background: "var(--bg-overlay)",
        border: "2px solid var(--accent)",
        borderRadius: 2,
        cursor: "nwse-resize",
      }}
    />
  );
}
