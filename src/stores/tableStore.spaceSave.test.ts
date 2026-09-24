/**
 * 表格保存语义测试（内容面后端 × 写盘时序）。
 *
 * 覆盖：
 * - 同一文件的两次写不得同时发出（防抖保存与 `flush()` 并发时后到者基于旧内容重算，会丢先到者的写入）；
 * - 文件被删除时回退全量写重建；
 * - 协作补丁回放（服务端落地后广播含发起者自己）应用后，补丁覆盖的实体随补丁推进落盘基线：
 *   回放落在写盘在途窗口内（服务端先广播后返回响应）也不重发回放实体——否则每次保存全量重传成环；
 * - 远端已落地实体在写盘在途到达时不被客户端重发，磁盘收敛到两端内容；
 * - 行序补丁推进行序基线，重排不被当作本地增量重发。
 *
 * 后端用 stub 空间后端（内存树 + 稳定 id 合并补丁），与真实后端同语义的部分才被断言。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { MockInstance } from "vitest";
import { TABLE_SCHEMA } from "@/constants/table";
import type { TablePatch } from "@/types";

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

describe("协作补丁回放与落盘基线", () => {
  it("延迟回放（保存完成后到达）：干净端基线对齐，下一次保存只携带真实变更的行", async () => {
    stub.seed(FILE, tableJson([{ id: "r1" }, { id: "r2" }]));
    await openTable();

    table.useTableStore.getState().updateCell("r1", "f1", "改一");
    await vi.advanceTimersByTimeAsync(500);
    // 服务端落地后把补丁帧广播回房间（含发起者自己，帧无 peerId）：
    // 客户端经远端补丁同一路径应用自己的回放，应用会翻新补丁内实体的引用。
    // 回放帧经 JSON 传输边界（HTTP 落地 → WS 广播），实体必为新对象——用 JSON 往返如实模拟
    const landed = JSON.parse(JSON.stringify(tableWrites()[0]?.patch)) as TablePatch;
    table.useTableStore.getState().applyRemotePatch(FILE, landed);

    table.useTableStore.getState().updateCell("r2", "f1", "改二");
    await vi.advanceTimersByTimeAsync(500);
    // 落盘基线须随回放应用对齐到当前内存：否则回放翻新过的 r1 被当增量重发
    const second = tableWrites()[1]?.patch as TablePatch | undefined;
    expect(second).toBeDefined();
    expect(second?.upsertRows.map((r) => r.id)).toEqual(["r2"]);
  });

  it("自收回放在写盘在途窗口应用：基线随回放推进，不重发回放实体", async () => {
    stub.seed(FILE, tableJson([{ id: "r1" }, { id: "r2" }]));
    await openTable();
    stub.writeDelayMs = 50;

    table.useTableStore.getState().updateCell("r1", "f1", "改一");
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 服务端先广播后返回响应，回放必然先于保存响应到达（落在写盘在途窗口内，此刻 dirty）。
    // 回放帧经 JSON 传输边界（HTTP 落地 → WS 广播），实体必为新对象——用 JSON 往返如实模拟
    const landed = JSON.parse(JSON.stringify(tableWrites()[0]?.patch)) as TablePatch;
    table.useTableStore.getState().applyRemotePatch(FILE, landed);

    await vi.advanceTimersByTimeAsync(200); // 第一次写落地
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮
    // 落盘基线须随回放推进到补丁覆盖的实体：否则回放翻新过引用的 r1 被当增量重发，
    // 服务端再落地再回放，成环不止
    expect(tableWrites()).toHaveLength(1);
    expect(table.useTableStore.getState().dirty).toBe(false);

    // 基线只吃补丁内实体：后续真实编辑仍正常落盘，且不捎带回放实体
    table.useTableStore.getState().updateCell("r2", "f1", "改二");
    await vi.advanceTimersByTimeAsync(500);
    const second = tableWrites()[1]?.patch as TablePatch | undefined;
    expect(second).toBeDefined();
    expect(second?.upsertRows.map((r) => r.id)).toEqual(["r2"]);
  });

  it("写盘在途收到远端补丁：远端已落地实体不重发，磁盘收敛到两端内容", async () => {
    await openTable();
    stub.writeDelayMs = 50;

    table.useTableStore.getState().updateCell("r1", "f1", "本地改");
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 远端补丁新增 r2，服务端已落地（落地后才广播）——内存树同步预置该内容模拟落地结果
    stub.seed(
      FILE,
      JSON.stringify({
        schema: TABLE_SCHEMA,
        id: "t1",
        title: "t1",
        fields: [{ id: "f1", name: "列", type: "text" }],
        rows: [
          { id: "r1", values: { f1: "x" } },
          { id: "r2", values: { f1: "远端改" } },
        ],
        createdAt: 1,
        updatedAt: 1,
      }),
    );
    table.useTableStore.getState().applyRemotePatch(FILE, {
      id: "t1",
      upsertFields: [],
      removedFieldIds: [],
      upsertRows: [{ id: "r2", values: { f1: "远端改" } }],
      removedRowIds: [],
    });
    await vi.advanceTimersByTimeAsync(200); // 第一次写落地（在预置内容上合并）
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮

    // 远端实体已由服务端落地，客户端不得把它当本地增量重发（重发 → 再落地 → 再回放成环）
    expect(
      tableWrites().some(
        (w) => (w.patch as TablePatch | undefined)?.upsertRows.some((r) => r.id === "r2"),
      ),
    ).toBe(false);
    const disk = JSON.parse(stub.files.get(FILE) as string) as {
      rows: { id: string; values: Record<string, string> }[];
    };
    expect(disk.rows.find((r) => r.id === "r1")?.values.f1).toBe("本地改");
    expect(disk.rows.find((r) => r.id === "r2")?.values.f1).toBe("远端改");
    expect(table.useTableStore.getState().rows.find((r) => r.id === "r2")?.values.f1).toBe(
      "远端改",
    );
    expect(table.useTableStore.getState().error).toBeNull();
  });

  it("写盘在途收到远端重排补丁：行序基线随补丁推进，不重发行序", async () => {
    stub.seed(FILE, tableJson([{ id: "r1" }, { id: "r2" }]));
    await openTable();
    stub.writeDelayMs = 50;

    table.useTableStore.getState().updateCell("r1", "f1", "本地改");
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 远端把行序重排为 [r2, r1]，服务端已落地——内存树预置重排后的内容
    stub.seed(FILE, tableJson([{ id: "r2" }, { id: "r1" }]));
    table.useTableStore.getState().applyRemotePatch(FILE, {
      id: "t1",
      upsertFields: [],
      removedFieldIds: [],
      upsertRows: [],
      removedRowIds: [],
      rowOrder: ["r2", "r1"],
    });
    await vi.advanceTimersByTimeAsync(200); // 第一次写落地
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮

    // 行序基线随补丁推进：任何一轮都不得把重排当本地增量重发（否则重排被反复重发成环）
    expect(
      tableWrites().every((w) => (w.patch as TablePatch | undefined)?.rowOrder === undefined),
    ).toBe(true);
    const disk = JSON.parse(stub.files.get(FILE) as string) as {
      rows: { id: string; values: Record<string, string> }[];
    };
    expect(disk.rows.map((r) => r.id)).toEqual(["r2", "r1"]);
    expect(disk.rows.find((r) => r.id === "r1")?.values.f1).toBe("本地改");
    expect(table.useTableStore.getState().error).toBeNull();

    // 后续真实编辑不重排、不捎带：只携带被编辑的行
    table.useTableStore.getState().updateCell("r2", "f1", "改二");
    await vi.advanceTimersByTimeAsync(500);
    const last = tableWrites()[tableWrites().length - 1]?.patch as TablePatch | undefined;
    expect(last).toBeDefined();
    expect(last?.upsertRows.map((r) => r.id)).toEqual(["r2"]);
    expect(last?.rowOrder).toBeUndefined();
  });

  it("自收回放含删除：基线剔除被删实体，不把删除当本地增量重发", async () => {
    stub.seed(FILE, tableJson([{ id: "r1" }, { id: "r2" }]));
    await openTable();
    stub.writeDelayMs = 50;

    table.useTableStore.getState().updateCell("r1", "f1", "改一");
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 回放删除 r2，服务端已落地（落地后才广播）——内存树预置删除后的内容
    stub.seed(FILE, tableJson([{ id: "r1" }]));
    table.useTableStore.getState().applyRemotePatch(FILE, {
      id: "t1",
      upsertFields: [],
      removedFieldIds: [],
      upsertRows: [],
      removedRowIds: ["r2"],
    });
    await vi.advanceTimersByTimeAsync(200); // 第一次写落地
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮

    // 基线须随回放剔除 r2：否则「基线有、内存无」被当本地删除重发，服务端再落地再回放成环
    expect(
      tableWrites().every(
        (w) => ((w.patch as TablePatch | undefined)?.removedRowIds ?? []).length === 0,
      ),
    ).toBe(true);
    expect(table.useTableStore.getState().rows.map((r) => r.id)).toEqual(["r1"]);
    expect(table.useTableStore.getState().error).toBeNull();
  });
});
