/**
 * 插件卸载三选一对话框：保留配置卸载（默认）/ 彻底卸载（含配置）/ 取消。
 * 仅用于有落位目录的仓库/Git 来源插件；随应用分发行与本地链接行用普通确认框即可
 * （前者无磁盘数据可保留，后者数据在源目录内、卸载不受影响）。
 */
import { useEffect, useRef } from "react";
import { AlertTriangle } from "lucide-react";
import { useBackHandler } from "@/hooks/useBackHandler";

export function PluginUninstallDialog({
  title,
  description,
  onKeepData,
  onDeleteAll,
  onCancel,
}: {
  title: string;
  description: string;
  /** 保留配置卸载（默认）：插件数据搬到保留区，重装同 id 自动恢复。 */
  onKeepData: () => void;
  /** 彻底卸载：插件目录连同数据一并删除，不可恢复。 */
  onDeleteAll: () => void;
  onCancel: () => void;
}) {
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancelRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useBackHandler(true, () => {
    onCancelRef.current();
    return true;
  });

  return (
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center"
      style={{ background: "var(--scrim)" }}
      onClick={onCancel}
    >
      <div
        className="w-80 max-w-[calc(100vw-2rem)] rounded-[var(--radius-lg)] border shadow-[var(--shadow-pop)] p-4"
        style={{ background: "var(--bg-overlay)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start gap-2 mb-2">
          <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" style={{ color: "var(--warning)" }} />
          <h3 className="text-sm font-medium leading-5" style={{ color: "var(--text-primary)" }}>
            {title}
          </h3>
        </div>
        <p className="text-xs mb-3 whitespace-pre-wrap break-words" style={{ color: "var(--text-muted)" }}>
          {description}
        </p>
        <div className="flex flex-col gap-2 mt-4">
          <button
            onClick={onKeepData}
            className="px-3 py-1.5 text-xs rounded hover:opacity-90"
            style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
          >
            保留配置卸载（重装时自动恢复）
          </button>
          <button
            onClick={onDeleteAll}
            className="px-3 py-1.5 text-xs rounded bg-[var(--danger-fill)] hover:opacity-90 text-white"
          >
            彻底卸载（删除全部数据）
          </button>
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs rounded hover:bg-[var(--hover)]"
            style={{ color: "var(--text-primary)" }}
          >
            取消
          </button>
        </div>
      </div>
    </div>
  );
}
