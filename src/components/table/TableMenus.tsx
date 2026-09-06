/**
 * 表格编辑器弹层菜单（字段菜单 / 表头右键列菜单 / 行菜单 / 数据单元格复制粘贴菜单 /
 * 添加字段浮层 / 整表右键菜单 / 状态栏计算类型菜单）。
 *
 * 弹层壳统一走 `common/PopupLayer`（视口钳制 + Esc/点击外部关闭 + 容器样式，
 * 经 `common/Menu` 包装）；状态栏计算菜单（StatMenu）固定向上弹出走 PopupLayer 的 align="bottom"。
 */
import {
  AlignVerticalSpaceAround,
  ArrowLeft,
  ArrowRight,
  Check,
  ClipboardPaste,
  Columns3,
  Copy,
  Pencil,
  Scissors,
  Trash2,
  Type,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { PopupLayer } from "@/components/common/PopupLayer";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import { CALC_TYPE_LABELS, CALC_TYPES_BY_FIELD, FIELD_TYPE_LABELS } from "@/constants/table";
import { useTableStore } from "@/stores/tableStore";
import { columnAutoWidth } from "@/utils/table";
import type { FieldType, TableField } from "@/types";

/** 数据单元格右键菜单：复制 / 剪切 / 粘贴 / 格式（格式 = 在原坐标浮出气泡格式工具栏，见 TableEditor）。
 *  复制 = 当前选中区域 TSV 到系统剪贴板；剪切 = 复制成功后清空选中区域（一步撤销，图片格跳过）；
 *  粘贴 = 剪贴板 TSV 以选区锚点为起点写入，越界自动补行/补列。 */
export function CellMenu({
  x,
  y,
  onClose,
  onFormat,
}: {
  x: number;
  y: number;
  onClose: () => void;
  onFormat?: () => void;
}) {
  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-32" stopPointerDown>
      <MenuItem onClick={() => { useTableStore.getState().copySelection(); onClose(); }}>
        <Copy size={14} /> 复制
      </MenuItem>
      <MenuItem onClick={() => { void useTableStore.getState().cutSelection(); onClose(); }}>
        <Scissors size={14} /> 剪切
      </MenuItem>
      <MenuItem onClick={() => { void useTableStore.getState().pasteFromClipboard(); onClose(); }}>
        <ClipboardPaste size={14} /> 粘贴
      </MenuItem>
      {onFormat && (
        <MenuItem onClick={() => { onClose(); onFormat(); }}>
          <Type size={14} /> 格式
        </MenuItem>
      )}
    </Menu>
  );
}

/** 字段菜单（⋮ 按钮）：专注字段修改——重命名 / 改类型 / 单选选项管理 / 删除（就地确认）。
 *  列级操作（列宽自适应、左右插入字段）走表头右键 `ColumnMenu`。 */
