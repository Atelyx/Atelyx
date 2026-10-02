/**
 * 表格气泡格式工具栏：对当前选中区域（单格/框选/行/列/整表）应用单元格格式——
 * 字体 / 字号 / 粗体 / 斜体 / 下划线 / 删除线 / 文字颜色 / 背景色 / 清除格式。
 *
 * 与常驻工具条无关，是跟随选区的瞬时浮层：触发方（TableEditor）在拖框选完成 / 再次点击
 * 已选中单元格 / 右键菜单「格式」时浮出，锚点为选区左上角或菜单坐标；外点 / Esc /
 * 表格滚动 / 开始在单元格键入时自动关闭。条保持打开支持连续格式化（多级内层弹层经
 * `data-popup-layer` 排除在「外点关闭」之外）。
 *
 * 格式与值**正交**（存 `TableRow.styles[fieldId]`，见 `types/table.ts` `CellStyle`）：
 * 值/复制粘贴/快照不受影响；动作走 `tableStore.applyCellStyle`（一步撤销 + 防抖落盘 +
 * 协作广播，选区不一致时各属性呈三态：布尔半选、颜色/字体/字号「混合」）。
 */
import { Bold, Eraser, Italic, Strikethrough, Underline } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { PopupLayer } from "@/components/common/PopupLayer";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { useClampedMenuPosition } from "@/hooks/useClampedMenuPosition";
import { useTableStore } from "@/stores/tableStore";
import { BG_COLOR_PRESETS, FONT_PRESETS, FONT_SIZE_OPTIONS, TEXT_COLOR_PRESETS } from "@/constants/table";
import { selectionStyleSummary } from "@/utils/table";

interface Props {
  /** 锚点（视口坐标）：选区左上角或右键菜单坐标；条向上浮出。 */
  anchor: { x: number; y: number };
  onClose: () => void;
}

/** 布尔格式键（工具栏切换项）。 */
type FlagKey = "b" | "i" | "u" | "s";

/** 触发按钮三态样式：true = 实心强调；"mixed" = 淡强调 + 圆点；false = 默认。 */
function flagButtonStyle(state: boolean | "mixed") {
  if (state === true) return { background: "var(--accent)", color: "var(--accent-fg)" };
  if (state === "mixed") return { background: "var(--accent-soft)", color: "var(--accent)" };
  return undefined;
}

/** 自定义颜色输入：原生取色器拖动时仅本地预览（onChange = input 事件），
 *  关闭取色器（native change）才提交一次——避免拖动过程刷 N 次撤销单元。 */
function ColorInput({ value, onCommit }: { value: string; onCommit: (hex: string) => void }) {
  const ref = useRef<HTMLInputElement>(null);
  const [v, setV] = useState(value);
  const commitRef = useRef(onCommit);
  // 渲染期不写 ref（并发语义不稳）：commitRef 每渲染后同步到最新 onCommit，监听只挂一次
  useEffect(() => {
    commitRef.current = onCommit;
  }, [onCommit]);
  // 外部值变化（如点击预设色板后选区样式更新）时跟随显示，保持取色器与选区一致
  useEffect(() => {
    setV(value);
  }, [value]);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onChange = () => commitRef.current(el.value);
    el.addEventListener("change", onChange);
    return () => el.removeEventListener("change", onChange);
  }, []);
  return (
    <input
      ref={ref}
      type="color"
      value={v}
      onChange={(e) => setV(e.target.value)}
      className="w-6 h-6 cursor-pointer rounded border border-[var(--border)] bg-transparent p-0.5"
      title="自定义颜色"
    />
  );
}

