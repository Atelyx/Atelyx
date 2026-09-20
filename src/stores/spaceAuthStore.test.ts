/**
 * 协作空间登录态 store 竞态测试（stores/spaceAuthStore.ts）。
 *
 * restore 逐服务器网络校验耗时长，期间并发 login/register 会写进 servers——
 * restore 完成时必须按服务器地址合并（state 已有条目优先），不得用旧快照整体覆盖。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

/** restoreSession 挂起的门（armed = 调用已到门上；release = 放行）。 */
const gate = vi.hoisted(() => {
  const state = {
    spaceServers: [] as string[],
    restoreResults: new Map<string, { serverUrl: string; userId: string; username: string; displayName: string } | null>(),
    /** 挂起 restoreSession 的目标地址（null = 不挂起）。 */
    gatedServer: null as string | null,
    armed: null as Promise<void> | null,
    resolveArmed: null as (() => void) | null,
    release: null as Promise<void> | null,
    resolveRelease: null as (() => void) | null,
  };
  return {
    state,
    /** 挂起指定服务器的下一次 restoreSession（在途恢复模拟）。 */
    gateRestore(serverUrl: string) {
      state.gatedServer = serverUrl;
      state.armed = new Promise<void>((r) => (state.resolveArmed = r));
      state.release = new Promise<void>((r) => (state.resolveRelease = r));
    },
    /** 等 restoreSession 已挂到门上。 */
    async waitArmed() {
      await state.armed;
    },
    /** 放行挂起的恢复。 */
    release() {
      state.resolveRelease?.();
      state.gatedServer = null;
    },
  };
});

vi.mock("@/services/space/auth", () => ({
  login: async (serverUrl: string) => ({
    serverUrl,
    userId: `user-login-${serverUrl}`,
    username: "loginer",
    displayName: "登录用户",
  }),
  register: async () => {
    throw new Error("测试中不应调用 register");
  },
  logout: async () => undefined,
  restoreSession: async (serverUrl: string) => {
    if (gate.state.gatedServer === serverUrl) {
      gate.state.resolveArmed?.();
      await gate.state.release;
    }
    return gate.state.restoreResults.get(serverUrl) ?? null;
  },
  listDevices: async () => [],
  revokeDevice: async () => undefined,
}));

vi.mock("@/services/global", () => ({
  readGlobalConfig: async () => ({ config: { spaceServers: gate.state.spaceServers } }),
  updateGlobalConfig: async () => null,
}));

import { useSpaceAuthStore } from "./spaceAuthStore";

function entryOf(serverUrl: string, from: "login" | "restore") {
  return from === "login"
    ? { serverUrl, userId: `user-login-${serverUrl}`, username: "loginer", displayName: "登录用户" }
    : { serverUrl, userId: `user-restore-${serverUrl}`, username: "restorer", displayName: "恢复用户" };
}

beforeEach(() => {
  gate.state.spaceServers = [];
  gate.state.restoreResults = new Map();
  gate.state.gatedServer = null;
  gate.state.armed = null;
  gate.state.release = null;
  gate.state.resolveArmed = null;
  gate.state.resolveRelease = null;
  useSpaceAuthStore.setState({ servers: [], restored: false, busy: false });
});

describe("restore 与并发登录的竞态", () => {
  it("restore 在途期间并发 login：完成后 login 的会话不被旧快照覆盖", async () => {
    gate.state.spaceServers = ["http://s1", "http://s2"];
    gate.state.restoreResults.set("http://s1", entryOf("http://s1", "restore"));
    // s2 的恢复挂起在途
    gate.gateRestore("http://s2");
    const restoring = useSpaceAuthStore.getState().restore();
    await gate.waitArmed();

    // 恢复在途期间用户完成 s2 的登录（真实流程：authLogin 落 keychain 后 set servers）
    await useSpaceAuthStore.getState().login("http://s2", "loginer", "pw");
    expect(useSpaceAuthStore.getState().getServer("http://s2")).toEqual(entryOf("http://s2", "login"));

    // 恢复继续完成（放行后 s2 的 restoreSession 返回一个「更旧」的恢复结果）
    gate.state.restoreResults.set("http://s2", entryOf("http://s2", "restore"));
    gate.release();
    await restoring;

    const servers = useSpaceAuthStore.getState().servers;
    // s1 由恢复补足；s2 保留并发登录的新会话（不被恢复结果覆盖）
    expect(servers).toHaveLength(2);
    expect(servers.find((e) => e.serverUrl === "http://s1")).toEqual(entryOf("http://s1", "restore"));
    expect(servers.find((e) => e.serverUrl === "http://s2")).toEqual(entryOf("http://s2", "login"));
    expect(useSpaceAuthStore.getState().restored).toBe(true);
  });

  it("restore 重入复用进行中的恢复（并发调用共享同一次恢复）", async () => {
    gate.state.spaceServers = ["http://s1"];
    gate.state.restoreResults.set("http://s1", entryOf("http://s1", "restore"));
    const first = useSpaceAuthStore.getState().restore();
    const second = useSpaceAuthStore.getState().restore();
    await Promise.all([first, second]);
    expect(useSpaceAuthStore.getState().restored).toBe(true);
    expect(useSpaceAuthStore.getState().servers).toEqual([entryOf("http://s1", "restore")]);
  });
});
