/**
 * 最近打开面板（主页）：当前仓库最近打开的文件（去重置顶、上限），点击打开。
 * 数据来自 uiStateStore.recentFiles（应用级、跨仓库记录），此处按当前仓库身份过滤；
 * 文件已被删除/移动 → 置灰不可点（惰性清理由下次打开去重自然覆盖）。
 */
import { Clock } from "lucide-react";
import { useMemo } from "react";
import { useAppStore, selectVaultIdentityKey } from "@/stores/appStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore } from "@/stores/vaultStore";
import { FileKindIcon, openFileByKind } from "@/components/common/FileKindIcon";
import { noteTitleFromFile } from "@/utils/filename";
import { relTime } from "@/utils/time";
import type { FileTreeNode } from "@/types/canvas";

function collectPaths(nodes: FileTreeNode[], out: Set<string>): void {
  for (const n of nodes) {
    if (n.isDir) collectPaths(n.children, out);
    else out.add(n.path);
  }
}

export function RecentPanel() {
  const canvases = useAppStore((s) => s.canvases);
  const vaultKey = useAppStore(selectVaultIdentityKey);
  const recentFiles = useUiStateStore((s) => s.recentFiles);
  const tree = useVaultStore((s) => s.tree);

  const existing = useMemo(() => {
    const set = new Set<string>();
    collectPaths(tree, set);
    return set;
  }, [tree]);

  // 按仓库身份键过滤（local = root；space = `space:<serverUrl>#<spaceId>`）
  const rows = useMemo(
    () =>
      recentFiles
        .filter((r) => r.vaultKey === vaultKey)
        .sort((a, b) => b.openedAt - a.openedAt),
    [recentFiles, vaultKey],
  );

  return (
    <div className="h-full w-full flex flex-col" style={{ background: "var(--bg-primary)" }}>
      <div className="flex items-center gap-1.5 px-3 py-2 flex-shrink-0 select-none" style={{ borderBottom: "1px solid var(--border)" }}>
        <Clock size={13} style={{ color: "var(--accent)" }} />
        <span className="text-xs font-medium" style={{ color: "var(--text-primary)" }}>
          最近打开
        </span>
        <span className="text-[10px] ml-auto" style={{ color: "var(--text-muted)" }}>
          当前仓库
        </span>
      </div>
      <div className="flex-1 min-h-0 overflow-auto p-2 space-y-0.5">
        {rows.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-1 py-8 text-center px-6">
            <Clock size={20} style={{ color: "var(--text-muted)" }} />
            <div className="text-xs" style={{ color: "var(--text-muted)" }}>
              打开过文件后会显示在这里
            </div>
          </div>
        ) : (
          rows.map((r) => {
            // 已删/已移 = 置灰；画布另需在 canvases 列表命中（打开画布按列表查行，未同步时同样置灰）
            const alive =
              existing.has(r.file) &&
              (r.kind === "canvas" ? canvases.some((c) => c.file === r.file) : true);
            return (
              <button
                key={`${r.vaultKey}-${r.file}`}
                disabled={!alive}
                onClick={() => openFileByKind(r.file, r.kind)}
                className="w-full flex items-center gap-1.5 text-xs px-1.5 py-1 rounded text-left disabled:opacity-40 disabled:cursor-default hover:opacity-80"
                style={{ color: "var(--text-secondary)" }}
                title={alive ? r.file : `${r.file}（已不存在）`}
              >
                <FileKindIcon kind={r.kind} />
                <span className="truncate flex-1">{noteTitleFromFile(r.file)}</span>
                <span className="text-[10px] flex-shrink-0" style={{ color: "var(--text-muted)" }}>
                  {relTime(r.openedAt)}
                </span>
              </button>
            );
          })
        )}
      </div>
    </div>
  );
}