export function FieldMenu({
  field,
  x,
  y,
  onClose,
}: {
  field: TableField | undefined;
  x: number;
  y: number;
  onClose: () => void;
}) {
  const renameField = useTableStore((s) => s.renameField);
  const retypeField = useTableStore((s) => s.retypeField);
  const setFieldOptions = useTableStore((s) => s.setFieldOptions);
  const removeField = useTableStore((s) => s.removeField);

  const [mode, setMode] = useState<"menu" | "rename" | "options">("menu");
  const [draft, setDraft] = useState("");
  const [optionsDraft, setOptionsDraft] = useState((field?.options ?? []).join("\n"));
  const [confirming, setConfirming] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const optionsRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (mode === "options") optionsRef.current?.focus();
    else if (mode !== "menu") inputRef.current?.focus();
  }, [mode]);

  if (!field) return null;

  // 重命名：inline 输入
  if (mode === "rename") {
    return (
      <Menu x={x} y={y} onClose={onClose} widthClass="w-44" contentClassName="py-2 px-2.5" repositionDeps={[mode]} stopPointerDown>
        <input
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              renameField(field.id, draft.trim() || field.name);
              onClose();
            }
            if (e.key === "Escape") onClose();
          }}
          onBlur={onClose}
          placeholder="字段名称"
          className="w-full bg-transparent border-b border-[var(--accent)] outline-none text-xs"
          style={{ color: "var(--text-primary)" }}
        />
      </Menu>
    );
  }

  if (mode === "options") {
    return (
      <Menu x={x} y={y} onClose={onClose} widthClass="w-52" contentClassName="p-2.5" repositionDeps={[mode]} stopPointerDown>
        <p className="text-[10px] mb-1" style={{ color: "var(--text-muted)" }}>
          每行一个选项
        </p>
        <textarea
          ref={optionsRef}
          value={optionsDraft}
          onChange={(e) => setOptionsDraft(e.target.value)}
          rows={5}
          className="w-full bg-transparent border border-[var(--border)] rounded p-1.5 outline-none text-xs resize-none"
          style={{ color: "var(--text-primary)" }}
        />
        <div className="flex justify-end gap-1 mt-1.5">
          <button
            onClick={onClose}
            className="px-2 py-0.5 rounded text-xs hover:bg-[var(--hover)]"
            style={{ color: "var(--text-secondary)" }}
          >
            取消
          </button>
          <button
            onClick={() => {
              setFieldOptions(
                field.id,
                optionsDraft.split("\n").map((s) => s.trim()).filter(Boolean),
              );
              onClose();
            }}
            className="px-2 py-0.5 rounded text-xs"
            style={{ background: "color-mix(in srgb, var(--accent) 15%, transparent)", color: "var(--accent)" }}
          >
            确定
          </button>
        </div>
      </Menu>
    );
  }

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-44" repositionDeps={[mode, confirming]} stopPointerDown>
      {confirming ? (
        <div className="px-3 py-1.5">
          <p className="text-xs mb-1.5" style={{ color: "var(--text-muted)" }}>
            删除字段将清空所有行该列的值
          </p>
          <MenuItem
            onClick={() => {
              removeField(field.id);
              onClose();
            }}
            danger
            className="rounded mb-1"
          >
            <span className="inline-flex items-center gap-1.5">
              <Trash2 size={14} /> 确认删除
            </span>
          </MenuItem>
          <MenuItem
            onClick={() => setConfirming(false)}
            className="rounded"
          >
            取消
          </MenuItem>
        </div>
      ) : (
        <>
          <MenuItem onClick={() => { setMode("rename"); setDraft(field.name); }}>
            <Pencil size={14} /> 重命名
          </MenuItem>
          <div className="px-3 py-1">
            <p className="text-[10px] mb-1" style={{ color: "var(--text-muted)" }}>
              字段类型
            </p>
            <div className="flex flex-col gap-0.5">
              {(Object.keys(FIELD_TYPE_LABELS) as FieldType[]).map((t) => (
                <button
                  key={t}
                  onClick={() => {
                    retypeField(field.id, t);
                    onClose();
                  }}
                  className="w-full text-left px-2 py-0.5 text-xs rounded hover:bg-[var(--hover)] inline-flex items-center justify-between"
                  style={{ color: field.type === t ? "var(--accent)" : "var(--text-primary)" }}
                >
                  {FIELD_TYPE_LABELS[t]}
                  {field.type === t && <Check size={11} />}
                </button>
              ))}
            </div>
          </div>
          {field.type === "singleSelect" && (
            <MenuItem onClick={() => { setMode("options"); setOptionsDraft((field.options ?? []).join("\n")); }}>
              单选选项管理
            </MenuItem>
          )}
          <MenuDivider />
          <MenuItem onClick={() => setConfirming(true)} danger>
            <Trash2 size={14} /> 删除字段
          </MenuItem>
        </>
      )}
    </Menu>
  );
}

/** 表头右键菜单：专注列修改/调整——列宽自适应 / 左侧插入字段 / 右侧插入字段（inline 输入）。
 *  字段属性修改（重命名/类型/选项/删除）走 ⋮ `FieldMenu`。 */
