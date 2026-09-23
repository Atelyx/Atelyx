/**
 * 协作空间 HTTP 客户端契约测试（services/space/client）。
 *
 * 核心回归：URL/方法/头正确；非 2xx/网络/超时统一归一成 `SpaceApiError`（中文可定位 + `status`）；
 * meta 三方法的请求形状（路径 + 查询 + 体）与服务端契约一致。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { createSpaceClient, SpaceApiError } from "./client";

const SERVER = "http://192.168.1.10:11224";
const TOKEN = "tok-123";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function lastCall(fetchMock: ReturnType<typeof vi.fn>): [string, RequestInit] {
  const call = fetchMock.mock.calls[fetchMock.mock.calls.length - 1];
  return [call[0] as string, call[1] as RequestInit];
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("请求形状", () => {
  it("login 发 POST /api/auth/login，不带 Authorization，体为 credentials", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ userId: "u1", username: "alice", displayName: "A", token: "t", sessionId: "s" }));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);
    await client.auth.login({ username: "alice", password: "pw" });
    const [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/auth/login");
    expect(init.method).toBe("POST");
    expect((init.headers as Record<string, string>)["Authorization"]).toBeUndefined();
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
    expect(JSON.parse(init.body as string)).toEqual({ username: "alice", password: "pw" });
  });

  it("鉴权请求附加 Bearer 令牌", async () => {
    const fetchMock = vi.fn(async () => jsonResponse([]));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);
    await client.spaces.list();
    const [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces");
    expect((init.headers as Record<string, string>)["Authorization"]).toBe(`Bearer ${TOKEN}`);
  });

  it("serverUrl 尾斜杠被规整", async () => {
    const fetchMock = vi.fn(async () => jsonResponse([]));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(`${SERVER}/`, async () => TOKEN);
    await client.spaces.list();
    expect(lastCall(fetchMock)[0]).toBe("http://192.168.1.10:11224/api/spaces");
  });

  it("content 读写走正确方法与路径参数", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ content: "x", updatedAt: 1 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);
    await client.content.readFile("sp1", "a/b.md");
    const [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/file?path=a%2Fb.md");
    expect(init.method).toBe("GET");

    fetchMock.mockReset();
    fetchMock.mockResolvedValue(jsonResponse({ updatedAt: 2 }));
    await client.content.writeFile("sp1", { path: "a/b.md", content: "hi" });
    const [wurl, winit] = lastCall(fetchMock);
    expect(wurl).toBe("http://192.168.1.10:11224/api/spaces/sp1/file");
    expect(winit.method).toBe("PUT");
    expect(JSON.parse(winit.body as string)).toEqual({ path: "a/b.md", content: "hi" });
  });
});

describe("错误归一化", () => {
  it("400 带服务端消息，status=400", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "用户名或密码错误" }), { status: 400 })),
    );
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const err = (await client.auth.login({ username: "a", password: "b" }).catch((e) => e)) as SpaceApiError;
    expect(err).toBeInstanceOf(SpaceApiError);
    expect(err.status).toBe(400);
    expect(err.code).toBe("http");
    expect(err.serverMessage).toBe("用户名或密码错误");
    expect(err.message).toContain("返回 400");
    expect(err.message).toContain(SERVER);
  });

  it("401 status=401（会话失效，供上层分类）", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "未认证或会话已失效" }), { status: 401 })),
    );
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const err = (await client.auth.listDevices().catch((e) => e)) as SpaceApiError;
    expect(err.status).toBe(401);
    expect(err.code).toBe("http");
  });

  it("网络断开 → code=network, status=0", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => {
      throw new TypeError("fetch failed");
    }));
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const err = (await client.spaces.list().catch((e) => e)) as SpaceApiError;
    expect(err).toBeInstanceOf(SpaceApiError);
    expect(err.status).toBe(0);
    expect(err.code).toBe("network");
    expect(err.message).toContain("无法连接协作服务器");
  });

  it("超时 → code=timeout, status=0", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => {
      const e = new Error("The operation was aborted");
      e.name = "AbortError";
      throw e;
    }));
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const err = (await client.spaces.list().catch((e) => e)) as SpaceApiError;
    expect(err).toBeInstanceOf(SpaceApiError);
    expect(err.status).toBe(0);
    expect(err.code).toBe("timeout");
    expect(err.message).toContain("超时");
  });
});

describe("meta 请求形状", () => {
  it("space 级 meta：GET 无 query / PATCH 带 values 体 / DELETE 带 scope=space&key", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ values: {} }));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);

    await client.meta.getSpaceMeta("sp1");
    let [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/meta");
    expect(init.method).toBe("GET");

    await client.meta.patchSpaceMeta("sp1", { values: { theme: "dark" } });
    [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/meta");
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(init.body as string)).toEqual({ values: { theme: "dark" } });

    await client.meta.deleteSpaceMeta("sp1", "theme");
    [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/meta?scope=space&key=theme");
    expect(init.method).toBe("DELETE");
  });

  it("my 级 meta：路径为 /meta/me，delete 仅带 key（无 scope）", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ values: {} }));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);

    await client.meta.getMyMeta("sp1");
    let [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/meta/me");
    expect(init.method).toBe("GET");

    await client.meta.patchMyMeta("sp1", { values: { nickname: "x" } });
    [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/meta/me");
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(init.body as string)).toEqual({ values: { nickname: "x" } });

    await client.meta.deleteMyMeta("sp1", "nickname");
    [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/meta/me?key=nickname");
    expect(url).not.toContain("scope");
    expect(init.method).toBe("DELETE");
  });
});

describe("增量补丁端点", () => {
  it("patchCanvas 发 POST /patches/canvas，200 返回成功结果", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ updatedAt: 42, file: "新.atlx" }));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const patch = { id: "c1" } as never;
    const result = await client.content.patchCanvas("sp1", { path: "c.atlx", patch });
    expect(result).toEqual({ updatedAt: 42, file: "新.atlx" });
    const [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/patches/canvas");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ path: "c.atlx", patch });
  });

  it("patchTable 200 返回成功结果与落盘后路径", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ updatedAt: 7, file: "t.atb" }));
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const patch = { id: "t1" } as never;
    const result = await client.content.patchTable("sp1", { path: "t.atb", patch });
    expect(result).toEqual({ updatedAt: 7, file: "t.atb" });
    const [url, init] = lastCall(fetchMock);
    expect(url).toBe("http://192.168.1.10:11224/api/spaces/sp1/patches/table");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ path: "t.atb", patch });
  });

  it("补丁端点错误照常抛 SpaceApiError（如 patch.id 不匹配 400）", async () => {
    const fetchMock = vi.fn(async () =>
      jsonResponse({ error: "补丁身份不匹配" }, 400),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = createSpaceClient(SERVER, async () => TOKEN);
    const err = (await client.content
      .patchCanvas("sp1", { path: "c.atlx", patch: { id: "c1" } as never })
      .catch((e) => e)) as SpaceApiError;
    expect(err).toBeInstanceOf(SpaceApiError);
    expect(err.status).toBe(400);
  });
});
