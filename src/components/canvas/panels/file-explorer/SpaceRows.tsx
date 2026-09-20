/**
 * 文件面板列表层的协作空间区：空间条目与本地仓库平级（仓库树的顶级条目同构）。
 *
 * 数据（合并后的空间条目）由 FileExplorerPanel 计算传入；本组件承载交互：
 * 点击进入（ok / need-login 引导登录 / error 已通知）、创建空间（创建后直接进入）、
 * 输入邀请码加入、行内重命名、右键菜单（SpaceMenu，由面板渲染）、激活空间就地展开文件树。
 *
 * 分层：只调 appStore / spaceDirectoryStore 动作，不直调 service。
 */
import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Cloud, Loader2, Plus, Ticket } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import type { FileTreeNode } from "@/types";
import type { VaultTreeProps } from "./VaultRows";
import { FileTree } from "./FileTree";
import { InlineInput } from "./InlineInput";

/** 列表层空间条目（最近条目与服务端列表合并后的展示形态）。 */
export interface SpaceEntry {
  serverUrl: string;
  spaceId: string;
  name: string;
  /** 本账号在该空间的成员角色（owner/editor；最近条目无摘要时为空串）。 */
  role: string;
}

interface SpaceRowsProps {
  entries: SpaceEntry[];
  /** 存在已登录服务器（决定空间区显示条目还是引导）。 */
  hasServers: boolean;
  /** 任一服务器列表加载中。 */
  loading: boolean;
  /** 任一服务器列表加载失败的首个错误（用户可见，重连/操作会重拉）。 */
  listError: string | null;
  /** 当前激活仓库身份（空间条目据此判定激活态）。 */
  identity: { kind: string; serverUrl?: string; spaceId?: string } | null;
  /** 切换进行中的目标（appStore.switchingVaultRoot，空间为 `space:server#id`）。 */
  switchingTo: string | null;
  /** 激活空间的文件树（仅激活空间有内容）。 */
  tree: FileTreeNode[];
  fileTree: VaultTreeProps;
  /** 行内重命中的空间条目 key（null = 无；面板持态并执行 renameSpace）。 */
  renamingKey: string | null;
  onRenameCommit: (key: string, name: string) => void;
  onRenameCancel: () => void;
  /** 操作提示（面板 notice 条）。 */
  onNotice: (message: string) => void;
  onOpenMenu: (x: number, y: number, target: { kind: "space"; serverUrl: string; spaceId: string; name: string; role: string }) => void;
}

const keyOf = (e: SpaceEntry) => `${e.serverUrl}#${e.spaceId}`;

