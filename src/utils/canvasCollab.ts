/**
 * 画布多人实时协作纯函数：引用 diff / 纯序列化 / 消息合并 / 锁主判定。
 *
 * 与磁盘保存同源但不带副作用——`diffCanvasEntities` 被 `patchCanvasVault`（磁盘）与协作广播
 * （relay `canvas-patch`）共用；`serializeNodeForCollab` 是 `toFileNode` 的纯版本（**不触发
 * `.md` 写入**——协作广播绝不能因广播而写文件）。接收端经 `deserializeNodeForCollab` 反解回
 * 运行时形态（conversation 的 `data.messages` 拆回 `messagesByConv` 并剥离）。
 *
 * 本模块全部为纯函数，无 I/O、无 store 依赖，便于单测。
 */
import type { Node } from "@xyflow/react";
import { CANVAS_SCHEMA } from "@/constants/canvas";
import type {
  CanvasEdge,
  CanvasFile,
  CanvasFileEdge,
  CanvasFileNode,
  CanvasPatch,
  Message,
} from "@/types";

/** 增量 diff 基线（与 canvasStore 落盘/广播快照同构；未变实体引用相同即视为未变化）。 */
export interface CanvasDiffBaseline {
  nodes: Node[];
  edges: CanvasEdge[];
  messagesByConv: Record<string, Message[]>;
}

/** 引用 diff 结果：只含变化/新增/删除的实体 id。 */
export interface CanvasDiffResult {
  upsertNodeIds: Set<string>;
  removedNodeIds: string[];
  upsertEdgeIds: Set<string>;
  removedEdgeIds: string[];
}

/**
 * 引用 diff（store 不可变更新，未变实体引用相同，O(N) 指针比对）：
 * - 节点：引用不同 = 变化/新增；对话节点消息数组引用变化（`messagesByConv` 与节点分离）也计入。
 * - 边：引用不同 = 变化/新增。
 * 与 `patchCanvasVault` 的磁盘 diff 完全同源（自动保存主路径），协作广播复用同一函数。
 */
export function diffCanvasEntities(
  nodes: Node[],
  edges: CanvasEdge[],
  messagesByConv: Record<string, Message[]>,
  lastSaved: CanvasDiffBaseline,
): CanvasDiffResult {
  // 预建 id → 实体索引替代循环内 .find()（大画布 O(N²) → O(N)；每次自动保存/广播都会跑）
  const lastNodesById = new Map(lastSaved.nodes.map((n) => [n.id, n]));
  const lastEdgesById = new Map(lastSaved.edges.map((e) => [e.id, e]));
  // 节点 diff：引用不同 = 变化/新增；对话节点消息变化时节点引用不变，按 conv id 补进 upsert
  const upsertNodeIds = new Set<string>();
  for (const n of nodes) {
    const ls = lastNodesById.get(n.id);
    if (!ls || ls !== n) upsertNodeIds.add(n.id);
  }
  for (const [convId, msgs] of Object.entries(messagesByConv)) {
    if (lastSaved.messagesByConv[convId] !== msgs) upsertNodeIds.add(convId);
  }
  const currentNodeIds = new Set(nodes.map((n) => n.id));
  const removedNodeIds = lastSaved.nodes
    .filter((n) => !currentNodeIds.has(n.id))
    .map((n) => n.id);
  const upsertEdgeIds = new Set<string>();
  for (const e of edges) {
    const ls = lastEdgesById.get(e.id);
    if (!ls || ls !== e) upsertEdgeIds.add(e.id);
  }
  const currentEdgeIds = new Set(edges.map((e) => e.id));
  const removedEdgeIds = lastSaved.edges
    .filter((e) => !currentEdgeIds.has(e.id))
    .map((e) => e.id);
  return { upsertNodeIds, removedNodeIds, upsertEdgeIds, removedEdgeIds };
}

/** 运行时边 → 磁盘/协作补丁边（磁盘全量写、增量补丁与 `canvas-patch` 广播共用同一形态；
 * createdAt 传 0——Rust 落盘时保留原值，协作接收端不关心）。 */
