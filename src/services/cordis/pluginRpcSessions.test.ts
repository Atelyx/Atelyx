/**
 * 插件 attach 会话登记表测试：登记/摘除、停用卸载、在途 attach 等待与未挂载清扫。
 * 卸载动作以假句柄注入（close 记调用），不 import 任何 service。
 */
import { describe, expect, it, vi } from "vitest";

import {
  detachPluginSessions,
  detachUnmountedPluginSessions,
  trackPendingSessionAttach,
  trackPluginSession,
  untrackPluginSession,
  type TrackedSession,
} from "./pluginRpcSessions";

/** 假内核根上下文（登记表按原型链归位，普通对象即可）。 */
function fakeKernel(): object {
  return {};
}

function fakeSession(sessionId: number, fail = false) {
  const session = {
    sessionId,
    closes: 0,
    close: vi.fn(async () => {
      session.closes += 1;
      if (fail) throw new Error("卸载失败");
    }),
  };
  return session;
}

describe("pluginRpcSessions 登记表", () => {
  it("登记与摘除：同内核按插件隔离，摘除后清空插件条目", async () => {
    const ctx = fakeKernel();
    const a = fakeSession(1);
    const b = fakeSession(2);
    trackPluginSession(ctx, "com.a", a);
    trackPluginSession(ctx, "com.b", b);

    const outcome = await detachPluginSessions(ctx, "com.a");
    expect(outcome).toEqual({ detached: 1, failed: [] });
    expect(a.closes).toBe(1);
    // 摘除后 com.a 无会话，com.b 不受影响（登记表按插件隔离）
    const bOutcome = await detachPluginSessions(ctx, "com.b");
    expect(bOutcome.detached).toBe(1);
  });

  it("untrack 后停用不再触发卸载（通道已自行关闭）", async () => {
    const ctx = fakeKernel();
    const session = fakeSession(7);
    trackPluginSession(ctx, "com.a", session);
    untrackPluginSession(ctx, "com.a", 7);
    const outcome = await detachPluginSessions(ctx, "com.a");
    expect(outcome).toEqual({ detached: 0, failed: [] });
    expect(session.closes).toBe(0);
  });

  it("卸载失败逐个隔离并带回原因", async () => {
    const ctx = fakeKernel();
    trackPluginSession(ctx, "com.a", fakeSession(1, true));
    trackPluginSession(ctx, "com.a", fakeSession(2));
    const outcome = await detachPluginSessions(ctx, "com.a");
    expect(outcome.detached).toBe(1);
    expect(outcome.failed).toEqual([{ sessionId: 1, message: "卸载失败" }]);
  });

  it("在途 attach：停用先等落地再卸载（启动后立即停用不漏会话）", async () => {
    const ctx = fakeKernel();
    let release!: (session: TrackedSession) => void;
    const gate = new Promise<TrackedSession>((resolve) => {
      release = resolve;
    });
    const attach = gate.then((session) => {
      trackPluginSession(ctx, "com.a", session);
      return session;
    });
    trackPendingSessionAttach(ctx, "com.a", attach);
    // 不等 attach 完成直接停用：登记表此刻还没有会话条目
    const stopping = detachPluginSessions(ctx, "com.a");
    const session = fakeSession(9);
    release(session);
    const outcome = await stopping;
    expect(outcome.detached).toBe(1);
    expect(session.closes).toBe(1);
  });

  it("未挂载清扫：只卸载不在挂载集里的插件会话", async () => {
    const ctx = fakeKernel();
    const mounted = fakeSession(1);
    const unmounted = fakeSession(2);
    trackPluginSession(ctx, "com.mounted", mounted);
    trackPluginSession(ctx, "com.gone", unmounted);
    const failures = await detachUnmountedPluginSessions(ctx, ["com.mounted"]);
    expect(mounted.closes).toBe(0);
    expect(unmounted.closes).toBe(1);
    expect(failures.size).toBe(0);
  });

  it("清扫把卸载失败按插件归集", async () => {
    const ctx = fakeKernel();
    trackPluginSession(ctx, "com.gone", fakeSession(3, true));
    const failures = await detachUnmountedPluginSessions(ctx, []);
    expect(failures.get("com.gone")?.failed).toEqual([{ sessionId: 3, message: "卸载失败" }]);
  });
});
