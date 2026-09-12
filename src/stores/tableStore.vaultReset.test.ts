/**
 * 数据边界测试：切仓库时的领域 store 清态（自注册 `onVaultLeaving`）。
 *
 * 表格运行时态按「仓库相对路径」写盘，残留的 tableFile/dirty 会让旧表的防抖保存写进新仓库同名文件
 * （跨仓库污染）。清态必须在模块加载时自注册：表格插件停用期间内核照常切仓库，挂插件生命周期会漏。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  /** 全部写盘调用（整表 + 增量补丁）：只记一种会让断言恒真——防抖主路径走 patch_table_vault。 */
  writes: [] as Array<{ cmd: string; file: string; table: unknown }>,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "write_table_vault" || cmd === "patch_table_vault") {
      h.writes.push({ cmd, file: String(args?.file), table: args?.table });
      return null;
    }
    return null;
  },
}));

// 插件注册表会把 noteSessionStore/全量领域 store 拉进模块图，并在 ESM 初始化期互相取用未就绪的 store。
// 本测试只关心表格 store 自身的数据边界，桩掉它斩断这条环。
vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

type TableStore = typeof import("./tableStore");

let table: TableStore;
let notifyVaultLeaving: typeof import("@/utils/kernelLifecycle").notifyVaultLeaving;
let hasDomainLifecycle: typeof import("@/utils/kernelLifecycle").hasDomainLifecycle;

beforeEach(async () => {
  vi.resetModules();
  h.writes = [];
  // 先起 collabStore 再取 tableStore：tableStore 的协作接线在模块加载期读 collabStore 服务面
  await import("./collabStore");
  table = await import("./tableStore");
  ({ notifyVaultLeaving, hasDomainLifecycle } = await import("@/utils/kernelLifecycle"));
});

describe("切仓库清空表格运行时态", () => {
  it("自注册 onVaultLeaving（不随插件启停撤销）", () => {
    // 注册存在性是本 store 数据边界的前提：钩子缺失时 notifyVaultLeaving 静默 no-op
    expect(hasDomainLifecycle("tableStore")).toBe(true);
  });

  it("清掉 tableFile/dirty/conflictPending 并取消未落盘的防抖保存", async () => {
    table.useTableStore.setState({
      tableFile: "旧仓库/表.atb",
      id: "t1",
      title: "表",
      fields: [{ id: "f1", name: "列", type: "text" }] as never,
      rows: [{ id: "r1", values: { f1: "x" } }] as never,
      baseUpdatedAt: 1,
      dirty: true,
      conflictPending: true,
      error: "写盘失败",
      selectedRowId: "r1",
    });

    notifyVaultLeaving();

    const s = table.useTableStore.getState();
    expect(s.tableFile).toBeNull();
    expect(s.dirty).toBe(false);
    expect(s.conflictPending).toBe(false);
    expect(s.error).toBeNull();
    expect(s.selectedRowId).toBeNull();
    expect(s.fields).toEqual([]);
    expect(s.rows).toEqual([]);
  });

  it("清态后残留的防抖 timer 不再写盘（旧仓库路径不得写进新仓库）", async () => {
    vi.useFakeTimers();
    try {
      table.useTableStore.setState({
        tableFile: "旧仓库/表.atb",
        id: "t1",
        fields: [{ id: "f1", name: "列", type: "text" }] as never,
        rows: [{ id: "r1", values: { f1: "x" } }] as never,
      });
      // 触发一次防抖保存，随即切仓库（清态须取消该 timer）
      table.useTableStore.getState().updateCell("r1", "f1", "改");
      notifyVaultLeaving();
      await vi.advanceTimersByTimeAsync(1000);

      expect(h.writes).toHaveLength(0);
    } finally {
      vi.useRealTimers();
    }
  });
});
