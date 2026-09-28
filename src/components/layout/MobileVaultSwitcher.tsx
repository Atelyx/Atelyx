/**
 * 移动端仓库/空间切换入口：顶栏当前仓库名点击唤出，列出最近仓库与最近协作空间。
 *
 * 本地仓库直接切换；协作空间走 selectSpace，无有效会话时与文件面板同一引导（预填地址 + 打开设置）。
 * 手机上暂不提供新增本地仓库（目录选择器在移动端不可用）。
 */
import { useRef } from "react";
import { ChevronDown, Cloud, HardDrive, Server } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import { PopupLayer } from "@/components/common/PopupLayer";
import { MenuDivider, MenuItem } from "@/components/common/Menu";
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

  const empty = recentVaults.length === 0 && recentSpaces.length === 0;

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
        className="flex-1 min-w-0 flex items-center gap-1 h-10 px-2 rounded-md"
        style={{ color: "var(--text-primary)" }}
        aria-haspopup="menu"
        aria-expanded={!!anchor}
      >
        <span className="text-sm truncate">{label}</span>
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
          onClick={() => {
            close();
            openSettings("collab");
          }}
        >
          <Server size={14} className="shrink-0" />
          连接服务器…
        </MenuItem>
      </PopupLayer>
    </>
  );
}
