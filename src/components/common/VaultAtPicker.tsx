import { File as FileIcon, Folder } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useVaultStore } from "@/stores/vaultStore";
import type { FileTreeNode } from "@/types";
import { FileKindIcon, vaultPathKind } from "@/components/common/FileKindIcon";
import { useDismissOnOutside } from "@/hooks/useDismissOnOutside";

/**
 * 仓库 @ 提及选择器（对话输入框键入 @ 唤起，画布对话节点与 AI 对话面板共用）。
 * 列出全仓库文件与文件夹（vaultStore 文件树展平，已排除隐藏/排除目录），@ 后继续输入按文件名实时过滤；
 * 画布模式（canvasFiles）下当前画布上有对应节点的文件排最前——命中节点走建边引用流，其余为纯路径引用。
 * 键盘：↑/↓ 循环高亮、Enter 确认、Esc 关闭（document 捕获阶段拦截，避免 Enter 误发送/输入）。
 */

/** 候选上限（全仓库展平后量可能很大，防超长渲染；过滤词收窄可见性）。 */
const MAX_CANDIDATES = 50;

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
  // active 供键盘监听读取（ref 避免监听随 active 每击键重绑）
  const activeRef = useRef(active);
  activeRef.current = active;

  // 文件树展平 + 过滤词匹配（文件名忽略大小写包含）+ 画布命中置顶（组内保持树序）
  const candidates = useMemo(() => {
    const q = query.trim().toLowerCase();
    const all: VaultPickTarget[] = [];
    const walk = (nodes: FileTreeNode[]) => {
      for (const n of nodes) {
        all.push({ path: n.path, name: n.name, isDir: n.isDir });
        if (n.isDir) walk(n.children);
      }
    };
    walk(tree);
    const matched = q ? all.filter((t) => t.name.toLowerCase().includes(q)) : all;
    const onCanvas: VaultPickTarget[] = [];
    const offCanvas: VaultPickTarget[] = [];
    for (const t of matched) (canvasFiles?.has(t.path) ? onCanvas : offCanvas).push(t);
    return [...onCanvas, ...offCanvas].slice(0, MAX_CANDIDATES);
  }, [tree, query, canvasFiles]);

  // 过滤词变化时高亮回到第一项
  useEffect(() => {
    setActive(0);
  }, [query]);

  // 候选收缩（树刷新）时高亮越界兜底：回退到末项
  useEffect(() => {
    setActive((i) => (candidates.length ? Math.min(i, candidates.length - 1) : 0));
  }, [candidates.length]);

  // 点击菜单外关闭（公共 hook；Esc 由下方捕获阶段键盘监听处理，hook 的 window Esc 为兜底，onClose 幂等）
  useDismissOnOutside(onClose, ref);

  // 键盘导航：捕获阶段拦截，防止输入框的 Enter 发送 / 方向键默认行为。
  // 方向键同样加 isComposing 守卫：IME 候选翻页的方向键不得被当作列表导航（Enter 分支已有）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        setActive((i) => (candidates.length ? (i + 1) % candidates.length : 0));
      } else if (e.key === "ArrowUp") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        setActive((i) => (candidates.length ? (i - 1 + candidates.length) % candidates.length : 0));
      } else if (e.key === "Enter") {
        // IME 组合期间 Enter 是上屏候选词，不触发选择（中文输入法必踩）
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        const t = candidates[activeRef.current];
        if (t) onPick(t);
      } else if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [candidates, onPick, onClose]);

  // 容器内 absolute 定位（调用方容器 position: relative；画布节点避免 React Flow transform 容器下 fixed 漂移）
  const style: React.CSSProperties = openUp
    ? { left: x, bottom: yBottom + 6 }
    : { left: x, top: y + 6 };

  const iconOf = (t: VaultPickTarget) => {
    if (t.isDir) return <Folder size={14} className="flex-shrink-0" />;
    const kind = vaultPathKind(t.name);
    return kind ? <FileKindIcon kind={kind} size={14} /> : <FileIcon size={14} className="flex-shrink-0" />;
  };

  return (
    <div
      ref={ref}
      className="absolute z-50 border rounded shadow-[var(--shadow-pop)] py-1 w-72 max-h-64 overflow-auto nowheel"
      style={{ ...style, background: "var(--bg-overlay)", borderColor: "var(--border)" }}
      onClick={(e) => e.stopPropagation()}
    >
      {candidates.length === 0 ? (
        <div className="px-3 py-1.5 text-sm" style={{ color: "var(--text-muted)" }}>
          无匹配文件
        </div>
      ) : (
        candidates.map((t, i) => {
          const isActive = i === active;
          return (
            <button
              key={t.path}
              onClick={() => onPick(t)}
              onMouseEnter={() => setActive(i)}
              className="w-full text-left px-3 py-1.5 text-sm block"
              style={{
                color: isActive ? "var(--accent-fg)" : "var(--text-primary)",
                background: isActive ? "var(--accent)" : undefined,
              }}
              title={t.path}
            >
              <span className="truncate flex items-center gap-1.5">
                {iconOf(t)}
                {t.name}
              </span>
            </button>
          );
        })
      )}
    </div>
  );
}
