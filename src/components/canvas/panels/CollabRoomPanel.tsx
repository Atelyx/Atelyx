/**
 * 协作房间面板（主页）：展示同仓库在线协作者 + 各自打开的文件（presence 实时）+ 各成员应用版本号，点击可跳转打开。
 *
 * 数据源：collabStore.peers（远端，含 hello 上报的 version）+ settingsStore 身份 + appStore 当前打开文件合成「我」行。
 * 打开文件动作回调直连 appStore（与 FilesView 同模式）。
 * 同一身份（昵称+设备）多连接合并为一行：主窗口常驻 + 撕裂窗口会各持一条连接（relay 每连接一个
 * peer），展示层合并防「自己重复出现」；「前往设置」仅主窗口有效（设置弹窗只在主窗口渲染）。
 */
import { Clock, Settings, Users, Wifi, WifiOff } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useCollabStore } from "@/stores/collabStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePanelStore } from "@/stores/panelStore";
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

/** 单个协作者行（含自己）：色点 + 昵称 + 设备 + 版本徽标 + 打开文件列表（聚焦文件置顶）。 */
function MemberRow({
  isSelf,
  nickname,
  color,
  device,
  version,
  openFiles,
}: {
  isSelf: boolean;
  nickname: string;
  color: string;
  device: string;
  version?: string;
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
          {version && (
            <span
              className="text-[10px] px-1 rounded flex-shrink-0"
              style={{ color: "var(--text-muted)", border: "1px solid var(--border)" }}
              title={`应用版本 ${version}`}
            >
              v{version}
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
                {/* 仅远端标「正在查看」（首个聚焦文件）；自己行无聚焦概念，不标 */}
                {!isSelf && i === 0 && files.length > 1 && (
                  <span className="text-[10px] flex-shrink-0" style={{ color: "var(--text-muted)" }}>
                    正在查看
                  </span>
                )}
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

/** 协作房间面板：连接状态 + 在线成员（自己 + 远端）+ 空态引导。 */
export function CollabRoomPanel() {
  const connected = useCollabStore((s) => s.connected);
  const peers = useCollabStore((s) => s.peers);
  const collabEnabled = useSettingsStore((s) => s.collabEnabled);
  const collabRelayUrl = useSettingsStore((s) => s.collabRelayUrl);
  const collabNickname = useSettingsStore((s) => s.collabNickname);
  const collabColor = useSettingsStore((s) => s.collabColor);
  const deviceName = useSettingsStore((s) => s.deviceName);
  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const openSettings = useAppStore((s) => s.openSettings);
  // 「前往设置」仅主窗口有效（设置弹窗只在主窗口渲染）
  const isMainWindow = usePanelStore((s) => s.windowId) === "main";
  // 「我」行应用版本号（组件不直连 service，经 appStore 取；失败静默不显示徽标）
  const getAppVersion = useAppStore((s) => s.getAppVersion);
  const [selfVersion, setSelfVersion] = useState<string | undefined>(undefined);
  useEffect(() => {
    void getAppVersion().then(setSelfVersion).catch(() => {});
  }, [getAppVersion]);

  // 「我」行：身份来自设置，打开文件来自 appStore 当前打开（无聚焦概念，全部平级展示）
  const selfOpenFiles: CollabPresence["openFiles"] = [];
  if (currentCanvasFile) selfOpenFiles.push({ file: currentCanvasFile, view: "canvas" });
  if (currentNoteFile) selfOpenFiles.push({ file: currentNoteFile, view: "note" });
  if (currentTableFile) selfOpenFiles.push({ file: currentTableFile, view: "table" });

  const nickname = collabNickname || deviceName || "用户";
  const color = collabColor || "#e06c75";

  // 同身份多连接合并（主窗口 + 撕裂窗口各持一条连接 → 同一用户重复出现），openFiles 取并集
  const mergedPeers = useMemo(() => {
    const byIdentity = new Map<string, CollabPeer[]>();
    for (const p of peers) {
      const key = `${p.nickname}::${p.deviceName}`;
      const arr = byIdentity.get(key) ?? [];
      arr.push(p);
      byIdentity.set(key, arr);
    }
    const rows: Array<{
      id: string;
      nickname: string;
      color: string;
      device: string;
      version?: string;
      openFiles: CollabPresence["openFiles"];
    }> = [];
    for (const group of byIdentity.values()) {
      // 分组恒非空（每组至少一个 peer）
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
        // 同身份多连接 = 同一应用实例，版本一致，取首条即可
        version: first.version,
        openFiles,
      });
    }
    return rows;
  }, [peers]);

  return (
    <div className="h-full w-full flex flex-col overflow-hidden" style={{ background: "var(--bg-primary)" }}>
      {/* 连接状态条 */}
      <div className="flex items-center gap-2 px-3 py-2 flex-shrink-0 select-none" style={{ borderBottom: "1px solid var(--border)" }}>
        {connected ? <Wifi size={13} style={{ color: "#22c55e" }} /> : <WifiOff size={13} style={{ color: "var(--text-muted)" }} />}
        <span className="text-xs" style={{ color: connected ? "#22c55e" : "var(--text-muted)" }}>
          {connected ? "已连接中转" : collabEnabled ? "未连接中转" : "多人协作未开启"}
        </span>
        {!connected && collabRelayUrl && (
          <span className="text-[10px] truncate flex-1 text-right" style={{ color: "var(--text-muted)" }} title={collabRelayUrl}>
            {collabRelayUrl.replace(/^ws:\/\//, "")}
          </span>
        )}
      </div>

      <div className="flex-1 min-h-0 overflow-auto p-2 space-y-1.5">
        {!collabEnabled ? (
          /* 协作未开启：空态引导去设置（多人协作 tab；仅主窗口可跳转） */
          <div className="h-full flex flex-col items-center justify-center gap-3 text-center px-6">
            <Users size={26} style={{ color: "var(--text-muted)" }} />
            <div className="text-sm" style={{ color: "var(--text-secondary)" }}>
              多人协作未开启
            </div>
            <div className="text-xs" style={{ color: "var(--text-muted)" }}>
              开启后可在局域网内与协作者实时互见
            </div>
            {isMainWindow && (
              <button
                onClick={() => openSettings("collab")}
                className="flex items-center gap-1 text-xs px-3 py-1.5 rounded-md hover:opacity-80"
                style={{ color: "var(--accent-fg)", background: "var(--accent)" }}
              >
                <Settings size={12} />
                前往设置
              </button>
            )}
          </div>
        ) : (
          <>
            {/* 自己 */}
            <MemberRow isSelf nickname={nickname} color={color} device={deviceName} version={selfVersion} openFiles={selfOpenFiles} />
            {/* 远端协作者（按身份合并；presence 无 file = 未在看文件，仍显示成员） */}
            {mergedPeers.length === 0 ? (
              <div className="flex flex-col items-center justify-center gap-1 py-8 text-center px-6">
                <Clock size={20} style={{ color: "var(--text-muted)" }} />
                <div className="text-xs" style={{ color: "var(--text-muted)" }}>
                  {connected ? "当前仓库没有其他协作者在线" : "连接中…"}
                </div>
              </div>
            ) : (
              mergedPeers.map((p) => (
                <MemberRow
                  key={p.id}
                  isSelf={false}
                  nickname={p.nickname}
                  color={p.color}
                  device={p.device}
                  version={p.version}
                  openFiles={p.openFiles}
                />
              ))
            )}
          </>
        )}
      </div>
    </div>
  );
}
