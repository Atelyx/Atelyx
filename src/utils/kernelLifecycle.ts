/**
 * 领域生命周期注册表（内核原语）：领域生命周期钩子（flush / 切仓库清态与进仓加载 / 视图进出
 * 窗口）经此注册，内核启动路径只做分发、不直接调用领域 store。注册/撤销随插件启停驱动；
 * 领域 store 自身的数据边界（如笔记运行时态随仓库清空）在模块加载时自注册，不随启停撤销。
 * 错误语义 = 失败快速传播（fail-fast）：按注册序执行，任一钩子抛错即向外传播，调用方自行
 * try/catch（如 selectVault 中 flush 失败即中止切换，防跨仓库数据污染）。纯数据容器，可直测。
 */
import type { ViewKind } from "@/types";

/** 进仓/切仓库生命周期分发上下文（当前仓库身份 = root 绝对路径；null = 未进仓）。 */
export interface VaultLifecycleContext {
  vaultRoot: string | null;
}

/** 单领域生命周期钩子（按领域插件 id 注册；无对应能力时字段省略）。 */
export interface DomainLifecycleHooks {
  /** 域标识（插件 id 如 `builtin.canvas`，或自注册的 store 标识如 `noteStore`；注册表键，撤销按此匹配）。 */
  id: string;
  /** 关窗前/切仓库前落盘全部 pending 改动（按注册序 await）。 */
  flush?: (ctx: VaultLifecycleContext) => Promise<void>;
  /** 切仓库同步清态（openVault 后、下一个 await 前调用——防跨仓库污染守卫要求同步执行）。 */
  onVaultLeaving?: () => void;
  /** 进入仓库后加载仓库上下文（文件树/列表刷新完成后；如 AI 会话读盘）。 */
  onVaultEntered?: (ctx: VaultLifecycleContext) => Promise<void>;
  /** 视图离开本窗口（撕裂出去/面板关闭）：flush 落盘 + 清内存；钩子自行判断是否处理该 view。 */
  releaseView?: (view: ViewKind) => Promise<void>;
  /** 视图进入本窗口（撕裂/布局变化带回；如 aichat 回归主窗重读盘）。 */
  onViewGained?: (view: ViewKind) => void;
  /** 视图离开窗口（布局变化带走；如画布面板消失清选中）。 */
  onViewRemoved?: (view: ViewKind) => void;
}

const hooks = new Map<string, DomainLifecycleHooks>();

/** 注册领域生命周期钩子（同 id 后注册者生效）；返回撤销函数（幂等）。 */
export function registerDomainLifecycle(h: DomainLifecycleHooks): () => void {
  hooks.set(h.id, h);
  return () => {
    if (hooks.get(h.id) === h) hooks.delete(h.id);
  };
}

/** 按 id 撤销领域生命周期钩子（幂等；测试用——插件侧撤销由 ctx.effect 承担）。 */
export function unregisterDomainLifecycle(id: string): void {
  hooks.delete(id);
}

/** 是否已注册（测试用）。 */
export function hasDomainLifecycle(id: string): boolean {
  return hooks.has(id);
}

/** 全部领域 flush（按注册序 await；失败向外传播，调用方按原语义处置）。 */
export async function flushAllDomains(ctx: VaultLifecycleContext): Promise<void> {
  for (const h of hooks.values()) {
    if (!h.flush) continue;
    await h.flush(ctx);
  }
}

/** 切仓库同步清态（同步按注册序；调用方须在 openVault 后、下一个 await 前调用）。 */
export function notifyVaultLeaving(): void {
  for (const h of hooks.values()) {
    if (!h.onVaultLeaving) continue;
    h.onVaultLeaving();
  }
}

/** 进入仓库后加载领域仓库上下文（按注册序 await）。 */
export async function notifyVaultEntered(ctx: VaultLifecycleContext): Promise<void> {
  for (const h of hooks.values()) {
    if (!h.onVaultEntered) continue;
    await h.onVaultEntered(ctx);
  }
}

/** 释放视图（视图离开本窗口：撕裂出去/面板关闭；按注册序 await，钩子自行判断 view）。 */
export async function releaseView(view: ViewKind): Promise<void> {
  for (const h of hooks.values()) {
    if (!h.releaseView) continue;
    await h.releaseView(view);
  }
}

/** 视图进入本窗口（布局变化带回；同步按注册序）。 */
export function notifyViewGained(view: ViewKind): void {
  for (const h of hooks.values()) {
    if (!h.onViewGained) continue;
    h.onViewGained(view);
  }
}

/** 视图离开窗口（布局变化带走；同步按注册序）。 */
export function notifyViewRemoved(view: ViewKind): void {
  for (const h of hooks.values()) {
    if (!h.onViewRemoved) continue;
    h.onViewRemoved(view);
  }
}

// ===== 仓库切换编排动作（appStore 分发；领域 store 模块加载时注册）=====

/** 仓库切换编排动作：appStore 的切仓/退仓/画布联动流程按固定顺序调用的领域动作。
 *  appStore 经此调度而不 import 领域 store（领域 → appStore 单向）；字段未登记 = 该领域
 *  模块未加载，调用方按 no-op 跳过。 */
export interface VaultSwitchActions {
  /** 清空领域文件面板视图（切仓瞬间同步执行；防跨仓库污染守卫要求在下一个 await 前完成）。 */
  clearViews(): void;
  /** 仓库级配置按当前身份加载（切仓收尾步骤）。 */
  loadConfig(): Promise<void>;
  /** 按当前身份刷新文件树（切仓加载步骤 + 画布 CRUD 后联动共用）。 */
  loadFiles(): Promise<void>;
  /** 插件行按新仓库重载（切仓步骤；reason 为触发语境标记）。 */
  reloadPlugins(reason: string): Promise<void>;
  /** 仓库级配置落盘（应用退出收尾）。 */
  flushConfig(): Promise<void>;
  /** 停协作连接（应用退出收尾；切仓不经过——切仓走重载而非断开）。 */
  disposeCollab(): void;
}

let vaultSwitch: Partial<VaultSwitchActions> = {};

/** 登记/更新仓库切换编排动作（同字段后注册者生效；store 模块加载时调用）。 */
export function registerVaultSwitchActions(patch: Partial<VaultSwitchActions>): void {
  vaultSwitch = { ...vaultSwitch, ...patch };
}

/** 读已登记动作（测试用；未登记字段由调用方按 no-op 跳过）。 */
export function vaultSwitchActions(): Partial<VaultSwitchActions> {
  return vaultSwitch;
}
