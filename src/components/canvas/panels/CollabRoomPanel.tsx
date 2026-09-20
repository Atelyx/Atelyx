/**
 * 协作面板：激活仓库为协作空间时显示成员名册 + 在线设备（presence 实时）；
 * 个人仓库无协作能力，显示空态。
 *
 * 空间形态数据源：spaceDirectoryStore.listMembers（名册，owner/editor 角色）+
 * collabStore.peers（服务端 peers 帧是连接语义——在线设备列表，不与名册强行合并同人，
 * 各设备当前打开文件经 presence 展示）。
 * 「我」行 = 本连接（身份来自 settingsStore，打开文件来自 appStore）。
 * 打开文件动作回调直连 appStore（与 FilesView 同模式）。
 */
import { Cloud, Loader2, Settings, Users, Wifi, WifiOff } from "lucide-react";
import { useEffect, useMemo } from "react";
import { useAppStore } from "@/stores/appStore";
import { useCollabStore } from "@/stores/collabStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePanelStore } from "@/stores/panelStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import { FileKindIcon, openFileByKind } from "@/components/common/FileKindIcon";
import { noteTitleFromFile } from "@/utils/filename";
import type { CollabPeer, CollabPresence } from "@/types";

/** 取 presence 的打开文件清单（优先 openFiles，回退聚焦文件；无 → 空）。恒返回非空数组。 */
function openFilesOf(presence: CollabPresence | null | undefined): NonNullable<CollabPresence["openFiles"]> {
  if (presence?.openFiles && presence.openFiles.length > 0) return presence.openFiles;
  if (presence?.file) {
    const view: "canvas" | "note" | "table" =
      presence.view === "canvas" ? "canvas" : presence.view === "note" ? "note" : "table";
    return [{ file: presence.file, view }];
  }
  return [];
}

