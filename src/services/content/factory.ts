/**
 * 内容面工厂：按激活仓库身份分派内容后端。
 *
 * 打开/切换仓库时激活身份（个人仓库 = root 绝对路径），此后所有内容 I/O 经
 * getActiveContentBackend 取后端——后端只看身份、不按路径判定。
 * 无激活身份时回落 localBackend：localBackend root 无关（当前仓库根由 Rust 侧持有）；
 * 空间仓库必须显式激活（缺失即抛错），未激活的上下文回落 localBackend 行为不变。
 */
import type { ContentBackend, VaultIdentity } from "./contract";
import { localBackend } from "./local";
import { createSpaceContentBackend } from "./spaceContent";

function identityKey(identity: VaultIdentity): string {
  return identity.kind === "local"
    ? `local:${identity.root}`
    : `space:${identity.serverUrl}#${identity.spaceId}`;
}

/** 仓库身份的稳定比较键（null = 未激活态，回落 localBackend）。跨窗口镜像/测试断言用。 */
export function identityKeyOf(identity: VaultIdentity | null): string {
  return identity ? identityKey(identity) : "none";
}

const backends = new Map<string, ContentBackend>();
let activeKey: string | null = null;
/** 当前激活的仓库身份对象（与 activeKey 同步维护；元数据分发层按它分流本地命令/空间 meta）。 */
let activeIdentity: VaultIdentity | null = null;

/** 当前激活仓库身份（null = 未激活态）。services 层按身份分流 I/O 的统一读取口。 */
export function getActiveVaultIdentity(): VaultIdentity | null {
  return activeIdentity;
}

/**
 * 激活仓库身份。`backend` 缺省时个人仓库自动登记共享 localBackend（root 无关，同 identity 复用）；
 * 空间仓库必须显式给后端（缺失即抛错——静默回落 localBackend 会把空间 I/O 写到本地磁盘）。
 * 重复激活同一身份 = 幂等换后端。
 */
export function activateContentVault(identity: VaultIdentity, backend?: ContentBackend): void {
  const key = identityKey(identity);
  if (backend) {
    backends.set(key, backend);
  } else if (identity.kind === "local") {
    if (!backends.has(key)) backends.set(key, localBackend);
  } else {
    throw new Error(`空间仓库缺少内容后端：${key}`);
  }
  activeKey = key;
  activeIdentity = identity;
}

/** 退出激活态（内容 I/O 随即回落 localBackend）。 */
export function deactivateContentVault(): void {
  activeKey = null;
  activeIdentity = null;
}

/**
 * 按仓库身份激活对应内容后端（null = 退出激活）。
 * 撕裂窗口是独立 webview，激活态不跨窗口共享：主窗口把当前身份广播过来后，
 * 撕裂窗口用本函数自建/复用同参后端（空间按身份 key 复用实例，本地 = 共享 localBackend）。
 */
export function activateContentIdentity(identity: VaultIdentity | null): void {
  if (!identity) {
    deactivateContentVault();
  } else if (identity.kind === "space") {
    createSpaceBackendRegistration(identity.serverUrl, identity.spaceId).activate();
  } else {
    activateContentVault(identity);
  }
}

/** 当前激活仓库的内容后端。 */
export function getActiveContentBackend(): ContentBackend {
  const active = activeKey ? backends.get(activeKey) : undefined;
  return active ?? localBackend;
}

/**
 * 协作空间后端登记辅助：返回一个带 `identity` 与 `activate()` 的对象。
 * `activate()` 按身份 key 复用同一后端实例（与 `backends` Map 协调，重复激活不重建），
 * 并设为当前激活后端。调用方在激活空间仓库时调它，避免静默回落 localBackend 写本地磁盘。
 */
export function createSpaceBackendRegistration(
  serverUrl: string,
  spaceId: string,
): { identity: VaultIdentity; activate(): ContentBackend } {
  const identity: VaultIdentity = { kind: "space", serverUrl, spaceId };
  const key = identityKey(identity);
  return {
    identity,
    activate() {
      let backend = backends.get(key);
      if (!backend) {
        backend = createSpaceContentBackend(serverUrl, spaceId);
        backends.set(key, backend);
      }
      activeKey = key;
      activeIdentity = identity;
      return backend;
    },
  };
}
