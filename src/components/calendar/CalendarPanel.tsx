/**
 * 日历面板（主页）：月视图，一格里聚合三类信息——
 * - 活动密度（当日历史版本数 + 文件 mtime 改动数，底部活动条）；
 * - 手动日程（`.atelyx/calendar.json`，色块 chip，点击进入编辑）；
 * - 带日期笔记（frontmatter `date`/`due` 自动标出，点击打开笔记）。
 * 底部编辑区：选中某天后增/改/删手动日程（改色循环 + 标题行内编辑 + 删除）。
 * 顶部过滤开关可分别隐藏 活动/日程/笔记。
 */
import {
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  FileText,
  Plus,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useCalendarStore } from "@/stores/calendarStore";
import { useRepoHistoryStore } from "@/stores/repoHistoryStore";
import { useVaultStore } from "@/stores/vaultStore";
import { CALENDAR_ITEM_COLORS } from "@/constants/calendar";
import { pad2 } from "@/utils/time";
import type { CalendarItem, DatedNote } from "@/types";
import type { FileTreeNode } from "@/types/canvas";

const ymd = (y: number, m: number, d: number) => `${y}-${pad2(m + 1)}-${pad2(d)}`;
const todayYmd = () => {
  const t = new Date();
  return ymd(t.getFullYear(), t.getMonth(), t.getDate());
};
const WEEKDAYS = ["一", "二", "三", "四", "五", "六", "日"];

/** 月视图格子（周一开头；首尾补齐到整周，行数视当月而定）。 */
function monthCells(year: number, month: number): Array<string | null> {
  const first = new Date(year, month, 1);
  const firstDay = (first.getDay() + 6) % 7; // 周一 = 0
  const daysInMonth = new Date(year, month + 1, 0).getDate();
  const cells: Array<string | null> = [];
  for (let i = 0; i < firstDay; i++) cells.push(null);
  for (let d = 1; d <= daysInMonth; d++) cells.push(ymd(year, month, d));
  while (cells.length % 7 !== 0) cells.push(null);
  return cells;
}

function cycleColor(c: string): string {
  const i = CALENDAR_ITEM_COLORS.indexOf(c);
  return CALENDAR_ITEM_COLORS[(i + 1) % CALENDAR_ITEM_COLORS.length] ?? CALENDAR_ITEM_COLORS[0];
}

