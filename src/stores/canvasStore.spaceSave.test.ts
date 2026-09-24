/**
 * 画布保存语义测试（内容面后端 × 写盘时序）。
 *
 * 覆盖直接影响内容安全的行为：
 * - 同一文件的两次写不得同时发出（防抖保存与 `flush()` 并发时后到者基于旧内容重算，会丢先到者的写入）；
 * - 写盘在途期间又发生编辑时，本轮**已落盘**的 title 改名落点必须照常同步（不同步会让下一轮写
 *   已被改名删除的旧路径 → 404 回退全量写，凭空多出一个画布文件）；
 * - 文件被删除时回退全量写；纯测量变更（React Flow 回填 `measured`）不产生落盘写；
 * - 协作补丁回放（服务端落地后广播含发起者自己）应用后，补丁覆盖的实体随补丁推进落盘基线：
 *   回放落在写盘在途窗口内（服务端先广播后返回响应）也不重发回放实体——否则每次保存全量重传成环；
 * - 远端已落地实体在写盘在途到达时不被客户端重发，磁盘收敛到两端内容；
 * - conversation 消息基线随补丁推进（mergeMessages 本地无独有消息时原样返回远端引用），
 *   对话节点不被反复重发。
 *
 * 后端用 stub 空间后端（内存树 + 稳定 id 合并补丁），与真实后端同语义的部分才被断言。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { MockInstance } from "vitest";
import { CANVAS_SCHEMA } from "@/constants/canvas";
import type { CanvasPatch, Message } from "@/types";

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

const msg = (id: string): Message => ({ id, role: "user", content: "内容", createdAt: 1 });

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

describe("协作补丁回放与落盘基线", () => {
  it("延迟回放（保存完成后到达）：干净端基线对齐，下一次保存只携带真实变更的节点", async () => {
    stub.seed(FILE, canvasJson([textNode("n1", "节点一"), textNode("n2", "节点二")]));
    await openCanvas();

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(500);
    // 服务端落地后把补丁帧广播回房间（含发起者自己，帧无 peerId）：
    // 客户端经远端补丁同一路径应用自己的回放，应用会翻新补丁内实体的引用。
    // 回放帧经 JSON 传输边界（HTTP 落地 → WS 广播），实体必为新对象——用 JSON 往返如实模拟
    const landed = JSON.parse(JSON.stringify(canvasWrites()[0]?.patch)) as CanvasPatch;
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, landed);

    canvas.useCanvasStore.getState().updateNodeData("n2", { title: "改二" });
    await vi.advanceTimersByTimeAsync(500);
    // 落盘基线须随回放应用对齐到当前内存：否则回放翻新过的 n1 被当增量重发
    const second = canvasWrites()[1]?.patch as CanvasPatch | undefined;
    expect(second).toBeDefined();
    expect(second?.upsertNodes.map((n) => n.id)).toEqual(["n2"]);
  });

  it("自收回放在写盘在途窗口应用：基线随回放推进，不重发回放实体", async () => {
    stub.seed(FILE, canvasJson([textNode("n1", "节点一"), textNode("n2", "节点二")]));
    await openCanvas();
    stub.writeDelayMs = 50;

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 服务端先广播后返回响应，回放必然先于保存响应到达（落在写盘在途窗口内，此刻 dirty）。
    // 回放帧经 JSON 传输边界（HTTP 落地 → WS 广播），实体必为新对象——用 JSON 往返如实模拟
    const landed = JSON.parse(JSON.stringify(canvasWrites()[0]?.patch)) as CanvasPatch;
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, landed);

    await vi.advanceTimersByTimeAsync(200); // 第一次写落地
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮
    // 落盘基线须随回放推进到补丁覆盖的实体：否则回放翻新过引用的 n1 被当增量重发，成环不止
    expect(canvasWrites()).toHaveLength(1);
    expect(canvas.useCanvasStore.getState().dirty).toBe(false);

    // 基线只吃补丁内实体：后续真实编辑仍正常落盘，且不捎带回放实体
    canvas.useCanvasStore.getState().updateNodeData("n2", { title: "改二" });
    await vi.advanceTimersByTimeAsync(500);
    const second = canvasWrites()[1]?.patch as CanvasPatch | undefined;
    expect(second).toBeDefined();
    expect(second?.upsertNodes.map((n) => n.id)).toEqual(["n2"]);
  });

  it("写盘在途收到远端补丁：远端已落地实体不重发，磁盘收敛到两端内容", async () => {
    await openCanvas();
    stub.writeDelayMs = 50;

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "本地改" });
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 远端补丁新增 n2，服务端已落地（落地后才广播）——内存树同步预置该内容模拟落地结果。
    // 内联正文文本节点的 data 形态不在磁盘 union 成员内（生产序列化同此口径，经转型表达）
    stub.seed(
      FILE,
      canvasJson([
        textNode("n1", "节点一"),
        { id: "n2", type: "text", x: 0, y: 0, data: { title: "远端改", bodyMd: "远端" } },
      ]),
    );
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, {
      id: "c1",
      upsertNodes: [
        { id: "n2", type: "text", x: 0, y: 0, data: { title: "远端改", bodyMd: "远端" } },
      ],
      removedNodeIds: [],
      upsertEdges: [],
      removedEdgeIds: [],
    } as unknown as CanvasPatch);
    await vi.advanceTimersByTimeAsync(200); // 第一次写落地（在预置内容上合并）
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮

    // 远端实体已由服务端落地，客户端不得把它当本地增量重发（重发 → 再落地 → 再回放成环）
    expect(
      canvasWrites().some(
        (w) => (w.patch as CanvasPatch | undefined)?.upsertNodes.some((n) => n.id === "n2"),
      ),
    ).toBe(false);
    const disk = JSON.parse(stub.files.get(FILE) as string) as {
      nodes: { id: string; data: { title?: string } }[];
    };
    expect(disk.nodes.find((n) => n.id === "n1")?.data.title).toBe("本地改");
    expect(disk.nodes.find((n) => n.id === "n2")?.data.title).toBe("远端改");
    expect(
      (canvas.useCanvasStore.getState().nodes.find((n) => n.id === "n2")?.data as { title?: string })
        .title,
    ).toBe("远端改");
    expect(canvas.useCanvasStore.getState().error).toBeNull();
  });

  it("对话消息自收回放在写盘在途窗口应用：消息基线随补丁推进，不重发对话节点", async () => {
    stub.seed(FILE, canvasJson([textNode("n1", "节点一"), textNode("n2", "节点二")]));
    await openCanvas();
    // 预置对话节点（干净状态下经远端补丁到达：节点与消息一并推进基线）
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, {
      id: "c1",
      upsertNodes: [
        { id: "cv1", type: "conversation", x: 0, y: 0, data: { messages: [msg("m1")] } },
      ],
      removedNodeIds: [],
      upsertEdges: [],
      removedEdgeIds: [],
    } as unknown as CanvasPatch);
    stub.writeDelayMs = 50;

    // 本端新增消息（messagesByConv 引用翻新 → 下一次保存携带对话节点），叠加节点编辑触发保存
    canvas.useCanvasStore.setState({
      messagesByConv: { cv1: [msg("m1"), msg("m2")] },
    });
    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(500); // 保存发出：补丁含 n1 + cv1（消息变化）
    const first = canvasWrites()[0]?.patch as CanvasPatch | undefined;
    expect(first).toBeDefined();
    expect(first?.upsertNodes.map((n) => n.id)).toEqual(["n1", "cv1"]);
    // 服务端先广播后返回响应，回放落在写盘在途窗口内；JSON 往返如实模拟传输边界
    const landed = JSON.parse(JSON.stringify(first)) as CanvasPatch;
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, landed);

    await vi.advanceTimersByTimeAsync(200); // 第一次写落地
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮
    // 消息基线随回放推进（mergeMessages 本地无独有消息时原样返回远端引用）：
    // 对话节点不得被重发，否则每轮回放再翻新消息引用，成环不止
    expect(canvasWrites()).toHaveLength(1);

    // 基线只吃补丁内实体：后续真实编辑仍正常落盘，且不捎带对话节点
    canvas.useCanvasStore.getState().updateNodeData("n2", { title: "改二" });
    await vi.advanceTimersByTimeAsync(500);
    const second = canvasWrites()[1]?.patch as CanvasPatch | undefined;
    expect(second).toBeDefined();
    expect(second?.upsertNodes.map((n) => n.id)).toEqual(["n2"]);
  });

  it("自收回放含删除：基线剔除被删实体，不把删除当本地增量重发", async () => {
    stub.seed(FILE, canvasJson([textNode("n1", "节点一"), textNode("n2", "节点二")]));
    await openCanvas();
    stub.writeDelayMs = 50;

    canvas.useCanvasStore.getState().updateNodeData("n1", { title: "改一" });
    await vi.advanceTimersByTimeAsync(500); // 第一次写在途
    // 回放删除 n2，服务端已落地（落地后才广播）——内存树预置删除后的内容
    stub.seed(FILE, canvasJson([textNode("n1", "节点一")]));
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, {
      id: "c1",
      upsertNodes: [],
      removedNodeIds: ["n2"],
      upsertEdges: [],
      removedEdgeIds: [],
    } as unknown as CanvasPatch);
    await vi.advanceTimersByTimeAsync(200); // 第一次写落地
    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮

    // 基线须随回放剔除 n2：否则「基线有、内存无」被当本地删除重发，成环不止
    expect(
      canvasWrites().every(
        (w) => ((w.patch as CanvasPatch | undefined)?.removedNodeIds ?? []).length === 0,
      ),
    ).toBe(true);
    expect(canvas.useCanvasStore.getState().nodes.map((n) => n.id)).toEqual(["n1"]);
    expect(canvas.useCanvasStore.getState().error).toBeNull();
  });
});
