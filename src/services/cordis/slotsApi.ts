/**
 * ctx.slots：插件 UI 注册 API（视图槽 + 表格视图）。
 *
 * 服务对象带 Cordis tracker（`symbols.tracker`）——经插件 ctx 读取时 `this.ctx` 解析为
 * 调用方插件上下文，注册随其 fiber 生命周期自动撤销（ctx.effect）。pluginId 经挂载器的
 * contextToPluginId 归属（追原型链，见 loader.pluginIdOf）。
 */
import { symbols } from "@atelyx/cordis";
import type { ComponentType, ReactNode } from "react";
import type { Context } from "@atelyx/cordis";
import { registerViewSlot } from "./slots";
import { pluginIdOf } from "./loader";
import { registerPluginTableView, unregisterPluginUi } from "./ui";

/** 视图槽注册载荷（工作区面板视图；render 优先，重型视图承载宿主面板 id）。 */
export interface RegisterViewOptions {
  kind: string;
  label: string;
  component?: ComponentType;
  /** 按宿主面板/撕裂窗口 id 渲染（重型视图用）。 */
  render?: (hostId: string) => ReactNode;
}

/** 表格视图注册载荷（表格编辑器内视图）。 */
export interface RegisterTableViewOptions {
  kind: string;
  label: string;
  component: ComponentType;
}

/** ctx.slots 服务契约（types.ts 声明合并挂到 Context）。 */
export interface SlotsApi {
  registerView(opts: RegisterViewOptions): () => void;
  registerTableView(opts: RegisterTableViewOptions): () => void;
}

interface SlotsApiInstance extends SlotsApi {
  /** 调用方插件上下文（tracker 机制注入，非对象自有属性）。 */
  ctx: Context;
}

/** 构造 ctx.slots 服务（内核 provide；调用方插件经 ctx.slots 触达）。 */
export function createSlotsApi(): SlotsApi {
  const api = {
    registerView(this: SlotsApiInstance, opts: RegisterViewOptions): () => void {
      if (typeof opts.kind !== "string" || opts.kind.length === 0) throw new Error("视图槽需要非空 kind");
      if (!opts.component && !opts.render) throw new Error("视图槽需要 component 或 render");
      const ctx = this.ctx;
      const pluginId = pluginIdOf(ctx) ?? "plugin";
      return ctx.effect(() =>
        registerViewSlot(opts.kind, pluginId, {
          label: opts.label,
          ...(opts.component ? { component: opts.component } : {}),
          ...(opts.render ? { render: opts.render } : {}),
        }),
      );
    },
    registerTableView(this: SlotsApiInstance, opts: RegisterTableViewOptions): () => void {
      if (typeof opts.kind !== "string" || opts.kind.length === 0) throw new Error("表格视图需要非空 kind");
      const ctx = this.ctx;
      const pluginId = pluginIdOf(ctx) ?? "plugin";
      return ctx.effect(() => {
        registerPluginTableView(pluginId, opts.kind, opts.label, opts.component);
        return () => unregisterPluginUi(pluginId);
      });
    },
  };
  Object.defineProperty(api, symbols.tracker, { value: { property: "ctx" } });
  return api;
}