export function serializeEdgeForCollab(e: CanvasEdge): CanvasFileEdge {
  return {
    id: e.id,
    source: e.source,
    target: e.target,
    sourceHandle: e.sourceHandle ?? undefined,
    targetHandle: e.targetHandle ?? undefined,
    directed: e.directed,
    linkMode: e.linkMode,
    createdAt: 0,
  };
}

/**
 * 运行时画布 → 历史快照（`.atlx` 文件格式，`runtimeToCanvasFile` 的纯版本——复用
 * `serializeNodeForCollab` 不触发 `.md` 写入：历史快照绝不能因记快照而把旧正文写回共享盘）。
 * 输出与 `runtimeToCanvasFile` 结构一致（text 有 file 只带 `{title, file}`、conversation 嵌入
 * messages），回滚经 `writeCanvasVault` 全量写回兼容。
 */
export function serializeCanvasSnapshot(
  canvasId: string,
  title: string,
  nodes: Node[],
  edges: CanvasEdge[],
  messagesByConv: Record<string, Message[]>,
): CanvasFile {
  return {
    schema: CANVAS_SCHEMA,
    id: canvasId,
    title,
    nodes: nodes.map((n) => serializeNodeForCollab(n, messagesByConv)),
    edges: edges.map(serializeEdgeForCollab),
    createdAt: 0,
    updatedAt: Date.now(),
  };
}

/**
 * 运行时节点 → 协作补丁节点（`toFileNode` 的纯版本，无 `.md` 写副作用）：
 * - text 有 file = 笔记节点：正文在共享盘 `.md`（对端靠 watcher 同步），只携带结构 `{title, file}`
 * - text 无 file = 画布内文本节点：`bodyMd` 随补丁内嵌携带
 * - conversation：嵌入 `messagesByConv[id]` 到 `data.messages`（接收端反解）
 * - group/link/table：与磁盘序列化一致（table 快照在 `.atb`，接收端自行补读）
 * - media：有引用时剥离 thumb/body（按引用读回），无引用时带上（内容只在此）
 */
export function serializeNodeForCollab(
  n: Node,
  messagesByConv: Record<string, Message[]>,
): CanvasFileNode {
  let data: Record<string, unknown>;
  if (n.type === "text") {
    const td = n.data as unknown as { title?: string; file?: string; bodyMd?: string };
    if (td.file) {
      data = { title: td.title || "未命名", file: td.file };
    } else {
      data = { title: td.title || "未命名", bodyMd: td.bodyMd ?? "" };
    }
  } else if (n.type === "conversation") {
    data = {
      ...n.data,
      messages: (messagesByConv[n.id] ?? []).map(stripCollabAttachmentPayload),
    };
  } else if (n.type === "group") {
    const gd = n.data as unknown as { label?: string; color?: string };
    data = { label: gd.label ?? "分组", color: gd.color };
  } else if (n.type === "link") {
    data = { url: (n.data as unknown as { url?: string }).url ?? "" };
  } else if (n.type === "table") {
    const td = n.data as unknown as { title?: string; file?: string };
    data = { title: td.title || "未命名", file: td.file };
  } else if (n.type === "media") {
    // 媒体节点与落盘同一口径：内容在文件（仓库附件或未入库临时区），只带引用与展示元信息。
    // 有引用时 thumb/body 是按引用读回的运行时缓存，广播它们等于把图片字节发给同房对端
    // （与其初衷「只走路径引用」相反）；无引用时内容只存在于节点自身，必须带上。
    const md = n.data as unknown as { file?: string };
    const rest = { ...(n.data as unknown as Record<string, unknown>) };
    if (md.file) {
      delete rest.thumb;
      delete rest.body;
    }
    data = rest;
  } else {
    data = { ...n.data };
  }
  return {
    id: n.id,
    type: n.type as CanvasFileNode["type"],
    x: n.position.x,
    y: n.position.y,
    width: n.width,
    height: n.height,
    data: data as unknown as CanvasFileNode["data"],
  };
}

/**
 * 协作补丁里的消息附件内容剥离：只剥**有引用**的附件（内容可按引用读回），
 * 无引用的附件内容只存在于消息自身，剥掉即让对端拿到一张空图。
 */
function stripCollabAttachmentPayload(m: Message): Message {
  if (!m.attachments?.length) return m;
  return {
    ...m,
    attachments: m.attachments.map((att) => {
      if (!att.file) return att;
      const { payload: _dropped, ...rest } = att;
      return rest;
    }),
  };
}

