/**
 * 文件面板列表层的协作空间区：空间条目与本地仓库平级（仓库树的顶级条目同构）。
 *
 * 数据（合并后的空间条目）由 FileExplorerPanel 计算传入；本组件承载条目交互：
 * 点击进入（ok / need-login 引导登录 / error 已通知）、行内重命名、右键菜单（SpaceMenu，
 * 由面板渲染）、激活空间就地展开文件树，以及列表加载/错误/空态与无已登录服务器的引导。
 * 新增空间的三条途径在面板工具条（见 `SpaceAddPopover`）。
 *
 * 分层：只调 appStore / spaceDirectoryStore 动作，不直调 service。
 */
import { useEffect, useState } from "react";
import { ChevronDown, ChevronRight, Cloud, Loader2 } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
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
  onOpenMenu: (x: number, y: number, target: { kind: "space"; serverUrl: string; spaceId: string; name: string; role: string }) => void;
}

const keyOf = (e: SpaceEntry) => `${e.serverUrl}#${e.spaceId}`;

export function SpaceRows({ entries, hasServers, loading, listError, identity, switchingTo, tree, fileTree, renamingKey, onRenameCommit, onRenameCancel, onOpenMenu }: SpaceRowsProps) {
  const selectSpace = useAppStore((s) => s.selectSpace);
  const openSettings = useAppStore((s) => s.openSettings);

  // 激活空间行收起集合（面板挂载期间记忆；与本地仓库行同语义）
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  // 行内重命名草稿（面板置 renamingKey 进入，提交/取消后由面板清除）
  const [draft, setDraft] = useState("");

  // 进入行内重命名时清空草稿（防残留上一次输入被当成新名称）
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
          {loading && (
            <div className="flex items-center gap-1.5 px-2 py-0.5 text-[11px]" style={{ color: "var(--text-muted)" }}>
              <Loader2 size={12} className="animate-spin" />
              加载空间列表…
            </div>
          )}
          {/* 无条目时的去向提示：新增入口在面板工具条，本区只放条目 */}
          {!loading && !listError && entries.length === 0 && (
            <div className="px-2 py-0.5 text-[11px]" style={{ color: "var(--text-muted)" }}>
              还没有协作空间，用工具条的新增协作空间按钮创建或加入。
            </div>
          )}
        </li>
      )}
    </>
  );
}
