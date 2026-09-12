/**
 * 画布协作纯函数契约测试（utils/canvasCollab）。
 *
 * - `diffCanvasEntities`：引用 diff（未变实体引用相同即不产生补丁；消息数组引用变化计入）。
 * - `computeCanvasCollabPatch`：空补丁返回 null；conversation 消息嵌入、text 有 file 剥离 bodyMd。
 * - `mergeMessages`：并发 append 都保留（确定性）；远端空/本地空语义。
 * - `computeLockOwner`：since 最小者持有、同 since 按 peerId 确定性取小。
 */
import { describe, it, expect } from "vitest";
import type { Node } from "@xyflow/react";
import type { CanvasEdge, Message } from "@/types";
import {
  computeCanvasCollabPatch,
  computeLockOwner,
  deserializeNodeForCollab,
  diffCanvasEntities,
  diffCanvasVersions,
  mergeMessages,
  summarizeCanvasSnapshot,
} from "./canvasCollab";

function node(id: string, x = 0, data: Record<string, unknown> = {}): Node {
  return { id, type: "text", position: { x, y: 0 }, width: 100, height: 50, data };
}

function convNode(id: string, x = 0): Node {
  return { id, type: "conversation", position: { x, y: 0 }, width: 200, height: 100, data: {} };
}

function msg(id: string): Message {
  return {
    id,
    conversationId: "c1",
    role: "user",
    content: `msg-${id}`,
    createdAt: 0,
  } as Message;
}

const edge = (id: string, source: string, target: string): CanvasEdge =>
  ({ id, source, target, directed: true }) as CanvasEdge;

describe("diffCanvasEntities", () => {
  it("未变实体（同引用）不产生补丁", () => {
    const n = node("a");
    const e = edge("e1", "a", "b");
    const baseline = { nodes: [n], edges: [e], messagesByConv: {} };
    const d = diffCanvasEntities([n], [e], {}, baseline);
    expect([...d.upsertNodeIds]).toEqual([]);
    expect(d.removedNodeIds).toEqual([]);
    expect([...d.upsertEdgeIds]).toEqual([]);
    expect(d.removedEdgeIds).toEqual([]);
  });

  it("引用变化/新增/删除被捕获", () => {
    const a = node("a");
    const b = node("b");
    const e1 = edge("e1", "a", "b");
    const baseline = { nodes: [a, b], edges: [e1], messagesByConv: {} };
    // a 位置变化（新对象）、b 删除、c 新增、e1 删除
    const aMoved = { ...a, position: { x: 99, y: 0 } };
    const c = node("c", 5);
    const d = diffCanvasEntities([aMoved, c], [], {}, baseline);
    expect([...d.upsertNodeIds].sort()).toEqual(["a", "c"]);
    expect(d.removedNodeIds.sort()).toEqual(["b"]);
    expect(d.removedEdgeIds).toEqual(["e1"]);
  });

  it("对话节点消息数组引用变化计入节点 upsert", () => {
    const c = convNode("c1");
    const baseline = { nodes: [c], edges: [], messagesByConv: { c1: [] } };
    const msgs = [msg("m1")];
    const d = diffCanvasEntities([c], [], { c1: msgs }, baseline);
    expect([...d.upsertNodeIds]).toEqual(["c1"]);
  });
});

