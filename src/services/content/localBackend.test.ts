/**
 * localBackend 契约符合性测试：每个契约方法必须映射到既定 Rust 命令、
 * 参数原样透传、返回值原样透传、命令失败如实抛出（内容 I/O 失败不得静默）。
 * readTable 的行归一化（磁盘旧形态 → 内存形态）属于契约返回形状，一并锁定。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> }[],
  /** 命令 → 返回值；未登记的命令返回 undefined。 */
  results: new Map<string, unknown>(),
  /** 命令 → 抛出错误；登记即拒。 */
  errors: new Map<string, Error>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    h.calls.push({ cmd, args: args ?? {} });
    const err = h.errors.get(cmd);
    if (err) throw err;
    return h.results.get(cmd);
  },
}));

import { READ_WINDOW_DEFAULT_LINES } from "@/constants/tools";
import { CANVAS_SCHEMA } from "@/constants/canvas";
import { TABLE_SCHEMA } from "@/constants/table";
import { localBackend } from "./local";
import type { ContentBackend } from "./contract";

/** 契约方法 → 命令与参数的逐项映射（方法面与 Rust 命令一一对应）。 */
const MAPPING: Array<{
  name: string;
  call: (b: ContentBackend) => Promise<unknown>;
  cmd: string;
  args: Record<string, unknown>;
  returns?: unknown;
}> = [
  {
    name: "listTree",
    call: (b) => b.listTree(),
    cmd: "list_vault_tree",
    args: {},
    returns: [],
  },
  {
    name: "listDir",
    call: (b) => b.listDir("笔记"),
    cmd: "list_vault_dir",
    args: { dir: "笔记" },
    returns: { entries: [], total: 0, capped: false },
  },
  {
    name: "listDir（缺省 = 仓库根）",
    call: (b) => b.listDir(),
    cmd: "list_vault_dir",
    args: { dir: "" },
  },
  { name: "readFile", call: (b) => b.readFile("a.txt"), cmd: "read_vault_file", args: { file: "a.txt" }, returns: "x" },
  {
    name: "readFileWindow（offset/limit 缺省）",
    call: (b) => b.readFileWindow("a.txt"),
    cmd: "read_vault_file_window",
    args: { file: "a.txt", offset: 1, limit: READ_WINDOW_DEFAULT_LINES },
    returns: { lines: [], totalLines: 0, truncated: false },
  },
  {
    name: "readFileWindow（显式窗口）",
    call: (b) => b.readFileWindow("a.txt", { offset: 3, limit: 7 }),
    cmd: "read_vault_file_window",
    args: { file: "a.txt", offset: 3, limit: 7 },
  },
  { name: "readNote", call: (b) => b.readNote("a.md"), cmd: "read_note", args: { file: "a.md" }, returns: "正文" },
  { name: "fileExists", call: (b) => b.fileExists("a.md"), cmd: "file_exists", args: { file: "a.md" }, returns: true },
  {
    name: "readCanvas",
    call: (b) => b.readCanvas("c.atlx"),
    cmd: "read_canvas_vault",
    args: { file: "c.atlx" },
    returns: { id: "1", title: "c", nodes: [], edges: [], createdAt: 0, updatedAt: 0, schema: 1 },
  },
  { name: "listCanvases", call: (b) => b.listCanvases(), cmd: "list_canvases_vault", args: {}, returns: [] },
  {
    name: "readAttachmentDataUrl",
    call: (b) => b.readAttachmentDataUrl("img.png"),
    cmd: "read_attachment_data_url",
    args: { file: "img.png" },
    returns: "data:image/png;base64,",
  },
  {
    name: "writeFile",
    call: (b) => b.writeFile("a.txt", "内容"),
    cmd: "write_vault_file",
    args: { file: "a.txt", content: "内容" },
  },
  {
    name: "writeNote",
    call: (b) => b.writeNote("a.md", "正文"),
    cmd: "write_note",
    args: { file: "a.md", content: "正文" },
  },
  {
    name: "writeCanvas",
    call: (b) =>
      b.writeCanvas(
        { id: "1", title: "c", nodes: [], edges: [], createdAt: 0, updatedAt: 1, schema: CANVAS_SCHEMA },
        "c.atlx",
      ),
    cmd: "write_canvas_vault",
    args: {
      canvas: { id: "1", title: "c", nodes: [], edges: [], createdAt: 0, updatedAt: 1, schema: CANVAS_SCHEMA },
      file: "c.atlx",
    },
    returns: 9,
  },
  {
    name: "writeTable",
    call: (b) =>
      b.writeTable(
        { id: "t", title: "表", schema: TABLE_SCHEMA, fields: [], rows: [], createdAt: 0, updatedAt: 1 },
        "t.atb",
      ),
    cmd: "write_table_vault",
    args: {
      table: { id: "t", title: "表", schema: TABLE_SCHEMA, fields: [], rows: [], createdAt: 0, updatedAt: 1 },
      file: "t.atb",
    },
    returns: 9,
  },
  {
    name: "createCanvas",
    call: (b) => b.createCanvas("新画布", "目录"),
    cmd: "create_canvas_vault",
    args: { title: "新画布", dir: "目录" },
    returns: { id: "1", file: "目录/新画布.atlx" },
  },
  {
    name: "createTable",
    call: (b) => b.createTable("新表格", ""),
    cmd: "create_table_vault",
    args: { title: "新表格", dir: "" },
    returns: { id: "t", file: "新表格.atb" },
  },
  {
    name: "patchCanvas",
    call: (b) => b.patchCanvas({ id: "1", upsertNodes: [], removedNodeIds: [], upsertEdges: [], removedEdgeIds: [] }, "c.atlx"),
    cmd: "patch_canvas_vault",
    args: {
      patch: { id: "1", upsertNodes: [], removedNodeIds: [], upsertEdges: [], removedEdgeIds: [] },
      file: "c.atlx",
    },
    returns: { updatedAt: 9, file: "c.atlx" },
  },
  {
    name: "patchTable",
    call: (b) =>
      b.patchTable(
        { id: "t", upsertFields: [], removedFieldIds: [], upsertRows: [], removedRowIds: [] },
        "t.atb",
      ),
    cmd: "patch_table_vault",
    args: {
      patch: { id: "t", upsertFields: [], removedFieldIds: [], upsertRows: [], removedRowIds: [] },
      file: "t.atb",
    },
    returns: { updatedAt: 9, file: "t.atb" },
  },
  {
    name: "renameNote",
    call: (b) => b.renameNote("a.md", "b.md"),
    cmd: "rename_note",
    args: { oldFile: "a.md", newFile: "b.md" },
    returns: { rewrites: [] },
  },
  {
    name: "renameCanvas",
    call: (b) => b.renameCanvas("c.atlx", "新名"),
    cmd: "rename_canvas_vault",
    args: { file: "c.atlx", newTitle: "新名" },
  },
  { name: "moveCanvas", call: (b) => b.moveCanvas("c.atlx", "d/c.atlx"), cmd: "move_canvas_vault", args: { oldFile: "c.atlx", newFile: "d/c.atlx" } },
  {
    name: "renameTable",
    call: (b) => b.renameTable("t.atb", "新名"),
    cmd: "rename_table_vault",
    args: { file: "t.atb", newTitle: "新名" },
  },
  { name: "moveTable", call: (b) => b.moveTable("t.atb", "d/t.atb"), cmd: "move_table_vault", args: { oldFile: "t.atb", newFile: "d/t.atb" } },
  {
    name: "renameAttachment",
    call: (b) => b.renameAttachment("img.png", "new.png"),
    cmd: "rename_attachment",
    args: { oldFile: "img.png", newFile: "new.png" },
  },
  {
    name: "renameFolder",
    call: (b) => b.renameFolder("旧目录", "新目录"),
    cmd: "rename_folder",
    args: { oldDir: "旧目录", newDir: "新目录" },
    returns: { rewrites: [] },
  },
  { name: "deleteNote", call: (b) => b.deleteNote("a.md"), cmd: "delete_note", args: { file: "a.md" } },
  {
    name: "deleteAttachment",
    call: (b) => b.deleteAttachment("img.png"),
    cmd: "delete_attachment",
    args: { file: "img.png" },
  },
  { name: "deleteCanvas", call: (b) => b.deleteCanvas("c.atlx"), cmd: "delete_canvas_vault", args: { file: "c.atlx" } },
  { name: "deleteTable", call: (b) => b.deleteTable("t.atb"), cmd: "delete_table_vault", args: { file: "t.atb" } },
  {
    name: "deleteFolder",
    call: (b) => b.deleteFolder("目录", true),
    cmd: "delete_folder",
    args: { dir: "目录", force: true },
    returns: { deleted: true },
  },
  { name: "createFolder", call: (b) => b.createFolder("目录"), cmd: "create_folder", args: { dir: "目录" }, returns: "目录" },
  {
    name: "copyFile",
    call: (b) => b.copyFile("a.md", "b.md"),
    cmd: "copy_vault_file",
    args: { oldFile: "a.md", newFile: "b.md" },
  },
  {
    name: "copyFolder",
    call: (b) => b.copyFolder("目录", "副本"),
    cmd: "copy_vault_folder",
    args: { oldDir: "目录", newDir: "副本" },
  },
  {
    name: "rebuildLinks",
    call: (b) => b.rebuildLinks(),
    cmd: "rebuild_internal_links",
    args: {},
    returns: { changed: 0 },
  },
  {
    name: "remapSideloads",
    call: (b) => b.remapSideloads("a.md", "b.md"),
    cmd: "remap_sideloads",
    args: { oldFile: "a.md", newFile: "b.md" },
  },
  {
    name: "remapSideloadsByDir",
    call: (b) => b.remapSideloadsByDir("旧目录", "新目录"),
    cmd: "remap_sideloads_by_dir",
    args: { oldDir: "旧目录", newDir: "新目录" },
  },
  {
    name: "writeTempAttachment",
    call: (b) => b.writeTempAttachment("canvas1", "pic.png", "aGVsbG8="),
    cmd: "write_temp_attachment",
    args: { canvasId: "canvas1", fileName: "pic.png", base64Data: "aGVsbG8=" },
    returns: ".atelyx/temp/ab/pic.png",
  },
  {
    name: "importAttachment",
    call: (b) => b.importAttachment(".atelyx/temp/ab/pic.png", "pic.png"),
    cmd: "import_vault_attachment",
    args: { rel: ".atelyx/temp/ab/pic.png", fileName: "pic.png" },
    returns: { file: "pic.png" },
  },
  {
    name: "importTableImage",
    call: (b) => b.importTableImage({ fileName: "pic.png", base64Data: "aGVsbG8=" }, "table1"),
    cmd: "import_table_image_vault",
    args: { fileName: "pic.png", data: "aGVsbG8=", tableId: "table1" },
    returns: ".atelyx/attachments/table1/pic.png",
  },
  {
    name: "cleanupCanvasTempAttachments",
    call: (b) => b.cleanupCanvasTempAttachments("canvas1", "c.atlx"),
    cmd: "cleanup_canvas_temp_attachments",
    args: { canvasId: "canvas1", canvasFile: "c.atlx" },
    returns: 3,
  },
  {
    name: "cleanupTableAttachments",
    call: (b) => b.cleanupTableAttachments("t.atb"),
    cmd: "cleanup_table_attachments_vault",
    args: { file: "t.atb" },
    returns: 2,
  },
  {
    name: "scanBacklinks",
    call: (b) => b.scanBacklinks("笔记名", "a.md"),
    cmd: "scan_wiki_backlinks",
    args: { noteName: "笔记名", noteFile: "a.md" },
    returns: [],
  },
  { name: "scanTags", call: (b) => b.scanTags(), cmd: "scan_vault_tags", args: {}, returns: [] },
  {
    name: "glob",
    call: (b) => b.glob("**/*.md", { path: "笔记" }),
    cmd: "glob_vault",
    args: { pattern: "**/*.md", path: "笔记" },
    returns: { root: "笔记", paths: [], total: 0, capped: false },
  },
  {
    name: "grep",
    call: (b) => b.grep("关键词", { include: "*.md" }),
    cmd: "grep_vault",
    args: { pattern: "关键词", path: undefined, include: "*.md" },
    returns: { matches: [], total: 0, capped: false },
  },
  {
    name: "repoHistoryAggregate",
    call: (b) => b.repoHistoryAggregate(),
    cmd: "list_repo_history",
    args: {},
    returns: { entries: [], dailyCounts: [] },
  },
  {
    name: "listDatedNotes",
    call: (b) => b.listDatedNotes(),
    cmd: "list_dated_notes",
    args: {},
    returns: [],
  },
];

