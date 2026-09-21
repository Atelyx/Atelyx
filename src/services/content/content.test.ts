/**
 * 协作空间内容后端测试（services/content/spaceContent）。
 *
 * 风格照 client.test.ts：桩 `global.fetch` 逐 URL/方法/体回包，断言透传与错误传播；
 * 引用改写用受控 grep 回包 + 文件内容，断言真实变更文件被读改写、无关文件零读写。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import type { TreeNode } from "@/services/space/client";
import type { ContentBackend } from "./contract";
import type { GrepMatchRow } from "@/types";
import { createSpaceContentBackend, UnsupportedInSpaceError } from "./spaceContent";
import { createSpaceBackendRegistration } from "./factory";
import { bytesToBase64 } from "@/utils/base64";
import { SpaceApiError } from "@/services/space/client";
import { bumpRecentSpace, removeRecentSpace, spaceKey } from "@/services/global";

vi.mock("@/services/space/auth", () => ({
  getToken: async () => "tok-test",
}));

const SERVER = "http://space.test";
const SPACE = "SP";

interface State {
  tree?: TreeNode[];
  files?: Record<string, { content: string; updatedAt: number }>;
  folderDelete?: { deleted?: boolean; needsConfirm?: boolean };
  grep?: Record<string, GrepMatchRow[]>;
  backlinks?: unknown[];
  tags?: unknown[];
  globPaths?: string[];
}

let calls: Array<{ method: string; url: string; body?: unknown }>;

function responder(state: State) {
  return (url: string, method: string, body?: unknown) => {
    const path = url.split("?")[0];
    if (/\/file\?/.test(url)) {
      const p = new URL(url).searchParams.get("path") ?? "";
      if (method === "GET") return { body: state.files?.[p] ?? { content: "", updatedAt: 0 } };
    } else if (path.endsWith("/file")) {
      if (method === "PUT") return { body: { updatedAt: 1 } };
      if (method === "DELETE") return { body: { deleted: true } };
    } else if (path.endsWith("/patches/canvas") || path.endsWith("/patches/table")) {
      return { body: { updatedAt: 7, file: (body as { path: string }).path } };
    } else if (path.endsWith("/tree")) {
      return { body: state.tree ?? [] };
    } else if (path.endsWith("/rename")) {
      return { body: {} };
    } else if (path.endsWith("/folder")) {
      if (method === "POST") return { body: { path: (body as { path: string }).path } };
      if (method === "DELETE") return { body: state.folderDelete ?? { deleted: true } };
    } else if (path.endsWith("/grep")) {
      const matches = state.grep?.[(body as { pattern: string }).pattern] ?? [];
      return { body: { matches, total: matches.length, capped: false } };
    } else if (path.endsWith("/backlinks")) {
      return { body: state.backlinks ?? [] };
    } else if (path.endsWith("/tags")) {
      return { body: state.tags ?? [] };
    } else if (path.endsWith("/glob")) {
      const paths = state.globPaths ?? [];
      return { body: { root: (body as { path?: string }).path ?? "", paths, total: paths.length, capped: false } };
    }
    return { body: {} };
  };
}

function setupFetch(state: State) {
  calls = [];
  const fn = async (url: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    const body = init?.body ? JSON.parse(init.body as string) : undefined;
    calls.push({ method, url, body });
    const res = responder(state)(url, method, body);
    return new Response(JSON.stringify(res.body), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };
  vi.stubGlobal("fetch", fn);
}

function backend() {
  return createSpaceContentBackend(SERVER, SPACE);
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("树映射", () => {
  it("服务端 tree 形状 → FileTreeNode（含嵌套目录）", async () => {
    setupFetch({
      tree: [
        {
          name: "笔记",
          path: "笔记",
          isDir: true,
          updatedAt: 100,
          children: [
            { name: "a.md", path: "笔记/a.md", isDir: false, updatedAt: 50, children: [] },
          ],
        },
        { name: "根.md", path: "根.md", isDir: false, updatedAt: 70, children: [] },
      ],
    });
    const nodes = await backend().listTree();
    expect(nodes).toEqual([
      {
        name: "笔记",
        path: "笔记",
        isDir: true,
        updatedAt: 100,
        children: [{ name: "a.md", path: "笔记/a.md", isDir: false, updatedAt: 50, children: [] }],
      },
      { name: "根.md", path: "根.md", isDir: false, updatedAt: 70, children: [] },
    ]);
  });

  it("listDir 切单层（目录在前），不额外发请求", async () => {
    setupFetch({
      tree: [
        { name: "z.md", path: "z.md", isDir: false, updatedAt: 1, children: [] },
        {
          name: "d1",
          path: "d1",
          isDir: true,
          updatedAt: 2,
          children: [
            { name: "inner.md", path: "d1/inner.md", isDir: false, updatedAt: 3, children: [] },
          ],
        },
      ],
    });
    const res = await backend().listDir();
    expect(calls.filter((c) => c.url.endsWith("/tree")).length).toBe(1);
    expect(res.entries).toEqual([
      { name: "d1", kind: "dir", children: 1 },
      { name: "z.md", kind: "file" },
    ]);
    expect(res.total).toBe(2);
    expect(res.capped).toBe(false);
  });
});

describe("读/写/删透传与错误传播", () => {
  it("readFile / writeFile / deleteNote 透传对应端点", async () => {
    setupFetch({ files: { "a.md": { content: "正文", updatedAt: 9 } } });
    const b = backend();
    expect(await b.readFile("a.md")).toBe("正文");
    await b.writeFile("a.md", "新");
    await b.deleteNote("a.md");
    const methods = calls.map((c) => `${c.method} ${c.url}`);
    expect(methods).toContain("GET http://space.test/api/spaces/SP/file?path=a.md");
    expect(methods).toContain("PUT http://space.test/api/spaces/SP/file");
    expect(methods).toContain("DELETE http://space.test/api/spaces/SP/file?path=a.md");
  });

  it("写失败如实抛出（内容 I/O 失败不得静默）", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "磁盘已满" }), { status: 500 })),
    );
    await expect(backend().writeFile("a.md", "x")).rejects.toThrow();
  });
});

describe("画布/表格写路径与增量补丁", () => {
  /** 固定状态码回包：409/400 分支测试用（成功分支走 setupFetch）。 */
  function setupFetchStatus(status: number, body: unknown) {
    calls = [];
    const fn = async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      const parsed = init?.body ? JSON.parse(init.body as string) : undefined;
      calls.push({ method, url, body: parsed });
      return new Response(JSON.stringify(body), {
        status,
        headers: { "Content-Type": "application/json" },
      });
    };
    vi.stubGlobal("fetch", fn);
  }

  it("writeCanvas：PUT /file 序列化 .atlx 全量 + baseUpdatedAt 透传，返回 updatedAt", async () => {
    setupFetch({});
    const canvas = { id: "c1", title: "画布" } as never;
    const updatedAt = await backend().writeCanvas(canvas, "c.atlx", 10);
    expect(updatedAt).toBe(1);
    const write = calls.find((c) => c.method === "PUT");
    expect(write?.body).toEqual({
      path: "c.atlx",
      content: JSON.stringify(canvas),
      baseUpdatedAt: 10,
    });
  });

  it("writeTable：baseUpdatedAt 缺省时不携带该字段", async () => {
    setupFetch({});
    const table = { id: "t1", title: "表格" } as never;
    await backend().writeTable(table, "t.atb");
    const write = calls.find((c) => c.method === "PUT");
    expect(write?.body).toEqual({ path: "t.atb", content: JSON.stringify(table) });
  });

  it("writeCanvas 冲突（409）抛本地同形错误，store 冲突分支可判定", async () => {
    setupFetchStatus(409, { error: "版本冲突", updatedAt: 99 });
    const err = await backend()
      .writeCanvas({ id: "c1" } as never, "c.atlx", 10)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("画布已被外部修改，请重载后再编辑");
  });

  it("writeTable 冲突（409）抛本地同形错误", async () => {
    setupFetchStatus(409, { error: "版本冲突", updatedAt: 99 });
    const err = await backend()
      .writeTable({ id: "t1" } as never, "t.atb", 10)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("表格已被外部修改，请重载后再编辑");
  });

  it("patchCanvas：POST /patches/canvas，成功返回 {updatedAt, file}", async () => {
    setupFetch({});
    const patch = { id: "c1" } as never;
    const result = await backend().patchCanvas(patch, "c.atlx", 10);
    expect(result).toEqual({ updatedAt: 7, file: "c.atlx" });
    const call = calls.find((c) => c.url.endsWith("/patches/canvas"));
    expect(call?.method).toBe("POST");
    expect(call?.body).toEqual({ path: "c.atlx", patch, baseUpdatedAt: 10 });
  });

  it("patchTable：POST /patches/table，force 透传", async () => {
    setupFetch({});
    const patch = { id: "t1" } as never;
    await backend().patchTable(patch, "t.atb", 3, true);
    const call = calls.find((c) => c.url.endsWith("/patches/table"));
    expect(call?.body).toEqual({ path: "t.atb", patch, baseUpdatedAt: 3, force: true });
  });

  it("patchCanvas 冲突（409）抛本地同形错误（与 Tauri 补丁冲突一致走抛错分支）", async () => {
    setupFetchStatus(409, { error: "版本冲突", updatedAt: 99 });
    const err = await backend()
      .patchCanvas({ id: "c1" } as never, "c.atlx", 10)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("画布已被外部修改，请重载后再编辑");
  });

  it("patchTable 冲突（409）抛本地同形错误", async () => {
    setupFetchStatus(409, { error: "版本冲突", updatedAt: 99 });
    const err = await backend()
      .patchTable({ id: "t1" } as never, "t.atb", 10, false)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("表格已被外部修改，请重载后再编辑");
  });

  it("patchCanvas 400（patch.id 不匹配）按 SpaceApiError 如实抛出", async () => {
    setupFetchStatus(400, { error: "补丁身份不匹配" });
    const err = await backend()
      .patchCanvas({ id: "别的画布" } as never, "c.atlx", 10)
      .catch((e) => e);
    expect(err).toBeInstanceOf(SpaceApiError);
    expect((err as SpaceApiError).status).toBe(400);
  });

  it("patchCanvas 404（文件被外部删除）抛本地逐字字符串，store 据此回退全量写", async () => {
    setupFetchStatus(404, { error: "画布文件不存在（已从磁盘删除）" });
    const err = await backend()
      .patchCanvas({ id: "c1" } as never, "c.atlx", 10)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("画布文件不存在（已从磁盘删除）");
  });

  it("patchTable 404（文件被外部删除）抛本地逐字字符串", async () => {
    setupFetchStatus(404, { error: "表格文件不存在（已从磁盘删除）" });
    const err = await backend()
      .patchTable({ id: "t1" } as never, "t.atb", 10, false)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("表格文件不存在（已从磁盘删除）");
  });

  it("writeCanvas 404（路径级错误）抛字符串（与本地 Tauri 字符串错误形态一致）", async () => {
    setupFetchStatus(404, { error: "路径不存在：c.atlx (os error 3)" });
    const err = await backend()
      .writeCanvas({ id: "c1" } as never, "c.atlx", 10)
      .catch((e) => e);
    expect(typeof err).toBe("string");
    expect(err).toBe("路径不存在：c.atlx (os error 3)");
  });
});