describe("computeCanvasCollabPatch", () => {
  it("无变化返回 null", () => {
    const n = node("a");
    const baseline = { nodes: [n], edges: [], messagesByConv: {}, title: "画布" };
    const p = computeCanvasCollabPatch({
      canvasId: "cv",
      title: "画布",
      nodes: [n],
      edges: [],
      messagesByConv: {},
      lastSaved: baseline,
    });
    expect(p).toBeNull();
  });

  it("conversation 补丁嵌入 messages；接收端反解剥离回 messagesByConv", () => {
    const c = convNode("c1");
    const baseline = { nodes: [c], edges: [], messagesByConv: {}, title: "画布" };
    const msgs = [msg("m1"), msg("m2")];
    const p = computeCanvasCollabPatch({
      canvasId: "cv",
      title: "画布",
      nodes: [c],
      edges: [],
      messagesByConv: { c1: msgs },
      lastSaved: baseline,
    });
    expect(p).not.toBeNull();
    expect(p!.upsertNodes).toHaveLength(1);
    const filed = p!.upsertNodes[0];
    expect((filed.data as { messages?: Message[] }).messages).toHaveLength(2);
    const back = deserializeNodeForCollab(filed);
    expect(back.messages).toHaveLength(2);
    expect((back.node.data as Record<string, unknown>).messages).toBeUndefined();
  });

  it("text 有 file 剥离 bodyMd 只携带结构；接收端标记补读正文", () => {
    const n = node("t1", 0, { title: "笔记", file: "笔记/a.md", bodyMd: "hello" });
    // 基线不含该节点（视为新增）→ 产生补丁
    const baseline = { nodes: [], edges: [], messagesByConv: {}, title: "画布" };
    const p = computeCanvasCollabPatch({
      canvasId: "cv",
      title: "画布",
      nodes: [n],
      edges: [],
      messagesByConv: {},
      lastSaved: baseline,
    });
    const filed = p!.upsertNodes[0];
    expect((filed.data as { bodyMd?: string }).bodyMd).toBeUndefined();
    expect((filed.data as { file?: string }).file).toBe("笔记/a.md");
    expect(deserializeNodeForCollab(filed).refreshBodyMdFile).toBe("笔记/a.md");
  });

  it("media 有 file 剥离 thumb/body（内容在附件，按引用读回）", () => {
    const n = { ...node("m1"), type: "media" } as Node;
    n.data = {
      file: ".atelyx/temp/abc/att-x.png",
      mime: "image/png",
      kind: "image",
      name: "截图.png",
      thumb: "data:image/png;base64,AAAA",
      body: "不该出现的正文",
    };
    const baseline = { nodes: [], edges: [], messagesByConv: {}, title: "画布" };
    const p = computeCanvasCollabPatch({
      canvasId: "cv",
      title: "画布",
      nodes: [n],
      edges: [],
      messagesByConv: {},
      lastSaved: baseline,
    });
    const data = p!.upsertNodes[0].data as unknown as Record<string, unknown>;
    expect(data.file).toBe(".atelyx/temp/abc/att-x.png");
    expect(data.thumb).toBeUndefined();
    expect(data.body).toBeUndefined();
    expect(data.name).toBe("截图.png");
  });

  it("media 无 file 时保留 thumb/body（内容是唯一副本，剥离即销毁）", () => {
    const n = { ...node("m1"), type: "media" } as Node;
    n.data = { mime: "image/png", kind: "image", thumb: "data:image/png;base64,AAAA" };
    const baseline = { nodes: [], edges: [], messagesByConv: {}, title: "画布" };
    const p = computeCanvasCollabPatch({
      canvasId: "cv",
      title: "画布",
      nodes: [n],
      edges: [],
      messagesByConv: {},
      lastSaved: baseline,
    });
    expect((p!.upsertNodes[0].data as unknown as Record<string, unknown>).thumb).toBe(
      "data:image/png;base64,AAAA",
    );
  });

  it("消息附件有 file 剥离 payload、无 file 保留（内嵌附件唯一副本）", () => {
    const withFile = {
      ...msg("m1"),
      attachments: [
        { kind: "image" as const, mime: "image/png", file: "附件/a.png", payload: "data:image/png;base64,AAAA" },
      ],
    };
    const embedded = {
      ...msg("m2"),
      attachments: [{ kind: "image" as const, mime: "image/png", payload: "data:image/png;base64,BBBB" }],
    };
    const c = convNode("c1");
    const baseline = { nodes: [], edges: [], messagesByConv: {}, title: "画布" };
    const p = computeCanvasCollabPatch({
      canvasId: "cv",
      title: "画布",
      nodes: [c],
      edges: [],
      messagesByConv: { c1: [withFile, embedded] },
      lastSaved: baseline,
    });
    const sent = (p!.upsertNodes[0].data as { messages: Message[] }).messages;
    expect(sent[0].attachments?.[0].payload).toBeUndefined();
    expect(sent[0].attachments?.[0].file).toBe("附件/a.png");
    expect(sent[1].attachments?.[0].payload).toBe("data:image/png;base64,BBBB");
  });
});

