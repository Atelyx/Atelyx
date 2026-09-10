/**
 * 应用级 UI 使用状态读服务提供器（ctx.uiState）：由内核提供（root 作用域）。
 *
 * 只读：非布局 JS 权威字段 + 布局镜像（改动须走 layoutOp/uiStatePatch 对应入口）。
 * 访问延迟到方法调用时读取（服务可随内核创建）。
 */
import { getPluginUiStateAccess } from "./access";
import type { UiStateService } from "./types";

/** 读取 UI 状态访问（未接线时抛错；服务方法调用时触发）。 */
function access() {
  const a = getPluginUiStateAccess();
  if (!a) throw new Error("UI 状态能力未就绪");
  return a;
}

/** 构造 UI 状态读服务（访问经延迟检查；使用前要求 pluginStore.ensureUiStateAccess 已填充）。 */
export function createUiStateService(): UiStateService {
  return {
    read: () => access().read(),
  };
}
