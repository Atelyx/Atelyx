import { File as FileIcon, Folder, FileText, Palette, Table as TableIcon } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVaultStore } from "@/stores/vaultStore";
import type { FileTreeNode } from "@/types";
import { FileKindIcon, vaultPathKind } from "@/components/common/FileKindIcon";
import { useDismissOnOutside } from "@/hooks/useDismissOnOutside";

/**
 * 仓库 # 提及选择器（对话输入框键入 # 或「添加上下文」唤起，画布对话节点与 AI 对话面板共用）。
 * 无过滤词 = 分类入口（笔记/表格/画布/文件/文件夹），进入分类后列出该类候选（条目上限同平铺页），
 * 首行为返回分类入口；有过滤词 = 跨分类平铺过滤（文件名忽略大小写包含）。
 * 候选来自 vaultStore 文件树展平（已排除隐藏/排除目录）；画布模式（canvasFiles）下当前画布上
 * 有对应节点的文件排最前——命中节点走建边引用流，其余为纯路径引用。
 * 键盘：↑/↓ 循环高亮、Enter 确认、Esc 关闭（document 捕获阶段拦截，避免 Enter 误发送/输入）。
 */

/** 候选上限（全仓库展平后量可能很大，防超长渲染；过滤词收窄可见性）。 */
const MAX_CANDIDATES = 50;

/** 分类维度（仓库内实际存在的可引用对象，按扩展名分派）。 */
type PickCategory = "note" | "table" | "canvas" | "file" | "dir";

const CATEGORIES: Array<{ key: PickCategory; label: string }> = [
  { key: "note", label: "笔记" },
  { key: "table", label: "表格" },
  { key: "canvas", label: "画布" },
  { key: "file", label: "文件" },
  { key: "dir", label: "文件夹" },
];

function categoryOf(t: VaultPickTarget): PickCategory {
  if (t.isDir) return "dir";
  const kind = vaultPathKind(t.name);
  if (kind === "note") return "note";
  if (kind === "table") return "table";
  if (kind === "canvas") return "canvas";
  return "file";
}

export interface VaultPickTarget {
  path: string;
  name: string;
  isDir: boolean;
}

interface Props {
  query: string;
  /** 画布模式：当前画布上有节点引用的文件路径集合（命中排最前）；面板不传。 */
  canvasFiles?: Set<string>;
  x: number;
  y: number;
  /** 向上弹出模式：菜单底边到输入框底边的容器内距离 */
  openUp: boolean;
  yBottom: number;
  onPick: (target: VaultPickTarget) => void;
  onClose: () => void;
}