/** 协作补丁节点反解结果（运行时形态）。 */
export interface DeserializedCollabNode {
  /** 运行时节点（position 展开；conversation data 已剥离 `messages`；group 补 zIndex -1）。 */
  node: Node;
  /** conversation 节点：提取出的消息数组（接收端按 id 合并进 `messagesByConv`）。 */
  messages?: Message[];
  /** text 节点（有 file）：接收端需补读正文（bodyMd 不在补丁内，在共享盘 `.md`）。 */
  refreshBodyMdFile?: string;
}

/**
 * 协作补丁节点 → 运行时节点（`canvasFileToRuntime` 的协作版，同步处理逻辑一致）：
 * - conversation：`data.messages` 提取并剥离 → `messages`
 * - text 有 file：标记 `refreshBodyMdFile`（对端从共享盘 `.md` 补读正文）
 * - group：运行时补 `zIndex: -1`（低层背景容器，与 load 路径一致，防协作新增分组盖到节点上）
 */
export function deserializeNodeForCollab(fileNode: CanvasFileNode): DeserializedCollabNode {
  const data: Record<string, unknown> = { ...fileNode.data };
  let messages: Message[] | undefined;
  let refreshBodyMdFile: string | undefined;
  if (fileNode.type === "conversation") {
    const cd = data as unknown as { messages?: Message[] };
    messages = cd.messages ?? [];
    delete data.messages;
  } else if (fileNode.type === "text") {
    const td = data as unknown as { file?: string };
    if (td.file) refreshBodyMdFile = td.file;
  }
  const node: Node = {
    id: fileNode.id,
    type: fileNode.type,
    position: { x: fileNode.x, y: fileNode.y },
    width: fileNode.width,
    height: fileNode.height,
    data: data as Node["data"],
  };
  if (fileNode.type === "group") node.zIndex = -1;
  return { node, messages, refreshBodyMdFile };
}

/**
 * 按 id 合并对话消息：远端为基底，本地独有消息（对端尚未见到的进行中/流式消息）按原序补入。
 * 与 `mergeFromDisk` 的消息合并语义一致——锁模型下对端不会并发写消息，此合并仅兜底保护
 * 本端进行中内容不被对端陈旧节点快照覆盖（如对端移动本端正在生成的节点）。
 *
 * 同 id 消息以远端为胜，但**附件内容缓存例外**：`payload` 不随补丁传输（有 `file` 时被剥离），
 * 它是本端按引用读回的运行时数据——引用相同即沿用本端 payload，否则对端每次发言都会把本端
 * 已回填的历史气泡图片打回文件名 chip（内容没丢，但要等重载或本端再发才恢复）。
 */
export function mergeMessages(remote: Message[], local: Message[]): Message[] {
  if (local.length === 0) return remote;
  if (remote.length === 0) return local;
  const localById = new Map(local.map((m) => [m.id, m]));
  let changed = false;
  const merged = remote.map((m) => {
    const mine = localById.get(m.id);
    if (!mine?.attachments?.length || !m.attachments?.length) return m;
    let filled = false;
    const attachments = m.attachments.map((a, i) => {
      if (a.payload || !a.file) return a;
      const counterpart = mine.attachments?.[i];
      if (!counterpart?.payload || counterpart.file !== a.file) return a;
      filled = true;
      return { ...a, payload: counterpart.payload };
    });
    if (!filled) return m;
    changed = true;
    return { ...m, attachments };
  });
  const remoteIds = new Set(remote.map((m) => m.id));
  const extras = local.filter((m) => !remoteIds.has(m.id));
  if (!changed) return extras.length === 0 ? remote : [...remote, ...extras];
  return extras.length === 0 ? merged : [...merged, ...extras];
}

