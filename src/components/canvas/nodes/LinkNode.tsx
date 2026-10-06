/**
 * 链接节点：URL 卡片（对等外部白板格式的 link 节点）。
 * 单击卡片在系统浏览器打开 URL；不可打开的协议（见 `isOpenableUrl`）静默不响应。
 */
import { Link2 } from "lucide-react";
import { type NodeProps } from "@xyflow/react";
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { DEFAULT_LINK_HEIGHT, DEFAULT_LINK_WIDTH } from "@/constants/canvas";
import type { LinkFileData } from "@/types";
import { isOpenableUrl } from "@/utils/markdown";
import { ConnectionFrame } from "./ConnectionFrame";
import { ResizeHandle } from "./ResizeHandle";

function hostOf(raw: string): string {
  try {
    return new URL(raw).hostname || raw;
  } catch {
    return raw;
  }
}

export function LinkNode({ data, width, height, selected }: NodeProps) {
  const { url } = data as unknown as LinkFileData;
  const openUrl = useAppStore((s) => s.openUrl);
  // 只读白板（外部白板格式）：禁 resize（手柄不渲染）
  const readOnly = useCanvasStore((s) => s.readOnly);

  const handleOpen = () => {
    if (!url || !isOpenableUrl(url)) return;
    void openUrl(url);
  };

  return (
    <div
      className="rounded-md border flex flex-col text-sm cursor-pointer"
      style={{
        width: width ?? DEFAULT_LINK_WIDTH,
        height: height ?? DEFAULT_LINK_HEIGHT,
        minWidth: 160,
        minHeight: 60,
        background: "var(--bg-secondary)",
        borderColor: selected ? "var(--accent)" : "var(--border)",
        boxShadow: selected ? "var(--shadow-pop), var(--accent-glow)" : "var(--shadow-pop)",
        position: "relative",
      }}
      onClick={handleOpen}
      title={url ? (isOpenableUrl(url) ? `打开 ${url}` : url) : undefined}
    >
      {/* link 只出 source：它参与的连线一律分类为关联自由线，其它关系类型连不上 */}
      <ConnectionFrame topType="source" selected={selected} />

      <div className="flex-1 min-h-0 flex flex-col justify-center px-3 py-2 gap-1">
        <span
          className="inline-flex items-center gap-1.5 text-xs font-medium truncate"
          style={{ color: "var(--text-primary)" }}
        >
          <Link2
            size={13}
            className="flex-shrink-0"
            style={{ color: "var(--accent)" }}
          />
          <span className="truncate">{hostOf(url)}</span>
        </span>
        <span
          className="text-micro truncate"
          style={{ color: "var(--text-muted)" }}
        >
          {url}
        </span>
      </div>

      {!readOnly && <ResizeHandle />}
    </div>
  );
}
