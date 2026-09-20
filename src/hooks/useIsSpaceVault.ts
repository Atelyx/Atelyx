/**
 * 激活仓库是否为协作空间的组件内 selector：服务端无对应能力的入口（仓库历史/笔记·画布·表格
 * 历史回滚）与空间元数据只读域的写入口（Agent 配置、系统提示词标记）禁用统一走这里，
 * 与 `useAppStore.getState().vaultIdentity?.kind === "space"` 同语义。
 */
import { useAppStore } from "@/stores/appStore";

export function useIsSpaceVault(): boolean {
  return useAppStore((s) => s.vaultIdentity?.kind === "space");
}