export function SpaceRows({ entries, hasServers, loading, listError, identity, switchingTo, tree, fileTree, renamingKey, onRenameCommit, onRenameCancel, onNotice, onOpenMenu }: SpaceRowsProps) {
  const selectSpace = useAppStore((s) => s.selectSpace);
  const openSettings = useAppStore((s) => s.openSettings);
  const createSpace = useSpaceDirectoryStore((s) => s.createSpace);
  const acceptInvite = useSpaceDirectoryStore((s) => s.acceptInvite);
  const authServers = useSpaceAuthStore((s) => s.servers);
  // 服务器地址清单（selector 只回原数组引用，派生在 useMemo 防每次渲染新引用）
  const servers = useMemo(() => authServers.map((e) => e.serverUrl), [authServers]);

  // 激活空间行收起集合（面板挂载期间记忆；与本地仓库行同语义）
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // 内联输入态：创建空间（输入空间名）/ 加入（输入邀请码）
  const [creating, setCreating] = useState(false);
  const [joining, setJoining] = useState(false);
  const [draft, setDraft] = useState("");
  // 多服务器时的目标服务器（单服务器免选）
  const [serverPick, setServerPick] = useState<string>("");
  const [busy, setBusy] = useState(false);

  const targetServer = serverPick || servers[0] || "";

  // 进入行内重命名时清空草稿（防残留上一次创建/加入的输入被当成新名称）
  useEffect(() => {
    setDraft("");
  }, [renamingKey]);

  /** 点击空间条目：selectSpace 三分支——ok 进入；need-login 打开设置「多人协作」并预填地址
   *  （登录成功后由设置区自动重试进入）；error 已有通知不重复。 */
  const enter = async (entry: SpaceEntry) => {
    const result = await selectSpace({
      serverUrl: entry.serverUrl,
      spaceId: entry.spaceId,
      name: entry.name,
    });
    if (result === "need-login") {
      useSpaceDirectoryStore.getState().requestLogin({
        serverUrl: entry.serverUrl,
        retry: { serverUrl: entry.serverUrl, spaceId: entry.spaceId, name: entry.name },
      });
      openSettings("collab");
    }
  };

  const submitCreate = async () => {
    const name = draft.trim();
    if (!name || !targetServer) return;
    setBusy(true);
    try {
      const created = await createSpace(targetServer, name);
      setCreating(false);
      setDraft("");
      await selectSpace({ serverUrl: targetServer, spaceId: created.spaceId, name: created.name });
    } catch (e) {
      console.error("创建空间失败", e);
      onNotice(`创建空间失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const submitJoin = async () => {
    const code = draft.trim();
    if (!code || !targetServer) return;
    setBusy(true);
    try {
      await acceptInvite(targetServer, code);
      setJoining(false);
      setDraft("");
      onNotice("已加入协作空间");
    } catch (e) {
      console.error("加入协作空间失败", e);
      onNotice(`加入失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const serverPicker = servers.length > 1 && (creating || joining) && (
    <select
      value={targetServer}
      onChange={(e) => setServerPick(e.target.value)}
      className="text-xs rounded px-1 py-0.5 outline-none max-w-[160px]"
      style={{ background: "var(--input-bg)", color: "var(--text-primary)", border: "1px solid var(--input-border)" }}
    >
      {servers.map((s) => (
        <option key={s} value={s}>
          {s}
        </option>
      ))}
    </select>
  );

  return (
    <>
      {entries.map((entry) => {
        const key = keyOf(entry);
        const active = identity?.kind === "space" && identity.serverUrl === entry.serverUrl && identity.spaceId === entry.spaceId;
        const expanded = active && !collapsed.has(key);
        const switching = switchingTo === `space:${entry.serverUrl}#${entry.spaceId}`;
        return (
          <li key={key}>
            {renamingKey === key ? (
              <div className="flex items-center px-2 py-1 min-h-8">
                <div className="flex-1 min-w-0">
                  <InlineInput
                    value={draft || entry.name}
                    onChange={setDraft}
                    onCommit={() => onRenameCommit(key, draft.trim() || entry.name)}
                    onCancel={onRenameCancel}
                    placeholder="空间名称"
                  />
                </div>
              </div>
            ) : (
              <div
                className="flex items-center gap-1 px-2 py-1 min-h-8 select-none cursor-default rounded-md hover:bg-[var(--hover)]"
                style={{
                  background: active ? "color-mix(in srgb, var(--accent) 20%, transparent)" : undefined,
                }}
                title={`${entry.name}（${entry.serverUrl}）`}
                onClick={() => {
                  if (switchingTo) return;
                  if (active) {
                    setCollapsed((prev) => {
                      const next = new Set(prev);
                      if (next.has(key)) next.delete(key);
                      else next.add(key);
                      return next;
                    });
                  } else {
                    void enter(entry);
                  }
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  onOpenMenu(e.clientX, e.clientY, { kind: "space", ...entry });
                }}
              >
                <span className="flex items-center">{expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
                {switching ? (
                  <Loader2 size={14} className="animate-spin" style={{ color: "var(--accent)" }} />
                ) : (
                  <Cloud size={14} style={{ color: active ? "var(--accent)" : "var(--text-muted)" }} />
                )}
                <span className="flex-1 truncate text-xs" style={{ color: "var(--text-primary)" }}>
                  {entry.name}
                </span>
                <span className="flex-shrink-0 text-[10px] truncate max-w-[45%]" style={{ color: "var(--text-muted)" }} title={entry.serverUrl}>
                  {entry.serverUrl}
                </span>
              </div>
            )}
            {expanded && (
              <ul className="relative">
                {/* 展开指示线：与仓库行/文件夹行同位 */}
                <div
                  className="absolute top-0 bottom-0 w-px pointer-events-none z-10"
                  style={{ left: 13, background: "var(--text-muted)", opacity: 0.6 }}
                />
                <FileTree nodes={tree} depth={1} parentDir="" {...fileTree} />
              </ul>
            )}
          </li>
        );
      })}

      {!hasServers ? (
        <li>
          <div className="flex flex-col items-start gap-1.5 px-2 py-2 text-xs" style={{ color: "var(--text-muted)" }}>
            <span className="flex items-center gap-1.5">
              <Cloud size={13} />
              连接服务器后可见
            </span>
            <button
              onClick={() => openSettings("collab")}
              className="px-2 py-1 rounded border hover:opacity-80"
              style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
            >
              连接服务器
            </button>
          </div>
        </li>
      ) : (
        <li>
          {listError && (
            <div className="px-2 py-0.5 text-[11px] break-all" style={{ color: "#f87171" }} title={listError}>
              空间列表加载失败：{listError}
            </div>
          )}
          {creating || joining ? (
            <div className="flex items-center gap-1.5 px-2 py-1 flex-wrap">
              {serverPicker}
              <div className="flex-1 min-w-[80px]">
                <InlineInput
                  value={draft}
                  onChange={setDraft}
                  onCommit={() => void (creating ? submitCreate() : submitJoin())}
                  onCancel={() => {
                    setCreating(false);
                    setJoining(false);
                    setDraft("");
                  }}
                  placeholder={creating ? "空间名称" : "邀请码"}
                />
              </div>
              {busy && <Loader2 size={13} className="animate-spin flex-shrink-0" style={{ color: "var(--text-muted)" }} />}
            </div>
          ) : (
            <div className="flex items-center gap-2 px-2 py-1 text-[11px]" style={{ color: "var(--text-muted)" }}>
              <button
                onClick={() => {
                  setJoining(false);
                  setCreating(true);
                  setDraft("");
                }}
                disabled={loading}
                className="flex items-center gap-1 px-1 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40"
                title="在已登录的服务器上创建协作空间"
              >
                <Plus size={12} />
                创建空间
              </button>
              <button
                onClick={() => {
                  setCreating(false);
                  setJoining(true);
                  setDraft("");
                }}
                disabled={loading}
                className="flex items-center gap-1 px-1 py-0.5 rounded hover:bg-[var(--hover)] disabled:opacity-40"
                title="输入邀请码加入协作空间"
              >
                <Ticket size={12} />
                输入邀请码
              </button>
              {loading && <Loader2 size={12} className="animate-spin" />}
            </div>
          )}
        </li>
      )}
    </>
  );
}
