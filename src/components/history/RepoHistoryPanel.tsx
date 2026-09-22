/**
 * 仓库历史面板（主页）：全仓库版本历史流，按文件分组折叠——
 * 每文件一个可折叠卡片：头部显示图标 + 标题 + 版本数 + 最近时间（点击展开/收起），
 * 收起态显示「最近版本」一行概览，展开态列出该文件全部版本（时间/作者/行为/摘要，点击开 HistoryModal）。
 * 默认只列最近编辑的若干文件（可「显示全部」）。
 *
 * 数据来自 repoHistoryStore（按仓库身份经内容面聚合全部版本，ts 倒序、上限）。
 * 不含「最近文件活动」区——最近文件统一由「最近打开」面板承载，避免与主页重复。
 */
import { ChevronDown, ChevronRight, ExternalLink, History, RefreshCw } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useAppStore, selectVaultIdentityKey } from "@/stores/appStore";
import { useRepoHistoryStore } from "@/stores/repoHistoryStore";
import { FileKindIcon, openFileByKind } from "@/components/common/FileKindIcon";
import { HistoryModal, ACTION_LABEL } from "@/components/history/HistoryModal";
import { noteTitleFromFile } from "@/utils/filename";
import { relTime } from "@/utils/time";
import type { RepoHistoryEntry } from "@/types";

/** 默认只列最近编辑的文件数（「显示全部」可展开全部）。 */
const TOP_FILES_LIMIT = 15;

/**
 * 按文件分组的版本组。组内 entries 保持 Rust feed 的 ts 倒序（隐式契约：最新版 = entries[0]，
 * latestTs = 首条 ts），与 `commands/home.rs` 的 `sort_by(b.ts.cmp(a.ts))` 绑定。
 */
interface VersionGroup {
  file: string;
  kind: "note" | "canvas" | "table";
  latestTs: number;
  entries: RepoHistoryEntry[];
}

/** 单条版本行：时间 + 作者（AI 徽标）+ 行为徽标 + 摘要，点击打开该文件历史。 */
function VersionRow({
  entry,
  onOpenHistory,
}: {
  entry: RepoHistoryEntry;
  onOpenHistory: (t: { kind: "note" | "canvas" | "table"; file: string }) => void;
}) {
  const isAgent = entry.authorId === "ai-agent";
  const kind = entry.kind;
  return (
    <button
      onClick={() => onOpenHistory({ kind, file: entry.file })}
      className="w-full flex items-center gap-1.5 text-[10px] px-1.5 py-1 rounded text-left hover:opacity-90"
      style={{ color: "var(--text-secondary)" }}
      title={`${entry.summary ?? ""}${entry.note ? `（${entry.note}）` : ""}`}
    >
      <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
        {relTime(entry.ts)}
      </span>
      <span
        className="px-1 rounded flex-shrink-0"
        style={{
          color: isAgent ? "var(--accent)" : "var(--text-secondary)",
          border: `1px solid ${isAgent ? "var(--accent)" : "var(--border)"}`,
        }}
      >
        {entry.authorName || entry.authorDevice || "用户"}
      </span>
      <span
        className="px-1 rounded flex-shrink-0"
        style={{ color: "var(--accent-hover)", border: "1px solid var(--border)" }}
      >
        {ACTION_LABEL[entry.action] ?? entry.action}
      </span>
      <span className="truncate flex-1">{entry.summary}</span>
    </button>
  );
}

