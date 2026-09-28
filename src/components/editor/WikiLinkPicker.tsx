/**
 * 双链候选浮层：编辑器内光标处于未闭合的 `[[查询词` / `【【查询词` 片段时原地弹出，
 * 列出全仓库 `.md` 笔记（vaultStore noteList），继续键入按文件名实时过滤。
 * 查询词无同名笔记命中且 onCreate 可用时，列表末尾追加「新建笔记」项——选中即创建
 * 并按新文件路径回填插入链接。
 * 键盘：↑/↓ 循环高亮、Enter 选中、Esc 关闭——document 捕获阶段拦截，
 * 防方向键/Enter 落入 CodeMirror（移动光标/换行）；IME 组合期按键不拦截。
 * 壳 = PopupLayer（portal + 实测钳制/翻转 + 外点关闭）；关闭语义（粘滞等）归调用方。
 */
import { FileText, Plus } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { PopupLayer, type PopupAnchor } from "@/components/common/PopupLayer";
import { useVaultStore } from "@/stores/vaultStore";

/** 候选上限（超大仓库防超长渲染；过滤词收窄可见性）。 */
const MAX_CANDIDATES = 50;

/** 候选项：仓库笔记或「新建笔记」动作项。 */
type PickerItem =
  | { kind: "note"; file: string; name: string }
  | { kind: "create"; name: string };

interface Props {
  /** 当前查询词（触发符到光标之间的文本）。 */
  query: string;
  anchor: PopupAnchor;
  /** 选中笔记候选：file = 仓库相对路径，name = 含扩展名文件名（新建项创建成功后同样回调）。 */
  onPick: (file: string, name: string) => void;
  /** 新建名为 query 的笔记（返回新文件相对路径；失败 = null）。未提供时不出新建项。 */
  onCreate?: (name: string) => Promise<string | null>;
  onClose: () => void;
}

export function WikiLinkPicker({ query, anchor, onPick, onCreate, onClose }: Props) {
  const noteList = useVaultStore((s) => s.noteList);
  const [active, setActive] = useState(0);
  // active 供键盘监听读取（ref 避免监听随 active 每击键重绑）
  const activeRef = useRef(active);
  activeRef.current = active;
  /** 新建笔记在途（防创建窗口内重复点击建出第二篇笔记）。 */
  const creatingRef = useRef(false);

  // 文件名子串过滤（忽略大小写），保持 noteList 树序
  const candidates = useMemo(() => {
    const q = query.trim().toLowerCase();
    const matched = q ? noteList.filter((n) => n.name.toLowerCase().includes(q)) : noteList;
    return matched.slice(0, MAX_CANDIDATES);
  }, [noteList, query]);

  // 新建项：onCreate 可用 + 查询词非空 + 无同名笔记命中（大小写不敏感：Windows/APFS 上
  // `NOTE` 与已有的 `note.md` 是同一文件，放行新建会用空内容覆盖已有笔记）
  const trimmed = query.trim();
  const nameEquals = (n: string) => n.replace(/\.md$/i, "").toLowerCase() === trimmed.toLowerCase();
  const canCreate = !!onCreate && trimmed !== "" && !noteList.some((n) => nameEquals(n.name));

  const items = useMemo<PickerItem[]>(() => {
    const notes: PickerItem[] = candidates.map((n) => ({ kind: "note", file: n.file, name: n.name }));
    return canCreate ? [...notes, { kind: "create", name: trimmed }] : notes;
  }, [candidates, canCreate, trimmed]);

  /** 选中项：笔记直选；新建项创建成功后按新文件路径走同一回填插入（在途期间忽略重复点击）。 */
  const pickItem = useCallback(
    (it: PickerItem | undefined) => {
      if (!it || creatingRef.current) return;
      if (it.kind === "note") {
        onPick(it.file, it.name);
        return;
      }
      creatingRef.current = true;
      void onCreate?.(it.name)
        .then((file) => {
          // 创建失败（无权限/路径非法）：保持浮层打开可重试
          if (!file) return;
          // 显示名取实际落盘文件（被去重/净化时 ≠ 查询词）
          onPick(file, file.split("/").pop()?.replace(/\.md$/i, "") ?? it.name);
        })
        .catch(() => {})
        .finally(() => {
          creatingRef.current = false;
        });
    },
    [onPick, onCreate],
  );

  // 过滤词变化时高亮回到第一项
  useEffect(() => {
    setActive(0);
  }, [query]);

  // 候选收缩时高亮越界兜底：回退到末项
  useEffect(() => {
    setActive((i) => (items.length ? Math.min(i, items.length - 1) : 0));
  }, [items.length]);

  // 键盘导航（捕获阶段拦截，防编辑器默认行为；IME 组合期间键是候选词操作，不拦截）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowDown") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        setActive((i) => (items.length ? (i + 1) % items.length : 0));
      } else if (e.key === "ArrowUp") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        setActive((i) => (items.length ? (i - 1 + items.length) % items.length : 0));
      } else if (e.key === "Enter") {
        if (e.isComposing) return;
        e.preventDefault();
        e.stopPropagation();
        pickItem(items[activeRef.current]);
      } else if (e.key === "Escape") {
        if (e.isComposing) return;
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [items, pickItem, onClose]);

  return (
    <PopupLayer
      anchor={anchor}
      onClose={onClose}
      widthClass="w-72"
      contentClassName="py-1"
      // 候选数量变化（浮层高度增减）时重新钳制定位，防向下溢出视口
      repositionDeps={[items.length]}
    >
      <div className="max-h-64 overflow-y-auto">
        {items.length === 0 ? (
          <div className="px-3 py-1.5 text-sm" style={{ color: "var(--text-muted)" }}>
            无匹配笔记
          </div>
        ) : (
          items.map((it, i) => {
            const isActive = i === active;
            const isCreate = it.kind === "create";
            return (
              <button
                key={it.kind === "note" ? it.file : `create:${it.name}`}
                onClick={() => pickItem(it)}
                onMouseEnter={() => setActive(i)}
                className="w-full text-left px-3 py-1.5 text-sm block"
                style={{
                  color: isActive ? "#fff" : isCreate ? "var(--accent)" : "var(--text-primary)",
                  background: isActive ? "var(--accent)" : undefined,
                }}
                title={it.kind === "note" ? it.file : `新建笔记「${it.name}」并插入链接`}
              >
                <span className="truncate flex items-center gap-1.5">
                  {it.kind === "note" ? (
                    <FileText size={14} className="flex-shrink-0" />
                  ) : (
                    <Plus size={14} className="flex-shrink-0" />
                  )}
                  {it.kind === "note" ? it.name.replace(/\.md$/i, "") : `新建笔记「${it.name}」`}
                </span>
              </button>
            );
          })
        )}
      </div>
    </PopupLayer>
  );
}