export function ColumnMenu({
  field,
  x,
  y,
  onClose,
  onFormat,
}: {
  field: TableField | undefined;
  x: number;
  y: number;
  onClose: () => void;
  onFormat?: () => void;
}) {
  const setFieldWidth = useTableStore((s) => s.setFieldWidth);
  const insertField = useTableStore((s) => s.insertField);
  const rows = useTableStore((s) => s.rows);
  const fieldIndex = useTableStore((s) => (field ? s.fields.findIndex((f) => f.id === field.id) : -1));

  const [mode, setMode] = useState<"menu" | "insertLeft" | "insertRight">("menu");
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (mode !== "menu") inputRef.current?.focus();
  }, [mode]);

  if (!field) return null;

  // 插入字段：inline 输入
  if (mode !== "menu") {
    return (
      <Menu x={x} y={y} onClose={onClose} widthClass="w-44" contentClassName="py-2 px-2.5" repositionDeps={[mode]} stopPointerDown>
        <input
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              const idx = mode === "insertLeft" ? fieldIndex : fieldIndex + 1;
              insertField(idx, draft.trim() || "字段", "text");
              onClose();
            }
            if (e.key === "Escape") onClose();
          }}
          onBlur={onClose}
          placeholder="新字段名称"
          className="w-full bg-transparent border-b border-[var(--accent)] outline-none text-xs"
          style={{ color: "var(--text-primary)" }}
        />
      </Menu>
    );
  }

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-44" repositionDeps={[mode]} stopPointerDown>
      <MenuItem
        onClick={() => {
          // 菜单触发的自适应 = 单次撤销单元（拖拽路径在首次实际变化时入栈，这里显式入栈）
          useTableStore.getState().pushUndo();
          setFieldWidth(field.id, columnAutoWidth(field, rows));
          onClose();
        }}
        title="按该列内容宽度自适应（不小于字段名宽度）"
      >
        <Columns3 size={14} /> 列宽自适应
      </MenuItem>
      {onFormat && (
        <MenuItem onClick={() => { onClose(); onFormat(); }}>
          <Type size={14} /> 格式
        </MenuItem>
      )}
      <MenuItem onClick={() => { setMode("insertLeft"); setDraft(""); }}>
        <ArrowLeft size={14} /> 左侧插入字段
      </MenuItem>
      <MenuItem onClick={() => { setMode("insertRight"); setDraft(""); }}>
        <ArrowRight size={14} /> 右侧插入字段
      </MenuItem>
    </Menu>
  );
}

/** 行菜单：复制行 / 上移 / 下移 / 行高自适应 / 格式 / 删除行。 */
export function RowMenu({
  rowId,
  x,
  y,
  onClose,
  onFormat,
}: {
  rowId: string;
  x: number;
  y: number;
  onClose: () => void;
  onFormat?: () => void;
}) {
  const rows = useTableStore((s) => s.rows);
  const duplicateRow = useTableStore((s) => s.duplicateRow);
  const removeRow = useTableStore((s) => s.removeRow);
  const moveRow = useTableStore((s) => s.moveRow);
  const clearRowHeight = useTableStore((s) => s.clearRowHeight);
  const from = rows.findIndex((r) => r.id === rowId);

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-36" stopPointerDown>
      <MenuItem onClick={() => { duplicateRow(rowId); onClose(); }}>
        <Columns3 size={14} /> 复制行
      </MenuItem>
      <MenuItem
        disabled={from <= 0}
        onClick={() => { moveRow(rowId, from - 1); onClose(); }}
      >
        上移
      </MenuItem>
      <MenuItem
        disabled={from < 0 || from >= rows.length - 1}
        onClick={() => { moveRow(rowId, from + 2); onClose(); }}
      >
        下移
      </MenuItem>
      <MenuItem
        onClick={() => { clearRowHeight(rowId); onClose(); }}
        title="清除手动行高，恢复按内容自然撑开"
      >
        <AlignVerticalSpaceAround size={14} /> 行高自适应
      </MenuItem>
      {onFormat && (
        <MenuItem onClick={() => { onClose(); onFormat(); }}>
          <Type size={14} /> 格式
        </MenuItem>
      )}
      <MenuDivider />
      <MenuItem
        onClick={() => { removeRow(rowId); onClose(); }}
        danger
      >
        <Trash2 size={14} /> 删除行
      </MenuItem>
    </Menu>
  );
}