describe("readFileWindow 切窗", () => {
  it("offset/limit/截断标记正确", async () => {
    setupFetch({ files: { "a.md": { content: "L1\nL2\nL3\nL4", updatedAt: 1 } } });
    const b = backend();
    const r = await b.readFileWindow("a.md", { offset: 2, limit: 1 });
    expect(r).toEqual({
      lines: [{ number: 2, text: "L2" }],
      totalLines: 4,
      truncated: true,
    });
  });

  it("末段窗口不截断", async () => {
    setupFetch({ files: { "a.md": { content: "L1\nL2", updatedAt: 1 } } });
    const r = await backend().readFileWindow("a.md", { offset: 1, limit: 2000 });
    expect(r.truncated).toBe(false);
    expect(r.lines.length).toBe(2);
  });
});

describe("索引方法透传", () => {
  it("scanBacklinks/scanTags/glob/grep 走新增端点", async () => {
    setupFetch({
      backlinks: [{ file: "x.md", title: "x" }],
      tags: [{ tag: "t", count: 1 }],
      globPaths: ["a.md"],
      grep: { kw: [{ path: "a.md", lineNumber: 1, line: "kw" }] },
    });
    const b = backend();
    expect(await b.scanBacklinks("n", "n.md")).toEqual([{ file: "x.md", title: "x" }]);
    expect(await b.scanTags()).toEqual([{ tag: "t", count: 1 }]);
    const g = await b.glob("**/*.md", { path: "d" });
    expect(g).toEqual({ root: "d", paths: ["a.md"], total: 1, capped: false });
    const gr = await b.grep("kw");
    expect(gr.matches).toEqual([{ path: "a.md", lineNumber: 1, line: "kw" }]);
  });
});

