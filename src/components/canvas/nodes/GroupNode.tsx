/**
 * 分组节点：画布背景矩形容器（对等外部白板格式的 group 节点）。
 *
 * - 半透明彩色填充 + 实线边框按色板颜色（1-5 + 默认灰）渲染
 * - label 双击 inline 编辑（nodrag）；header 右侧色块按钮弹出色板切换颜色
 * - 可拖拽移动 / NodeResizeControl 调整大小；仅无向关联可连线（有向模式被拦截）
 */
import { Box } from "lucide-react";
import { useRef, useState } from "react";
import type { NodeProps } from "@xyflow/react";
import { Input } from "@/components/common/Input";
import { useCanvasStore } from "@/stores/canvasStore";
import {
  DEFAULT_GROUP_HEIGHT,
  DEFAULT_GROUP_WIDTH,
  GROUP_COLORS,
} from "@/constants/canvas";
import type { GroupFileData } from "@/types";
import { ConnectionFrame } from "./ConnectionFrame";
import { ResizeHandle } from "./ResizeHandle";
import { useInlineEdit } from "@/hooks/useInlineEdit";
import { useDismissOnOutside } from "@/hooks/useDismissOnOutside";

/** 色板顺序（色块按钮循环展示用）：1-5 + 默认（无色）。 */
const COLOR_OPTIONS: (string | undefined)[] = [
  "1",
  "2",
  "3",
  "4",
  "5",
  undefined,
];

export function GroupNode({ id, data, width, height, selected }: NodeProps) {
  const { label, color } = data as unknown as GroupFileData;
  const [colorMenu, setColorMenu] = useState(false);
  const colorMenuRef = useRef<HTMLDivElement>(null);
  const colorTriggerRef = useRef<HTMLButtonElement>(null);
  const readOnly = useCanvasStore((s) => s.readOnly);
  // label 双击 inline 编辑：空提交不修改（清空无意义，保留原文）
  const labelEdit = useInlineEdit({
    value: label,
    onCommit: (v) => {
      const t = v.trim();
      if (t && t !== label) {
        useCanvasStore.getState().updateNodeData(id, { label: t });
      }
    },
  });

  const base = GROUP_COLORS[color ?? ""] ?? "#8a8a8a";

  // 色板弹层：点击外部 / Esc 关闭（统一 useDismissOnOutside：pointerdown 语义 + trigger 排除，
  // 点触发器本体不关——开/关由按钮 onClick toggle，防 pointerdown 先关后 click 重开的竞态）

  const startEdit = () => {
    if (readOnly) return;
    labelEdit.start();
  };

  useDismissOnOutside(() => {
    if (!colorMenu) return;
    setColorMenu(false);
  }, colorMenuRef, colorTriggerRef);

  return (
    <div
      className="rounded-xl flex flex-col"
      style={{
        width: width ?? DEFAULT_GROUP_WIDTH,
        height: height ?? DEFAULT_GROUP_HEIGHT,
        minWidth: 160,
        minHeight: 80,
        // 半透明色板色填充 + 虚线边框：虚线表达「这是范围容器而非卡片」，边框色仍带用户选的组色
        background: `${base}1f`,
        border: `1.5px dashed ${selected ? "var(--accent)" : base}`,
        position: "relative",
        // 仅标题栏可拖拽移动；主体指针穿透由 CSS（.react-flow__node-group）统一处理
      }}
    >
      {/* 无向关联可连线（有向模式下被 isValidConnection 拦截）；低 zIndex 下 handle 随组 DOM，
          被上层节点覆盖的区域不可达，从组边缘空白处拉线即可 */}
      <ConnectionFrame topType="source" selected={selected} />

      <header
        className="px-3 py-1.5 text-sm font-medium flex-shrink-0 select-none flex items-center gap-1.5 rounded-t-xl"
        style={{
          // 标题行背景：色板色高不透明度（与主体 12% 填充拉开层次），文字用主色保证清晰
          background: `${base}40`,
          color: "var(--text-primary)",
          cursor: "grab",
        }}
      >
        {/* 类型标识：图标（分组 = 容器） */}
        <Box
          size={13}
          className="flex-shrink-0"
          style={{ color: "var(--text-secondary)" }}
        />
        {labelEdit.editing ? (
          <Input
            {...labelEdit.inputProps}
            autoFocus
            borderless
            onClick={(e) => e.stopPropagation()}
            className="nodrag min-w-0 !text-xs"
          />
        ) : (
          <span
            className="truncate inline-block max-w-full"
            style={{ color: "var(--text-primary)" }}
            onDoubleClick={startEdit}
            title={readOnly ? undefined : "双击重命名"}
          >
            {label || "分组"}
          </span>
        )}
        {!readOnly && (
          <button
            ref={colorTriggerRef}
            onClick={(e) => {
              e.stopPropagation();
              setColorMenu((v) => !v);
            }}
            className="nodrag ml-auto flex-shrink-0 w-3.5 h-3.5 rounded-full border transition-transform hover:scale-110"
            style={{
              background: color ? base : "transparent",
              borderColor: color ? base : "var(--text-muted)",
            }}
            title="分组颜色"
            aria-label="分组颜色"
          />
        )}
      </header>

      {/* 色板弹层（1-5 + 默认；点击切换颜色并关闭） */}
      {colorMenu && (
        <div
          ref={colorMenuRef}
          className="absolute top-7 left-3 z-10 flex items-center gap-1.5 p-1.5 rounded-md border"
          style={{
            background: "var(--bg-tertiary)",
            borderColor: "var(--border)",
            boxShadow: "var(--shadow-pop)",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          {COLOR_OPTIONS.map((c) => {
            const hex = c ? (GROUP_COLORS[c] ?? "#8a8a8a") : "#8a8a8a";
            const isActive = (c ?? undefined) === (color ?? undefined);
            return (
              <button
                key={c ?? "none"}
                onClick={() => {
                  useCanvasStore.getState().updateNodeData(id, { color: c });
                  setColorMenu(false);
                }}
                className="w-4 h-4 rounded-full border flex-shrink-0 transition-transform hover:scale-110"
                style={{
                  background: c ? hex : "transparent",
                  borderColor: hex,
                  outline: isActive ? "1.5px solid var(--accent)" : undefined,
                  outlineOffset: 1,
                }}
                title={c ? `颜色 ${c}` : "默认"}
              />
            );
          })}
        </div>
      )}

      {!readOnly && <ResizeHandle />}
    </div>
  );
}
