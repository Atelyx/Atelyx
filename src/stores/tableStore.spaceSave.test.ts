/**
 * 表格保存语义测试（内容面后端 × 写盘时序）。
 *
 * 覆盖：
 * - 同一文件的两次写不得同时发出（防抖保存与 `flush()` 并发时后到者基于旧内容重算，会丢先到者的写入）；
 * - 文件被删除时回退全量写重建。
 *
 * 后端用 stub 空间后端（内存树 + 稳定 id 合并补丁），与真实后端同语义的部分才被断言。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { MockInstance } from "vitest";
import { TABLE_SCHEMA } from "@/constants/table";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

// 插件注册表会把全量领域 store 拉进模块图并在 ESM 初始化期互相取用未就绪的 store；
// 本测试只关心表格 store 自身，桩掉它斩断这条环（同 tableStore.vaultReset.test.ts）。
vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

type TableStore = typeof import("./tableStore");
type StubFactory = typeof import("@/services/content/stubSpaceBackend");

const FILE = "表/t1.atb";

let table: TableStore;
let stub: ReturnType<StubFactory["createSpaceStubBackend"]>;
let errorSpy: MockInstance;

function tableJson(rows: { id: string }[]): string {
  return JSON.stringify({
    schema: TABLE_SCHEMA,
    id: "t1",
    title: "t1",
    fields: [{ id: "f1", name: "列", type: "text" }],
    rows: rows.map((r) => ({ ...r, values: { f1: "x" } })),
    createdAt: 1,
    updatedAt: 1,
  });
}

/** 打开预置表格。 */
async function openTable(): Promise<void> {
  await table.useTableStore.getState().load(FILE);
  expect(table.useTableStore.getState().id).toBe("t1");
}

const tableWrites = () => stub.entityWrites.filter((w) => w.kind === "table");

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  // 空间历史追加走 HTTP 端点（stub 身份不可达），失败被历史层自行吞掉并记日志——静音即可
  errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  // 先起 collabStore 再取表格 store：表格 store 的协作接线在模块加载期读 collabStore 服务面
  await import("./collabStore");
  table = await import("./tableStore");
  const stubMod = await import("@/services/content/stubSpaceBackend");
  const factory = await import("@/services/content/factory");
  stub = stubMod.createSpaceStubBackend();
  stub.seed(FILE, tableJson([{ id: "r1" }]));
  factory.activateContentVault(stub.identity, stub.backend);
});

afterEach(() => {
  errorSpy.mockRestore();
  vi.useRealTimers();
});

describe("表格写盘时序", () => {
  it("防抖保存与 flush 并发：第二次写排在第一次之后发出", async () => {
    await openTable();
    stub.writeDelayMs = 50;

    table.useTableStore.getState().updateCell("r1", "f1", "改一");
    await vi.advanceTimersByTimeAsync(500);
    table.useTableStore.getState().updateCell("r1", "f1", "改二");
    const flushed = table.useTableStore.getState().flush();
    await vi.advanceTimersByTimeAsync(0);
    expect(tableWrites()).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(200);
    await flushed;
    expect(tableWrites().length).toBeGreaterThanOrEqual(2);
    expect(table.useTableStore.getState().error).toBeNull();
  });

  it("磁盘文件被删除：回退全量写重建", async () => {
    await openTable();
    stub.files.delete(FILE); // 外部删除（补丁端点按缺失拒绝）

    table.useTableStore.getState().updateCell("r1", "f1", "改一");
    await vi.advanceTimersByTimeAsync(700);

    expect(stub.files.has(FILE)).toBe(true);
    const disk = JSON.parse(stub.files.get(FILE) as string) as {
      rows: { id: string; values: Record<string, string> }[];
    };
    expect(disk.rows.map((r) => r.id)).toEqual(["r1"]);
    expect(disk.rows[0].values.f1).toBe("改一");
    expect(table.useTableStore.getState().error).toBeNull();
  });
});
