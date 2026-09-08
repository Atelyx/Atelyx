/**
 * 协作锁原语（内核，域无关）：确定性锁主判定纯函数。
 * 锁语义 = 声明制：各对端把对同一资源的锁声明（peerId + 请求时间戳 since）经协作
 * presence 通道互见；确定性锁主 = since 最小者持有、同 since 按 peerId 递增取小
 * （relay 全局递增分配 peerId，各对端对同一批声明计算出一致锁主 → UI 确定性只读不闪烁）。
 */
import type { CollabPeer } from "@/types";

export interface LockClaim {
  peerId: number;
  since: number;
}

/** 单资源独占锁判定结果（调用方按需取用）。 */
export interface LockResolution {
  /** 确定性锁主 peerId；无任何锁声明 = null。 */
  owner: number | null;
  /** 本端是否为锁主（无本端声明或未接入协作时恒 false）。 */
  lockedByMe: boolean;
}

/** 确定性锁主判定：`since` 最小者持有；同 `since` 按 `peerId` 递增取小。无声明返回 null。 */
export function computeCollabLockOwner(claims: LockClaim[]): number | null {
  if (claims.length === 0) return null;
  let owner = claims[0].peerId;
  let best = claims[0].since;
  for (let i = 1; i < claims.length; i++) {
    const c = claims[i];
    if (c.since < best || (c.since === best && c.peerId < owner)) {
      best = c.since;
      owner = c.peerId;
    }
  }
  return owner;
}

/**
 * 单资源独占编辑锁统一判定（画布节点等场景：canvasStore 写守卫 / 组件 / 发送前校验
 * 多处同源）：收集本端声明（mySince）+ 对端 presence.lockedNodes 声明（按 itemId 匹配，
 * 锁跨视图保活），经 computeCollabLockOwner 确定性判定锁主。本端声明仅在 myPeerId 已分配
 * 时参与——未接入协作时声明无判定意义，结果与「无对端声明」一致。
 */
export function resolveCollabLock(
  itemId: string,
  mySince: number | undefined,
  myPeerId: number | null,
  peers: CollabPeer[],
): LockResolution {
  const claims: LockClaim[] = [];
  if (mySince !== undefined && myPeerId !== null) {
    claims.push({ peerId: myPeerId, since: mySince });
  }
  for (const p of peers) {
    const c = p.presence?.lockedNodes?.find((l) => l.id === itemId);
    if (c) claims.push({ peerId: p.peerId, since: c.since });
  }
  const owner = computeCollabLockOwner(claims);
  return { owner, lockedByMe: owner !== null && owner === myPeerId };
}
