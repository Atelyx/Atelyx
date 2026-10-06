/**
 * 布局服务提供器（ctx.layout）：由内核提供（root 作用域，跨插件生命周期常驻）。
 * 布局权威在 Rust layout.rs（layout-op 是唯一变更入口）；本服务只读布局镜像 + 发布布局操作（`LayoutOp` 与 Rust 侧逐字段对齐，由命令层受理）。
 */
import { symbols } from "@atelyx/cordis";
import { getPluginLayoutAccess } from "./access";
import { pluginIdOf } from "./loader";
import type { Context } from "@atelyx/cordis";
import type { PluginDefaultLayoutSpec } from "@/types";
import type { LayoutService } from "./types";

/** 读取布局访问（未接线时抛错；服务方法调用时触发）。 */
function access() {
  const a = getPluginLayoutAccess();
  if (!a) throw new Error("布局能力未就绪");
  return a;
}

interface LayoutServiceInstance extends LayoutService {
  /** 调用方插件上下文（tracker 机制注入，非对象自有属性）。 */
  ctx: Context;
}

/** 构造布局服务（访问经延迟检查；使用前要求 pluginStore.ensureLayoutAccess 已填充）。
 *  tracker 让插件经 ctx.layout 调用时 this.ctx 解析为调用方上下文（declareDefaultLayout 按此归属插件）。 */
export function createLayoutService(): LayoutService {
  const api: LayoutService = {
    activeLayoutId: () => access().activeLayoutId(),
    layouts: () => access().layouts(),
    addView: (panelId, view) => access().addView(panelId, view),
    op: (op) => access().op(op),
    declareDefaultLayout(this: LayoutServiceInstance, spec: PluginDefaultLayoutSpec): () => void {
      if (typeof spec?.name !== "string" || spec.name.trim().length === 0) {
        throw new Error("默认布局需要非空名称");
      }
      if (!spec.tree || typeof spec.tree !== "object") {
        throw new Error("默认布局需要布局规格树");
      }
      const ctx = this.ctx;
      // 布局声明按调用方插件记账（一次性标记按 id 落插件状态），缺归属即拒绝——
      // 静默落到兜底 id 会让 Rust 侧按非法 id 拒绝，报出误导性的「插件不存在」
      const pluginId = pluginIdOf(ctx);
      if (!pluginId) throw new Error("默认布局声明只能由插件上下文发起");
      return ctx.effect(() => {
        // 异步应用（Rust 侧一次性判定 + 落位）；失败由接线层通知用户，不阻断插件其余注册
        void access()
          .applyDefaultLayout(pluginId, spec)
          .catch(() => {});
        return () => {};
      });
    },
  };
  Object.defineProperty(api, symbols.tracker, { value: { property: "ctx" } });
  return api;
}
