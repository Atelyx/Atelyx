/**
 * 内容面工厂：按激活仓库身份分派内容后端。
 *
 * 打开/切换仓库时激活身份（个人仓库 = root 绝对路径），此后所有内容 I/O 经
 * getActiveContentBackend 取后端——后端只看身份、不按路径判定。
 * 无激活身份时回落 localBackend：localBackend root 无关（当前仓库根由 Rust 侧持有），
 * 未走激活流程的上下文（撕裂窗口独立 webview）行为不变；空间后端接入后激活成为必要前置。
 */
import type { ContentBackend, VaultIdentity } from "./contract";
import { localBackend } from "./local";

function identityKey(identity: VaultIdentity): string {
  return identity.kind === "local"
    ? `local:${identity.root}`
    : `space:${identity.serverUrl}#${identity.spaceId}`;
}

const backends = new Map<string, ContentBackend>();
let activeKey: string | null = null;

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
}

/** 退出激活态（回启动页等；内容 I/O 随即回落 localBackend）。 */
export function deactivateContentVault(): void {
  activeKey = null;
}

/** 当前激活仓库的内容后端。 */
export function getActiveContentBackend(): ContentBackend {
  const active = activeKey ? backends.get(activeKey) : undefined;
  return active ?? localBackend;
}
