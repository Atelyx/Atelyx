import { useState } from "react";
import { Check, RotateCcw } from "lucide-react";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import { foregroundFor } from "@/utils/color";
import { FOLDER_COLOR_PRESETS } from "./sort";

/** 文件夹图标颜色色板：预设色块 + 自定义取色 + 默认（清除）。预设/默认点击即应用并关闭；
 * 自定义取色器持续调节（原生颜色框 onChange 高频触发，仅应用不关闭，避免选取被中断）。 */
export function FolderColorMenu({
  x,
  y,
  currentColor,
  onChange,
  onClose,
}: {
  x: number;
  y: number;
  currentColor?: string;
  /** 应用颜色（undefined = 清除还原默认）；不负责关闭，由本组件选择时机调 onClose。 */
  onChange: (color: string | undefined) => void;
  onClose: () => void;
}) {
  const [custom, setCustom] = useState(currentColor ?? "#4f8fd0");
  const pick = (c: string | undefined) => {
    onChange(c);
    onClose();
  };
  return (
    <Menu
      x={x}
      y={y}
      onClose={onClose}
      widthClass="w-44"
      repositionDeps={[custom, currentColor]}
      stopPointerDown
    >
      <div className="px-3 pt-2 pb-1 flex flex-wrap gap-1.5">
        {FOLDER_COLOR_PRESETS.map((c) => {
          const active = c.toLowerCase() === currentColor?.toLowerCase();
          return (
            <button
              key={c}
              onClick={() => pick(c)}
              title={c}
              className="w-5 h-5 rounded-full flex items-center justify-center transition hover:scale-110 flex-shrink-0"
              style={{ background: c }}
            >
              {active && <Check size={11} style={{ color: foregroundFor(c) }} />}
            </button>
          );
        })}
      </div>
      <div className="px-3 py-1 flex items-center gap-1.5">
        <input
          type="color"
          value={custom}
          onChange={(e) => {
            setCustom(e.target.value);
            onChange(e.target.value);
          }}
          title="自定义颜色"
          className="w-5 h-5 rounded cursor-pointer bg-transparent p-0 border-0 flex-shrink-0"
        />
        <span className="text-micro truncate" style={{ color: "var(--text-muted)" }}>
          自定义
        </span>
      </div>
      <MenuDivider />
      <MenuItem onClick={() => pick(undefined)} title="清除该文件夹图标颜色，还原默认">
        <span className="inline-flex items-center gap-1.5">
          <RotateCcw size={14} />
          默认
        </span>
      </MenuItem>
    </Menu>
  );
}