describe("localBackend 契约符合性", () => {
  beforeEach(() => {
    h.calls.length = 0;
    h.results.clear();
    h.errors.clear();
  });

  for (const row of MAPPING) {
    it(`${row.name} → ${row.cmd}`, async () => {
      if (row.returns !== undefined) h.results.set(row.cmd, row.returns);
      const out = await row.call(localBackend);
      expect(h.calls).toEqual([{ cmd: row.cmd, args: row.args }]);
      if (row.returns !== undefined) expect(out).toEqual(row.returns);
    });
  }

  it("readTable 归一化行到内存形态（契约返回形状）", async () => {
    h.results.set("read_table_vault", {
      id: "t",
      title: "表",
      fields: [],
      rows: [{ id: "r1", values: { pics: ["a.png"] } }],
      createdAt: 0,
      updatedAt: 1,
    });
    const table = await localBackend.readTable("t.atb");
    expect(table.rows[0]!.values.pics).toEqual({ images: ["a.png"] });
  });

  it("命令失败如实抛出（内容 I/O 失败不得静默）", async () => {
    h.errors.set("write_note", new Error("磁盘已满"));
    await expect(localBackend.writeNote("a.md", "正文")).rejects.toThrow("磁盘已满");
  });
});

describe("工厂激活与后端替换", () => {
  // 每用例重置模块：注册表（backends Map）与激活态随之清空，用例间零耦合
  let factoryMod: typeof import("./factory");
  let backendMod: typeof import("./local");

  beforeEach(async () => {
    vi.resetModules();
    h.calls.length = 0;
    h.results.clear();
    h.errors.clear();
    factoryMod = await import("./factory");
    backendMod = await import("./local");
  });

  it("激活 stub 后端后，读经 stub 内存树而非 Rust 命令", async () => {
    const { createSpaceStubBackend } = await import("./stubSpaceBackend");
    const stub = createSpaceStubBackend();
    await stub.write("a.md", "空间正文");
    factoryMod.activateContentVault(stub.identity, stub.backend);
    expect(factoryMod.getActiveContentBackend()).toBe(stub.backend);
    expect(await factoryMod.getActiveContentBackend().readNote("a.md")).toBe("空间正文");
    expect(h.calls).toEqual([]);
  });

  it("未激活任何身份时回落 localBackend（root 无关语义不变）", async () => {
    h.results.set("read_note", "本地正文");
    expect(factoryMod.getActiveContentBackend()).toBe(backendMod.localBackend);
    expect(await factoryMod.getActiveContentBackend().readNote("a.md")).toBe("本地正文");
  });

  it("同 root 复用已登记后端（身份按 root 路径区分）", async () => {
    const { createSpaceStubBackend } = await import("./stubSpaceBackend");
    factoryMod.activateContentVault({ kind: "local", root: "E:/v1" }, createSpaceStubBackend().backend);
    // 同 root 再激活不带后端：不覆盖已登记
    factoryMod.activateContentVault({ kind: "local", root: "E:/v1" });
    expect(factoryMod.getActiveContentBackend()).not.toBe(backendMod.localBackend);
    // 不同 root：自动登记共享 localBackend
    factoryMod.activateContentVault({ kind: "local", root: "E:/v2" });
    expect(factoryMod.getActiveContentBackend()).toBe(backendMod.localBackend);
  });

  it("空间身份缺少显式后端：激活即抛错，不静默回落 localBackend", async () => {
    expect(() =>
      factoryMod.activateContentVault({ kind: "space", serverUrl: "s", spaceId: "1" }),
    ).toThrow("缺少内容后端");
  });
});