describe("不支持方法与无操作", () => {
  const unsupportedMethods: Array<[string, (b: ContentBackend) => Promise<unknown>]> = [
    ["repoHistoryAggregate", (b) => b.repoHistoryAggregate()],
  ];

  for (const [feature, call] of unsupportedMethods) {
    it(`${feature} 抛 UnsupportedInSpaceError 且 feature 正确`, async () => {
      setupFetch({});
      const b = backend();
      const err = (await call(b).catch((e) => e)) as UnsupportedInSpaceError;
      expect(err).toBeInstanceOf(UnsupportedInSpaceError);
      expect(err.feature).toBe(feature);
      expect(err.message).toBe("协作空间暂不支持该功能");
    });
  }

  it("remapSideloads / remapSideloadsByDir 静默成功（空间无历史侧文件）", async () => {
    setupFetch({});
    const b = backend();
    await expect(b.remapSideloads("a.md", "b.md")).resolves.toBeUndefined();
    await expect(b.remapSideloadsByDir("d", "e")).resolves.toBeUndefined();
    // 不应有任何 I/O 请求
    expect(calls).toEqual([]);
  });
});

describe("结构变更透传", () => {
  it("deleteFolder 映射 needsConfirm / deleted", async () => {
    setupFetch({ folderDelete: { needsConfirm: true } });
    const r = await backend().deleteFolder("非空", false);
    expect(r).toEqual({ deleted: false, needsConfirm: true, itemCount: 0 });
    // force=true 递归删
    setupFetch({ folderDelete: { deleted: true } });
    const r2 = await backend().deleteFolder("非空", true);
    expect(r2).toEqual({ deleted: true, needsConfirm: false, itemCount: 0 });
  });

  it("createFolder 返回路径", async () => {
    setupFetch({});
    expect(await backend().createFolder("d/e")).toBe("d/e");
  });

  it("renameCanvas 退化为纯路径重命名（标题按文件名推算）", async () => {
    setupFetch({});
    await backend().renameCanvas("c.atlx", "新名");
    const rename = calls.find((c) => c.url.endsWith("/rename"));
    expect(rename?.body).toEqual({ oldPath: "c.atlx", newPath: "新名.atlx" });
  });
});

describe("renameNote 引用改写收敛", () => {
  it("引用文件被改写、无关（含旧名为子串）文件零读写", async () => {
    const unrelated = "notes/含旧名子串.md";
    setupFetch({
      files: {
        "A.md": { content: "见 [[旧名]]\n链接 [label](旧名.md)", updatedAt: 1 },
        [unrelated]: { content: "[[旧名2]] 与 [x](旧名2.md)", updatedAt: 1 },
      },
      grep: {
        // wiki 写法命中 A.md
        "\\[\\[旧名(?:\\||\\]\\])": [{ path: "A.md", lineNumber: 1, line: "见 [[旧名]]" }],
        // 路径写法命中 A.md（basename 形式旧名.md）
        "\\]\\([^)]*(?:旧名\\.md)": [{ path: "A.md", lineNumber: 2, line: "链接 [label](旧名.md)" }],
      },
    });
    const b = backend();
    const result = await b.renameNote("旧名.md", "新名.md");
    expect(result.rewritten).toEqual(["A.md"]);

    // 仅 A.md 被读、被写；无关文件零读写（rename 只发 POST，不读旧文件）
    const readPaths = calls
      .filter((c) => c.method === "GET" && /\/file\?/.test(c.url))
      .map((c) => new URL(c.url).searchParams.get("path"));
    const writePaths = calls
      .filter((c) => c.method === "PUT")
      .map((c) => (c.body as { path: string }).path);
    expect(readPaths).toEqual(["A.md"]);
    expect(writePaths).toEqual(["A.md"]);
    expect(readPaths).not.toContain(unrelated);
    expect(writePaths).not.toContain(unrelated);
  });

  it("改写内容：wiki 名与路径目标段均更新，别名保留", async () => {
    setupFetch({
      files: {
        "N.md": { content: "[[旧名]] 与 [[旧名|别名]] 和 [t](旧名.md)", updatedAt: 1 },
      },
      grep: {
        "\\[\\[旧名(?:\\||\\]\\])": [{ path: "N.md", lineNumber: 1, line: "..." }],
        "\\]\\([^)]*(?:旧名\\.md)": [{ path: "N.md", lineNumber: 1, line: "..." }],
      },
    });
    const b = backend();
    await b.renameNote("旧名.md", "新名.md");
    const write = calls.find((c) => c.method === "PUT");
    expect((write?.body as { content: string }).content).toBe(
      "[[新名]] 与 [[新名|别名]] 和 [t](新名.md)",
    );
  });

  it("图片语法 ![[旧名]] / ![](路径) 不改写", async () => {
    setupFetch({
      files: {
        "N.md": { content: "![[旧名]]\n![图](旧名.md)", updatedAt: 1 },
      },
      grep: {
        "\\[\\[旧名(?:\\||\\]\\])": [{ path: "N.md", lineNumber: 1, line: "..." }],
        "\\]\\([^)]*(?:旧名\\.md)": [{ path: "N.md", lineNumber: 2, line: "..." }],
      },
    });
    const b = backend();
    const result = await b.renameNote("旧名.md", "新名.md");
    expect(result.rewritten).toEqual([]); // 无真实改写，不写回
  });
});

