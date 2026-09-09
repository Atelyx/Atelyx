/**
 * 领域生命周期注册表（内核原语）。
 *
 * 内核启动路径（appStore/panelStore/App boot/页面）不直接调用领域 store：领域生命周期钩子
 * （flush / 切仓库清态与进仓加载 / 回启动页清理 / 释放视图 / 视图进出窗口）经此注册表注册，
 * 内核只做分发。注册/撤销随内置插件启停驱动（pluginStore.spawn/unload → builtinPayload.lifecycle）。
 *
 * 错误语义 = 失败快速传播（fail-fast）：分发按注册序执行，任一钩子抛错即向外传播——
 * 与重构前「内核直接调用领域 store 方法、由调用方 try/catch」的语义逐位一致（如 selectVault
 * 中 flush 失败即中止切换，防跨仓库数据污染）。调用方按重构前对直接调用的处置方式包 try/catch。
 *
 * 纯数据容器 + 纯函数，无 store/service 依赖，可直测（模式同 utils/collabHost.ts）。
 */
import type { ViewKind } from "@/types";

/** 进仓/切仓库生命周期分发上下文（当前仓库 vaultId；vaultId 为 null = 未进仓/回启动页）。 */
export interface VaultLifecycleContext {
  vaultId: string | null;
}

/** 单领域生命周期钩子（按领域插件 id 注册；无对应能力时字段省略）。 */
export interface DomainLifecycleHooks {
  /** 域标识（内置插件 id，如 `builtin.canvas`；注册表键，撤销按此匹配）。 */
  id: string;
  /** 关窗前/切仓库前落盘全部 pending 改动（按注册序 await）。 */
  flush?: (ctx: VaultLifecycleContext) => Promise<void>;
  /** 切仓库同步清态（openVault 后、下一个 await 前调用——防跨仓库污染守卫要求同步执行）。 */
  onVaultLeaving?: () => void;
  /** 进入仓库后加载仓库上下文（文件树/列表刷新完成后；如 AI 会话读盘）。 */
  onVaultEntered?: (ctx: VaultLifecycleContext) => Promise<void>;
  /** 回启动页/退出仓库清理（如会话类状态落盘；调用方按需 fire-and-forget）。 */
  onVaultExit?: () => Promise<void>;
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

/** 按 id 撤销领域生命周期钩子（幂等；pluginStore 卸载/停用内置插件时调用）。 */
export function unregisterDomainLifecycle(id: string): void {
  hooks.delete(id);
}

/** 是否已注册（pluginStore 撤钩前 flush 判定用）。 */
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

/** 回启动页/退出仓库清理（按注册序 await；调用方可 fire-and-forget）。 */
export async function notifyVaultExit(): Promise<void> {
  for (const h of hooks.values()) {
    if (!h.onVaultExit) continue;
    await h.onVaultExit();
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