/** 计算协作广播补丁（diff + 纯序列化）；无变化返回 null（调用方跳过广播）。 */
export function computeCanvasCollabPatch(opts: {
  canvasId: string;
  title: string;
  nodes: Node[];
  edges: CanvasEdge[];
  messagesByConv: Record<string, Message[]>;
  lastSaved: CanvasDiffBaseline & { title: string };
}): CanvasPatch | null {
  const { canvasId, title, nodes, edges, messagesByConv, lastSaved } = opts;
  const { upsertNodeIds, removedNodeIds, upsertEdgeIds, removedEdgeIds } = diffCanvasEntities(
    nodes,
    edges,
    messagesByConv,
    lastSaved,
  );
  const upsertNodes: CanvasFileNode[] = [];
  for (const n of nodes) {
    if (!upsertNodeIds.has(n.id)) continue;
    upsertNodes.push(serializeNodeForCollab(n, messagesByConv));
  }
  const upsertEdges: CanvasFileEdge[] = edges
    .filter((e) => upsertEdgeIds.has(e.id))
    .map(serializeEdgeForCollab);
  const titleChanged = title !== lastSaved.title;
  if (
    upsertNodes.length === 0 &&
    upsertEdges.length === 0 &&
    removedNodeIds.length === 0 &&
    removedEdgeIds.length === 0 &&
    !titleChanged
  ) {
    return null;
  }
  return {
    id: canvasId,
    upsertNodes,
    removedNodeIds,
    upsertEdges,
    removedEdgeIds,
    ...(titleChanged ? { title } : {}),
  };
}

/**
 * 协作锁原语（确定性锁主判定）实现在内核 `services/collab/locks.ts`；此处 re-export
 * 保持画布域既有导入路径（canvasStore/组件/测试零改动）。语义：声明制 + since 最小者持有、
 * 同 since 按 peerId 递增取小（详见 locks.ts 头注释）。
 */
export {
  computeCollabLockOwner as computeLockOwner,
  resolveCollabLock as resolveLockState,
  type LockResolution,
} from "@/services/collab/locks";

/** 协作画布重命名抑制窗口（ms）：对端收到远端 title 补丁已同步新路径，watcher 收到
 * 旧路径 delete / 新路径 create 事件时在窗口内跳过 reload/conflict（内容已由补丁应用，
 * 含本地脏编辑不丢，待下次保存走乐观锁自动合并收敛）。仿 utils/selfSave 的时间窗模式。 */
const COLLAB_RENAME_SUPPRESS_MS = 10_000;
const collabRenameAt = new Map<string, number>();

/** 登记协作重命名涉及的文件路径（旧 + 新），watcher 据此跳过（见 canvasStore.applyRemoteCanvasPatch）。 */
export function markCollabCanvasRename(paths: string[]): void {
  const now = Date.now();
  for (const [p, at] of collabRenameAt) {
    if (now - at >= COLLAB_RENAME_SUPPRESS_MS) collabRenameAt.delete(p);
  }
  for (const p of paths) collabRenameAt.set(p, now);
}

/** watcher 画布分支判断该路径事件是否为协作重命名的回波（窗口内 = 跳过 reload/conflict）。 */
export function isCollabCanvasRenamePath(path: string): boolean {
  return Date.now() - (collabRenameAt.get(path) ?? 0) < COLLAB_RENAME_SUPPRESS_MS;
}

// ===== 历史版本摘要 / diff（画布历史面板可读化）=====

/** 节点展示名（conversation/text → data.title；group → label；link → url；media → 文件名）。 */
function canvasNodeLabel(n: CanvasFileNode): string {
  const d = n.data as unknown as Record<string, unknown>;
  if (typeof d.title === "string" && d.title.trim()) return d.title.trim();
  if (typeof d.label === "string" && d.label.trim()) return d.label.trim();
  if (typeof d.url === "string" && d.url.trim()) {
    const u = d.url.trim();
    return u.length > 20 ? `${u.slice(0, 20)}…` : u;
  }
  if (typeof d.file === "string" && d.file) return d.file.split("/").pop() ?? d.file;
  return "节点";
}

/** 对话节点消息总数（conversation 的 data.messages 内嵌在 .atlx 快照里）。 */
function canvasMessageCount(nodes: CanvasFileNode[]): number {
  let n = 0;
  for (const node of nodes) {
    const msgs = (node.data as { messages?: unknown[] }).messages;
    if (Array.isArray(msgs)) n += msgs.length;
  }
  return n;
}

