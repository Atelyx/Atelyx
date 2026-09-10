/**
 * 仓库文件事件发射器（内核事件总线）。
 *
 * 两类来源统一走此总线，内核（vaultStore）不直接调用领域 store：
 * - watcher 分发：`canvas:changed`/`note:changed`/`table:changed`/`attachment:changed`/`chat:changed`
 *   （载荷只带路径；自写回波/重命名中路径的抑制判定仍归内核，事件载荷不含抑制信息）；
 * - 文件动作联动：note/table/attachment 重命名/移动/删除、文件夹重命名/移动（载荷含 old/new 路径，
 *   供领域订阅者做画布节点引用同步/撤销栈路径迁移/UI 状态 remap 等）。
 *
 * 订阅随插件启停注册（cordis/builtins 的 vaultEventHandlers）；未注册 kind 静默丢弃。
 * 投递两种口径：
 * - `emitVaultEvent`：同步按注册序投递（watcher 信号泵用；领域反应的 await 不阻塞投递方，
 *   与既有「泵内 fire-and-forget」语义一致）；
 * - `emitVaultEventAsync`：逐个 await handler（文件动作路径用；领域反应须在调用方继续前完成，
 *   如画布乐观锁基准同步不得晚于后续自动保存）。
 * handler 抛错向外传播（调用方自行 try/catch，失败不静默）。
 * 纯数据容器 + 纯函数，无 store/service 依赖，可直测（模式同 utils/collabHost.ts）。
 */

/** 文件动作事件（重命名/移动/删除/文件夹重命名）：载荷含新旧路径。 */
export type VaultActionEvent =
  | { kind: "note:renamed" | "note:moved" | "table:renamed" | "table:moved" | "attachment:renamed" | "attachment:moved" | "canvas:renamed" | "canvas:moved"; oldPath: string; newPath: string; newTitle?: string | null }
  | { kind: "note:deleted" | "table:deleted" | "canvas:deleted"; path: string }
  | { kind: "folder:renamed" | "folder:moved"; oldDir: string; newDir: string }
  | { kind: "canvas:error"; message: string };

/** watcher 变更事件：纯路径信号，领域按需再读盘/取快照（判别联合，kind 为单个字面量）。 */
export type VaultWatchEvent =
  | { kind: "canvas:changed"; path: string }
  | { kind: "note:changed"; path: string }
  | { kind: "table:changed"; path: string }
  | { kind: "attachment:changed"; path: string }
  | { kind: "chat:changed"; path: string };

/** 全部 vault 事件（判别联合；订阅者按 kind 收窄载荷）。 */
export type VaultEvent = VaultWatchEvent | VaultActionEvent;

export type VaultEventHandler = (event: VaultEvent) => void | Promise<void>;

/** 某 kind 事件的具体载荷类型：交叉收窄（成员 kind 为联合时也能正确落到单个字面量）。 */
export type VaultEventOf<K extends VaultEvent["kind"]> = VaultEvent & { kind: K };

/** 订阅条目（声明式，cordis/builtins 直接消费；handler 为宽化事件，按 kind 自行收窄）。 */
export interface VaultEventSubscription {
  kind: VaultEvent["kind"];
  handler: VaultEventHandler;
}

const handlers = new Map<VaultEvent["kind"], Set<VaultEventHandler>>();

/** 按声明条目订阅某 kind 的 vault 事件（随插件启停注册；返回撤销函数，幂等）。 */
export function subscribeVaultEvent(sub: VaultEventSubscription): () => void {
  const { kind, handler } = sub;
  let set = handlers.get(kind);
  if (!set) {
    set = new Set();
    handlers.set(kind, set);
  }
  set.add(handler);
  return () => {
    set.delete(handler);
    if (set.size === 0) handlers.delete(kind);
  };
}

/** 订阅某 kind 的 vault 事件（handler 载荷已按 kind 收窄，如 `note:renamed` → 含 oldPath/newPath）。 */
export function onVaultEvent<K extends VaultEvent["kind"]>(
  kind: K,
  handler: (event: VaultEventOf<K>) => void,
): () => void {
  return subscribeVaultEvent({ kind, handler: handler as VaultEventHandler });
}

/** 广播一个 vault 事件（同步按注册序投递；handler 抛错向外传播）。
 *  同步口径不等待领域反应：返回 Promise 的 handler 由调用方自担收尾，此处只兜住未捕获拒绝。 */
export function emitVaultEvent(event: VaultEvent): void {
  const set = handlers.get(event.kind);
  if (!set) return;
  for (const h of set) {
    const ret = h(event);
    if (ret instanceof Promise) void ret.catch((e) => console.error("仓库事件订阅方失败", e));
  }
}

/** 广播并等待全部 handler 完成（按注册序；文件动作路径用）。
 *  快照订阅者列表：等待期间有注册/撤销不改变本次投递对象（避免漏投/重投）。
 *  单个订阅方失败不阻断其余订阅方（领域反应彼此独立），全部投递完再抛聚合错误。 */
export async function emitVaultEventAsync(event: VaultEvent): Promise<void> {
  const set = handlers.get(event.kind);
  if (!set) return;
  const failures: unknown[] = [];
  for (const h of [...set]) {
    try {
      await h(event);
    } catch (e) {
      failures.push(e);
      console.error("仓库事件订阅方失败", e);
    }
  }
  if (failures.length > 0) {
    throw new Error(`仓库事件 ${event.kind} 有 ${failures.length} 个订阅方失败`);
  }
}
