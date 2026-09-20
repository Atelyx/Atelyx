/**
 * 协作空间成员管理弹窗（文件面板空间条目右键「成员管理」）：
 * 名册（名字 + 角色 owner/editor）+ 移除成员 + 转让 owner（服务端校验权限）。
 * 打开即拉取名册；操作成功后自动刷新。
 */
import { useEffect, useState } from "react";
import { Loader2, ShieldCheck, X } from "lucide-react";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";

interface Props {
  serverUrl: string;
  spaceId: string;
  spaceName: string;
  onClose: () => void;
}

/** 成员角色 → 中文徽标文案（未知值原样显示）。 */
function roleLabel(role: string): string {
  if (role === "owner") return "所有者";
  if (role === "editor") return "编辑者";
  return role || "成员";
}

export function SpaceMembersDialog({ serverUrl, spaceId, spaceName, onClose }: Props) {
  const members = useSpaceDirectoryStore((s) => s.members);
  const membersLoading = useSpaceDirectoryStore((s) => s.membersLoading);
  const membersError = useSpaceDirectoryStore((s) => s.membersError);
  const loadMembers = useSpaceDirectoryStore((s) => s.loadMembers);
  const removeMember = useSpaceDirectoryStore((s) => s.removeMember);
  const transferOwnership = useSpaceDirectoryStore((s) => s.transferOwnership);

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmTransfer, setConfirmTransfer] = useState<string | null>(null);

  useEffect(() => {
    void loadMembers(serverUrl, spaceId);
  }, [loadMembers, serverUrl, spaceId]);

  const act = (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    fn()
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setBusy(false));
  };

  return (
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onClick={onClose}
    >
      <div
        className="w-[420px] max-h-[70vh] flex flex-col rounded-lg border shadow-xl"
        style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <header className="px-4 py-3 border-b flex items-center justify-between" style={{ borderColor: "var(--border)" }}>
          <h3 className="text-sm font-medium" style={{ color: "var(--text-primary)" }}>
            成员管理 · {spaceName}
          </h3>
          <button onClick={onClose} style={{ color: "var(--text-muted)" }} className="hover:opacity-80">
            <X size={14} />
          </button>
        </header>

        <div className="flex-1 overflow-auto p-3 space-y-1.5">
          {membersError && (
            <div className="text-xs mb-2" style={{ color: "#f87171" }}>
              {membersError}
            </div>
          )}
          {membersLoading && members.length === 0 ? (
            <div className="flex items-center justify-center gap-2 py-6 text-xs" style={{ color: "var(--text-muted)" }}>
              <Loader2 size={14} className="animate-spin" />
              加载成员中…
            </div>
          ) : (
            members.map((m) => (
              <div key={m.userId} className="flex items-center gap-2 px-2 py-1.5 rounded hover:bg-[var(--hover)]">
                <div className="flex-1 min-w-0">
                  <div className="text-xs truncate" style={{ color: "var(--text-primary)" }}>
                    {m.displayName || m.username}
                  </div>
                  <div className="text-[10px]" style={{ color: "var(--text-muted)" }}>
                    @{m.username}
                  </div>
                </div>
                <span
                  className="text-[10px] px-1.5 py-0.5 rounded flex-shrink-0"
                  style={{ color: "var(--text-muted)", border: "1px solid var(--border)" }}
                >
                  {roleLabel(m.role)}
                </span>
                {m.role !== "owner" && (
                  <>
                    <button
                      onClick={() => setConfirmTransfer(m.userId)}
                      disabled={busy}
                      title="转让后对方成为空间所有者"
                      className="text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40 flex-shrink-0"
                      style={{ color: "var(--text-secondary)" }}
                    >
                      <ShieldCheck size={12} className="inline mr-0.5" />
                      转让
                    </button>
                    <button
                      onClick={() => act(() => removeMember(serverUrl, spaceId, m.userId))}
                      disabled={busy}
                      title="将该成员移出空间"
                      className="text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40 flex-shrink-0"
                      style={{ color: "#f87171" }}
                    >
                      移除
                    </button>
                  </>
                )}
                {confirmTransfer === m.userId && (
                  <span className="flex items-center gap-1 flex-shrink-0">
                    <button
                      onClick={() => {
                        const to = m.userId;
                        setConfirmTransfer(null);
                        act(() => transferOwnership(serverUrl, spaceId, to));
                      }}
                      className="text-[11px] px-1.5 py-0.5 rounded"
                      style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
                    >
                      确认转让
                    </button>
                    <button
                      onClick={() => setConfirmTransfer(null)}
                      className="text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)]"
                      style={{ color: "var(--text-muted)" }}
                    >
                      取消
                    </button>
                  </span>
                )}
              </div>
            ))
          )}
        </div>

        {error && (
          <div className="px-4 py-2 border-t text-xs" style={{ borderColor: "var(--border)", color: "#f87171" }}>
            {error}
          </div>
        )}
      </div>
    </div>
  );
}
