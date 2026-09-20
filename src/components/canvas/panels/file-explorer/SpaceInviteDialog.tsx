/**
 * 协作空间邀请码弹窗（文件面板空间条目右键「邀请码」）：
 * 服务端邀请表单照实暴露——角色两档（owner/editor）+ 可选过期时长/可用次数；
 * 生成后展示可复制的 code 与过期/次数，并可撤销该邀请码。
 */
import { useState } from "react";
import { Copy, Loader2, X } from "lucide-react";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import { useAppStore } from "@/stores/appStore";
import type { InviteInfo } from "@/types";

interface Props {
  serverUrl: string;
  spaceId: string;
  spaceName: string;
  onClose: () => void;
}

function fmtExpiry(invite: InviteInfo): string {
  if (!invite.expiresAt) return "永久有效";
  return `过期于 ${new Date(invite.expiresAt).toLocaleString()}`;
}

function fmtUses(invite: InviteInfo): string {
  if (invite.maxUses == null) return "不限次数";
  return `限用 ${invite.maxUses} 次`;
}

export function SpaceInviteDialog({ serverUrl, spaceId, spaceName, onClose }: Props) {
  const createInvite = useSpaceDirectoryStore((s) => s.createInvite);
  const revokeInvite = useSpaceDirectoryStore((s) => s.revokeInvite);
  const writeClipboardText = useAppStore((s) => s.writeClipboardText);

  const [role, setRole] = useState("editor");
  const [expiresInHours, setExpiresInHours] = useState("");
  const [maxUses, setMaxUses] = useState("");
  const [invite, setInvite] = useState<InviteInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const generate = async () => {
    setBusy(true);
    setError(null);
    try {
      const body: Parameters<typeof createInvite>[2] = { role };
      const hours = Number(expiresInHours);
      if (expiresInHours.trim() && Number.isFinite(hours) && hours > 0) body.expiresInHours = hours;
      const uses = Number(maxUses);
      if (maxUses.trim() && Number.isFinite(uses) && uses > 0) body.maxUses = uses;
      setInvite(await createInvite(serverUrl, spaceId, body));
      setCopied(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const copyCode = async () => {
    if (!invite) return;
    try {
      await writeClipboardText(invite.code);
      setCopied(true);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const revoke = async () => {
    if (!invite) return;
    setBusy(true);
    setError(null);
    try {
      await revokeInvite(serverUrl, spaceId, invite.code);
      setInvite(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const inputStyle = {
    color: "var(--text-primary)",
    background: "var(--input-bg)",
    border: "1px solid var(--input-border)",
  } as const;

  return (
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onClick={onClose}
    >
      <div
        className="w-[380px] rounded-lg border shadow-xl"
        style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <header className="px-4 py-3 border-b flex items-center justify-between" style={{ borderColor: "var(--border)" }}>
          <h3 className="text-sm font-medium" style={{ color: "var(--text-primary)" }}>
            邀请码 · {spaceName}
          </h3>
          <button onClick={onClose} style={{ color: "var(--text-muted)" }} className="hover:opacity-80">
            <X size={14} />
          </button>
        </header>

        <div className="p-4 space-y-3">
          <div className="flex items-center gap-2 text-xs">
            <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
              角色
            </span>
            <select
              value={role}
              onChange={(e) => setRole(e.target.value)}
              className="text-xs rounded px-2 py-1 outline-none flex-1"
              style={inputStyle}
            >
              <option value="editor">编辑者（可编辑内容）</option>
              <option value="owner">所有者（可管理成员与邀请）</option>
            </select>
          </div>
          <div className="flex items-center gap-2 text-xs">
            <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
              过期（小时）
            </span>
            <input
              value={expiresInHours}
              onChange={(e) => setExpiresInHours(e.target.value)}
              placeholder="留空 = 服务端默认"
              className="text-xs rounded px-2 py-1 outline-none flex-1 min-w-0"
              style={inputStyle}
            />
            <span className="flex-shrink-0" style={{ color: "var(--text-muted)" }}>
              次数
            </span>
            <input
              value={maxUses}
              onChange={(e) => setMaxUses(e.target.value)}
              placeholder="留空 = 不限"
              className="text-xs rounded px-2 py-1 outline-none flex-1 min-w-0"
              style={inputStyle}
            />
          </div>

          {invite && (
            <div className="rounded px-2.5 py-2 space-y-1" style={{ background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}>
              <div className="flex items-center gap-2">
                <code className="text-sm flex-1 truncate" style={{ color: "var(--text-primary)" }}>
                  {invite.code}
                </code>
                <button
                  onClick={() => void copyCode()}
                  title="复制邀请码"
                  className="flex items-center gap-1 text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)] flex-shrink-0"
                  style={{ color: "var(--text-secondary)" }}
                >
                  <Copy size={12} />
                  {copied ? "已复制" : "复制"}
                </button>
              </div>
              <div className="text-[10px]" style={{ color: "var(--text-muted)" }}>
                {invite.role === "owner" ? "所有者" : "编辑者"} · {fmtExpiry(invite)} · {fmtUses(invite)}
              </div>
              <button
                onClick={() => void revoke()}
                disabled={busy}
                className="text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40"
                style={{ color: "#f87171" }}
              >
                撤销该邀请码
              </button>
            </div>
          )}

          {error && (
            <div className="text-xs" style={{ color: "#f87171" }}>
              {error}
            </div>
          )}
        </div>

        <footer className="px-4 py-3 border-t flex justify-end gap-2" style={{ borderColor: "var(--border)" }}>
          <button
            onClick={() => void generate()}
            disabled={busy}
            className="px-3 py-1.5 text-xs rounded hover:opacity-80 disabled:opacity-50 flex items-center gap-1.5"
            style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
          >
            {busy && <Loader2 size={12} className="animate-spin" />}
            {invite ? "重新生成" : "生成邀请码"}
          </button>
        </footer>
      </div>
    </div>
  );
}