/** 单个协作者行（含自己）：色点 + 昵称 + 设备 + 打开文件列表。 */
function MemberRow({
  isSelf,
  nickname,
  color,
  device,
  openFiles,
}: {
  isSelf: boolean;
  nickname: string;
  color: string;
  device: string;
  openFiles: CollabPresence["openFiles"] | null;
}) {
  const files = openFiles ?? [];
  return (
    <div className="flex items-start gap-2 px-3 py-2 rounded-md hover:opacity-90" style={{ background: "var(--bg-secondary)" }}>
      <span className="mt-1 w-2.5 h-2.5 rounded-full flex-shrink-0" style={{ background: color }} aria-hidden />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 text-sm">
          <span className="truncate font-medium" style={{ color: "var(--text-primary)" }}>
            {nickname}
          </span>
          {isSelf && (
            <span
              className="text-[10px] px-1 rounded flex-shrink-0"
              style={{ color: "var(--text-muted)", border: "1px solid var(--border)" }}
            >
              我
            </span>
          )}
          {device && (
            <span className="text-[10px] truncate flex-shrink-0" style={{ color: "var(--text-muted)" }}>
              {device}
            </span>
          )}
        </div>
        <div className="mt-1 space-y-0.5">
          {files.length === 0 ? (
            <div className="text-xs" style={{ color: "var(--text-muted)" }}>
              暂无打开文件
            </div>
          ) : (
            files.map((f, i) => (
              <button
                key={`${f.file}-${i}`}
                onClick={() => openFileByKind(f.file, f.view)}
                className="flex items-center gap-1.5 text-xs max-w-full px-1 py-0.5 rounded hover:opacity-80 text-left"
                style={{ color: "var(--text-secondary)" }}
                title={`打开 ${f.file}`}
              >
                <FileKindIcon kind={f.view} />
                <span className="truncate">{noteTitleFromFile(f.file)}</span>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

/** 成员角色 → 中文徽标（未知值原样显示）。 */
function roleLabel(role: string): string {
  if (role === "owner") return "所有者";
  if (role === "editor") return "编辑者";
  return role || "成员";
}

/** 协作空间形态：连接状态 + 成员名册 + 在线设备（含自己）。 */
function SpaceMembersView({ serverUrl, spaceId }: { serverUrl: string; spaceId: string }) {
  const connected = useCollabStore((s) => s.connected);
  const peers = useCollabStore((s) => s.peers);
  const collabNickname = useSettingsStore((s) => s.collabNickname);
  const collabColor = useSettingsStore((s) => s.collabColor);
  const deviceName = useSettingsStore((s) => s.deviceName);
  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const openSettings = useAppStore((s) => s.openSettings);
  const isMainWindow = usePanelStore((s) => s.windowId) === "main";

  const members = useSpaceDirectoryStore((s) => s.members);
  const membersKey = useSpaceDirectoryStore((s) => s.membersKey);
  const membersLoading = useSpaceDirectoryStore((s) => s.membersLoading);
  const membersError = useSpaceDirectoryStore((s) => s.membersError);
  const loadMembers = useSpaceDirectoryStore((s) => s.loadMembers);

  useEffect(() => {
    void loadMembers(serverUrl, spaceId);
  }, [loadMembers, serverUrl, spaceId]);

  const nickname = collabNickname || deviceName || "用户";
  const color = collabColor || "#e06c75";

  // 「我」行：打开文件来自 appStore 当前打开（无聚焦概念，全部平级展示）
  const selfOpenFiles: CollabPresence["openFiles"] = [];
  if (currentCanvasFile) selfOpenFiles.push({ file: currentCanvasFile, view: "canvas" });
  if (currentNoteFile) selfOpenFiles.push({ file: currentNoteFile, view: "note" });
  if (currentTableFile) selfOpenFiles.push({ file: currentTableFile, view: "table" });

  // 在线设备（同身份多连接合并：主窗口 + 撕裂窗口各持一条连接），openFiles 取并集
  const mergedPeers = useMemo(() => {
    const byIdentity = new Map<string, CollabPeer[]>();
    for (const p of peers) {
      const key = `${p.nickname}::${p.deviceName}`;
      const arr = byIdentity.get(key) ?? [];
      arr.push(p);
      byIdentity.set(key, arr);
    }
    const rows: Array<{ id: string; nickname: string; color: string; device: string; openFiles: CollabPresence["openFiles"] }> = [];
    for (const group of byIdentity.values()) {
      const first = group[0]!;
      const openFiles: CollabPresence["openFiles"] = [];
      const seen = new Set<string>();
      for (const p of group) {
        for (const f of openFilesOf(p.presence)) {
          if (!seen.has(f.file)) {
            seen.add(f.file);
            openFiles.push(f);
          }
        }
      }
      rows.push({
        id: group.map((p) => p.peerId).join("-"),
        nickname: first.nickname,
        color: first.color,
        device: first.deviceName,
        openFiles,
      });
    }
    return rows;
  }, [peers]);

  const rosterStale = membersKey !== `${serverUrl}#${spaceId}`;

  return (
    <div className="h-full w-full flex flex-col overflow-hidden" style={{ background: "var(--bg-primary)" }}>
      {/* 连接状态条 */}
      <div className="flex items-center gap-2 px-3 py-2 flex-shrink-0 select-none" style={{ borderBottom: "1px solid var(--border)" }}>
        {connected ? <Wifi size={13} style={{ color: "#22c55e" }} /> : <WifiOff size={13} style={{ color: "var(--text-muted)" }} />}
        <span className="text-xs" style={{ color: connected ? "#22c55e" : "var(--text-muted)" }}>
          {connected ? "已连接协作空间" : "未连接"}
        </span>
      </div>

      <div className="flex-1 min-h-0 overflow-auto p-2 space-y-1.5">
        {/* 成员名册 */}
        <div className="px-1 text-[11px] flex items-center gap-1.5" style={{ color: "var(--text-muted)" }}>
          <Users size={12} />
          空间成员
        </div>
        {membersError ? (
          <div className="flex flex-col items-start gap-1.5 px-3 py-2 text-xs" style={{ color: "var(--text-muted)" }}>
            <span style={{ color: "#f87171" }}>成员名册加载失败：{membersError}</span>
            {isMainWindow && (
              <button
                onClick={() => openSettings("collab")}
                className="flex items-center gap-1 px-2 py-1 rounded border hover:opacity-80"
                style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
              >
                <Settings size={11} />
                检查登录状态
              </button>
            )}
          </div>
        ) : membersLoading && rosterStale ? (
          <div className="flex items-center justify-center gap-2 py-4 text-xs" style={{ color: "var(--text-muted)" }}>
            <Loader2 size={13} className="animate-spin" />
            加载成员中…
          </div>
        ) : (
          (rosterStale ? [] : members).map((m) => (
            <div key={m.userId} className="flex items-center gap-2 px-3 py-1.5 rounded-md" style={{ background: "var(--bg-secondary)" }}>
              <span className="flex-1 min-w-0 truncate text-xs" style={{ color: "var(--text-primary)" }}>
                {m.displayName || m.username}
                <span className="ml-1.5" style={{ color: "var(--text-muted)" }}>
                  @{m.username}
                </span>
              </span>
              <span
                className="text-[10px] px-1 rounded flex-shrink-0"
                style={{ color: "var(--text-muted)", border: "1px solid var(--border)" }}
              >
                {roleLabel(m.role)}
              </span>
            </div>
          ))
        )}

        {/* 在线设备（连接语义：与名册分别展示，不强行合并同人） */}
        <div className="px-1 pt-1 text-[11px] flex items-center gap-1.5" style={{ color: "var(--text-muted)" }}>
          <Cloud size={12} />
          在线设备
        </div>
        <MemberRow isSelf nickname={nickname} color={color} device={deviceName} openFiles={selfOpenFiles} />
        {mergedPeers.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-1 py-4 text-center px-6">
            <div className="text-xs" style={{ color: "var(--text-muted)" }}>
              {connected ? "当前空间没有其他设备在线" : "连接中…"}
            </div>
          </div>
        ) : (
          mergedPeers.map((p) => (
            <MemberRow key={p.id} isSelf={false} nickname={p.nickname} color={p.color} device={p.device} openFiles={p.openFiles} />
          ))
        )}
      </div>
    </div>
  );
}

/** 协作面板：按激活仓库身份分派空间形态 / 本地空态。 */
export function CollabRoomPanel() {
  const identity = useAppStore((s) => s.vaultIdentity);

  if (identity?.kind === "space") {
    return <SpaceMembersView serverUrl={identity.serverUrl} spaceId={identity.spaceId} />;
  }

  return (
    <div className="h-full w-full flex flex-col overflow-hidden" style={{ background: "var(--bg-primary)" }}>
      <div className="flex-1 flex flex-col items-center justify-center gap-3 text-center px-6">
        <Users size={26} style={{ color: "var(--text-muted)" }} />
        <div className="text-sm" style={{ color: "var(--text-secondary)" }}>
          {identity ? "个人仓库无协作能力" : "未进入仓库"}
        </div>
        <div className="text-xs" style={{ color: "var(--text-muted)" }}>
          {identity ? "多人协作请使用协作空间（文件面板 → 连接服务器）" : "进入仓库或协作空间后可协作"}
        </div>
      </div>
    </div>
  );
}
