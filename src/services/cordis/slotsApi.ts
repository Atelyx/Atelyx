/**
 * ctx.slots：插件 UI 注册 API（视图/节点/边/表格视图/设置项/应用页/命令/主题设置项）。
 *
 * 服务对象带 Cordis tracker（`symbols.tracker`）——经插件 ctx 读取时 `this.ctx` 解析为
 * 调用方插件上下文，注册随其 fiber 生命周期自动撤销（ctx.effect）。pluginId 经挂载器的
 * contextToPluginId 归属（追原型链，见 loader.pluginIdOf）。
 * 视图/节点/边/表格视图为 slots 槽贡献（single 胜出，priority 定胜负）；设置项/应用页/
 * 命令/主题设置项为 ui.ts 键值平面（同 key last-wins）。
 */
import { symbols } from "@atelyx/cordis";
import type { ComponentType, ReactNode } from "react";
import type { Context } from "@atelyx/cordis";
import { registerViewSlot, registerNodeSlot, registerEdgeSlot, registerTableViewSlot, registerUiSlot, registerSlotContrib } from "./slots";
import { pluginIdOf } from "./loader";
import {
  registerPluginAppPage,
  registerPluginCommand,
  registerPluginSetting,
  registerPluginThemeSetting,
} from "./ui";
import type { ThemeSettingComponentProps } from "./ui";

/** 视图槽注册载荷（工作区面板视图；render 优先，重型视图承载宿主面板 id）。 */
export interface RegisterViewOptions {
  kind: string;
  label: string;
  component?: ComponentType;
  /** 按宿主面板/撕裂窗口 id 渲染（重型视图用）。 */
  render?: (hostId: string) => ReactNode;
  /** 槽优先级（higher wins；缺省 0。替换同 kind 的默认实现时设高值）。 */
  priority?: number;
}

/** 表格视图注册载荷（表格编辑器内视图）。 */
export interface RegisterTableViewOptions {
  kind: string;
  label: string;
  component: ComponentType;
  /** 槽优先级（higher wins；缺省 0）。 */
  priority?: number;
}

/** 画布节点注册载荷（CanvasView nodeTypes 合并；single 胜出。同 type 注册即覆盖内置基座，
 *  多个插件同 type 时高 priority 胜出）。 */
export interface RegisterNodeOptions {
  type: string;
  component: ComponentType;
  /** 槽优先级（higher wins；缺省 0）。 */
  priority?: number;
}

/** 画布边注册载荷（CanvasView edgeTypes 合并；single 胜出）。 */
export interface RegisterEdgeOptions {
  type: string;
  component: ComponentType;
  /** 槽优先级（higher wins；缺省 0）。 */
  priority?: number;
}

/** 设置项注册载荷（设置页左侧 tab）。 */
export interface RegisterSettingOptions {
  key: string;
  label: string;
  component: ComponentType;
}

/** 应用级页面注册载荷（全页接管）。 */
export interface RegisterAppPageOptions {
  id: string;
  label: string;
  component: ComponentType;
}

/** 主线程命令注册载荷（管理 UI「运行命令」入口；shortcut 可选，主线程统一监听）。 */
export interface RegisterCommandOptions {
  id: string;
  label: string;
  run: () => unknown;
  /** 快捷键（如 "mod+k"；可选）。 */
  shortcut?: string;
}

/** 主题插件设置项注册载荷（主题页设置区）。 */
export interface RegisterThemeSettingOptions {
  key: string;
  label: string;
  component: ComponentType<ThemeSettingComponentProps>;
}

/** 通用 UI 区域注册载荷（任意具名槽位：toolbar/<region>、panelhead/<region>、contextmenu/<target>、
 *  settings/<block>、statusbar/<region> 等；list 槽多贡献有序，priority 降序）。 */
export interface RegisterUiOptions {
  /** 槽名（如 "toolbar/note/right"）。 */
  slot: string;
  /** 渲染组件（无 props 契约）。 */
  component: ComponentType;
  /** 槽优先级（higher wins；list 槽排序用，缺省 0）。 */
  priority?: number;
}

/** 右键菜单项注册载荷（contextmenu/<target> 槽；list 多贡献，priority 降序）。 */
export interface RegisterMenuOptions {
  /** 菜单目标（如 "canvas"、"node"、"file"、"folder"、"panel-tab"）。 */
  target: string;
  /** 菜单项文案。 */
  label: string;
  /** 点击回调。 */
  onClick: () => void;
  /** 槽优先级（higher wins；缺省 0）。 */
  priority?: number;
}