describe("renameFolder 引用改写", () => {
  it("指向旧目录的路径链接前缀改写，wiki 名不变", async () => {
    setupFetch({
      files: {
        "其它/B.md": {
          content: "[x](旧目录/ note.md) 与 [[某笔记]] 和 [y](旧目录/sub/z.md)",
          updatedAt: 1,
        },
      },
      grep: {
        "\\]\\([^)]*旧目录": [
          { path: "其它/B.md", lineNumber: 1, line: "..." },
        ],
      },
    });
    const b = backend();
    await b.renameFolder("旧目录", "新目录");
    const write = calls.find((c) => c.method === "PUT");
    expect((write?.body as { content: string }).content).toBe(
      "[x](新目录/ note.md) 与 [[某笔记]] 和 [y](新目录/sub/z.md)",
    );
  });
});

// ===== 画布/表格/附件全量（内存服务端桩） =====

/**
 * 最小内存服务端：文件树（文本 + base64 二进制）+ 保留媒体目录 + 结构变更。
 * 比 setupFetch 的声明式回包更能表达多步读写链（复制/引用改写/清理都跨多请求）。
 */
function setupSpaceServer(init?: {
  files?: Record<string, string>;
  b64?: Record<string, string>;
  media?: Record<string, Array<{ name: string; size: number }>>;
  /** 团队元数据键值（GET /meta；附件入库目录等设定来自这里）。 */
  metaValues?: Record<string, string>;
  /** 补丁端点返回的实际路径（缺省 = 请求路径；表格改名漂移用）。 */
  patchFile?: string;
}) {
  const files = new Map(Object.entries(init?.files ?? {}));
  const b64 = new Map(Object.entries(init?.b64 ?? {}));
  const media = new Map(Object.entries(init?.media ?? {}));
  const metaValues = init?.metaValues ?? {};
  let patchFile = init?.patchFile;
  calls = [];
  const writes: Array<{ path: string; encoding?: string }> = [];
  const deletes: string[] = [];
  const copies: Array<{ from: string; to: string }> = [];
  const reads: string[] = [];

  const ok = (body: unknown, status = 200) =>
    new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  const notFound = () => ok({ error: "文件不存在" }, 404);

  const fn = async (url: string, initReq?: RequestInit) => {
    const method = initReq?.method ?? "GET";
    const u = new URL(url);
    const body = initReq?.body ? JSON.parse(initReq.body as string) : undefined;
    calls.push({ method, url, body });
    const p = u.pathname.replace(/^\/api\/spaces\/[^/]+/, "");

    if (p === "/file") {
      const path = u.searchParams.get("path") ?? "";
      if (method === "GET") {
        reads.push(path);
        if (u.searchParams.get("encoding") === "base64") {
          const c = b64.get(path);
          return c === undefined ? notFound() : ok({ content: c, updatedAt: 5, encoding: "base64" });
        }
        const c = files.get(path);
        return c === undefined ? notFound() : ok({ content: c, updatedAt: 5 });
      }
      if (method === "PUT") {
        if (body.encoding === "base64") b64.set(body.path, body.content);
        else files.set(body.path, body.content);
        writes.push({ path: body.path, encoding: body.encoding });
        return ok({ updatedAt: 6 });
      }
      if (method === "DELETE") {
        files.delete(path);
        b64.delete(path);
        deletes.push(path);
        return ok({ deleted: true });
      }
    }
    if (p === "/rename" && method === "POST") {
      const { oldPath, newPath } = body as { oldPath: string; newPath: string };
      for (const map of [files, b64]) {
        for (const key of [...map.keys()]) {
          if (key === oldPath || key.startsWith(`${oldPath}/`)) {
            map.set(key.replace(oldPath, newPath), map.get(key)!);
            map.delete(key);
          }
        }
      }
      return ok({});
    }
    if (p === "/copy" && method === "POST") {
      const { fromPath, toPath } = body as { fromPath: string; toPath: string };
      copies.push({ from: fromPath, to: toPath });
      for (const map of [files, b64]) {
        for (const key of [...map.keys()]) {
          if (key === fromPath || key.startsWith(`${fromPath}/`)) {
            map.set(key.replace(fromPath, toPath), map.get(key)!);
          }
        }
      }
      return ok({});
    }
    if (p === "/glob" && method === "POST") {
      const { pattern, path: basePath } = body as { pattern: string; path?: string };
      const all = [...files.keys(), ...b64.keys()];
      // 目录列举形态（`dir/*`，服务端 literal_separator：`*` 不跨 `/`）与其余扩展名形态分开解析
      const paths = pattern.endsWith("/*")
        ? all.filter((k) => {
            const dir = pattern.slice(0, -2);
            return k.startsWith(`${dir}/`) && !k.slice(dir.length + 1).includes("/");
          })
        : all.filter((k) => {
            const ext = pattern.slice(pattern.lastIndexOf(".") + 1);
            return k.endsWith(`.${ext}`);
          });
      return ok({ root: basePath ?? "", paths, total: paths.length, capped: false });
    }
    if (p === "/grep" && method === "POST") {
      const { pattern, include } = body as { pattern: string; include?: string };
      const re = new RegExp(pattern);
      const matches: Array<{ path: string; lineNumber: number; line: string }> = [];
      for (const [key, content] of files) {
        if (include && !key.endsWith(include.replace("*", ""))) continue;
        for (const [i, line] of content.split("\n").entries()) {
          if (re.test(line)) matches.push({ path: key, lineNumber: i + 1, line });
        }
      }
      return ok({ matches, total: matches.length, capped: false });
    }
    if (p === "/media/list" && method === "GET") {
      const path = u.searchParams.get("path") ?? "";
      const entries = media.get(path);
      return entries === undefined ? notFound() : ok({ entries });
    }
    if (p === "/meta" && method === "GET") {
      return ok({ values: metaValues });
    }
    if (p.endsWith("/patches/canvas") || p.endsWith("/patches/table")) {
      return ok({ updatedAt: 7, file: patchFile ?? (body as { path: string }).path });
    }
    return ok({});
  };
  vi.stubGlobal("fetch", fn);
  return {
    files,
    b64,
    media,
    writes,
    deletes,
    copies,
    reads,
    setPatchFile(f: string) {
      patchFile = f;
    },
  };
}