/** 添加字段浮层：名称 + 类型选择 + 添加。 */
export function AddFieldMenu({ x, y, onClose }: { x: number; y: number; onClose: () => void }) {
  const addField = useTableStore((s) => s.addField);
  const [name, setName] = useState("");
  const [type, setType] = useState<FieldType>("text");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-48" contentClassName="p-2.5" stopPointerDown>
      <input
        ref={inputRef}
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            addField(name.trim() || "字段", type);
            onClose();
          }
        }}
        placeholder="字段名称"
        className="w-full bg-transparent border-b border-[var(--accent)] outline-none text-xs mb-2"
        style={{ color: "var(--text-primary)" }}
      />
      <div className="flex flex-col gap-0.5 mb-2">
        {(Object.keys(FIELD_TYPE_LABELS) as FieldType[]).map((t) => (
          <button
            key={t}
            onClick={() => setType(t)}
            className="w-full text-left px-2 py-0.5 text-xs rounded hover:bg-[var(--hover)] inline-flex items-center justify-between"
            style={{ color: type === t ? "var(--accent)" : "var(--text-primary)" }}
          >
            {FIELD_TYPE_LABELS[t]}
            {type === t && <Check size={11} />}
          </button>
        ))}
      </div>
      <div className="flex justify-end">
        <button
          onClick={() => {
            addField(name.trim() || "字段", type);
            onClose();
          }}
          className="px-2 py-0.5 rounded text-xs"
          style={{ background: "color-mix(in srgb, var(--accent) 15%, transparent)", color: "var(--accent)" }}
        >
          添加
        </button>
      </div>
    </Menu>
  );
}

/** 整表选中右键菜单：全部列宽自适应 / 全部行高自适应 / 格式。 */
export function SelectAllMenu({
  x,
  y,
  onClose,
  onFormat,
}: {
  x: number;
  y: number;
  onClose: () => void;
  onFormat?: () => void;
}) {
  const fields = useTableStore((s) => s.fields);
  const rows = useTableStore((s) => s.rows);
  const setFieldWidth = useTableStore((s) => s.setFieldWidth);
  const clearAllRowHeights = useTableStore((s) => s.clearAllRowHeights);

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-44" stopPointerDown>
      <MenuItem
        onClick={() => {
          // 整表自适应 = 单次撤销单元（批量入栈一次，防每列一个撤销单元）
          useTableStore.getState().pushUndo();
          for (const f of fields) setFieldWidth(f.id, columnAutoWidth(f, rows));
          onClose();
        }}
        title="按各列内容宽度自适应（不小于字段名宽度）"
      >
        <Columns3 size={14} /> 列宽自适应
      </MenuItem>
      <MenuItem
        onClick={() => {
          clearAllRowHeights();
          onClose();
        }}
        title="清除全部手动行高，恢复按内容自然撑开"
      >
        <AlignVerticalSpaceAround size={14} /> 行高自适应
      </MenuItem>
      {onFormat && (
        <MenuItem onClick={() => { onClose(); onFormat(); }}>
          <Type size={14} /> 格式
        </MenuItem>
      )}
    </Menu>
  );
}

/** 状态栏计算类型菜单：无 + 字段类型可用计算（当前项 accent + Check）。
 *  固定向上弹出（PopupLayer align="bottom"，底边贴点击位置上方不遮住点击处），
 *  左边缘对齐点击单元格，实测尺寸视口钳制。 */
export function StatMenu({
  field,
  x,
  y,
  onClose,
}: {
  field: TableField | undefined;
  x: number;
  y: number;
  onClose: () => void;
}) {
  const setCalcType = useTableStore((s) => s.setCalcType);

  if (!field) return null;

  return (
    <PopupLayer anchor={{ x, y: y - 4 }} align="bottom" onClose={onClose} widthClass="w-24">
      <MenuItem
        className="justify-between"
        style={{ color: field.calcType ? "var(--text-primary)" : "var(--accent)" }}
        onClick={() => {
          setCalcType(field.id, undefined);
          onClose();
        }}
      >
        <span>无</span>
        {!field.calcType && <Check size={12} />}
      </MenuItem>
      {CALC_TYPES_BY_FIELD[field.type]?.map((t) => (
        <MenuItem
          key={t}
          className="justify-between"
          style={{ color: field.calcType === t ? "var(--accent)" : "var(--text-primary)" }}
          onClick={() => {
            setCalcType(field.id, t);
            onClose();
          }}
        >
          <span>{CALC_TYPE_LABELS[t]}</span>
          {field.calcType === t && <Check size={12} />}
        </MenuItem>
      ))}
    </PopupLayer>
  );
}
