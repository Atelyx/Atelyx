/**
 * 画布保存语义测试（内容面后端 × 写盘时序）。
 *
 * 覆盖三件直接影响内容安全的行为：
 * - 同一文件的两次写不得同时发出（防抖保存与 `flush()` 并发时后到者基于旧内容重算，会丢先到者的写入）；
 * - 写盘在途期间又发生编辑时，本轮**已落盘**的 title 改名落点必须照常同步（不同步会让下一轮写
 *   已被改名删除的旧路径 → 404 回退全量写，凭空多出一个画布文件）；
 * - 文件被删除时回退全量写；纯测量变更（React Flow 回填 `measured`）不产生落盘写。
 *
 * 后端用 stub 空间后端（内存树 + 稳定 id 合并补丁），与真实后端同语义的部分才被断言。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { MockInstance } from "vitest";
import { CANVAS_SCHEMA } from "@/constants/canvas";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

// 插件注册表会把全量领域 store 拉进模块图并在 ESM 初始化期互相取用未就绪的 store；
// 本测试只关心画布 store 自身，桩掉它斩断这条环（同 canvasStore.undoAtomic.test.ts）。
vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

type CanvasStore = typeof import("./canvasStore");
type StubFactory = typeof import("@/services/content/stubSpaceBackend");

const FILE = "画布/c1.atlx";

let canvas: CanvasStore;
let stub: ReturnType<StubFactory["createSpaceStubBackend"]>;
let errorSpy: MockInstance;

const textNode = (id: string, title: string) => ({
  id,
  type: "text",
  x: 0,
  y: 0,
  data: { title, bodyMd: "" },
});

function canvasJson(nodes: unknown[]): string {
  return JSON.stringify({
    schema: CANVAS_SCHEMA,
    id: "c1",
    title: "c1",
    nodes,
    edges: [],
    createdAt: 1,
    updatedAt: 1,
  });
}

/** 打开预置画布。 */
async function openCanvas(): Promise<void> {
  await canvas.useCanvasStore.getState().load(FILE);
  expect(canvas.useCanvasStore.getState().canvasId).toBe("c1");
}

const canvasWrites = () => stub.entityWrites.filter((w) => w.kind === "canvas");

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  // 空间历史追加走 HTTP 端点（stub 身份不可达），失败被历史层自行吞掉并记日志——静音即可
  errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  // 先起 collabStore 再取画布 store：画布 store 的协作接线在模块加载期读 collabStore 服务面
  await import("./collabStore");
  canvas = await import("./canvasStore");
  const stubMod = await import("@/services/content/stubSpaceBackend");
  const factory = await import("@/services/content/factory");
  stub = stubMod.createSpaceStubBackend();
  stub.seed(FILE, canvasJson([textNode("n1", "节点一")]));
  factory.activateContentVault(stub.identity, stub.backend);
});

afterEach(() => {
  errorSpy.mockRestore();
  vi.useRealTimers();
});

describe("画布写盘时序", () => {
  it("防抖保存与 flush 并发：第二次写排在第一次之后发出", async () => {
    await openCanvas();
    stub.writeDelayMs = 50;

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(500); // 防抖到点：第一次写已发出（停在后端注入延迟）
    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改二" });
    const flushed = canvas.useCanvasStore.getState().flush();
    await vi.advanceTimersByTimeAsync(0);
    // 第一次写仍在途：第二次写不得已经发出
    expect(canvasWrites()).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(200);
    await flushed;
    expect(canvasWrites().length).toBeGreaterThanOrEqual(2);
    expect(canvas.useCanvasStore.getState().error).toBeNull();
  });

  it("写盘在途又编辑：title 改名落点照常同步，下一轮写新路径", async () => {
    await openCanvas();
    stub.writeDelayMs = 50;
    // 标题随下一次补丁携带（后端按标题改文件名，返回新相对路径）
    canvas.useCanvasStore.setState({ canvasTitle: "新名" });

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(500);
    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改二" }); // 写盘期间再编辑
    await vi.advanceTimersByTimeAsync(200);

    const newFile = "画布/新名.atlx";
    expect(canvas.useCanvasStore.getState().canvasFile).toBe(newFile);
    expect(stub.files.has(FILE)).toBe(false); // 旧路径已被改名带走

    // 下一轮保存（写盘期间那次编辑）落到新路径
    await vi.advanceTimersByTimeAsync(700);
    expect(canvasWrites()[canvasWrites().length - 1].file).toBe(newFile);
  });

  it("磁盘文件被删除：回退全量写重建", async () => {
    await openCanvas();
    stub.files.delete(FILE); // 外部删除（补丁端点按缺失拒绝）

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(700);

    expect(stub.files.has(FILE)).toBe(true);
    const disk = JSON.parse(stub.files.get(FILE) as string) as { nodes: { id: string }[] };
    expect(disk.nodes.map((n) => n.id)).toEqual(["n1"]);
    expect(canvas.useCanvasStore.getState().error).toBeNull();
  });
});

describe("画布落盘触发条件", () => {
  it("纯测量变更不落盘；真实变更仍落盘", async () => {
    await openCanvas();

    // React Flow 挂载/内容高度变化后回填 measured：无 resizing、无 setAttributes
    canvas.useCanvasStore.getState().onNodesChange([
      { id: "n1", type: "dimensions", dimensions: { width: 200, height: 120 } },
    ]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(stub.entityWrites).toHaveLength(0);
    expect(canvas.useCanvasStore.getState().dirty).toBe(false);

    canvas.useCanvasStore.getState().onNodesChange([
      { id: "n1", type: "position", position: { x: 12, y: 34 } },
    ]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(canvasWrites()).toHaveLength(1);
  });

  it("resize 结束（resizing: false）仍落盘", async () => {
    await openCanvas();

    canvas.useCanvasStore.getState().onNodesChange([
      { id: "n1", type: "dimensions", dimensions: { width: 300, height: 160 }, resizing: false },
    ]);
    await vi.advanceTimersByTimeAsync(1000);
    expect(canvasWrites()).toHaveLength(1);
  });
});
