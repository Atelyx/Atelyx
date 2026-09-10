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
 * 同步投递保序；handler 抛错向外传播（调用方自行 try/catch，失败不静默）。
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

export type VaultEventHandler = (event: VaultEvent) => void;

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

/** 广播一个 vault 事件（同步按注册序投递；handler 抛错向外传播）。 */
export function emitVaultEvent(event: VaultEvent): void {
  const set = handlers.get(event.kind);
  if (!set) return;
  for (const h of set) h(event);
}
