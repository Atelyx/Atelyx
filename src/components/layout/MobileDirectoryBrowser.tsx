/**
 * 移动端目录浏览（纯展示）：从外部存储根逐级进入并选定一个文件夹（用作仓库根）。
 *
 * 安卓 WebView 内没有系统文件夹选择器，故自研；数据与动作由入口对话框透传（其编排在
 * mobileVaultStore）。作为浮层的一层登记返回处理器：返回键先关它，不落到上一层。
 */
import { ArrowUp, Folder, Loader2, X } from "lucide-react";
import { IconButton } from "@/components/common/Button";
import { useBackHandler } from "@/hooks/useBackHandler";
import type { AbsoluteDirListing } from "@/types";

export function MobileDirectoryBrowser({
  listing,
  loading,
  error,
  onEnter,
  onPick,
  onClose,
}: {
  listing: AbsoluteDirListing | null;
  loading: boolean;
  error: string;
  /** 进入某个子目录。 */
  onEnter: (path: string) => void;
  /** 选用当前文件夹。 */
  onPick: (path: string) => void;
  onClose: () => void;
}) {
  useBackHandler(true, () => {
    onClose();
    return true;
  });

  return (
    <div
      className="fixed inset-0 z-[190] flex flex-col"
      style={{ background: "var(--bg-primary)", paddingTop: "env(safe-area-inset-top)" }}
      role="dialog"
      aria-modal="true"
      aria-label="选择文件夹"
    >
      <div
        className="flex-shrink-0 flex items-center gap-2 px-2 h-12 border-b"
        style={{ borderColor: "var(--border-subtle)", background: "var(--bg-secondary)" }}
      >
        <IconButton icon={<X size={18} />} label="关闭" size="touch" onClick={onClose} />
        <span
          className="flex-1 min-w-0 truncate text-xs"
          style={{ color: "var(--text-secondary)", fontFamily: "var(--font-mono)" }}
        >
          {listing?.path ?? "…"}
        </span>
        <IconButton
          icon={<ArrowUp size={18} />}
          label="返回上级"
          size="touch"
          disabled={!listing?.parent}
          onClick={() => listing?.parent && onEnter(listing.parent)}
        />
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        {loading && (
          <div className="flex items-center justify-center gap-2 py-6 text-xs" style={{ color: "var(--text-muted)" }}>
            <Loader2 size={14} className="animate-spin" />
            读取中…
          </div>
        )}
        {!loading && error && (
          <div className="px-4 py-6 text-xs break-words" style={{ color: "var(--danger)" }}>
            {error}
          </div>
        )}
        {!loading && !error && listing?.entries.length === 0 && (
          <div className="px-4 py-6 text-xs" style={{ color: "var(--text-muted)" }}>
            该目录下没有子文件夹
          </div>
        )}
        {!loading &&
          listing?.entries.map((entry) => (
            <button
              key={entry.path}
              onClick={() => onEnter(entry.path)}
              disabled={!entry.readable}
              title={entry.path}
              className="w-full flex items-center gap-2.5 px-3 h-12 text-left rounded-sm disabled:opacity-40"
              style={{ color: "var(--text-secondary)" }}
            >
              <Folder size={16} className="flex-shrink-0" style={{ color: "var(--text-muted)" }} />
              <span className="truncate flex-1 text-sm">{entry.name}</span>
            </button>
          ))}
      </div>

      <div
        className="flex-shrink-0 p-2 border-t"
        style={{
          borderColor: "var(--border-subtle)",
          background: "var(--bg-secondary)",
          paddingBottom: "calc(env(safe-area-inset-bottom) + 0.5rem)",
        }}
      >
        <button
          onClick={() => listing && onPick(listing.path)}
          disabled={!listing}
          className="w-full h-11 rounded-sm text-sm font-medium disabled:opacity-50"
          style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
        >
          选用此文件夹
        </button>
      </div>
    </div>
  );
}