/** 单个文件版本卡片（可折叠；收起显示最近版本一行概览）。 */
function FileGroup({
  group,
  expanded,
  onToggle,
  onOpenHistory,
}: {
  group: VersionGroup;
  expanded: boolean;
  onToggle: () => void;
  onOpenHistory: (t: { kind: "note" | "canvas" | "table"; file: string }) => void;
}) {
  const latest = group.entries[0];
  return (
    <div className="rounded" style={{ background: "var(--bg-secondary)", border: "1px solid var(--border)" }}>
      <div
        onClick={onToggle}
        className="flex items-center gap-1.5 text-xs px-2 py-1.5 cursor-pointer select-none"
        style={{ color: "var(--text-primary)" }}
        title={group.file}
      >
        <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
          {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        </span>
        <FileKindIcon kind={group.kind} />
        <span className="truncate flex-1">{noteTitleFromFile(group.file)}</span>
        <span className="text-[10px] flex-shrink-0" style={{ color: "var(--text-muted)" }}>
          {group.entries.length} 版 · {relTime(group.latestTs)}
        </span>
        <button
          onClick={(e) => {
            e.stopPropagation();
            openFileByKind(group.file, group.kind);
          }}
          className="w-5 h-5 flex items-center justify-center rounded hover:opacity-80 flex-shrink-0"
          style={{ color: "var(--text-muted)" }}
          title="打开文件"
        >
          <ExternalLink size={11} />
        </button>
      </div>
      {expanded ? (
        <div className="px-2 pb-2 space-y-0.5">
          {group.entries.map((e, i) => (
            <VersionRow key={`${e.kind}-${e.file}-${e.ts}-${e.authorId}-${i}`} entry={e} onOpenHistory={onOpenHistory} />
          ))}
        </div>
      ) : (
        /* latest = 组内最新版本（entries 建组时恒非空） */
        <button
          onClick={() => onOpenHistory({ kind: group.kind, file: group.file })}
          className="w-full flex items-center gap-1 text-[10px] pl-7 pr-2 pb-1.5 truncate text-left hover:opacity-80"
          style={{ color: "var(--text-muted)" }}
        >
          <span className="truncate">
            {relTime(latest.ts)} · {latest.authorName || latest.authorDevice || "用户"} ·{" "}
            {ACTION_LABEL[latest.action] ?? latest.action}
            {latest.summary ? ` · ${latest.summary}` : ""}
          </span>
        </button>
      )}
    </div>
  );
}

/** 仓库历史面板：按文件分组的版本流（可折叠 + 只列最近文件）。 */
export function RepoHistoryPanel() {
  const vaultIdentityKey = useAppStore(selectVaultIdentityKey);
  const entries = useRepoHistoryStore((s) => s.entries);
  const loading = useRepoHistoryStore((s) => s.loading);
  const [historyTarget, setHistoryTarget] = useState<{ kind: "note" | "canvas" | "table"; file: string } | null>(null);
  const [expandedFiles, setExpandedFiles] = useState<Set<string>>(new Set());
  const [showAllFiles, setShowAllFiles] = useState(false);

  // 切仓库/面板挂载：重载版本流 + 清空展开/显示全部（防旧仓库状态残留）
  useEffect(() => {
    void useRepoHistoryStore.getState().load();
    setExpandedFiles(new Set());
    setShowAllFiles(false);
  }, [vaultIdentityKey]);

  const groups = useMemo(() => {
    const map = new Map<string, VersionGroup>();
    for (const e of entries) {
      let g = map.get(e.file);
      if (!g) {
        g = { file: e.file, kind: e.kind, latestTs: e.ts, entries: [] };
        map.set(e.file, g);
      }
      g.entries.push(e);
    }
    return [...map.values()].sort((a, b) => b.latestTs - a.latestTs);
  }, [entries]);

  const visibleGroups = showAllFiles ? groups : groups.slice(0, TOP_FILES_LIMIT);

  const toggleFile = (file: string) => {
    setExpandedFiles((prev) => {
      const next = new Set(prev);
      if (next.has(file)) next.delete(file);
      else next.add(file);
      return next;
    });
  };

  return (
    <div className="h-full w-full flex flex-col" style={{ background: "var(--bg-primary)" }}>
      {/* 头：标题 + 加载指示 + 显示全部文件 */}
      <div className="flex items-center gap-1.5 px-3 py-2 flex-shrink-0 select-none" style={{ borderBottom: "1px solid var(--border)" }}>
        <History size={13} style={{ color: "var(--accent)" }} />
        <span className="text-xs font-medium" style={{ color: "var(--text-primary)" }}>
          仓库历史
        </span>
        {loading && <RefreshCw size={11} className="animate-spin" style={{ color: "var(--text-muted)" }} />}
        {groups.length > TOP_FILES_LIMIT && (
          <button
            onClick={() => setShowAllFiles((v) => !v)}
            className="ml-auto text-[10px] px-1.5 py-0.5 rounded hover:opacity-80"
            style={{ color: "var(--text-secondary)", border: "1px solid var(--border)" }}
          >
            {showAllFiles ? "收起" : `显示全部 ${groups.length} 个文件`}
          </button>
        )}
      </div>

      {/* 按文件分组的版本流 */}
      <div className="flex-1 min-h-0 overflow-auto p-2 space-y-1">
        {visibleGroups.length === 0 ? (
          <div className="py-8 text-center text-xs" style={{ color: "var(--text-muted)" }}>
            {loading ? "加载中…" : "暂无版本历史（编辑保存后自动记录）"}
          </div>
        ) : (
          visibleGroups.map((g) => (
            <FileGroup
              key={g.file}
              group={g}
              expanded={expandedFiles.has(g.file)}
              onToggle={() => toggleFile(g.file)}
              onOpenHistory={setHistoryTarget}
            />
          ))
        )}
      </div>

      {historyTarget && (
        <HistoryModal
          kind={historyTarget.kind}
          file={historyTarget.file}
          open
          onClose={() => setHistoryTarget(null)}
        />
      )}
    </div>
  );
}
