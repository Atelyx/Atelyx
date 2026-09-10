/**
 * 领域历史服务提供器（ctx.history）：由内核提供（root 作用域，跨插件生命周期常驻）。
 *
 * 实现 = 注入的历史访问对象（pluginStore 接线填充，见 ensureHistoryAccess）——
 * list 走通用历史服务、rollback 按 kind 分派文件存储链、repoHistory 读仓库聚合。
 * 访问延迟到方法调用时读取（服务可随内核创建，使用前要求访问已接线）。
 */
import { getPluginHistoryAccess } from "./access";
import type { HistoryService } from "./types";

/** 读取历史访问（未接线时抛错；服务方法调用时触发）。 */
function access() {
  const a = getPluginHistoryAccess();
  if (!a) throw new Error("历史能力未就绪");
  return a;
}

/** 构造历史服务（访问经延迟检查；使用前要求 pluginStore.ensureHistoryAccess 已填充）。 */
export function createHistoryService(): HistoryService {
  return {
    list: (kind, file) => access().list(kind, file),
    rollback: (kind, file, seq) => access().rollback(kind, file, seq),
    repoHistory: () => access().repoHistory(),
  };
}