/** 颜色弹层（字色/底色共用）：默认（清除） + 预设色板 + 自定义取色。取色后不关闭，支持连续微调。 */
function ColorPopover({
  trigger,
  target,
  current,
}: {
  trigger: (ref: RefObject<HTMLButtonElement>, open: boolean, onToggle: () => void) => ReactNode;
  target: "color" | "bg";
  /** 选区一致值（undefined = 全部默认）；"mixed" = 不一致。 */
  current: string | undefined | "mixed";
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(triggerRef);
  const apply = useTableStore((s) => s.applyCellStyle);

  const pick = (hex: string) => (target === "color" ? apply({ color: hex }) : apply({ bg: hex }));
  const clear = () => (target === "color" ? apply({ color: undefined }) : apply({ bg: undefined }));
  const swatches = target === "color" ? TEXT_COLOR_PRESETS : BG_COLOR_PRESETS;

  return (
    <>
      {trigger(triggerRef, !!anchor, toggle)}
      <PopupLayer anchor={anchor} onClose={close} triggerRef={triggerRef} zClass="z-[1100]">
        <div className="p-2 w-44">
          <button
            onClick={clear}
            className="w-full text-left px-2 py-1 rounded text-xs hover:bg-[var(--hover)]"
            style={{ color: current === undefined ? "var(--accent)" : "var(--text-primary)" }}
          >
            默认
          </button>
          <div className="grid grid-cols-5 gap-1.5 mt-1.5">
            {swatches.map((hex) => (
              <button
                key={hex}
                onClick={() => pick(hex)}
                className="w-7 h-7 rounded transition-transform hover:scale-110"
                style={{ background: hex, boxShadow: current === hex ? "inset 0 0 0 2px var(--accent)" : undefined }}
                title={hex}
              />
            ))}
          </div>
          <div className="flex items-center gap-1.5 mt-2">
            <ColorInput value={current === undefined || current === "mixed" ? "#e05252" : current} onCommit={pick} />
            <span className="text-[10px]" style={{ color: "var(--text-muted)" }}>自定义</span>
          </div>
        </div>
      </PopupLayer>
    </>
  );
}

export function TableFormatToolbar({ anchor, onClose }: Props) {
  const fields = useTableStore((s) => s.fields);
  const rows = useTableStore((s) => s.rows);
  const selection = useTableStore((s) => s.selection);
  const apply = useTableStore((s) => s.applyCellStyle);
  // 选区样式汇总（三态来源）；selection 变化即重算，条保持打开随选区状态刷新
  const summary = useMemo(() => selectionStyleSummary(selection, fields, rows), [selection, fields, rows]);

  const { ref: posRef, pos } = useClampedMenuPosition(anchor.x, anchor.y, [selection], { alignBottom: true });

  // 关闭交互：外点（工具栏内/内层弹层保持）关闭；Esc 关闭；在单元格键入/导航（cell editor 聚焦）时关闭
  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      const t = e.target as HTMLElement;
      if (posRef.current?.contains(t)) return;
      if (t.closest("[data-popup-layer]")) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      // 在单元格键入/导航（cell editor 聚焦）时关闭格式条；放行修饰键组合（Ctrl+Z/Y/C/V 等不关条）
      const active = document.activeElement as HTMLElement | null;
      if (active?.hasAttribute("data-cell-editor") && !e.ctrlKey && !e.metaKey && !e.altKey) onClose();
    };
    document.addEventListener("pointerdown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose, posRef]);

  const toggleFlag = (key: FlagKey, state: boolean | "mixed") => {
    const off = state === true; // 全开 → 关；mixed/全关 → 开（拉齐）
    if (key === "b") apply(off ? { b: undefined } : { b: true });
    else if (key === "i") apply(off ? { i: undefined } : { i: true });
    else if (key === "u") apply(off ? { u: undefined } : { u: true });
    else apply(off ? { s: undefined } : { s: true });
  };

  const flags: { key: FlagKey; icon: ReactNode; title: string; state: boolean | "mixed" }[] = [
    { key: "b", icon: <Bold size={13} />, title: "粗体", state: summary.bold },
    { key: "i", icon: <Italic size={13} />, title: "斜体", state: summary.italic },
    { key: "u", icon: <Underline size={13} />, title: "下划线", state: summary.underline },
    { key: "s", icon: <Strikethrough size={13} />, title: "删除线", state: summary.strike },
  ];

  return createPortal(
    <div
      ref={posRef}
      data-table-format-bar
      className="fixed z-50 flex items-center gap-0.5 px-1.5 py-1 rounded-[var(--radius-md)] border shadow-[var(--shadow-pop)]"
      style={{ left: pos.x, top: pos.y, background: "var(--bg-secondary)", borderColor: "var(--border)" }}
    >
      {/* 字体 */}
      <DropdownSelect
        value={summary.font === "mixed" ? "" : (summary.font ?? "default")}
        onChange={(v) => apply(v === "default" ? { font: undefined } : { font: v })}
        options={FONT_PRESETS.map((f) => ({ value: f.key, label: f.label }))}
        placeholder="字体"
        className="w-20 h-7 px-1.5 text-xs rounded hover:bg-[var(--hover)]"
        style={{ color: "var(--text-secondary)" }}
        title="字体"
      />
      {/* 字号 */}
      <DropdownSelect
        value={summary.size === "mixed" ? "" : summary.size === undefined ? "default" : String(summary.size)}
        onChange={(v) => apply(v === "default" ? { size: undefined } : { size: Number(v) })}
        options={[
          { value: "default", label: "默认" },
          ...FONT_SIZE_OPTIONS.map((s) => ({ value: String(s), label: String(s) })),
        ]}
        placeholder="字号"
        className="w-12 h-7 px-1.5 text-xs rounded hover:bg-[var(--hover)]"
        style={{ color: "var(--text-secondary)" }}
        title="字号"
      />

      <span className="w-px h-4 mx-0.5 flex-shrink-0" style={{ background: "var(--border)" }} />

      {/* 粗体/斜体/下划线/删除线 */}
      {flags.map((f) => (
        <button
          key={f.key}
          onClick={() => toggleFlag(f.key, f.state)}
          title={f.title}
          className="relative w-7 h-7 flex items-center justify-center rounded transition-colors hover:bg-[var(--hover)]"
          style={flagButtonStyle(f.state)}
        >
          {f.icon}
          {f.state === "mixed" && (
            <span className="absolute bottom-0.5 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full" style={{ background: "var(--accent)" }} />
          )}
        </button>
      ))}

      <span className="w-px h-4 mx-0.5 flex-shrink-0" style={{ background: "var(--border)" }} />

      {/* 文字颜色 / 背景色 */}
      <ColorPopover
        target="color"
        current={summary.color}
        trigger={(ref, open, onToggle) => (
          <button
            ref={ref}
            onClick={onToggle}
            title="文字颜色"
            className="w-7 h-7 flex flex-col items-center justify-center rounded transition-colors hover:bg-[var(--hover)]"
            style={{ color: open ? "var(--accent)" : "var(--text-secondary)" }}
          >
            <span className="text-[13px] font-bold leading-none">A</span>
            <span
              className="block w-3.5 h-[3px] rounded-full mt-0.5"
              style={{
                background:
                  summary.color === "mixed"
                    ? "linear-gradient(90deg, #e05252, #4f8fd0, #4fae6a)"
                    : (summary.color ?? "color-mix(in srgb, var(--text-muted) 35%, transparent)"),
              }}
            />
          </button>
        )}
      />
      <ColorPopover
        target="bg"
        current={summary.bg}
        trigger={(ref, open, onToggle) => (
          <button
            ref={ref}
            onClick={onToggle}
            title="背景色"
            className="w-7 h-7 flex items-center justify-center rounded transition-colors hover:bg-[var(--hover)]"
            style={{ color: open ? "var(--accent)" : "var(--text-secondary)" }}
          >
            <span
              className="block w-3.5 h-3.5 rounded-[var(--radius-sm)] border border-[var(--border)]"
              style={{
                background:
                  summary.bg === "mixed"
                    ? "linear-gradient(45deg, #fde2e2 50%, #e0ecfb 50%)"
                    : (summary.bg ?? "transparent"),
              }}
            />
          </button>
        )}
      />

      <span className="w-px h-4 mx-0.5 flex-shrink-0" style={{ background: "var(--border)" }} />

      {/* 清除格式 */}
      <button
        onClick={() => apply(null)}
        title="清除格式"
        className="w-7 h-7 flex items-center justify-center rounded transition-colors hover:bg-[var(--hover)]"
        style={{ color: "var(--text-secondary)" }}
      >
        <Eraser size={13} />
      </button>
    </div>,
    document.body,
  );
}
