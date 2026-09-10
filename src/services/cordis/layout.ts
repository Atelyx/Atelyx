/**
 * 布局服务提供器（ctx.layout）：由内核提供（root 作用域，跨插件生命周期常驻）。
 *
 * 布局权威在 Rust layout.rs（layout-op 是唯一变更入口）；本服务只读布局镜像 +
 * 发布安全操作子集（addView/op）。访问延迟到方法调用时读取（服务可随内核创建）。
 */
import { getPluginLayoutAccess } from "./access";
import type { LayoutService } from "./types";

/** 读取布局访问（未接线时抛错；服务方法调用时触发）。 */
function access() {
  const a = getPluginLayoutAccess();
  if (!a) throw new Error("布局能力未就绪");
  return a;
}

/** 构造布局服务（访问经延迟检查；使用前要求 pluginStore.ensureLayoutAccess 已填充）。 */
export function createLayoutService(): LayoutService {
  return {
    activeLayoutId: () => access().activeLayoutId(),
    layouts: () => access().layouts(),
    addView: (panelId, view) => access().addView(panelId, view),
    op: (op) => access().op(op),
  };
}
