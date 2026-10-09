/**
 * 表格保存 → 历史记录调用链测试。
 *
 * 保存收尾必须对「真实落盘的保存」发起历史版本记录（用户可见行为：编辑产生历史版本）。
 * 记录层自身的 HTTP 失败被历史层静默吞掉，不在本测试范围。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

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

// 历史记录替换为可观察 spy：store 层只负责发起调用（file/content 参数正确性在此断言），
// 记录层内部（HTTP/侧文件）由 services/history 与服务端集成测试覆盖。
const recordHistoryVersion = vi.fn(async (..._args: unknown[]) => {});
vi.mock("@/services/history", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/services/history")>();
  return { ...actual, recordHistoryVersion };
});

import { createSpaceStubBackend } from "@/test-support/stubSpaceBackend";
import { TABLE_SCHEMA } from "@/constants/table";

const FILE = "表/t1.atb";
let stub: ReturnType<typeof createSpaceStubBackend>;

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

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  vi.spyOn(console, "error").mockImplementation(() => {});
  recordHistoryVersion.mockClear();
  await import("./collabStore");
  const tableMod = await import("./tableStore");
  void tableMod;
  const stubMod = await import("@/test-support/stubSpaceBackend");
  const factory = await import("@/services/content/factory");
  stub = stubMod.createSpaceStubBackend();
  stub.seed(FILE, tableJson([{ id: "r1" }]));
  factory.activateContentVault(stub.identity, stub.backend);
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("表格保存 → 历史记录", () => {
  it("编辑落盘后发起历史版本记录（file = 落盘路径）", async () => {
    const table = await import("./tableStore");
    await table.useTableStore.getState().load(FILE);
    table.useTableStore.getState().updateCell("r1", "f1", "改");
    await vi.advanceTimersByTimeAsync(700);

    expect(stub.entityWrites.filter((w) => w.kind === "table")).not.toHaveLength(0);
    expect(recordHistoryVersion).toHaveBeenCalled();
    const [kind, file] = recordHistoryVersion.mock.calls[0];
    expect(kind).toBe("table");
    expect(file).toBe(FILE);
  });

  it("空补丁（无变更）不落盘也不记历史", async () => {
    const table = await import("./tableStore");
    await table.useTableStore.getState().load(FILE);
    await vi.advanceTimersByTimeAsync(700);
    expect(recordHistoryVersion).not.toHaveBeenCalled();
  });

  it("保存在途收到自收回放（服务端广播含发起者）：真实落盘的保存仍记历史", async () => {
    const table = await import("./tableStore");
    await table.useTableStore.getState().load(FILE);
    stub.writeDelayMs = 50;

    table.useTableStore.getState().updateCell("r1", "f1", "改");
    await vi.advanceTimersByTimeAsync(500); // persist 开始、写盘在途
    // 服务端落地后先广播帧（含发起者回放）再返回 HTTP 响应——帧在此刻到达
    table.useTableStore.getState().applyRemotePatch(FILE, {
      id: "t1",
      upsertFields: [],
      upsertRows: [{ id: "r1", values: { f1: "改" } }],
      removedFieldIds: [],
      removedRowIds: [],
    } as never);
    await vi.advanceTimersByTimeAsync(200);
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮 timer

    expect(stub.entityWrites.filter((w) => w.kind === "table")).not.toHaveLength(0);
    expect(recordHistoryVersion).toHaveBeenCalled();
    const calls = recordHistoryVersion.mock.calls;
    const recorded = calls.some(([, file]) => file === FILE);
    expect(recorded).toBe(true);
  });
});