export function CalendarPanel() {
  // 重载按仓库身份触发（身份键语义）：空间模式下 vaultRoot 恒 null，按 root 依赖会让
  // 空间 A→B 切换时不触发重载、面板停留在 A 的数据
  const vaultIdentity = useAppStore((s) => s.vaultIdentity);
  const openNote = useAppStore((s) => s.openNote);
  const items = useCalendarStore((s) => s.items);
  const datedNotes = useCalendarStore((s) => s.datedNotes);
  const addItem = useCalendarStore((s) => s.addItem);
  const updateItem = useCalendarStore((s) => s.updateItem);
  const removeItem = useCalendarStore((s) => s.removeItem);
  const dailyCounts = useRepoHistoryStore((s) => s.dailyCounts);
  const tree = useVaultStore((s) => s.tree);

  // 切仓库/面板挂载：重载手动日程 + 带日期笔记 + 仓库历史（活动计数）；清空选中日期防残留上一仓库
  useEffect(() => {
    void useCalendarStore.getState().load();
    void useRepoHistoryStore.getState().load();
    setSelectedDate(null);
  }, [vaultIdentity]);

  const now = new Date();
  const [viewYear, setViewYear] = useState(now.getFullYear());
  const [viewMonth, setViewMonth] = useState(now.getMonth());
  const [selectedDate, setSelectedDate] = useState<string | null>(null);
  const [filters, setFilters] = useState({ activity: true, schedule: true, notes: true });
  const [draftTitle, setDraftTitle] = useState("");
  const [draftNote, setDraftNote] = useState("");
  const [draftColor, setDraftColor] = useState(CALENDAR_ITEM_COLORS[0]);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingDraft, setEditingDraft] = useState("");

  const cells = useMemo(() => monthCells(viewYear, viewMonth), [viewYear, viewMonth]);
  const tYmd = todayYmd();

  const itemsByDate = useMemo(() => {
    const map = new Map<string, CalendarItem[]>();
    for (const it of items) {
      const arr = map.get(it.date) ?? [];
      arr.push(it);
      map.set(it.date, arr);
    }
    return map;
  }, [items]);

  const notesByDate = useMemo(() => {
    const map = new Map<string, DatedNote[]>();
    const push = (d: string, n: DatedNote) => {
      const arr = map.get(d) ?? [];
      arr.push(n);
      map.set(d, arr);
    };
    for (const n of datedNotes) {
      if (n.date) push(n.date, n);
      // due 与 date 同日时只计一次（防同一笔记在某天重复出现）
      if (n.due && n.due !== n.date) push(n.due, n);
    }
    return map;
  }, [datedNotes]);

  /** 活动密度：历史版本按日计数 + 仓库文件 mtime 按日计数（尽力而为，缺失不显示）。 */
  const activityByDate = useMemo(() => {
    const map = new Map<string, number>();
    for (const dc of dailyCounts) map.set(dc.date, (map.get(dc.date) ?? 0) + dc.count);
    const walk = (nodes: FileTreeNode[]) => {
      for (const n of nodes) {
        if (n.isDir) walk(n.children);
        else if (n.updatedAt) {
          const d = new Date(n.updatedAt * 1000);
          const key = ymd(d.getFullYear(), d.getMonth(), d.getDate());
          map.set(key, (map.get(key) ?? 0) + 1);
        }
      }
    };
    walk(tree);
    return map;
  }, [dailyCounts, tree]);

  const visibleCounts = cells.filter(Boolean).map((c) => activityByDate.get(c!) ?? 0);
  const maxCount = Math.max(1, ...visibleCounts);

  const goMonth = (delta: number) => {
    let y = viewYear;
    let m = viewMonth + delta;
    if (m < 0) { m = 11; y--; }
    if (m > 11) { m = 0; y++; }
    setViewYear(y);
    setViewMonth(m);
  };

  const goToday = () => {
    const t = new Date();
    setViewYear(t.getFullYear());
    setViewMonth(t.getMonth());
    setSelectedDate(tYmd);
  };

  const submitAdd = () => {
    if (!selectedDate || !draftTitle.trim()) return;
    addItem(selectedDate, draftTitle.trim(), draftNote, draftColor);
    setDraftTitle("");
    setDraftNote("");
  };

  const commitEdit = (id: string) => {
    const trimmed = editingDraft.trim();
    if (trimmed) updateItem(id, { title: trimmed });
    setEditingId(null);
    setEditingDraft("");
  };

  const selectedItems = selectedDate ? itemsByDate.get(selectedDate) ?? [] : [];
  const selectedNotes = selectedDate ? notesByDate.get(selectedDate) ?? [] : [];

  return (
    <div className="h-full w-full flex flex-col" style={{ background: "var(--bg-primary)" }}>
      {/* 顶部：年月导航 + 今天 + 过滤开关 */}
      <div className="flex items-center gap-1 px-2 py-1.5 flex-shrink-0 select-none" style={{ borderBottom: "1px solid var(--border)" }}>
        <button onClick={() => goMonth(-1)} className="w-6 h-6 flex items-center justify-center rounded hover:opacity-80" style={{ color: "var(--text-secondary)" }} title="上一月">
          <ChevronLeft size={14} />
        </button>
        <button onClick={() => goMonth(1)} className="w-6 h-6 flex items-center justify-center rounded hover:opacity-80" style={{ color: "var(--text-secondary)" }} title="下一月">
          <ChevronRight size={14} />
        </button>
        <span className="text-sm font-medium min-w-[90px] text-center" style={{ color: "var(--text-primary)" }}>
          {viewYear} 年 {viewMonth + 1} 月
        </span>
        <button
          onClick={goToday}
          className="text-[10px] px-1.5 py-0.5 rounded hover:opacity-80"
          style={{ color: "var(--text-secondary)", border: "1px solid var(--border)" }}
          title="回到今天"
        >
          今天
        </button>
        <div className="ml-auto flex items-center gap-1.5">
          {(
            [
              ["activity", "活动"],
              ["schedule", "日程"],
              ["notes", "笔记"],
            ] as const
          ).map(([key, label]) => (
            <button
              key={key}
              onClick={() => setFilters((f) => ({ ...f, [key]: !f[key] }))}
              className="text-[10px] px-1.5 py-0.5 rounded"
              style={{
                color: filters[key] ? "var(--accent-fg)" : "var(--text-muted)",
                background: filters[key] ? "var(--accent)" : "transparent",
                border: "1px solid var(--border)",
              }}
              title={`${filters[key] ? "隐藏" : "显示"}${label}`}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* 周头 */}
      <div className="grid grid-cols-7 gap-0.5 px-2 pt-1 flex-shrink-0">
        {WEEKDAYS.map((w) => (
          <div key={w} className="text-center text-[10px]" style={{ color: "var(--text-muted)" }}>
            {w}
          </div>
        ))}
      </div>

      {/* 月网格 */}
      <div className="flex-1 min-h-0 overflow-auto p-2">
        <div className="grid grid-cols-7 gap-0.5">
          {cells.map((date, idx) => {
            if (!date) return <div key={`empty-${idx}`} className="min-h-[56px] rounded" style={{ background: "transparent" }} />;
            const day = Number(date.slice(8));
            const isToday = date === tYmd;
            const isSelected = date === selectedDate;
            const dayItems = itemsByDate.get(date) ?? [];
            const dayNotes = notesByDate.get(date) ?? [];
            const count = activityByDate.get(date) ?? 0;
            return (
              <div
                key={date}
                onClick={() => setSelectedDate(date)}
                className="relative rounded px-1 py-0.5 cursor-pointer select-none overflow-hidden min-h-[56px]"
                style={{
                  background: isToday ? "color-mix(in srgb, var(--accent) 8%, transparent)" : "var(--bg-secondary)",
                  outline: isSelected ? "1px solid var(--accent)" : "1px solid var(--border)",
                }}
              >
                <span className="text-[10px]" style={{ color: isToday ? "var(--accent)" : "var(--text-secondary)" }}>
                  {day}
                </span>
                <div className="mt-0.5 space-y-0.5">
                  {filters.schedule &&
                    dayItems.slice(0, 2).map((it) => (
                      <div
                        key={it.id}
                        className="flex items-center gap-1 text-[10px] rounded px-0.5 truncate"
                        style={{ color: it.color ?? "var(--text-secondary)", background: "color-mix(in srgb, " + (it.color ?? "#888") + " 15%, transparent)" }}
                        title={it.title}
                      >
                        <span className="w-1.5 h-1.5 rounded-full flex-shrink-0" style={{ background: it.color ?? "var(--accent)" }} />
                        <span className="truncate">{it.title}</span>
                      </div>
                    ))}
                  {filters.schedule && dayItems.length > 2 && (
                    <div className="text-[10px] pl-2" style={{ color: "var(--text-muted)" }}>
                      +{dayItems.length - 2}
                    </div>
                  )}
                  {filters.notes && dayNotes.length > 0 && (
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        const n = dayNotes[0];
                        openNote(n.file, n.title);
                      }}
                      className="flex items-center gap-1 text-[10px] px-0.5 rounded hover:opacity-80"
                      style={{ color: "var(--accent-hover)" }}
                      title={`来自笔记：${dayNotes.map((n) => n.title).join("、")}`}
                    >
                      <FileText size={12} />
                      <span className="truncate">{dayNotes.length > 1 ? `${dayNotes.length} 篇笔记` : dayNotes[0].title}</span>
                    </button>
                  )}
                </div>
                {filters.activity && count > 0 && (
                  <div
                    className="absolute bottom-0.5 left-1 h-[3px] rounded"
                    style={{
                      width: `${Math.max(12, Math.round((count / maxCount) * 80))}%`,
                      background: "color-mix(in srgb, var(--accent) 60%, transparent)",
                    }}
                  />
                )}
              </div>
            );
          })}
        </div>
      </div>

      {/* 底部编辑区（选中某天后）：添加 + 改/删当天日程 */}
      {selectedDate && (
        <div className="flex-shrink-0 border-t px-3 py-2 space-y-1.5" style={{ borderColor: "var(--border)", background: "var(--bg-secondary)" }}>
          <div className="flex items-center gap-1.5">
            <CalendarDays size={12} style={{ color: "var(--accent)" }} />
            <span className="text-xs font-medium" style={{ color: "var(--text-primary)" }}>
              {selectedDate}
            </span>
            <button onClick={() => setSelectedDate(null)} className="ml-auto w-5 h-5 flex items-center justify-center rounded hover:opacity-80" style={{ color: "var(--text-muted)" }} title="关闭">
              <X size={12} />
            </button>
          </div>

          {/* 当天已有手动日程：改色循环 + 标题行内编辑 + 删除 */}
          {selectedItems.length > 0 && (
            <div className="space-y-1">
              {selectedItems.map((it) => (
                <div key={it.id} className="flex items-center gap-1.5 text-xs">
                  <button
                    onClick={() => updateItem(it.id, { color: cycleColor(it.color ?? CALENDAR_ITEM_COLORS[0]) })}
                    className="w-3 h-3 rounded-full flex-shrink-0 hover:opacity-80"
                    style={{ background: it.color ?? "var(--accent)" }}
                    title="切换颜色"
                  />
                  {editingId === it.id ? (
                    <input
                      autoFocus
                      value={editingDraft}
                      onChange={(e) => setEditingDraft(e.target.value)}
                      onBlur={() => commitEdit(it.id)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") commitEdit(it.id);
                        if (e.key === "Escape") setEditingId(null);
                      }}
                      className="flex-1 min-w-0 bg-transparent border-b outline-none text-xs"
                      style={{ color: "var(--text-primary)", borderColor: "var(--accent)" }}
                    />
                  ) : (
                    <button
                      onClick={() => {
                        setEditingId(it.id);
                        setEditingDraft(it.title);
                      }}
                      className="flex-1 min-w-0 text-left truncate hover:opacity-80"
                      style={{ color: "var(--text-primary)" }}
                      title={it.note ?? it.title}
                    >
                      {it.title}
                      {it.note ? <span className="ml-1 text-[10px]" style={{ color: "var(--text-muted)" }}>{it.note}</span> : null}
                    </button>
                  )}
                  <button onClick={() => removeItem(it.id)} className="w-5 h-5 flex items-center justify-center rounded hover:opacity-80" style={{ color: "var(--text-muted)" }} title="删除">
                    <Trash2 size={11} />
                  </button>
                </div>
              ))}
            </div>
          )}

          {/* 当天带日期笔记（点击打开） */}
          {selectedNotes.length > 0 && (
            <div className="flex items-center gap-1.5 flex-wrap text-xs">
              <span className="text-[10px]" style={{ color: "var(--text-muted)" }}>来自笔记：</span>
              {selectedNotes.map((n) => (
                <button
                  key={n.file}
                  onClick={() => openNote(n.file, n.title)}
                  className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:opacity-80"
                  style={{ color: "var(--accent-hover)", border: "1px solid var(--border)" }}
                >
                  <FileText size={9} />
                  {n.title}
                </button>
              ))}
            </div>
          )}

          {/* 添加表单（标题 + 可选备注 + 颜色） */}
          <div className="space-y-1">
            <div className="flex items-center gap-1.5">
              <input
                value={draftTitle}
                onChange={(e) => setDraftTitle(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && submitAdd()}
                placeholder="添加日程标题…"
                className="flex-1 min-w-0 text-xs rounded px-2 py-1 outline-none focus:ring-1 focus:ring-[var(--accent)]"
                style={{ color: "var(--text-primary)", background: "var(--input-bg)", border: "1px solid var(--input-border)" }}
              />
              <div className="flex items-center gap-0.5">
                {CALENDAR_ITEM_COLORS.map((c) => (
                  <button
                    key={c}
                    onClick={() => setDraftColor(c)}
                    className="w-3 h-3 rounded-full"
                    style={{ background: c, outline: draftColor === c ? "1px solid var(--accent)" : "1px solid var(--border)" }}
                    title="选择颜色"
                  />
                ))}
              </div>
              <button
                onClick={submitAdd}
                disabled={!draftTitle.trim()}
                className="flex items-center gap-1 text-xs px-2 py-1 rounded disabled:opacity-50"
                style={{ color: "var(--accent-fg)", background: "var(--accent)" }}
              >
                <Plus size={11} />
                添加
              </button>
            </div>
            <input
              value={draftNote}
              onChange={(e) => setDraftNote(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submitAdd()}
              placeholder="备注（可选）…"
              className="w-full text-xs rounded px-2 py-1 outline-none focus:ring-1 focus:ring-[var(--accent)]"
              style={{ color: "var(--text-primary)", background: "var(--input-bg)", border: "1px solid var(--input-border)" }}
            />
          </div>
        </div>
      )}
    </div>
  );
}