describe("mergeMessages", () => {
  it("远端基底 + 本地独有消息按原序补入（并发 append 都保留）", () => {
    const remote = [msg("m1"), msg("m3")];
    const local = [msg("m1"), msg("m2")];
    const merged = mergeMessages(remote, local);
    expect(merged.map((m) => m.id)).toEqual(["m1", "m3", "m2"]);
  });

  it("任一侧为空：远端空保留本地（本端进行中消息不丢）；本地空用远端", () => {
    expect(mergeMessages([], [msg("m1")]).map((m) => m.id)).toEqual(["m1"]);
    expect(mergeMessages([msg("m1")], []).map((m) => m.id)).toEqual(["m1"]);
    expect(mergeMessages([], [])).toEqual([]);
  });

  it("远端已含本地全部消息 → 直接用远端（幂等无重复）", () => {
    const remote = [msg("m1"), msg("m2")];
    const merged = mergeMessages(remote, [msg("m1"), msg("m2")]);
    expect(merged.map((m) => m.id)).toEqual(["m1", "m2"]);
  });

  it("同 id 消息：附件内容缓存沿用本端（远端补丁里 payload 已被剥离）", () => {
    const attachments = [
      { kind: "image" as const, mime: "image/png", file: "附件/a.png", payload: "data:image/png;base64,AAAA" },
    ];
    const local = [{ ...msg("m1"), attachments }];
    const remote = [
      { ...msg("m1"), attachments: attachments.map((a) => ({ ...a, payload: undefined })) },
    ];

    const merged = mergeMessages(remote, local);

    expect(merged[0].attachments?.[0].payload).toBe("data:image/png;base64,AAAA");
  });

  it("同 id 消息：远端换了引用则不用本端旧内容", () => {
    const local = [
      {
        ...msg("m1"),
        attachments: [
          { kind: "image" as const, mime: "image/png", file: "附件/old.png", payload: "data:old" },
        ],
      },
    ];
    const remote = [
      {
        ...msg("m1"),
        attachments: [{ kind: "image" as const, mime: "image/png", file: "附件/new.png" }],
      },
    ];

    const merged = mergeMessages(remote, local);

    expect(merged[0].attachments?.[0].file).toBe("附件/new.png");
    expect(merged[0].attachments?.[0].payload).toBeUndefined();
  });
});

describe("computeLockOwner", () => {
  it("since 最小者持有；同 since 按 peerId 递增取小（确定性）", () => {
    // 两人先后声明同一节点
    expect(computeLockOwner([{ peerId: 5, since: 200 }, { peerId: 3, since: 100 }])).toBe(3);
    // 同 since → 小 peerId 胜
    expect(computeLockOwner([{ peerId: 5, since: 100 }, { peerId: 3, since: 100 }])).toBe(3);
    // 交换输入顺序结果不变（确定性）
    expect(computeLockOwner([{ peerId: 3, since: 100 }, { peerId: 5, since: 200 }])).toBe(3);
    // 无声明 → null
    expect(computeLockOwner([])).toBeNull();
  });
});

// ===== 历史版本摘要 / diff（画布历史面板可读化）=====

/** 磁盘格式快照（.atlx 的 nodes/edges 形态）。 */
function canvasSnap(nodes: unknown[], edges: unknown[] = []): string {
  return JSON.stringify({
    schema: "atelyx-canvas/v1",
    id: "c1",
    title: "方案",
    nodes,
    edges,
    createdAt: 0,
    updatedAt: 0,
  });
}

describe("diffCanvasVersions / summarizeCanvasSnapshot", () => {
  it("节点增删与内容修改计入；仅消息变化不算节点修改", () => {
    const prev = canvasSnap([{ id: "n1", type: "text", x: 0, y: 0, data: { title: "提示词" } }]);
    const next = canvasSnap([
      { id: "n1", type: "text", x: 0, y: 0, data: { title: "提示词 v2" } },
      { id: "n2", type: "text", x: 10, y: 0, data: { title: "灵感" } },
    ]);
    const d = diffCanvasVersions(prev, next);
    expect(d.addedNodes).toEqual(["灵感"]);
    expect(d.modifiedNodes).toEqual(["提示词 v2"]);
    const conv = (messages: unknown[]) => ({ id: "c1", type: "conversation", x: 0, y: 0, data: { title: "对话", messages } });
    const d2 = diffCanvasVersions(canvasSnap([conv([{ id: "m1" }])]), canvasSnap([conv([{ id: "m1" }, { id: "m2" }])]));
    expect(d2.msgDelta).toBe(1);
    expect(d2.modifiedNodes).toEqual([]);
  });

  it("首版（prev 空）摘要为新建统计", () => {
    expect(summarizeCanvasSnapshot("", canvasSnap([{ id: "n1", type: "text", x: 0, y: 0, data: { title: "a" } }], [{ id: "e1" }]))).toBe(
      "新建 · 1 节点 · 1 连线",
    );
  });

  it("摘要覆盖节点/连线/消息变化", () => {
    const prev = canvasSnap([{ id: "n1", type: "text", x: 0, y: 0, data: { title: "a" } }]);
    const next = canvasSnap(
      [{ id: "n1", type: "text", x: 0, y: 0, data: { title: "a" } }, { id: "n2", type: "text", x: 0, y: 0, data: { title: "b" } }],
      [{ id: "e1" }],
    );
    const s = summarizeCanvasSnapshot(prev, next);
    expect(s).toContain("新增 1 节点");
    expect(s).toContain("新增 1 连线");
  });
});
