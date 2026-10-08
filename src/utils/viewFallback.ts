/**
 * 视图贡献缺失时的回退判定：首次装配在途 → 骨架（不闪假的「插件已卸载」），
 * 行停用/卸载 → 处置提示，其余 → 空白占位。
 */
/** 视图贡献缺失时的渲染去向。 */
export type ViewFallback = "skeleton" | "provider" | "blank";

/** `viewFallbackOf` 的输入（= 判定所需的本窗口装配状态切片）。 */
export interface ViewFallbackInput {
  /** 全量重载在途（pluginStore.assemblyLoading）。 */
  assemblyLoading: boolean;
  /** 本窗口是否已完成过一次装配（pluginStore.initialized）。 */
  initialized: boolean;
  /** 该 kind 的随应用分发行状态；undefined = 非默认组合提供（或未知 kind）。 */
  provider?: { enabled: boolean };
}

/**
 * 视图贡献缺失时的回退判定：**首次装配在途**（尚未装配完成 + 在途）→ 骨架——此时贡献不在
 * 注册表里只说明还没装载完，按提供者状态渲染会闪一次假的「插件已卸载」（撕裂窗口 boot 期可见）。
 * 行停用/卸载 → 处置提示；行在却无贡献、或未知 kind → 空白占位。
 * 已装配完成后的重载空档不算首次：沿用既有空白占位，不为每次启停/切仓库引入骨架闪现。
 */
export function viewFallbackOf({ assemblyLoading, initialized, provider }: ViewFallbackInput): ViewFallback {
  if (assemblyLoading && !initialized) return "skeleton";
  return provider && !provider.enabled ? "provider" : "blank";
}
