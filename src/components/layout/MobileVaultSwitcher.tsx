/**
 * 移动端仓库/空间切换入口：顶栏当前仓库名点击唤出，列出最近仓库与最近协作空间。
 * 本地仓库直接切换；协作空间无有效会话时与文件面板同一引导（预填地址 + 打开设置）；
 * 「添加本地仓库」走自研目录浏览（安卓无系统文件夹选择器），权限与回落口径由入口对话框说明。
 */
import { useRef, useState } from "react";
import { BookOpen, ChevronDown, Cloud, FolderPlus, HardDrive, Server } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import { PopupLayer } from "@/components/common/PopupLayer";
import { MenuDivider, MenuItem } from "@/components/common/Menu";
import { MobileLocalVaultDialog } from "@/components/layout/MobileLocalVaultDialog";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import type { RecentSpace } from "@/types";

export function MobileVaultSwitcher({ label }: { label: string }) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(triggerRef);

  const recentVaults = useAppStore((s) => s.recentVaults);
  const recentSpaces = useAppStore((s) => s.recentSpaces);
  const identity = useAppStore((s) => s.vaultIdentity);
  const switching = useAppStore((s) => s.switchingVaultRoot);
  const selectVault = useAppStore((s) => s.selectVault);
  const selectSpace = useAppStore((s) => s.selectSpace);
  const openSettings = useAppStore((s) => s.openSettings);

  const [addLocal, setAddLocal] = useState(false);

  const empty = recentVaults.length === 0 && recentSpaces.length === 0;

  // 顶栏标题前缀图标：区分个人仓库 / 协作空间（未进仓库用中性图标）
  const VaultIcon = identity?.kind === "space" ? Cloud : identity?.kind === "local" ? HardDrive : BookOpen;

  const enterSpace = async (entry: RecentSpace) => {
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
      <button
        ref={triggerRef}
        onClick={toggle}
        className="flex-1 min-w-0 flex items-center gap-1.5 h-11 px-3 text-sm font-semibold rounded-sm"
        style={{ color: "var(--text-primary)" }}
        aria-haspopup="menu"
        aria-expanded={!!anchor}
      >
        <VaultIcon size={14} className="flex-shrink-0" style={{ color: "var(--accent)" }} />
        <span className="truncate">{label}</span>
        <ChevronDown size={14} className="flex-shrink-0" style={{ color: "var(--text-muted)" }} />
      </button>
      <PopupLayer anchor={anchor} onClose={close} triggerRef={triggerRef} widthClass="w-64">
        {empty && (
          <div className="px-3 py-2 text-xs" style={{ color: "var(--text-muted)" }}>
            还没有仓库或协作空间
          </div>
        )}
        {recentVaults.map((v) => {
          const active = identity?.kind === "local" && identity.root === v.root;
          return (
            <MenuItem
              key={v.root}
              disabled={!!switching}
              noDisabledCursor
              title={v.root}
              className="h-12 gap-2.5"
              style={active ? { color: "var(--accent)" } : undefined}
              onClick={() => {
                close();
                void selectVault(v.root);
              }}
            >
              <HardDrive size={14} className="shrink-0" />
              <span className="truncate flex-1">{v.name}</span>
            </MenuItem>
          );
        })}
        {recentSpaces.map((s) => {
          const active =
            identity?.kind === "space" &&
            identity.serverUrl === s.serverUrl &&
            identity.spaceId === s.spaceId;
          return (
            <MenuItem
              key={`${s.serverUrl}#${s.spaceId}`}
              disabled={!!switching}
              noDisabledCursor
              title={`${s.name}（${s.serverUrl}）`}
              className="h-12 gap-2.5"
              style={active ? { color: "var(--accent)" } : undefined}
              onClick={() => {
                close();
                void enterSpace(s);
              }}
            >
              <Cloud size={14} className="shrink-0" />
              <span className="truncate flex-1">{s.name}</span>
            </MenuItem>
          );
        })}
        <MenuDivider />
        <MenuItem
          className="h-12 gap-2.5"
          onClick={() => {
            close();
            setAddLocal(true);
          }}
        >
          <FolderPlus size={14} className="shrink-0" />
          添加本地仓库…
        </MenuItem>
        <MenuItem
          className="h-12 gap-2.5"
          onClick={() => {
            close();
            openSettings("collab");
          }}
        >
          <Server size={14} className="shrink-0" />
          连接服务器…
        </MenuItem>
      </PopupLayer>
      {addLocal && <MobileLocalVaultDialog onClose={() => setAddLocal(false)} />}
    </>
  );
}