function canvasJson(nodes: Array<Record<string, unknown>>, extra: Record<string, unknown> = {}): string {
  return JSON.stringify({ schema: "atelyx-canvas/v1", id: "cv", title: "画布", nodes, edges: [], createdAt: 1, updatedAt: 1, ...extra });
}

describe("画布/表格读与列举", () => {
  it("readCanvas：JSON 解析 + updatedAt 取 readFile 响应（乐观锁基准）", async () => {
    setupSpaceServer({
      files: { "目录/画布.atlx": JSON.stringify({ id: "c1", title: "画布", updatedAt: 99, nodes: [] }) },
    });
    const c = await backend().readCanvas("目录/画布.atlx");
    expect(c.id).toBe("c1");
    expect(c.title).toBe("画布");
    expect(c.nodes).toEqual([]);
    expect(c.updatedAt).toBe(5);
  });

  it("readCanvas 损坏 JSON 如实抛错（错误含路径）", async () => {
    setupSpaceServer({ files: { "c.atlx": "{ 坏" } });
    await expect(backend().readCanvas("c.atlx")).rejects.toThrow("画布文件损坏，无法解析：c.atlx");
  });

  it("readTable：行归一化（图片单元格 string[] → ImageCellValue）", async () => {
    setupSpaceServer({
      files: {
        "t.atb": JSON.stringify({
          id: "t1",
          rows: [{ id: "r1", values: { f1: ["a.png"] } }],
        }),
      },
    });
    const t = await backend().readTable("t.atb");
    expect(t.rows[0].values.f1).toEqual({ images: ["a.png"] });
    expect(t.updatedAt).toBe(5);
  });

  it("listCanvases：glob → 逐个读 → 组 CanvasFileRow（updatedAt 倒序、损坏跳过）", async () => {
    setupSpaceServer({
      files: {
        "a.atlx": JSON.stringify({ id: "a", title: "A", updatedAt: 10 }),
        "d/b.atlx": JSON.stringify({ id: "b", title: "B", updatedAt: 99 }),
        "坏.atlx": "{ 坏",
        "n.md": "不是画布",
      },
    });
    const rows = await backend().listCanvases();
    expect(rows).toEqual([
      { id: "b", title: "B", file: "d/b.atlx", updatedAt: 99 },
      { id: "a", title: "A", file: "a.atlx", updatedAt: 10 },
    ]);
  });
});

describe("createCanvas / createTable", () => {
  it("createCanvas：最小磁盘 JSON 直写（服务端写自动建父目录，无需先建夹）", async () => {
    const server = setupSpaceServer({});
    const r = await backend().createCanvas("新画布", "目录");
    expect(r.file).toBe("目录/新画布.atlx");
    expect(r.id).not.toBe("");
    const write = server.writes.find((w) => w.path === "目录/新画布.atlx");
    expect(write).toBeDefined();
    const parsed = JSON.parse(server.files.get("目录/新画布.atlx")!);
    expect(parsed).toMatchObject({
      schema: "atelyx-canvas/v1",
      id: r.id,
      title: "新画布",
      nodes: [],
      edges: [],
    });
    // 无 createFolder 调用（服务端 PUT 自动建父目录）
    expect(calls.some((c) => c.url.endsWith("/folder"))).toBe(false);
  });

  it("createCanvas 重名拒绝（与本地「画布名冲突」同策略）", async () => {
    setupSpaceServer({ files: { "已存在.atlx": "{}" } });
    await expect(backend().createCanvas("已存在", "")).rejects.toThrow("画布名冲突：已存在");
  });

  it("createTable：.atb 最小磁盘 JSON + 重名拒绝", async () => {
    const server = setupSpaceServer({ files: { "表格.atb": "{}" } });
    await expect(backend().createTable("表格", "")).rejects.toThrow("表格名冲突：表格");
    const r = await backend().createTable("新表", "");
    const parsed = JSON.parse(server.files.get("新表.atb")!);
    expect(parsed).toMatchObject({ schema: "atelyx-table/v1", id: r.id, fields: [], rows: [] });
  });

  it("deleteCanvas / deleteTable 走文件删除端点（不更新画布引用）", async () => {
    const server = setupSpaceServer({});
    await backend().deleteCanvas("c.atlx");
    await backend().deleteTable("t.atb");
    expect(server.deletes).toEqual(["c.atlx", "t.atb"]);
  });
});

