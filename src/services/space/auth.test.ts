/**
 * 协作空间登录态 service 测试（services/space/auth）。
 *
 * 令牌/用户身份经 mock 的 keychain service 入库；服务器清单经 mock 的 global service 维护。
 * 覆盖：login/register 成功后令牌入库 + 清单更新；restore 会话失效从清单剔除；
 * logout 调服务端并清令牌与清单。
 */
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@/services/keychain", () => ({
  getAppSecret: vi.fn(async () => ""),
  setAppSecret: vi.fn(async () => undefined),
  deleteAppSecret: vi.fn(async () => undefined),
}));
vi.mock("@/services/global", () => ({
  getHostname: vi.fn(async () => "桌面端"),
  readGlobalConfig: vi.fn(async () => ({ config: { spaceServers: [] }, corruptBackup: null })),
  updateGlobalConfig: vi.fn(async () => null),
}));

import { getAppSecret, setAppSecret, deleteAppSecret } from "@/services/keychain";
import { readGlobalConfig, updateGlobalConfig } from "@/services/global";
import { login, register, logout, restoreSession } from "./auth";

const SERVER = "http://192.168.1.10:11224";

/** 按 URL/方法响应：探测 /spaces(401=可达) / login / register / logout / devices。 */
function authFetchMock() {
  return vi.fn(async (url: string, init: RequestInit) => {
    const u = url as string;
    const m = (init.method ?? "GET").toUpperCase();
    if (u.includes("/api/spaces") && m === "GET") {
      return new Response(JSON.stringify({ error: "未认证或会话已失效" }), {
        status: 401,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (u.includes("/api/auth/login") && m === "POST") {
      return new Response(JSON.stringify({ userId: "u1", username: "alice", displayName: "Alice", token: "tok-abc", sessionId: "s1" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (u.includes("/api/auth/register") && m === "POST") {
      return new Response(JSON.stringify({ userId: "u2", username: "bob", displayName: "Bob", token: "tok-def", sessionId: "s2" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (u.includes("/api/auth/logout") && m === "POST") {
      return new Response("{}", { status: 200 });
    }
    if (u.includes("/api/auth/devices") && m === "GET") {
      return new Response(JSON.stringify([]), { status: 200 });
    }
    return new Response("{}", { status: 200 });
  });
}

function tokenCall() {
  return (setAppSecret as ReturnType<typeof vi.fn>).mock.calls.find((c) => String(c[0]).startsWith("space-token-"));
}
function userCall() {
  return (setAppSecret as ReturnType<typeof vi.fn>).mock.calls.find((c) => String(c[0]).startsWith("space-user-"));
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe("login", () => {
  it("成功后令牌与用户身份入库，并加入 spaceServers 清单", async () => {
    vi.stubGlobal("fetch", authFetchMock());
    const info = await login(SERVER, "alice", "pw");
    expect(info).toEqual({ serverUrl: SERVER, userId: "u1", username: "alice", displayName: "Alice" });

    const tk = tokenCall();
    expect(tk).toBeTruthy();
    expect(tk?.[1]).toBe("tok-abc");

    const us = userCall();
    expect(us).toBeTruthy();
    expect(JSON.parse(String(us?.[1]))).toEqual({ userId: "u1", username: "alice", displayName: "Alice" });

    expect(updateGlobalConfig).toHaveBeenCalledWith({ spaceServers: [SERVER] });
  });
});

describe("register", () => {
  it("形状与 login 一致：令牌入库 + 清单更新", async () => {
    vi.stubGlobal("fetch", authFetchMock());
    const info = await register(SERVER, "bob", "pw");
    expect(info).toEqual({ serverUrl: SERVER, userId: "u2", username: "bob", displayName: "Bob" });
    expect(tokenCall()?.[1]).toBe("tok-def");
    expect(updateGlobalConfig).toHaveBeenCalledWith({ spaceServers: [SERVER] });
  });
});

describe("restoreSession", () => {
  it("会话失效（401）→ 清本地并从清单剔除，返回 null", async () => {
    (readGlobalConfig as ReturnType<typeof vi.fn>).mockResolvedValue({
      config: { spaceServers: [SERVER] },
      corruptBackup: null,
    });
    (getAppSecret as ReturnType<typeof vi.fn>).mockImplementation(async (name: string) => {
      if (String(name).startsWith("space-token-")) return "old-tok";
      if (String(name).startsWith("space-user-")) return JSON.stringify({ userId: "u1", username: "alice", displayName: "Alice" });
      return "";
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "未认证" }), { status: 401 })),
    );

    const result = await restoreSession(SERVER);
    expect(result).toBeNull();
    expect(deleteAppSecret).toHaveBeenCalled();
    expect(updateGlobalConfig).toHaveBeenCalledWith({ spaceServers: [] });
  });

  it("无本地令牌 → 返回 null（不触网）", async () => {
    (getAppSecret as ReturnType<typeof vi.fn>).mockResolvedValue("");
    const fetchSpy = vi.fn();
    vi.stubGlobal("fetch", fetchSpy);
    const result = await restoreSession(SERVER);
    expect(result).toBeNull();
    expect(fetchSpy).not.toHaveBeenCalled();
  });
});

describe("logout", () => {
  it("调服务端 logout 并清本地令牌 + 从清单移除", async () => {
    (readGlobalConfig as ReturnType<typeof vi.fn>).mockResolvedValue({
      config: { spaceServers: [SERVER] },
      corruptBackup: null,
    });
    vi.stubGlobal("fetch", authFetchMock());
    await logout(SERVER);
    expect(deleteAppSecret).toHaveBeenCalled();
    expect((deleteAppSecret as ReturnType<typeof vi.fn>).mock.calls.some((c) => String(c[0]).startsWith("space-token-"))).toBe(true);
    expect(updateGlobalConfig).toHaveBeenCalledWith({ spaceServers: [] });
  });
});
