/**
 * 画布保存 → 历史记录调用链测试。
 *
 * 保存收尾必须对「真实落盘的保存」发起历史版本记录（用户可见行为：编辑产生历史版本）。
 * 重点覆盖保存在途期间收到服务端自收回放帧（落地广播先于 HTTP 响应到达）的竞争：
 * 回放会重挂保存，收尾早退分支不得吞掉本轮已真实落盘内容的历史记录。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type { Node } from "@xyflow/react";
import { CANVAS_SCHEMA } from "@/constants/canvas";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

const recordHistoryVersion = vi.fn(async (..._args: unknown[]) => {});
vi.mock("@/services/history", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/services/history")>();
  return { ...actual, recordHistoryVersion };
});

import { createSpaceStubBackend } from "@/services/content/stubSpaceBackend";

const FILE = "画布/c1.atlx";
let stub: ReturnType<typeof createSpaceStubBackend>;

const textNode = (id: string): Node => ({
  id,
  type: "text",
  position: { x: 0, y: 0 },
  data: { title: id, bodyMd: "" },
});

/** 服务端广播帧里的节点是落盘格式（x/y 而非 position；文本节点正文在独立 .md）。 */
const fileNode = (id: string) => ({
  id,
  type: "text" as const,
  x: 0,
  y: 0,
  data: { title: id, file: `笔记/${id}.md` },
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

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  vi.spyOn(console, "error").mockImplementation(() => {});
  recordHistoryVersion.mockClear();
  await import("./collabStore");
  const canvas = await import("./canvasStore");
  const stubMod = await import("@/services/content/stubSpaceBackend");
  const factory = await import("@/services/content/factory");
  stub = stubMod.createSpaceStubBackend();
  stub.seed(FILE, canvasJson([]));
  factory.activateContentVault(stub.identity, stub.backend);
  // 经 load 打开：落盘基线（lastSaved）随磁盘内容初始化，保证后续 diff 只含真实编辑
  await canvas.useCanvasStore.getState().load(FILE);
  expect(canvas.useCanvasStore.getState().canvasId).toBe("c1");
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("画布保存 → 历史记录", () => {
  it("编辑落盘后发起历史版本记录（file = 落盘路径）", async () => {
    const canvas = await import("./canvasStore");
    canvas.useCanvasStore.getState().addNode(textNode("a"));
    const flushed = canvas.useCanvasStore.getState().flush();
    await vi.advanceTimersByTimeAsync(100);
    await flushed;

    expect(stub.entityWrites.filter((w) => w.kind === "canvas")).not.toHaveLength(0);
    expect(recordHistoryVersion).toHaveBeenCalled();
    const [kind, file] = recordHistoryVersion.mock.calls[0];
    expect(kind).toBe("canvas");
    expect(file).toBe(FILE);
  });

  it("保存在途收到自收回放（服务端广播含发起者）：真实落盘的保存仍记历史", async () => {
    const canvas = await import("./canvasStore");
    stub.writeDelayMs = 50;
    canvas.useCanvasStore.getState().addNode(textNode("a"));
    const flushed = canvas.useCanvasStore.getState().flush();
    await vi.advanceTimersByTimeAsync(0); // persist 已开始、写盘在途
    // 服务端落地后先广播帧（含发起者回放）再返回 HTTP 响应——帧在此刻到达
    canvas.useCanvasStore.getState().applyRemoteCanvasPatch(FILE, {
      id: "c1",
      upsertNodes: [fileNode("a")],
      removedNodeIds: [],
      upsertEdges: [],
      removedEdgeIds: [],
    });
    await vi.advanceTimersByTimeAsync(100); // 写盘完成，收尾遇版本重挂走早退分支
    expect(stub.entityWrites.filter((w) => w.kind === "canvas")).not.toHaveLength(0);
    // 断言在重挂轮之前：本轮真实落盘的收尾必须当场发起历史记录，
    // 防重挂轮自身的历史记录掩盖「本轮被吞」
    expect(recordHistoryVersion).toHaveBeenCalled();
    expect(
      recordHistoryVersion.mock.calls.some(([kind, file]) => kind === "canvas" && file === FILE),
    ).toBe(true);

    await vi.advanceTimersByTimeAsync(700); // 重挂的下一轮 timer（对推进后的基线为空补丁）
    await flushed;
  });
});