/** 画布历史版本 diff（相对上一版本；首版 prev 为空 → 全部节点/边计为新增）。 */
export interface CanvasVersionDiff {
  addedNodes: string[];
  removedNodes: string[];
  modifiedNodes: string[];
  addedEdges: number;
  removedEdges: number;
  msgDelta: number;
}

/** 对比两个画布历史快照：节点增删/修改（不含消息）+ 边增删 + 对话消息增减（纯函数，面板懒计算）。 */
export function diffCanvasVersions(prevRaw: string, nextRaw: string): CanvasVersionDiff {
  let prev: CanvasFile | null = null;
  let next: CanvasFile | null = null;
  try {
    if (prevRaw) prev = JSON.parse(prevRaw) as CanvasFile;
    next = JSON.parse(nextRaw) as CanvasFile;
  } catch {
    // 快照损坏：尽力而为，缺省为空 diff
  }
  if (!next || !Array.isArray(next.nodes)) {
    return { addedNodes: [], removedNodes: [], modifiedNodes: [], addedEdges: 0, removedEdges: 0, msgDelta: 0 };
  }
  const prevNodes = prev && Array.isArray(prev.nodes) ? prev.nodes : [];
  const prevEdges = prev && Array.isArray(prev.edges) ? prev.edges : [];
  const prevById = new Map(prevNodes.map((n) => [n.id, n]));
  const nextById = new Map(next.nodes.map((n) => [n.id, n]));
  const prevEdgeIds = new Set(prevEdges.map((e) => e.id));
  const nextEdgeIds = new Set(next.edges.map((e) => e.id));
  // 对话消息拆出单独统计：data.messages 变化不计入「修改节点」（避免每次发消息都算节点改动）
  const stripMessages = (d: Record<string, unknown>): Record<string, unknown> => {
    const { messages: _m, ...rest } = d;
    return rest;
  };
  return {
    addedNodes: next.nodes.filter((n) => !prevById.has(n.id)).map(canvasNodeLabel),
    removedNodes: prevNodes.filter((n) => !nextById.has(n.id)).map(canvasNodeLabel),
    modifiedNodes: next.nodes
      .filter((n) => {
        const p = prevById.get(n.id);
        return (
          p &&
          JSON.stringify(stripMessages(n.data as unknown as Record<string, unknown>)) !==
            JSON.stringify(stripMessages(p.data as unknown as Record<string, unknown>))
        );
      })
      .map(canvasNodeLabel),
    addedEdges: next.edges.filter((e) => !prevEdgeIds.has(e.id)).length,
    removedEdges: prevEdges.filter((e) => !nextEdgeIds.has(e.id)).length,
    msgDelta: canvasMessageCount(next.nodes) - canvasMessageCount(prevNodes),
  };
}

/**
 * 画布历史版本人话摘要（记录时生成，列表展示）：对比上一版本快照输出「新增/删除/修改 N 节点 ·
 * 连线增删 · 对话消息增减」；无上一版本 = 新建统计。损坏快照 → 空串。
 */
export function summarizeCanvasSnapshot(prevRaw: string, nextRaw: string): string {
  let next: CanvasFile | null = null;
  try {
    next = JSON.parse(nextRaw) as CanvasFile;
  } catch {
    return "";
  }
  if (!next || !Array.isArray(next.nodes)) return "";
  if (!prevRaw) return `新建 · ${next.nodes.length} 节点 · ${next.edges.length} 连线`;
  const diff = diffCanvasVersions(prevRaw, nextRaw);
  const parts: string[] = [];
  if (diff.addedNodes.length) parts.push(`新增 ${diff.addedNodes.length} 节点`);
  if (diff.removedNodes.length) parts.push(`删除 ${diff.removedNodes.length} 节点`);
  if (diff.modifiedNodes.length) parts.push(`修改 ${diff.modifiedNodes.length} 节点`);
  if (diff.addedEdges) parts.push(`新增 ${diff.addedEdges} 连线`);
  if (diff.removedEdges) parts.push(`删除 ${diff.removedEdges} 连线`);
  if (diff.msgDelta > 0) parts.push(`对话消息 +${diff.msgDelta}`);
  else if (diff.msgDelta < 0) parts.push(`对话消息 ${diff.msgDelta}`);
  return parts.length ? parts.join(" · ") : "未改动";
}
