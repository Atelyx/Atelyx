/**
 * 设置界面的「编辑目标」判定：仓库级设置的作用域恒为当前激活仓库/空间（设置页只编辑它）。
 *
 * 统一在此判定，避免各设置组件各拼一遍空间/角色判定；角色未知（空间列表未加载/离线）时按
 * 「可写」处理——不臆断权限，真被服务端拒绝会给出可读原因。
 */
import { useSettingsStore, selectVaultSettingsSession } from "@/stores/settingsStore";
import { useIsSpaceVault, useSpaceViewerOnly } from "@/hooks/useIsSpaceVault";

export interface EditingTargetFlags {
  /** 编辑目标是否为协作空间。 */
  isSpace: boolean;
  /** 编辑目标是否为「协作空间 + 本账号为查看者」：空间内的写入口对查看者禁用。 */
  viewerOnly: boolean;
}

export function useEditingTargetFlags(): EditingTargetFlags {
  const session = useSettingsStore(selectVaultSettingsSession);
  const activeIsSpace = useIsSpaceVault();
  const activeViewerOnly = useSpaceViewerOnly();
  if (session) {
    const target = session.target;
    return target.kind === "space"
      ? { isSpace: true, viewerOnly: target.role === "viewer" }
      : { isSpace: false, viewerOnly: false };
  }
  return { isSpace: activeIsSpace, viewerOnly: activeViewerOnly };
}
