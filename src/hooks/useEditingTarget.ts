/**
 * 设置界面的「编辑目标」判定：会话目标（编辑非激活仓库）优先，否则激活仓库。
 *
 * 仓库设置弹窗（可编辑未激活的仓库）与设置页共用同一套判断，避免各组件各拼一遍空间/角色判定；
 * 角色未知（空间列表未加载/离线）时按「可写」处理——不臆断权限，真被服务端拒绝会给出可读原因。
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
