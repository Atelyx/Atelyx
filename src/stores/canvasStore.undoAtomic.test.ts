/**
 * 画布「节点 + 边」入栈粒度与粘贴返回值契约测试。
 *
 * 两个契约都面向用户可见行为，不涉及真实仓库 I/O：
 * - `addNode` 传 `edge` 时必须整体入一次栈：分两次入栈会让用户按一次 Ctrl+Z 只退掉一半
 *   （节点没了边还在，或反之）。
 * - `pasteNodes` 必须如实返回「是否粘贴了节点」：快捷键据此决定是否接管 Ctrl+V，
 *   恒返回 true 会静默吞掉正文里的粘贴。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { Edge, Node } from "@xyflow/react";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

// 插件注册表会把全量领域 store 拉进模块图并在 ESM 初始化期互相取用未就绪的 store；
// 本测试只关心画布 store 自身，桩掉它斩断这条环（同 tableStore.vaultReset.test.ts）。
vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

type CanvasStore = typeof import("./canvasStore");

let canvas: CanvasStore;

const textNode = (id: string, selected = false): Node => ({
  id,
  type: "text",
  position: { x: 0, y: 0 },
  data: { title: id, bodyMd: "" },
  selected,
});

const linkEdge = (source: string, target: string): Edge => ({
  id: `${source}->${target}`,
  source,
  target,
  sourceHandle: null,
  targetHandle: null,
});

beforeEach(async () => {
  vi.resetModules();
  // 先起协作者再取画布 store：画布 store 的协作接线在模块加载期读 collabStore 服务面
  await import("./collabStore");
  canvas = await import("./canvasStore");
  canvas.useCanvasStore.setState({
    canvasId: "c1",
    canvasFile: "画布/c1.atlx",
    nodes: [],
    edges: [],
  });
});

describe("addNode 的入栈粒度", () => {
  it("只传节点：一次撤销回到空画布", () => {
    const store = canvas.useCanvasStore.getState();
    store.addNode(textNode("a"));
    expect(canvas.useCanvasStore.getState().nodes).toHaveLength(1);

    canvas.useCanvasStore.getState().undo();
    expect(canvas.useCanvasStore.getState().nodes).toHaveLength(0);
  });

  it("同时传边：整次操作只占一个撤销单元——撤销后节点与边一起消失", () => {
    const store = canvas.useCanvasStore.getState();
    store.addNode(textNode("a"));
    store.addNode(textNode("b"), linkEdge("a", "b"));
    expect(canvas.useCanvasStore.getState().nodes).toHaveLength(2);
    expect(canvas.useCanvasStore.getState().edges).toHaveLength(1);

    // 一次撤销 = 退掉「b + b 的边」，a 保留
    canvas.useCanvasStore.getState().undo();
    const after = canvas.useCanvasStore.getState();
    expect(after.nodes.map((n) => n.id)).toEqual(["a"]);
    expect(after.edges).toHaveLength(0);
  });
});

describe("pasteNodes 的返回值", () => {
  it("剪贴板为空：返回 false（快捷键须放行默认粘贴）", () => {
    expect(canvas.useCanvasStore.getState().pasteNodes({ x: 0, y: 0 })).toBe(false);
  });

  it("未打开画布：返回 false 且不写入节点（防占位面板残留状态）", () => {
    canvas.useCanvasStore.setState({ canvasId: null, nodes: [textNode("a", true)] });
    canvas.useCanvasStore.getState().copySelectedNodes();
    expect(canvas.useCanvasStore.getState().pasteNodes({ x: 0, y: 0 })).toBe(false);
    expect(canvas.useCanvasStore.getState().nodes.map((n) => n.id)).toEqual(["a"]);
  });

  it("复制后可粘贴：返回 true 并新增节点", () => {
    canvas.useCanvasStore.setState({ nodes: [textNode("a", true)] });
    expect(canvas.useCanvasStore.getState().copySelectedNodes()).toBe(true);
    expect(canvas.useCanvasStore.getState().pasteNodes({ x: 0, y: 0 })).toBe(true);
    expect(canvas.useCanvasStore.getState().nodes).toHaveLength(2);
  });
});