export function VaultAtPicker({ query, canvasFiles, x, y, openUp, yBottom, onPick, onClose }: Props) {
  const tree = useVaultStore((s) => s.tree);
  const ref = useRef<HTMLDivElement>(null);
  const [active, setActive] = useState(0);
  // 当前所在分类（null = 分类入口页；有过滤词时恒按平铺过滤展示）
  const [category, setCategory] = useState<PickCategory | null>(null);
  // active 供键盘监听读取（ref 避免监听随 active 每击键重绑）
  const activeRef = useRef(active);
  activeRef.current = active;

  const all = useMemo(() => {
    const out: VaultPickTarget[] = [];
    const walk = (nodes: FileTreeNode[]) => {
      for (const n of nodes) {
        out.push({ path: n.path, name: n.name, isDir: n.isDir });
        if (n.isDir) walk(n.children);
      }
    };
    walk(tree);
    return out;
  }, [tree]);

  // 可见行：过滤词非空 = 平铺候选；否则 = 分类入口或该类候选（画布命中置顶，组内保持树序）。
  // rows 统一承载「候选行 / 分类行 / 返回行」，键盘导航对三种行一视同仁。
  const rows = useMemo<Array<{ kind: "target"; target: VaultPickTarget } | { kind: "category"; key: PickCategory; label: string } | { kind: "back" }>>(() => {
    const q = query.trim().toLowerCase();
    if (q) {
      const matched = all.filter((t) => t.name.toLowerCase().includes(q));
      const onCanvas: VaultPickTarget[] = [];
      const offCanvas: VaultPickTarget[] = [];
      for (const t of matched) (canvasFiles?.has(t.path) ? onCanvas : offCanvas).push(t);
      return [...onCanvas, ...offCanvas]
        .slice(0, MAX_CANDIDATES)
        .map((target) => ({ kind: "target" as const, target }));
    }
    if (!category) {
      return CATEGORIES.map(({ key, label }) => ({ kind: "category" as const, key, label }));
    }
    const inCategory = all.filter((t) => categoryOf(t) === category);
    const onCanvas: VaultPickTarget[] = [];
    const offCanvas: VaultPickTarget[] = [];
    for (const t of inCategory) (canvasFiles?.has(t.path) ? onCanvas : offCanvas).push(t);
    return [
      { kind: "back" },
      ...[...onCanvas, ...offCanvas]
        .slice(0, MAX_CANDIDATES)
        .map((target) => ({ kind: "target" as const, target })),
    ];
  }, [all, query, canvasFiles, category]);

  // 可见行变化（过滤词/分类/树刷新）→ 高亮复位；候选收缩时高亮越界兜底回末项
  useEffect(() => {
    setActive(0);
  }, [query, category, rows.length]);
  useEffect(() => {
    setActive((i) => (rows.length ? Math.min(i, rows.length - 1) : 0));
  }, [rows.length]);

  // 点击菜单外关闭（公共 hook；Esc 由下方捕获阶段键盘监听处理，hook 的 window Esc 为兜底，onClose 幂等）
  useDismissOnOutside(onClose, ref);

  // 行激活：候选 → 选中；分类/返回行 → 切换分类页（onPick 由调用方稳定化，setCategory 恒稳定）
  const activateRow = useCallback(
    (row: (typeof rows)[number]) => {
      if (row.kind === "target") {
        onPick(row.target);
        return;
      }
      setCategory(row.kind === "back" ? null : row.key);
    },
    [onPick],
  );

  // 键盘导航：捕获阶段拦截，防止输入框的 Enter 发送 / 方向键默认行为。
  // 方向键同样加 isComposing 守卫：IME 候选翻页的方向键不得被当作列表导航（Enter 分支已有）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        setActive((i) => (rows.length ? (i + 1) % rows.length : 0));
      } else if (e.key === "ArrowUp") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        setActive((i) => (rows.length ? (i - 1 + rows.length) % rows.length : 0));
      } else if (e.key === "Enter") {
        // IME 组合期间 Enter 是上屏候选词，不触发选择（中文输入法必踩）
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        const row = rows[activeRef.current];
        if (row) activateRow(row);
      } else if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [rows, activateRow, onClose]);

  // 容器内 absolute 定位（调用方容器 position: relative；画布节点避免 React Flow transform 容器下 fixed 漂移）
  const style: React.CSSProperties = openUp
    ? { left: x, bottom: yBottom + 6 }
    : { left: x, top: y + 6 };

  const iconOf = (t: VaultPickTarget) => {
    if (t.isDir) return <Folder size={14} className="flex-shrink-0" />;
    const kind = vaultPathKind(t.name);
    return kind ? <FileKindIcon kind={kind} size={14} /> : <FileIcon size={14} className="flex-shrink-0" />;
  };

  const categoryIcon = (key: PickCategory) => {
    if (key === "dir") return <Folder size={14} className="flex-shrink-0" />;
    if (key === "file") return <FileIcon size={14} className="flex-shrink-0" />;
    if (key === "canvas") return <Palette size={14} className="flex-shrink-0" />;
    if (key === "table") return <TableIcon size={14} className="flex-shrink-0" />;
    return <FileText size={14} className="flex-shrink-0" />;
  };

  const rowClass = () => "w-full text-left px-3 py-1.5 text-sm block";

  const rowStyle = (isActive: boolean): React.CSSProperties =>
    isActive
      ? { color: "var(--accent-fg)", background: "var(--accent)" }
      : {};

  return (
    <div
      ref={ref}
      className="absolute z-50 border rounded shadow-[var(--shadow-pop)] py-1 w-72 max-h-64 overflow-auto nowheel"
      style={{ ...style, background: "var(--bg-overlay)", borderColor: "var(--border)" }}
      onClick={(e) => e.stopPropagation()}
    >
      {rows.length === 0 ? (
        <div className="px-3 py-1.5 text-sm" style={{ color: "var(--text-muted)" }}>
          无匹配文件
        </div>
      ) : (
        rows.map((row, i) => {
          const isActive = i === active;
          if (row.kind === "back") {
            return (
              <button
                key="__back"
                onClick={() => setCategory(null)}
                onMouseEnter={() => setActive(i)}
                className={rowClass()}
                style={rowStyle(isActive)}
              >
                <span className="truncate flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
                  ← 全部分类
                </span>
              </button>
            );
          }
          if (row.kind === "category") {
            return (
              <button
                key={row.key}
                onClick={() => setCategory(row.key)}
                onMouseEnter={() => setActive(i)}
                className={rowClass()}
                style={rowStyle(isActive)}
              >
                <span className="truncate flex items-center gap-1.5">
                  {categoryIcon(row.key)}
                  {row.label}
                </span>
              </button>
            );
          }
          return (
            <button
              key={row.target.path}
              onClick={() => onPick(row.target)}
              onMouseEnter={() => setActive(i)}
              className={rowClass()}
              style={rowStyle(isActive)}
              title={row.target.path}
            >
              <span className="truncate flex items-center gap-1.5">
                {iconOf(row.target)}
                {row.target.name}
              </span>
            </button>
          );
        })
      )}
    </div>
  );
}