describe("附件：base64 读写与入库", () => {
  it("readAttachmentDataUrl：base64 读 + mime 推断拼 dataURL（未知扩展回落 octet-stream）", async () => {
    const raw = "PNG-bytes-01";
    setupSpaceServer({ b64: { "图.png": btoa(raw), "file.bin": btoa("x") } });
    const b = backend();
    const url = await b.readAttachmentDataUrl("图.png");
    expect(url.startsWith("data:image/png;base64,")).toBe(true);
    // base64 往返：payload 解回与源一致
    expect(atob(url.slice(url.indexOf(",") + 1))).toBe(raw);
    expect(await b.readAttachmentDataUrl("file.bin")).toBe(`data:application/octet-stream;base64,${btoa("x")}`);
  });

  it("writeTempAttachment：唯一叶子名 + base64 写入临时目录，返回仓库相对路径", async () => {
    const server = setupSpaceServer({});
    const rel = await backend().writeTempAttachment("c1", "my photo.png", btoa("data"));
    expect(rel.startsWith(".space-media/temp/c1/att-")).toBe(true);
    expect(rel.endsWith("-my photo.png")).toBe(true);
    expect(server.b64.get(rel)).toBe(btoa("data"));
    const write = calls.find((c) => c.method === "PUT");
    expect((write?.body as { encoding: string }).encoding).toBe("base64");
  });

  it("importAttachment：复制进 attachments 固定目录（重名追加 ` (n)`），临时源保留", async () => {
    const server = setupSpaceServer({
      b64: {
        ".space-media/temp/c1/att-x-图.png": btoa("img"),
        // 已有同名附件经 glob 枚举（重名集合来自内容面，非 media/list——后者只允许 .space-media/ 内路径）
        "attachments/图.png": btoa("old"),
      },
    });
    const r = await backend().importAttachment(".space-media/temp/c1/att-x-图.png", "图.png");
    // 已有 图.png → 追加序号，不覆盖
    expect(r.file).toBe("attachments/图 (1).png");
    expect(server.b64.get("attachments/图 (1).png")).toBe(btoa("img"));
    // 复制而非移动：临时源可能被多个节点引用，残留由按引用清理回收
    expect(server.b64.has(".space-media/temp/c1/att-x-图.png")).toBe(true);
    // 重名枚举不得走 media/list（服务端对仓库可见目录拒绝）
    expect(calls.some((c) => /\/media\/list\?path=attachments/.test(c.url))).toBe(false);
  });

  it("importAttachment：无重名直落原基础名", async () => {
    const server = setupSpaceServer({
      b64: { ".space-media/temp/c1/att-x-图.png": btoa("img") },
    });
    const r = await backend().importAttachment(".space-media/temp/c1/att-x-图.png", "图.png");
    expect(r.file).toBe("attachments/图.png");
    expect(server.b64.get("attachments/图.png")).toBe(btoa("img"));
  });

  it("importAttachment：按团队元数据「附件文件夹」设定落位（重名仍追加序号，不覆盖）", async () => {
    const server = setupSpaceServer({
      metaValues: { "attachment-folder": JSON.stringify("素材/图片") },
      b64: {
        ".space-media/temp/c1/att-x-图.png": btoa("img"),
        "素材/图片/图.png": btoa("old"),
      },
    });
    const r = await backend().importAttachment(".space-media/temp/c1/att-x-图.png", "图.png");
    expect(r.file).toBe("素材/图片/图 (1).png");
    expect(server.b64.get("素材/图片/图 (1).png")).toBe(btoa("img"));
    expect(server.b64.get("素材/图片/图.png")).toBe(btoa("old"));
  });

  it("importAttachment：附件文件夹设定非法（隐藏目录/通配符/绝对路径/越界）即拒绝，不落盘", async () => {
    for (const bad of [".space-media/x", "./.space-media", ".hidden", "素材*", "C:/素材", "../外"]) {
      const server = setupSpaceServer({
        metaValues: { "attachment-folder": JSON.stringify(bad) },
        b64: { ".space-media/temp/c1/att-x-图.png": btoa("img") },
      });
      await expect(
        backend().importAttachment(".space-media/temp/c1/att-x-图.png", "图.png"),
      ).rejects.toThrow("附件文件夹设定无效");
      expect(server.writes.some((w) => w.path.endsWith("图.png"))).toBe(false);
    }
  });

  it("importTableImage：base64 源唯一命名写入表格媒体目录；非图片源拒绝", async () => {
    const server = setupSpaceServer({});
    const rel = await backend().importTableImage(
      { fileName: "photo.jpeg", base64Data: btoa("img") },
      "t1",
    );
    expect(rel).toMatch(/^\.space-media\/tables\/t1\/img-[0-9a-f-]+\.jpeg$/);
    expect(server.b64.get(rel)).toBe(btoa("img"));
    await expect(
      backend().importTableImage({ fileName: "doc.pdf", base64Data: btoa("x") }, "t1"),
    ).rejects.toThrow("非图片文件");
  });
});

describe("附件清理", () => {
  it("cleanupCanvasTempAttachments：只删未引用文件，被引用（含跨画布引用）保留，返回删除数", async () => {
    setupSpaceServer({
      files: {
        // 本画布引用 keep.png；另一画布（节点复制粘贴）引用 keep.png；gone.png 无人引用
        "a.atlx": canvasJson([
          { id: "n1", type: "media", x: 0, y: 0, data: { file: ".space-media/temp/c1/keep.png", mime: "image/png", kind: "image" } },
        ]),
        "b.atlx": canvasJson([
          { id: "n2", type: "media", x: 0, y: 0, data: { file: ".space-media/temp/c1/keep.png", mime: "image/png", kind: "image" } },
        ]),
      },
      media: { ".space-media/temp/c1": [{ name: "keep.png", size: 3 }, { name: "gone.png", size: 3 }] },
    });
    const n = await backend().cleanupCanvasTempAttachments("c1", "a.atlx");
    expect(n).toBe(1);
    expect(deletesTarget()).toEqual([".space-media/temp/c1/gone.png"]);
  });

  it("cleanupCanvasTempAttachments：画布损坏（引用集合未知）保守放弃，零删除", async () => {
    const server = setupSpaceServer({
      files: { "坏.atlx": "{ 坏" },
      media: { ".space-media/temp/c1": [{ name: "gone.png", size: 3 }] },
    });
    expect(await backend().cleanupCanvasTempAttachments("c1", "坏.atlx")).toBe(0);
    expect(server.deletes).toEqual([]);
  });

  it("cleanupTableAttachments：只删未被 image 单元格引用的孤儿，返回删除数；损坏表格返回 0", async () => {
    const server = setupSpaceServer({
      files: {
        "t.atb": JSON.stringify({
          schema: "atelyx-table/v1",
          id: "t1",
          fields: [{ id: "f1", name: "图", type: "image" }],
          rows: [
            { id: "r1", values: { f1: { images: [".space-media/tables/t1/keep.png", "data:image/png;base64,xxx"] } } },
          ],
        }),
        "坏.atb": "{ 坏",
      },
      media: { ".space-media/tables/t1": [{ name: "keep.png", size: 3 }, { name: "orphan.png", size: 3 }] },
    });
    const b = backend();
    expect(await b.cleanupTableAttachments("t.atb")).toBe(1);
    expect(server.deletes).toEqual([".space-media/tables/t1/orphan.png"]);
    // 损坏表格：引用集合未知 → 0 不清理
    expect(await b.cleanupTableAttachments("坏.atb")).toBe(0);
    expect(server.deletes).toEqual([".space-media/tables/t1/orphan.png"]);
  });
});