/** ctx.slots 服务契约（types.ts 声明合并挂到 Context）。 */
export interface SlotsApi {
  registerView(opts: RegisterViewOptions): () => void;
  registerTableView(opts: RegisterTableViewOptions): () => void;
  registerNode(opts: RegisterNodeOptions): () => void;
  registerEdge(opts: RegisterEdgeOptions): () => void;
  registerSetting(opts: RegisterSettingOptions): () => void;
  registerAppPage(opts: RegisterAppPageOptions): () => void;
  registerCommand(opts: RegisterCommandOptions): () => void;
  registerThemeSetting(opts: RegisterThemeSettingOptions): () => void;
  /** 向任意具名 UI 槽位贡献一个组件（list 槽多贡献有序）。 */
  registerUi(opts: RegisterUiOptions): () => void;
  /** 向右键菜单贡献一个菜单项（label + 回调；list 槽多贡献有序）。 */
  registerMenu(opts: RegisterMenuOptions): () => void;
}

interface SlotsApiInstance extends SlotsApi {
  /** 调用方插件上下文（tracker 机制注入，非对象自有属性）。 */
  ctx: Context;
}

/** 派生插件 id（经调用方上下文追原型链；缺省 "plugin"）。 */
function pluginIdOfCtx(ctx: Context): string {
  return pluginIdOf(ctx) ?? "plugin";
}

/** 构造 ctx.slots 服务（内核 provide；调用方插件经 ctx.slots 触达）。 */
export function createSlotsApi(): SlotsApi {
  const api = {
    registerView(this: SlotsApiInstance, opts: RegisterViewOptions): () => void {
      if (typeof opts.kind !== "string" || opts.kind.length === 0) throw new Error("视图槽需要非空 kind");
      if (!opts.component && !opts.render) throw new Error("视图槽需要 component 或 render");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() =>
        registerViewSlot(opts.kind, pluginId, {
          label: opts.label,
          ...(opts.component ? { component: opts.component } : {}),
          ...(opts.render ? { render: opts.render } : {}),
        }, { priority: opts.priority ?? 0 }),
      );
    },
    registerTableView(this: SlotsApiInstance, opts: RegisterTableViewOptions): () => void {
      if (typeof opts.kind !== "string" || opts.kind.length === 0) throw new Error("表格视图需要非空 kind");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerTableViewSlot(opts.kind, pluginId, { label: opts.label, component: opts.component }, { priority: opts.priority ?? 0 }));
    },
    registerNode(this: SlotsApiInstance, opts: RegisterNodeOptions): () => void {
      if (typeof opts.type !== "string" || opts.type.length === 0) throw new Error("节点槽需要非空 type");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerNodeSlot(opts.type, pluginId, opts.component, { priority: opts.priority ?? 0 }));
    },
    registerEdge(this: SlotsApiInstance, opts: RegisterEdgeOptions): () => void {
      if (typeof opts.type !== "string" || opts.type.length === 0) throw new Error("边槽需要非空 type");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerEdgeSlot(opts.type, pluginId, opts.component, { priority: opts.priority ?? 0 }));
    },
    registerUi(this: SlotsApiInstance, opts: RegisterUiOptions): () => void {
      if (typeof opts.slot !== "string" || opts.slot.length === 0) throw new Error("UI 槽需要非空槽名");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerUiSlot(opts.slot, pluginId, { component: opts.component }, { priority: opts.priority ?? 0 }));
    },
    registerMenu(this: SlotsApiInstance, opts: RegisterMenuOptions): () => void {
      if (typeof opts.target !== "string" || opts.target.length === 0) throw new Error("菜单目标需要非空");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() =>
        registerSlotContrib(`contextmenu/${opts.target}`, pluginId, { label: opts.label, onClick: opts.onClick }, { cardinality: "list", priority: opts.priority ?? 0 }),
      );
    },
    registerSetting(this: SlotsApiInstance, opts: RegisterSettingOptions): () => void {
      if (typeof opts.key !== "string" || opts.key.length === 0) throw new Error("设置项需要非空 key");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerPluginSetting(pluginId, opts.key, opts.label, opts.component));
    },
    registerAppPage(this: SlotsApiInstance, opts: RegisterAppPageOptions): () => void {
      if (typeof opts.id !== "string" || opts.id.length === 0) throw new Error("应用页需要非空 id");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerPluginAppPage(pluginId, opts.id, opts.label, opts.component));
    },
    registerCommand(this: SlotsApiInstance, opts: RegisterCommandOptions): () => void {
      if (typeof opts.id !== "string" || opts.id.length === 0) throw new Error("命令需要非空 id");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerPluginCommand(pluginId, opts.id, opts.label, opts.run, opts.shortcut));
    },
    registerThemeSetting(this: SlotsApiInstance, opts: RegisterThemeSettingOptions): () => void {
      if (typeof opts.key !== "string" || opts.key.length === 0) throw new Error("主题设置项需要非空 key");
      const ctx = this.ctx;
      const pluginId = pluginIdOfCtx(ctx);
      return ctx.effect(() => registerPluginThemeSetting(pluginId, opts.key, opts.label, opts.component));
    },
  };
  Object.defineProperty(api, symbols.tracker, { value: { property: "ctx" } });
  return api;
}
