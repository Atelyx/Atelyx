import { AlertTriangle, CircleHelp, FileText, Image } from "lucide-react";
import type { SyntheticEvent } from "react";
import type { NodeProps } from "@xyflow/react";
import { useCanvasStore } from "@/stores/canvasStore";
import type { MediaData } from "@/types";
import { ConnectionFrame } from "./ConnectionFrame";
import { ResizeHandle } from "./ResizeHandle";

/** 图片最长边封顶（px），避免大图占满画布 */
const IMG_MAX_DIM = 360;
/** 图片展示宽度下限（px），与节点 minWidth 对齐避免过窄 */
const IMG_MIN_WIDTH = 180;

/**
 * 媒体节点。
 * 图片显示缩略图，文件显示图标 + 文件名；解析失败标注「无法解析」。
 * 连线锚点：右侧 source 供接入对话节点。
 *
 * 尺寸策略：用 data.userResized 区分用户是否手动 resize 过。
 * - 未 resize：宽度跟随 data.displayWidth（按图片比例推导），高度 auto 让图片自然撑高；
 * - 已 resize：宽高跟随 NodeProps（用户拖拽值），内容区溢出滚动。
 */
export function MediaNode({ id, data, width, height, selected }: NodeProps) {
  const {
    name,
    kind,
    mime,
    thumb,
    parseFailed,
    displayWidth,
    userResized,
    fileMissing,
  } = data as unknown as MediaData;
  // 只读白板（外部白板格式）：禁 resize（手柄不渲染、图片加载不写显示宽度）
  const readOnly = useCanvasStore((s) => s.readOnly);

  const displayName =
    name || (mime ? mime.split("/").pop()?.toUpperCase() : "媒体");
  const nodeWidth = userResized ? width : (displayWidth ?? 260);
  const nodeHeight = userResized ? height : undefined;
  const hasFixedHeight = userResized;

  // 图片加载后按真实比例推导展示宽度并持久化，高度交由 auto 自然撑高
  const handleImgLoad = (e: SyntheticEvent<HTMLImageElement>) => {
    if (userResized || readOnly) return;
    const img = e.currentTarget;
    const W = img.naturalWidth;
    const H = img.naturalHeight;
    if (!W || !H) return;
    const longest = Math.max(W, H);
    const scale = longest > IMG_MAX_DIM ? IMG_MAX_DIM / longest : 1;
    const w = Math.max(
      IMG_MIN_WIDTH,
      Math.min(IMG_MAX_DIM, Math.round(W * scale)),
    );
    if ((displayWidth ?? 0) !== w) {
      useCanvasStore.getState().updateNodeData(id, { displayWidth: w });
    }
  };

  const handleResizeStart = () => {
    if (!userResized)
      useCanvasStore.getState().updateNodeData(id, { userResized: true });
  };

  return (
    <div
      className="rounded-md shadow-lg border flex flex-col text-sm"
      style={{
        width: nodeWidth,
        height: nodeHeight,
        minWidth: IMG_MIN_WIDTH,
        minHeight: 120,
        background: "var(--bg-card)",
        borderColor: selected ? "var(--accent)" : "var(--border)",
        position: "relative",
      }}
    >
      <ConnectionFrame topType="source" selected={selected} />

      <header
        className="px-3 py-1.5 border-b rounded-t-md text-xs font-medium flex-shrink-0"
        style={{
          cursor: "grab",
          borderColor: "var(--border)",
          color: "var(--text-secondary)",
        }}
      >
        <span className="inline-flex items-center gap-1">
          <Image size={14} className="flex-shrink-0" />
          媒体
        </span>
      </header>

      <div
        className={`nodrag nowheel overflow-auto flex flex-col gap-2 px-3 py-2 ${
          hasFixedHeight ? "flex-1 min-h-0" : ""
        }`}
        style={{ userSelect: "text", WebkitUserSelect: "text" }}
      >
        {fileMissing ? (
          <div
            className="rounded border flex items-center justify-center h-16 text-2xl"
            style={{
              borderColor: "var(--border)",
              background: "var(--bg-tertiary)",
            }}
          >
            <CircleHelp size={28} />
          </div>
        ) : kind === "image" && thumb ? (
          <img
            src={thumb}
            alt={displayName}
            onLoad={handleImgLoad}
            // w-full h-auto：宽度填满内容区，高度按图片比例自适应
            className="rounded border w-full h-auto"
            style={{
              borderColor: "var(--border)",
              background: "var(--bg-tertiary)",
            }}
            draggable={false}
          />
        ) : (
          <div
            className="rounded border flex items-center justify-center h-16 text-2xl"
            style={{
              borderColor: "var(--border)",
              background: "var(--bg-tertiary)",
            }}
          >
            <FileText size={28} />
          </div>
        )}
        <p
          className="text-xs break-all leading-snug"
          style={{ color: "var(--text-primary)" }}
          title={displayName}
        >
          {displayName}
        </p>
        {fileMissing && (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "var(--danger)" }}
          >
            <AlertTriangle size={14} className="flex-shrink-0" />
            文件缺失（已被删除或重命名）
          </p>
        )}
        {parseFailed && (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "var(--danger)" }}
          >
            <AlertTriangle size={14} className="flex-shrink-0" />
            无法解析（仅作画布参考，不注入模型）
          </p>
        )}
      </div>

      {!readOnly && <ResizeHandle onResizeStart={handleResizeStart} />}
    </div>
  );
}
