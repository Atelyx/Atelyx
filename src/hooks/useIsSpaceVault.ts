/**
 * 激活仓库的组件内身份判定：
 * - `useIsSpaceVault`：激活仓库是否为协作空间（设置界面的编辑目标判定等按身份分流展示的入口）；
 * - `useSpaceViewerOnly`：激活仓库是否为「协作空间 + 本账号为查看者」（空间内团队层写入会被服务端
 *   拒绝，UI 据此禁用写入口）。角色未知（空间列表未加载/离线）时返回 false：不臆断权限，
 *   真发生无权限写入时服务端会给出可读原因。
 */
import { useAppStore } from "@/stores/appStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";

export function useIsSpaceVault(): boolean {
  return useAppStore((s) => s.vaultIdentity?.kind === "space");
}

export function useSpaceViewerOnly(): boolean {
  const identity = useAppStore((s) => s.vaultIdentity);
  const role = useSpaceDirectoryStore((s) =>
    identity?.kind === "space"
      ? (s.spacesByServer[identity.serverUrl]?.find((x) => x.spaceId === identity.spaceId)?.role ??
        "")
      : "",
  );
  return identity?.kind === "space" && role === "viewer";
}