describe("复制与 id 重生成", () => {
  it("copyFile：.atlx 副本重新生成 id（原件不变）", async () => {
    const server = setupSpaceServer({
      files: { "a.atlx": canvasJson([], { id: "origin-id" }) },
    });
    await backend().copyFile("a.atlx", "b.atlx");
    expect(server.copies).toEqual([{ from: "a.atlx", to: "b.atlx" }]);
    const copy = JSON.parse(server.files.get("b.atlx")!);
    expect(copy.id).not.toBe("origin-id");
    expect(JSON.parse(server.files.get("a.atlx")!).id).toBe("origin-id");
  });

  it("copyFile：非画布/表格文件纯字节复制（不读改写）", async () => {
    const server = setupSpaceServer({ files: { "a.md": "正文", "b.md": "旧" } });
    await backend().copyFile("a.md", "b.md");
    expect(server.copies).toEqual([{ from: "a.md", to: "b.md" }]);
    expect(server.files.get("b.md")).toBe("正文");
    expect(server.writes).toEqual([]);
  });

  it("copyFile：副本 id 重生成读失败（源不存在）→ 删除副本并抛明确错误，不残留旧 id 文件", async () => {
    const server = setupSpaceServer({});
    await expect(backend().copyFile("缺失.atlx", "b.atlx")).rejects.toThrow(
      /复制文件 id 重生成失败，已删除副本：b\.atlx/,
    );
    expect(server.deletes).toEqual(["b.atlx"]);
    expect(server.files.has("b.atlx")).toBe(false);
    expect(server.b64.has("b.atlx")).toBe(false);
  });

  it("copyFile：副本 id 重生成解析失败（损坏内容）→ 删除副本并抛错", async () => {
    const server = setupSpaceServer({ files: { "坏.atlx": "{ 坏" } });
    await expect(backend().copyFile("坏.atlx", "b.atlx")).rejects.toThrow(
      /复制文件 id 重生成失败，已删除副本：b\.atlx/,
    );
    expect(server.deletes).toEqual(["b.atlx"]);
    expect(server.files.has("b.atlx")).toBe(false);
  });

  it("copyFolder：服务端递归复制 + 新目录内 .atlx/.atb 逐个重生成 id", async () => {
    const server = setupSpaceServer({
      files: {
        "src/x.atlx": canvasJson([], { id: "id-x" }),
        "src/sub/y.atb": JSON.stringify({ schema: "atelyx-table/v1", id: "id-y" }),
        "src/n.md": "# 留",
        "dst 之外.atlx": canvasJson([], { id: "别动我" }),
      },
    });
    await backend().copyFolder("src", "dst");
    expect(server.copies).toEqual([{ from: "src", to: "dst" }]);
    expect(JSON.parse(server.files.get("dst/x.atlx")!).id).not.toBe("id-x");
    expect(JSON.parse(server.files.get("dst/sub/y.atb")!).id).not.toBe("id-y");
    expect(server.files.get("dst/n.md")).toBe("# 留");
    // 目录外文件不受影响
    expect(JSON.parse(server.files.get("dst 之外.atlx")!).id).toBe("别动我");
  });
});

describe("rebuildLinks", () => {
  it("[[名]]/[[名|别名]] 转 markdown、空路径补全；已规范链接/代码块/图片/外部链接不动", async () => {
    const server = setupSpaceServer({
      files: {
        "note-a.md": "# A",
        "notes/note-b.md": "正文",
        "d.md": [
          "见 [[note-a]] 与 [[note-b|别名]] 与 [[missing]]",
          "已规范 [x](notes/note-b.md) 不动",
          "```",
          "[[note-a]] 在代码块内",
          "```",
          "行内 `[[note-a]]` 不动",
          "![图](note-a.md) 不动",
          "[外](https://example.com) 不动",
          "[空]() 不动",
        ].join("\n"),
      },
    });
    const r = await backend().rebuildLinks();
    expect(r).toEqual({ scanned: 3, modified: 1, links: 3 });
    expect(server.files.get("d.md")).toBe(
      [
        "见 [note-a](note-a.md) 与 [别名](notes/note-b.md) 与 [missing]()",
        "已规范 [x](notes/note-b.md) 不动",
        "```",
        "[[note-a]] 在代码块内",
        "```",
        "行内 `[[note-a]]` 不动",
        "![图](note-a.md) 不动",
        "[外](https://example.com) 不动",
        "[空]() 不动",
      ].join("\n"),
    );
    // 无链接文件零写
    expect(server.writes.map((w) => w.path)).toEqual(["d.md"]);
  });

  it("frontmatter 内与含 .. 的路径不动", async () => {
    const server = setupSpaceServer({
      files: {
        "note-a.md": "# A",
        "d.md": ["---", "tags:", "  - \"[[note-a]]\"", "---", "正文 [x](../外部/note-a.md) 不动"].join("\n"),
      },
    });
    const r = await backend().rebuildLinks();
    expect(r.modified).toBe(0);
    expect(r.links).toBe(0);
    expect(server.writes).toEqual([]);
  });
});

