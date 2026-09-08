/**
 * 协作锁原语纯函数测试（services/collab/locks.ts）。
 * 覆盖确定性锁主判定（since 最小/同 since peerId 取小/空数组）与单资源锁判定
 * （本端+对端声明收集、myPeerId 未分配时本端不参与）。
 */
import { describe, it, expect } from "vitest";
import type { CollabPeer } from "@/types";
import { computeCollabLockOwner, resolveCollabLock } from "./locks";

describe("computeCollabLockOwner", () => {
  it("since 最小者持有；同 since 按 peerId 递增取小；空数组 null", () => {
    expect(computeCollabLockOwner([{ peerId: 5, since: 200 }, { peerId: 3, since: 100 }])).toBe(3);
    expect(computeCollabLockOwner([{ peerId: 5, since: 100 }, { peerId: 3, since: 100 }])).toBe(3);
    expect(computeCollabLockOwner([{ peerId: 3, since: 100 }, { peerId: 5, since: 200 }])).toBe(3);
    expect(computeCollabLockOwner([])).toBeNull();
  });
});

describe("resolveCollabLock", () => {
  const peersWithLock = (locked: Array<{ id: string; since: number }>): CollabPeer[] => [
    {
      peerId: 10,
      nickname: "a",
      color: "#000",
      deviceName: "d",
      presence: {
        file: "x.atlx",
        view: "canvas",
        selection: null,
        lockedNodes: locked,
      },
    },
  ];

  it("本端+对端声明收集：确定性锁主", () => {
    // 本端 since 更早 → 本端持锁
    const mine = resolveCollabLock("n1", 100, 5, peersWithLock([{ id: "n1", since: 200 }]));
    expect(mine.owner).toBe(5);
    expect(mine.lockedByMe).toBe(true);
    // 对端 since 更早 → 对端持锁
    const theirs = resolveCollabLock("n1", 300, 5, peersWithLock([{ id: "n1", since: 200 }]));
    expect(theirs.owner).toBe(10);
    expect(theirs.lockedByMe).toBe(false);
  });

  it("无对端声明 + 有本端声明：本端持锁", () => {
    const r = resolveCollabLock("n1", 100, 5, peersWithLock([]));
    expect(r.owner).toBe(5);
    expect(r.lockedByMe).toBe(true);
  });

  it("myPeerId 未分配：本端声明不参与（结果与无对端声明一致）", () => {
    const r = resolveCollabLock("n1", 100, null, peersWithLock([]));
    expect(r.owner).toBeNull();
    expect(r.lockedByMe).toBe(false);
  });
});