describe("表格改名/附件改名的画布引用同步", () => {
  it("patchTable 改名漂移：返回 file ≠ 请求路径时改写画布 table 节点 file；无关画布零读写", async () => {
    const server = setupSpaceServer({
      files: {
        "目录/旧名.atb": JSON.stringify({ schema: "atelyx-table/v1", id: "t1" }),
        "画布.atlx": canvasJson([
          { id: "n1", type: "table", x: 0, y: 0, data: { title: "t", file: "目录/旧名.atb" } },
        ]),
        "无关.atlx": canvasJson([
          { id: "n2", type: "table", x: 0, y: 0, data: { title: "o", file: "别的.atb" } },
        ]),
      },
      patchFile: "目录/新名.atb",
    });
    const r = await backend().patchTable({ id: "t1" } as never, "目录/旧名.atb", 3, false);
    expect(r).toEqual({ updatedAt: 7, file: "目录/新名.atb" });
    const canvas = JSON.parse(server.files.get("画布.atlx")!);
    expect(canvas.nodes[0].data.file).toBe("目录/新名.atb");
    // 无关画布零读写（grep 只命中引用旧路径的画布）
    expect(server.reads).toContain("画布.atlx");
    expect(server.reads).not.toContain("无关.atlx");
    expect(server.writes.map((w) => w.path)).toEqual(["画布.atlx"]);
  });

  it("renameTable：同目录改文件名（净化标题）+ 画布引用同步", async () => {
    const server = setupSpaceServer({
      files: {
        "t.atb": JSON.stringify({ schema: "atelyx-table/v1", id: "t1" }),
        "画布.atlx": canvasJson([
          { id: "n1", type: "table", x: 0, y: 0, data: { title: "t", file: "t.atb" } },
        ]),
      },
    });
    await backend().renameTable("t.atb", "新表/名");
    // 标题净化（/ 为非法字符 → _），重命名 + 引用改写
    expect(server.files.has("新表_名.atb")).toBe(true);
    const canvas = JSON.parse(server.files.get("画布.atlx")!);
    expect(canvas.nodes[0].data.file).toBe("新表_名.atb");
  });

  it("renameAttachment：改名 + 画布 media 节点 file 引用同步", async () => {
    const server = setupSpaceServer({
      files: {
        "画布.atlx": canvasJson([
          { id: "n1", type: "media", x: 0, y: 0, data: { file: "attachments/旧.png", mime: "image/png", kind: "image" } },
        ]),
      },
    });
    await backend().renameAttachment("attachments/旧.png", "attachments/新.png");
    const canvas = JSON.parse(server.files.get("画布.atlx")!);
    expect(canvas.nodes[0].data.file).toBe("attachments/新.png");
  });
});

/** 最近一次 DELETE 的目标路径清单（清理/删除断言用）。 */
function deletesTarget(): string[] {
  return calls
    .filter((c) => c.method === "DELETE" && /\/file\?/.test(c.url))
    .map((c) => new URL(c.url).searchParams.get("path") ?? "");
}

describe("工厂登记与实例复用", () => {
  it("同身份重复激活复用同实例；不同身份不同实例", async () => {
    const a = createSpaceBackendRegistration("http://s", "SP");
    const b = createSpaceBackendRegistration("http://s", "SP");
    expect(a.activate()).toBe(b.activate());
    const c = createSpaceBackendRegistration("http://s", "OTHER");
    expect(c.activate()).not.toBe(a.activate());
  });

  it("登记对象携带空间身份", async () => {
    const reg = createSpaceBackendRegistration(SERVER, SPACE);
    expect(reg.identity).toEqual({ kind: "space", serverUrl: SERVER, spaceId: SPACE });
  });
});

describe("global 空间辅助", () => {
  const entry = { serverUrl: "http://s", spaceId: "SP", name: "空间" };
  it("bumpRecentSpace 置顶去重截断", () => {
    let list = bumpRecentSpace([], entry, 100);
    list = bumpRecentSpace(list, { serverUrl: "http://s2", spaceId: "X", name: "X" }, 200);
    // 重复 bump 同身份：去重置顶
    list = bumpRecentSpace(list, entry, 300);
    expect(list[0]).toEqual({ serverUrl: "http://s", spaceId: "SP", name: "空间", openedAt: 300 });
    expect(list.length).toBe(2);
    // key 带 serverUrl+spaceId
    expect(spaceKey("http://s", "SP")).toBe("http://s#SP");
  });
  it("removeRecentSpace 按 key 移除", () => {
    const list = [entry, { serverUrl: "http://s2", spaceId: "X", name: "X" }];
    const next = removeRecentSpace(list, spaceKey("http://s", "SP"));
    expect(next.length).toBe(1);
    expect(next[0].spaceId).toBe("X");
  });
});

describe("附件上传链：本机文件 base64 → 空间后端形状", () => {
  it("writeTempAttachment（service）：File 字节由前端读为 base64 上传，返回仓库相对路径引用", async () => {
    const server = setupSpaceServer({});
    createSpaceBackendRegistration(SERVER, SPACE).activate();
    const { writeTempAttachment } = await import("@/services/tempAttachment");
    // 本机文件（组件经文件选择/粘贴/拖拽拿到 File 对象，无路径语义）
    const file = new File([new Uint8Array([1, 2, 3, 250])], "截图.png");
    const ref = await writeTempAttachment("c1", file.name, file);
    expect(ref.startsWith(".space-media/temp/c1/att-")).toBe(true);
    expect(ref.endsWith("-截图.png")).toBe(true);
    // 调用形状：base64 载荷经空间 API 传输（解码后与源字节一致）
    expect(server.b64.get(ref)).toBe(btoa(String.fromCharCode(1, 2, 3, 250)));
    const write = calls.find((c) => c.method === "PUT");
    expect((write?.body as { encoding: string }).encoding).toBe("base64");
  });

  it("importTableImage（service）：本机图片 base64 直落表格媒体目录，单元格存仓库相对路径", async () => {
    const server = setupSpaceServer({});
    createSpaceBackendRegistration(SERVER, SPACE).activate();
    const { importTableImage } = await import("@/services/table");
    const file = new File([new Uint8Array([9, 8, 7])], "photo.jpg");
    const bytes = new Uint8Array(await file.arrayBuffer());
    const rel = await importTableImage({ fileName: file.name, base64Data: bytesToBase64(bytes) }, "t1");
    expect(rel).toMatch(/^\.space-media\/tables\/t1\/img-[0-9a-f-]+\.jpg$/);
    expect(server.b64.get(rel)).toBe(btoa(String.fromCharCode(9, 8, 7)));
  });
});
