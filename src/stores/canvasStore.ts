import { create } from "zustand";
import {
  applyNodeChanges,
  applyEdgeChanges,
  addEdge as rfAddEdge,
  type Connection,
  type Edge,
  type EdgeChange,
  type Node,
  type NodeChange,
  type OnConnect,
} from "@xyflow/react";
import {
  loadCanvasVault,
  loadWhiteboardVault,
  persistCanvasVault,
  patchCanvasVault,
  readAttachmentDataUrl,
  readCanvasVault,
  readNote,
  recordNoteDiskContent,
  renameCanvasVault,
  writeCanvasVault,
  type RuntimeCanvas,
} from "@/services/vault";
import { decideTextNodeRefresh } from "@/utils/noteRefresh";
import { readTableVault } from "@/services/table";
import { tableToSnapshotText } from "@/utils/table";
import {
  loadHistory as loadCanvasHistory,
  recordAgentFileWrite,
  recordHistoryVersion,
  versionContentAt,
  type HistoryVersion,
} from "@/services/history";
import {
  computeCanvasCollabPatch,
  deserializeNodeForCollab,
  markCollabCanvasRename,
  mergeMessages,
  resolveLockState,
  serializeCanvasSnapshot,
  serializeEdgeForCollab,
  serializeNodeForCollab,
  summarizeCanvasSnapshot,
} from "@/utils/canvasCollab";
import { toLlmMessages } from "@/services/ai/client";
import { abortAutoTitle } from "@/services/ai/autoTitle";
import { runSearch, resultsToText } from "@/services/search";
import { runAgentTools, assembleAgentSystemPrompt } from "@/services/ai/tools";
import {
  appendVaultFile,
  editVaultFile,
  globVault,
  grepVault,
  listVaultDir,
  readVaultFileWindow,
  writeVaultFile,
} from "@/services/vault/aiFiles";
import { readHistoryForAgent } from "@/services/history";
import {
  currentTodosBlock,
  readAgentTodos,
  writeAgentTodos,
} from "@/services/vault/agentTodos";
import { fetchWeb } from "@/services/web";
import {
  findFreeSpot,
  pickEdgeHandles,
  rectOf,
  collectGroupMembers,
} from "@/utils/layout";
import { createPersistController } from "@/utils/persist";
import { markSelfSave } from "@/utils/selfSave";
import { createUndoManager } from "@/utils/undoStack";
import { inferImageMime } from "@/utils/whiteboard";
import {
  CANVAS_SCHEMA,
  DEFAULT_CONVERSATION_WIDTH,
  DEFAULT_CONVERSATION_HEIGHT,
  DEFAULT_TEXT_NODE_WIDTH,
  DEFAULT_TEXT_NODE_HEIGHT,
  DEFAULT_TABLE_NODE_WIDTH,
  DEFAULT_TABLE_NODE_HEIGHT,
  DEFAULT_GROUP_WIDTH,
  DEFAULT_GROUP_HEIGHT,
} from "@/constants/canvas";
import { ERROR_PREFIX, TIMEOUT_ERROR_TEXT } from "@/constants/chat";
import {
  runStreamExchange,
  decideCleanup,
  runAutoNaming,
} from "./streaming";
import { isAssetConsumed } from "@/utils/consumed";
import { appendNarration, appendReasoning, assistantReplyText, fillAssistantReplyText, finalizeReplyText, mergeToolRuns } from "@/utils/agentSteps";
import { prefix, scanMentionHits } from "@/utils/text";
import { noteTitleFromFile, sanitizeFilename, siblingPath, tableTitleFromFile } from "@/utils/filename";
import { useSettingsStore } from "./settingsStore";
import { useAppStore } from "./appStore";
import {
  collabSendSink,
  publishCollabPresence,
  registerCollabChannel,
  registerCollabPresenceProvider,
  registerCollabReconnect,
  registerCollabTeardown,
  useCollabStore,
} from "./collabStore";
import { useVaultStore } from "./vaultStore";
import { emitPluginEvent, setPluginCanvasAccess } from "@/services/plugins";
import type {
  Attachment,
  CanvasEdge,
  CanvasFile,
  CanvasPatch,
  ConversationData,
  LinkMode,
  PluginCanvasEdge,
  PluginCanvasNode,
  TableData,
  TextData,
  MediaData,
  Message,
  PendingAttachment,
  SearchResultData,
  ToolSchema,
  LlmMessage,
} from "@/types";

/** Undo/Redo 快照（含 messagesByConv，否则分支撤销时消息状态会撕裂） */
interface Snapshot {
  nodes: Node[];
  edges: Edge[];
  messagesByConv: Record<string, Message[]>;
}

/**
 * 分组拖拽联动状态：dragStart 时快照组起始位置 + 组内成员起始位置，
 * dragging 期间按组位移同步平移成员（onNodesChange 中处理），dragStop 清空。
 * 成员判定 = 中心点落在组矩形内（collectGroupMembers，拖前位置一次快照）。
 */
interface GroupDragState {
  groupId: string;
  groupStart: { x: number; y: number };
  members: { id: string; start: { x: number; y: number } }[];
}
let groupDragState: GroupDragState | null = null;

/** 节点拖拽进行中（dragStart 置位 / dragStop 清除）：期间 position 变更只更新内存，
 * 落盘统一到 dragStop——连续拖动中的高频 position change 不再每帧重置 debounce/触发保存。 */
let dragInProgress = false;

/** 复制/粘贴剪贴板（模块级）：复制选中节点深拷贝快照，粘贴重新生成 id（不带边，语义同右键「复制节点」）。 */
interface ClipboardNode {
  type: string | undefined;
  position: { x: number; y: number };
  width?: number | null;
  height?: number | null;
  zIndex?: number;
  data: Record<string, unknown>;
}
let clipboardNodes: ClipboardNode[] = [];

/**
 * getReferencedInputs 返回的待注入引用（含源节点 id、显示名、注入内容）。
 * `.md` 笔记节点（文本且带 file）走「引用文件」路径块：`file` = 相对仓库根路径、`content` 恒空；
 * 其余节点（画布内文本/搜索/表格）走全文注入：`content` = 注入正文、`file` 为空。
 */
interface ReferencedInput {
  nodeId: string;
  label: string;
  content: string;
  /** .md 笔记节点相对仓库根路径（@引用/连边统一走路径块，模型 read_file 读取）。 */
  file?: string;
}

/** @ 提及映射：输入框内可见的 @显示名 → 源节点 id（发送时就地替换为引用内容）。 */
interface Mention {
  nodeId: string;
  text: string;
}

/**
 * 单个画布的运行时状态。
 * - 节点/边变更 debounce 500ms 落库
 * - messages 按对话节点分组，流式回复完成时 upsert
 * - Undo/Redo 栈深 50，覆盖增删节点/连线/移动
 */

interface CanvasState {
  canvasId: string | null;
  /** 当前画布磁盘路径（相对仓库根，画布任意文件夹存放；打开/保存按此路径）。 */
  canvasFile: string | null;
  canvasTitle: string;
  nodes: Node[];
  edges: CanvasEdge[];
  /** 只读查看（外部白板格式 .canvas）：不落盘、禁编辑/删除/重命名。 */
  readOnly: boolean;
  /** 更新关联边的箭头模式（无向/单向/双向，样式切换不入 undo 栈）。 */
  setEdgeLinkMode: (edgeId: string, linkMode: LinkMode) => void;
  /** messages 按 conversationId 分组 */
  messagesByConv: Record<string, Message[]>;
  /** 各对话节点是否正在流式回复 */
  streamingByConv: Record<string, boolean>;
  /** 拖线引用队列：conversationId → 待进输入框 @标签 的节点 id（不立即建边，发送时自动连线） */
  pendingMentionsByConv: Record<string, string[]>;
  /** 全局错误提示（如未配置 AI provider） */
  error: string | null;
  loading: boolean;
  saving: boolean;
  /** 是否有未保存变更（watcher 判断能否安全自动重载的依据）。 */
  dirty: boolean;
  /** 本地未保存改动与外部修改/磁盘版本冲突（提示用户重载，不自动覆盖或丢弃）。 */
  conflictPending: boolean;
  /** 乐观并发基准：加载时的磁盘 updatedAt，保存时透传 Rust 检查。 */
  baseUpdatedAt: number;
  load: (id: string) => Promise<void>;
  /** 更新画布标题 */
  renameCanvas: (title: string) => Promise<void>;
  onNodesChange: (changes: NodeChange[]) => void;
  onEdgesChange: (changes: EdgeChange[]) => void;
  onConnect: OnConnect;
  /** 节点开始拖拽时保存快照（组节点同时快照组内成员，拖动时联动平移） */
  onNodeDragStart: (event: unknown, node: Node) => void;
  /** 复制选中节点到内部剪贴板（深拷贝 data，不带边；多选全复制）。无选中返回 false。 */
  copySelectedNodes: () => boolean;
  /** 粘贴剪贴板节点到目标位置（整体中心对齐，重新生成 id；空剪贴板 no-op，入 undo 栈）。 */
  pasteNodes: (position: { x: number; y: number }) => void;
  /** 节点拖动结束：重算相连边的锚点（位置自适应）。 */
  onNodeDragStop: (event: unknown, node: Node) => void;
  /** 添加节点到画布。 */
  addNode: (node: Node) => void;
  /**
   * 从仓库 `.md` 文件建文本节点（文件面板拖拽）：读正文填 bodyMd，
   * file 引用该文件（不复制）。findFreeSpot 避让已有节点。
   */
  addTextNoteFromVault: (
    file: string,
    title: string,
    position: { x: number; y: number },
    exact?: boolean,
  ) => Promise<void>;
  /**
   * 从仓库附件建媒体节点（文件面板拖拽）：图片读 dataURL 设 thumb，文本类读内容设 body，
   * 解析失败标 parseFailed。file 引用该文件（不复制）。
   */
  addMediaFromVault: (
    file: string,
    name: string,
    position: { x: number; y: number },
    exact?: boolean,
  ) => Promise<void>;
  /**
   * 从仓库 `.atb` 表格建表格节点（文件面板拖拽）：读文件填快照 snapshot，
   * file 引用该文件（不复制）。findFreeSpot 避让已有节点。
   */
  addTableFromVault: (
    file: string,
    title: string,
    position: { x: number; y: number },
    exact?: boolean,
  ) => Promise<void>;
  /** 查找引用指定 `.md` 文件的文本节点 id（双击定位用，无则 null）。 */
  findTextNoteByFile: (file: string) => string | null;
  /** 添加边到画布。 */
  addEdge: (edge: Edge) => void;
  /** 拖线引用：不立即建边，进输入框 @标签 队列（发送时自动连线）。 */
  queueMention: (conversationId: string, nodeId: string) => void;
  /** 清空某对话的拖线引用队列（输入框消费后）。 */
  clearPendingMentions: (conversationId: string) => void;
  /** 属性面板选中的节点 id（单击节点设置、单击空白清空；跨面板共享，null = 未选中）。 */
  selectedNodeId: string | null;
  /** 设置属性面板选中节点（null = 清空选中）。 */
  selectNode: (nodeId: string | null) => void;
  /** 更新节点 data（模型切换等内容变更，自动落库）。 */
  updateNodeData: (nodeId: string, patch: Record<string, unknown>) => void;
  // ===== 多人实时协作（presence + canvas-patch 补丁）=====
  /** 本端独占编辑中的对话节点（convId → 获取时间戳 since；锁主判定见 utils/canvasCollab）。 */
  lockedConversations: Record<string, number>;
  /** 对话节点独占编辑锁：首次真实输入时获取（幂等，已有锁不刷新 since）。 */
  acquireConversationLock: (conversationId: string) => void;
  /** 释放独占编辑锁（失焦无输入/发送完成/节点删除；幂等）。 */
  releaseConversationLock: (conversationId: string) => void;
  /** 清空全部独占编辑锁（协作关闭/退出时调用，防陈旧锁声明残留）。 */
  clearConversationLocks: () => void;
  /** 协作实时广播钩子注入（collabStore init 时设置；null = 协作未启用，不广播）。 */
  setCollabBroadcast: (fn: ((file: string, patch: CanvasPatch) => void) | null) => void;
  /** 应用远端画布补丁（relay `canvas-patch`）：按 id LWW 合并，不置脏/不入撤销栈/不触发保存。 */
  applyRemoteCanvasPatch: (file: string, patch: CanvasPatch) => void;
  /** Rust 侧改过当前画布磁盘 .atlx 后同步乐观锁基准（重命名笔记/附件/画布），防下次保存被误判「已被外部修改」。 */
  syncBaseUpdatedAt: () => Promise<void>;
  /** 发送消息到指定对话节点，可携带待发送附件与 @提及；fileMentions = 纯路径引用（仓库文件/文件夹，画布无对应节点）。 */
  send: (
    conversationId: string,
    content: string,
    attachments?: PendingAttachment[],
    mentions?: Mention[],
    fileMentions?: { file: string; label: string }[],
  ) => Promise<void>;
  /** 重新生成该对话的最后一条 AI 回复（重发最后一条 user 消息，不追加新消息）。 */
  regenerate: (conversationId: string) => Promise<void>;
  /**
   * 「回到此处」：截断该消息（含）之后的全部消息，在该处继续对话。
   * 入 undo 栈可撤销（恢复被截断的消息）；已是最末消息则 no-op。
   */
  rollbackTo: (conversationId: string, messageId: string) => void;
  /**
   * 从父对话分支创建新对话节点：复制 upToMessageId（含）之前的全部完整消息到子节点，
   * 连一条父→子的有向边（仅表分支血缘，不注入数据），单次 undo 事务，快照语义。
   */
  branchFrom: (
    conversationId: string,
    upToMessageId: string,
    position: { x: number; y: number },
  ) => Promise<void>;
  /** 中止某对话节点的流式回复。 */
  abort: (conversationId: string) => void;
  /** 清除全局错误。 */
  clearError: () => void;
  /** 删除所有选中的节点及关联的边（Delete/Backspace 快捷键）。 */
  deleteSelected: () => void;
  /** 删除指定节点（连带其边/流/消息；Delete 键与插件 canvas 能力共用；extraEdgeIds 为额外删除的边）。 */
  deleteNodes: (ids: string[], extraEdgeIds?: string[]) => void;
  /** 获取某对话节点入边引用的文本/搜索节点（send 时一次性固化注入）。 */
  getReferencedInputs: (conversationId: string) => ReferencedInput[];
  undo: () => void;
  redo: () => void;
  /** 保存当前快照到 undo 栈（清空 redo 栈）。内部方法，外部也可调用。 */
  pushUndo: () => void;
  // ===== watcher：外部编辑实时同步 + 断链降级 =====
  /** 查找引用某附件路径的 media 节点 id（对称 findTextNoteByFile，watcher 定位用）。 */
  findMediaNoteByFile: (file: string) => string | null;
  /**
   * silent 刷新 text 节点正文（重读 `.md`，不 persist 避免回环）。
   * 读失败（文件被删/重命名）→ markFileMissing。无引用节点时 no-op。
   */
  refreshTextContent: (file: string) => Promise<void>;
  /**
   * silent 刷新 media 节点（图片重读 dataURL、文本类重读 body，不 persist）。
   * 读失败 → markFileMissing。
   */
  refreshMediaContent: (file: string) => Promise<void>;
  /**
   * silent 刷新 table 节点快照（重读 `.atb`，不 persist 避免回环）。
   * 读失败 → markFileMissing。无引用节点时 no-op。
   * opts.snapshot：调用方已持有最新内容（打开表格自写回波用内存字段/行构建）→ 免整表读盘。
   */
  refreshTableContent: (file: string, opts?: { snapshot?: string }) => Promise<void>;
  /** 标记某 file 引用缺失（删除/重命名事件用，不删节点保留位置与边）。 */
  markFileMissing: (file: string, kind: "text" | "media" | "table") => void;
  /** 重载当前画布（外部修改自动重载时调用，读磁盘最新内容）。 */
  reloadFromDisk: () => Promise<void>;
  /** 立即落盘当前画布（切画布/切仓库/关窗前调用，防 debounce 窗口内丢改动；无脏不写）。 */
  flush: () => Promise<boolean>;
  /** 冲突合并：以磁盘最新为基底，保留本地新增节点/边/消息（重叠以磁盘为准），合并后落盘。 */
  mergeFromDisk: () => Promise<void>;
  /** 清空当前画布运行时状态（删除当前画布时调用：取消保存定时器 + 中止流 + 复位全部画布态）。 */
  resetCanvasState: () => void;
  /** 读取画布历史版本列表（缺失/损坏 → 空数组，尽力而为）。 */
  canvasHistoryLoad: (file: string) => Promise<HistoryVersion[]>;
  /** 回滚画布到指定版本：写回快照 + 重载内存 + 记 restore 版本；成功返回快照内容，失败返回 null。 */
  canvasHistoryRollback: (file: string, seq: number) => Promise<string | null>;
  /** 重新执行搜索结果节点的搜索（失败降级重试）。 */
  retrySearch: (nodeId: string, query: string) => Promise<void>;
}

// ===== Undo/Redo 栈（快照 nodes/edges/messagesByConv，深 50）=====
// 通用快照栈（画布/表格共用工具）；快照/应用经 store 运行时状态读写。
const undoMgr = createUndoManager<Snapshot>({
  snapshot: () => {
    const { nodes, edges, messagesByConv } = useCanvasStore.getState();
    // 不可变更新保证引用即快照（零拷贝）：store 每次变更生成新数组/新消息对象，
    // 历史引用不会被污染——深拷贝只会拖慢每次撤销/入栈（大画布 + 长对话尤甚）
    return { nodes, edges, messagesByConv };
  },
  apply: (entry) =>
    useCanvasStore.setState({
      nodes: entry.nodes,
      edges: entry.edges,
      messagesByConv: entry.messagesByConv,
    }),
});

// ===== 自写回放抑制（watcher）=====
// 写盘/CRUD 完成时按「文件路径」记录时刻；watcher 收到同路径事件且在抑制窗口内 → 视为自写回放，
// 不弹「已被外部修改」误提示。重命名/移动类操作经 Rust 扫盘改写多个 .atlx（前端不知全集），
// 用全局标记兜底。.md/附件事件不抑制（刷新幂等，silent 更新不 persist）。
// 实现见 utils/selfSave.ts（画布/表格/面板/配置共用，防职责挂靠本 store 造成跨 store 反向依赖）。

/** 非 pushUndo 的数据变更后调用：作废 redo 栈。
 * 标准撤销语义——undo 后产生任何新变更，Ctrl+Y 不得再恢复旧快照（否则新消息会被 redo 从内存与磁盘抹掉）。 */
function touchRedo(): void {
  undoMgr.touchRedo();
}

/**
 * 增量保存基线快照：上次落盘时的状态引用。
 * 利用 store 不可变更新——未变实体引用不变，保存时与快照按引用 diff，只序列化变化实体。
 * 基线只在 load（=磁盘内容）与保存成功后同步；撤销/重做/合并不同步——基线恒为「已写盘状态」，
 * 撤销产生的差异经 diff 自然落盘（回退到恰等于已写盘状态时空补丁自动跳过写盘）。
 */
let lastSavedNodes: Node[] = [];
let lastSavedEdges: Edge[] = [];
let lastSavedMessages: Record<string, Message[]> = {};
let lastSavedTitle = "";

/** 把当前运行时状态引用记为「已落盘基线」（load/保存成功后调用）。 */
function syncLastSaved(): void {
  const s = useCanvasStore.getState();
  lastSavedNodes = s.nodes;
  lastSavedEdges = s.edges;
  lastSavedMessages = s.messagesByConv;
  lastSavedTitle = s.canvasTitle;
  syncBroadcastBaseline();
}

/** 协作广播基线：房间已看到的最新状态（本端已广播 + 已从远端应用）。
 * 与落盘基线分离——远端已应用内容对房间是已知的，不得随本地增量重发（否则广播补丁持续膨胀）；
 * 无远端补丁时恒等于落盘基线（无回归）。 */
let broadcastBaselineNodes: Node[] = [];
let broadcastBaselineEdges: Edge[] = [];
let broadcastBaselineMessages: Record<string, Message[]> = {};
let broadcastBaselineTitle = "";

/** 把当前运行时状态引用记为「协作广播基线」。应用远端补丁/冲突合并改写内存态后调用。 */
function syncBroadcastBaseline(): void {
  const s = useCanvasStore.getState();
  broadcastBaselineNodes = s.nodes;
  broadcastBaselineEdges = s.edges;
  broadcastBaselineMessages = s.messagesByConv;
  broadcastBaselineTitle = s.canvasTitle;
}

/** 协作实时广播钩子（collabStore.init 注入，dispose 清空；null = 协作未启用，不广播）。 */
let collabBroadcast: ((file: string, patch: CanvasPatch) => void) | null = null;

/**
 * 协作锁主判定（本端视角，读最新 store 状态）：conversation 节点是否被其他对端独占编辑。
 * 用于 store 层内容写守卫（updateNodeData/send/regenerate/deleteSelected）——UI 只读条覆盖
 * 常规路径，此为极窄竞态下的兜底（防两对端在锁传播窗口内同时写入同一节点）。
 * 收集与判定统一走 resolveLockState（与 useNodeCollab / ConversationNode 三处同源）。
 */
function isConversationLockedByPeer(conversationId: string): boolean {
  const { lockedConversations } = useCanvasStore.getState();
  const { peers, myPeerId } = useCollabStore.getState();
  const { owner, lockedByMe } = resolveLockState(
    conversationId,
    lockedConversations[conversationId],
    myPeerId,
    peers,
  );
  return owner !== null && !lockedByMe;
}

/** 协作对端是否同画布在线（presence.file 命中当前画布 + view=canvas）：共享盘保存竞争时据此
 * 自动三方合并（与表格 hasCollabPeerOnTable 同策略）；无对端 = 外部编辑 → 保持冲突条；
 * 亦用于 watcher 判别「磁盘写入是对端保存的广播回放（内容已应用，不得重载破坏运行态）」。 */
export function hasCollabPeerOnCanvas(file: string): boolean {
  return useCollabStore
    .getState()
    .peers.some((p) => p.presence?.file === file && p.presence?.view === "canvas");
}

/** 画布域协作接线（builtin.canvas 载荷调用，随插件启停）：注册 canvas-patch 通道 handler、
 *  presence 锁/流式合并 provider、重连补发画布 presence、presence 订阅、拆卸清理与广播钩子
 *  注入（经 collabSendSink 惰性读宿主 handle：断开时 no-op、重连后自动指向新连接）；
 *  返回撤销函数（停用/卸载时撤销注册 + 释放本端锁 + 复位广播钩子）。 */
export function registerCanvasCollabWiring(): () => void {
  const offs: Array<() => void> = [];
  offs.push(
    registerCollabChannel("canvas-patch", (peerId, file, patch) => {
      // 只应用当前打开的画布（applyRemoteCanvasPatch 内部按 file + id 守卫）；跳过自己
      if (peerId === useCollabStore.getState().myPeerId) return;
      useCanvasStore.getState().applyRemoteCanvasPatch(file, patch as CanvasPatch);
    }),
  );
  // 画布锁/流式跨视图保活：无论当前 view 槽（table/note/canvas）为何，都合并 canvas 的
  // 独占编辑锁与生成中节点——用户在看表格/笔记期间其画布锁仍对端可见，对话节点持续只读
  offs.push(
    registerCollabPresenceProvider((base) => {
      const cs = useCanvasStore.getState();
      const lockedNodes = Object.entries(cs.lockedConversations).map(([id, since]) => ({ id, since }));
      const streamingNodeIds = Object.entries(cs.streamingByConv)
        .filter(([, v]) => v)
        .map(([id]) => id);
      return {
        ...base,
        ...(lockedNodes.length ? { lockedNodes } : {}),
        ...(streamingNodeIds.length ? { streamingNodeIds } : {}),
      };
    }),
  );
  // 重连后补发画布 presence（priority 高于表格域——画布最后补发覆盖表格槽；画布为主工作区，
  // 锁/流式经 provider 合并，恢复对端锁）
  offs.push(
    registerCollabReconnect(
      () => {
        const cs = useCanvasStore.getState();
        if (!cs.canvasFile) return;
        publishCollabPresence({
          file: cs.canvasFile,
          selection: cs.selectedNodeId ? { kind: "node", nodeId: cs.selectedNodeId } : null,
          view: "canvas",
        });
      },
      1,
    ),
  );
  // 画布 presence 订阅：打开/切画布、选中节点、独占编辑锁、流式起止任一变化 → 广播
  // messagesByConv 逐 token 更新不在此订阅 → 流式 token 不刷屏 presence（只有流式起止变更）
  offs.push(
    useCanvasStore.subscribe((s, prev) => {
      const changed =
        s.canvasFile !== prev.canvasFile ||
        s.selectedNodeId !== prev.selectedNodeId ||
        s.lockedConversations !== prev.lockedConversations ||
        s.streamingByConv !== prev.streamingByConv;
      if (!changed) return;
      publishCollabPresence({
        file: s.canvasFile,
        selection: s.selectedNodeId ? { kind: "node", nodeId: s.selectedNodeId } : null,
        view: "canvas",
      });
    }),
  );
  // 广播钩子注入（schedulePersist 计算补丁后回调；宿主 handle 为模块级，重连自动生效）
  useCanvasStore.getState().setCollabBroadcast((file, patch) => {
    collabSendSink("canvas-patch")(file, patch);
  });
  offs.push(() => useCanvasStore.getState().setCollabBroadcast(null));
  // 拆卸：释放本端画布独占编辑锁（协作关闭后锁声明不再对端可见，内存清空防陈旧）
  offs.push(registerCollabTeardown(() => useCanvasStore.getState().clearConversationLocks()));
  // 撤销 = 画布退出协作：同样释放本端锁（与 collab 关闭的拆卸路径幂等重叠）
  offs.push(() => useCanvasStore.getState().clearConversationLocks());
  return () => {
    for (const off of offs) off();
  };
}

/** 画布插件能力接线（builtin.canvas 载荷调用，随插件启停）：注册 worker 平面 `canvas` 命名空间
 *  数据源 + `canvas:changed` 事件发射；返回撤销函数（停用/卸载时撤销，能力随之消失）。
 *  投影复用协作/磁盘序列化纯函数（serializeNodeForCollab 内嵌对话消息、剥离 React Flow 视图态，JSON 安全）；
 *  写方法守卫「已打开可写画布」；变更事件为轻量信号（只带 file，插件按需再调 snapshot()）——
 *  画布拖拽/流式是每帧高频变更，全量序列化快照会造成事件风暴与陈旧大载荷。 */
export function registerCanvasPluginWiring(): () => void {
  const requireWritable = (): void => {
    const s = useCanvasStore.getState();
    if (!s.canvasFile) throw new Error("未打开画布");
    if (s.readOnly) throw new Error("画布为只读");
  };
  setPluginCanvasAccess({
    snapshot: () => {
      const s = useCanvasStore.getState();
      const nodes: PluginCanvasNode[] = s.nodes.map((n) => {
        const ser = serializeNodeForCollab(n, s.messagesByConv);
        return {
          id: ser.id,
          type: ser.type,
          x: ser.x,
          y: ser.y,
          width: ser.width,
          height: ser.height,
          data: ser.data as unknown as Record<string, unknown>,
        };
      });
      const edges: PluginCanvasEdge[] = s.edges.map((e) => {
        const ser = serializeEdgeForCollab(e);
        return {
          id: ser.id,
          source: ser.source,
          target: ser.target,
          sourceHandle: ser.sourceHandle,
          targetHandle: ser.targetHandle,
          directed: ser.directed,
          linkMode: ser.linkMode,
        };
      });
      return {
        canvasFile: s.canvasFile,
        canvasTitle: s.canvasTitle,
        nodes,
        edges,
        selectedNodeId: s.selectedNodeId,
      };
    },
    addNode: (node) => {
      requireWritable();
      const id = crypto.randomUUID();
      useCanvasStore.getState().addNode({ id, type: node.type, position: node.position, data: node.data ?? {} });
      return id;
    },
    updateNode: (nodeId, patch) => {
      requireWritable();
      useCanvasStore.getState().updateNodeData(nodeId, patch);
    },
    moveNode: (nodeId, position) => {
      requireWritable();
      const st = useCanvasStore.getState();
      // position 变更经 onNodesChange 应用（自身不入 undo 栈），显式 pushUndo 使其可撤销
      st.pushUndo();
      st.onNodesChange([{ type: "position", id: nodeId, position, dragging: false }]);
    },
    deleteNode: (nodeId) => {
      requireWritable();
      useCanvasStore.getState().deleteNodes([nodeId]);
    },
    addEdge: (edge) => {
      requireWritable();
      const st = useCanvasStore.getState();
      const id = crypto.randomUUID();
      st.addEdge({ id, ...edge });
      return id;
    },
    deleteEdge: (edgeId) => {
      requireWritable();
      const st = useCanvasStore.getState();
      st.pushUndo();
      st.onEdgesChange([{ type: "remove", id: edgeId }]);
    },
    selectNode: (nodeId) => {
      useCanvasStore.getState().selectNode(nodeId);
    },
  });
  const offChange = useCanvasStore.subscribe((s, prev) => {
    if (
      s.canvasFile !== prev.canvasFile ||
      s.canvasTitle !== prev.canvasTitle ||
      s.nodes !== prev.nodes ||
      s.edges !== prev.edges ||
      s.messagesByConv !== prev.messagesByConv ||
      s.selectedNodeId !== prev.selectedNodeId
    ) {
      emitPluginEvent("canvas:changed", { file: s.canvasFile });
    }
  });
  return () => {
    setPluginCanvasAccess(null);
    offChange();
  };
}

/**
 * 三方合并：磁盘为基底 + 本地独有节点/边 + 本地独有消息按 id 补入（重叠同 id 以磁盘为准，
 * 防静默覆盖外部修改）。mergeFromDisk（手动）与协作自动合并（handleSaveConflict）共用。
 */
function mergeCanvasWithDisk(
  disk: RuntimeCanvas,
  localNodes: Node[],
  localEdges: CanvasEdge[],
  localMessages: Record<string, Message[]>,
): { nodes: Node[]; edges: CanvasEdge[]; messagesByConv: Record<string, Message[]> } {
  const diskNodeIds = new Set(disk.nodes.map((n) => n.id));
  const diskEdgeIds = new Set(disk.edges.map((e) => e.id));
  const nodes = [...disk.nodes, ...localNodes.filter((n) => !diskNodeIds.has(n.id))];
  const edges = [...disk.edges, ...localEdges.filter((e) => !diskEdgeIds.has(e.id))];
  const messagesByConv: Record<string, Message[]> = { ...disk.messagesByConv };
  for (const [convId, msgs] of Object.entries(localMessages)) {
    // mergeMessages = 远端（磁盘）为基底 + 本地独有消息按 id 补入，与协作消息合并同源
    const merged = mergeMessages(disk.messagesByConv[convId] ?? [], msgs);
    if (merged.length) messagesByConv[convId] = merged;
  }
  return { nodes, edges, messagesByConv };
}

/**
 * 磁盘基底合并共享骨架（协作自动合并 handleSaveConflict / 手动合并 mergeFromDisk 共用）：
 * 读盘 → 竞态守卫 → 磁盘基底入内存 + 同步落盘/广播基线 → 本地独有实体按 id 补入 → 防抖落盘。
 * - guardBasis：入口捕获的画布身份，await 读盘后校验未切换/未清空即放弃（旧画布的磁盘内容
 *   不得写进新画布，与 persistNow.finish 的守卫同策略——否则 local 已是新画布、data 是旧画布，
 *   交叉污染）；null = 调用方自行保证读盘窗口内画布不变，不做守卫。
 * - captureLocal：本地态捕获时机由调用方定义——自动合并取读盘后的最新内存态（缩小 await
 *   窗口内新编辑丢失的竞态窗口）；手动合并取入口态（先中止流再读盘，流回调不会再写内存）。
 * - clearUndo：整体替换消息状态的路径须清撤销栈（栈内旧消息快照不再适用）。
 */
async function mergeDiskIntoMemory(opts: {
  canvasFile: string;
  guardBasis: { canvasFile: string; canvasId: string | null } | null;
  clearUndo: boolean;
  resetLoading: boolean;
  captureLocal: () => {
    nodes: Node[];
    edges: CanvasEdge[];
    messagesByConv: Record<string, Message[]>;
  };
  onError: (e: unknown) => void;
}): Promise<void> {
  try {
    const data = await loadCanvasVault(opts.canvasFile);
    if (opts.guardBasis) {
      const cur = useCanvasStore.getState();
      if (
        cur.canvasFile !== opts.guardBasis.canvasFile ||
        cur.canvasId !== opts.guardBasis.canvasId
      ) {
        return;
      }
    }
    const local = opts.captureLocal();
    // 1) 以磁盘为基底入内存 + 落盘/广播基线（后续合并补丁只含本地独有实体，引用 diff）
    useCanvasStore.setState({
      canvasId: data.id,
      canvasTitle: data.title,
      nodes: data.nodes,
      edges: data.edges,
      messagesByConv: data.messagesByConv,
      baseUpdatedAt: data.updatedAt,
      conflictPending: false,
      dirty: false,
      ...(opts.resetLoading ? { loading: false } : {}),
    });
    if (opts.clearUndo) undoMgr.clear();
    syncLastSaved();
    // 2) 合并本地独有实体回内存（基线仍为磁盘基底 → diff 非空，本地改动不丢）
    const merged = mergeCanvasWithDisk(data, local.nodes, local.edges, local.messagesByConv);
    useCanvasStore.setState({
      nodes: merged.nodes,
      edges: merged.edges,
      messagesByConv: merged.messagesByConv,
      conflictPending: false,
      dirty: true,
    });
    // 3) 合并产物随下一轮防抖落盘（base 已同步为新磁盘版本，不再误冲突）
    schedulePersist();
  } catch (e) {
    opts.onError(e);
  }
}

/**
 * 乐观锁冲突处理：协作对端同画布在场 → 自动三方合并收敛（共享盘多人保存竞争，避免冲突条
 * 满天飞，与表格 retryMergePersist 同策略）；否则弹冲突条（外部编辑，保持原行为不静默覆盖）。
 * 自动合并不中止进行中的流（本地消息按 id 补入保留，流续写照常）；合并产物随下一轮防抖落盘。
 */
async function handleSaveConflict(): Promise<void> {
  const { canvasFile, canvasId } = useCanvasStore.getState();
  if (!canvasFile || !hasCollabPeerOnCanvas(canvasFile)) {
    useCanvasStore.setState({ conflictPending: true });
    return;
  }
  await mergeDiskIntoMemory({
    canvasFile,
    guardBasis: { canvasFile, canvasId },
    clearUndo: false,
    resetLoading: false,
    // 读盘完成后立刻快照本地（缩小 await 窗口内新编辑丢失的竞态窗口）
    captureLocal: () => {
      const cur = useCanvasStore.getState();
      return { nodes: cur.nodes, edges: cur.edges, messagesByConv: cur.messagesByConv };
    },
    onError: (e) => {
      console.error("协作自动合并失败", e);
      useCanvasStore.setState({ conflictPending: true });
    },
  });
}

/**
 * 记录画布历史版本（保存成功后的存档点）：以内存运行时内容构建 `.atlx` 格式快照 → 记一条 edit
 * 版本（60s 内连续编辑合并为一版，不逐键）。快照取内存且**纯序列化**（`serializeCanvasSnapshot`
 * 不写 `.md`——若用 toFileNode 路径，延迟执行时可能把旧正文写回共享盘覆盖新编辑；历史尽力而为，
 * 不阻塞保存流程）。
 */
function recordCanvasHistory(file: string): void {
  const s = useCanvasStore.getState();
  if (s.canvasFile !== file || !s.canvasId) return; // 切画布/未打开竞态：只记当前画布
  const { canvasId, canvasTitle, nodes, edges, messagesByConv } = s;
  const content = JSON.stringify(
    serializeCanvasSnapshot(canvasId, canvasTitle, nodes, edges, messagesByConv),
  );
  void recordHistoryVersion("canvas", file, {
    content,
    action: "edit",
    coalesceEditMs: 60_000,
    // 人话摘要：节点/连线增删改 · 对话消息增减（列表可读，替代 JSON 行级 diff）
    summarize: summarizeCanvasSnapshot,
  });
}

/**
 * 立即持久化当前画布（flush 与保存 timer 共用；timer 回调里读最新 state，
 * 确保流式完成时 onDone 触发的保存拿到最终消息内容）。
 * 增量保存：与 lastSaved 快照按引用 diff，只写变化实体（见 patchCanvasVault）；
 * 空补丁（撤销回退到已存状态等）跳过 IPC 仅清脏标志。
 * 写盘期间若又有新变更（persistCtl.version 已变）则保留 dirty，由下一轮 timer 再写。
 */
async function persistNow(): Promise<void> {
  const versionAtStart = persistCtl.version;
  const {
    canvasId,
    canvasFile,
    canvasTitle,
    nodes,
    edges,
    messagesByConv,
    baseUpdatedAt,
  } = useCanvasStore.getState();
  if (!canvasId || !canvasFile) return;
  // 写盘成功后的统一收尾：先抑制回放（watcher 同路径事件 2s 窗口），再同步新路径/
  // 乐观锁基准/快照/脏标志。updatedAt = null 表示空补丁（磁盘未动，无需标记自写）。
  const finish = (updatedAt: number | null, newFile?: string) => {
    if (updatedAt !== null) {
      markSelfSave(
        newFile && newFile !== canvasFile ? [canvasFile, newFile] : canvasFile,
      );
    }
    // 竞态守卫：await 期间可能已切换画布/清空状态（load 异步读盘），旧画布的写盘结果
    // 不得覆盖新画布的乐观锁基准/脏标记（否则新画布下次保存被误判冲突、脏编辑被吞）
    const cur = useCanvasStore.getState();
    if (cur.canvasId !== canvasId || cur.canvasFile !== canvasFile) return;
    if (persistCtl.version !== versionAtStart) {
      // 写盘期间有新变更（已挂新 timer）：保留 dirty，由下一轮 timer 再写盘，防本次成功吞掉新编辑；
      // 不推进快照/基准——下一轮 diff 仍以旧快照为基线（已写盘部分重发同内容 upsert，幂等）
      useCanvasStore.setState({ saving: false });
      return;
    }
    if (newFile && newFile !== canvasFile) {
      // title 变更导致路径漂移：同步 canvasFile + appStore.currentCanvasFile（同源）
      useCanvasStore.setState({ canvasFile: newFile });
      if (useAppStore.getState().currentCanvasFile === canvasFile) {
        useAppStore.setState({ currentCanvasFile: newFile });
      }
    }
    if (updatedAt !== null) {
      // 同步乐观锁基准为本次写入的磁盘版本，避免下次保存误判冲突
      useCanvasStore.setState({ baseUpdatedAt: updatedAt });
    }
    useCanvasStore.setState({ error: null, dirty: false, saving: false });
    // 存档点：以当前内容快照记历史（60s 内连续编辑合并为一版；fire-and-forget 不阻塞保存流程）
    if (updatedAt !== null) recordCanvasHistory(newFile ?? canvasFile);
    syncLastSaved();
  };
  const reportError = (e: unknown) => {
    useCanvasStore.setState({ saving: false });
    if (typeof e === "string" && e.includes("已被外部修改")) {
      // 乐观锁冲突：协作对端同画布在场 → 自动三方合并（共享盘保存竞争，见 handleSaveConflict）；
      // 否则不覆盖磁盘，提示用户重载（本地改动保留在内存供查看）
      void handleSaveConflict();
    } else {
      console.error("自动保存失败", e);
      useCanvasStore.setState({ error: "自动保存失败，请检查磁盘空间或权限" });
    }
  };
  try {
    const result = await patchCanvasVault({
      file: canvasFile,
      canvasId,
      title: canvasTitle,
      nodes,
      edges,
      messagesByConv,
      lastSaved: {
        nodes: lastSavedNodes,
        edges: lastSavedEdges,
        messagesByConv: lastSavedMessages,
        title: lastSavedTitle,
      },
      baseUpdatedAt,
    });
    finish(result ? result.updatedAt : null, result?.file);
  } catch (e) {
    if (typeof e === "string" && e.includes("画布文件不存在（已从磁盘删除）")) {
      // 磁盘文件被外部删除：补丁只含变化实体，重建会丢未变化部分——回退全量写（与旧行为一致）
      try {
        const newUpdatedAt = await persistCanvasVault(
          canvasId,
          canvasFile,
          canvasTitle,
          nodes,
          edges,
          messagesByConv,
          baseUpdatedAt,
        );
        finish(newUpdatedAt);
      } catch (e2) {
        reportError(e2);
      }
    } else {
      reportError(e);
    }
  }
}

/** 防抖持久化控制器：timer 管理 + 代数防吞统一在此（各 store 共用工具）。 */
const persistCtl = createPersistController({
  persist: persistNow,
  beforeSchedule: () => useCanvasStore.setState({ saving: true, dirty: true }),
});

/**
 * debounce 500ms 持久化画布到仓库（增量补丁：只写变化实体，text 正文写 .md）。
 * 同时协作实时广播：编辑即达（不等防抖落盘）。补丁 = 与广播基线的引用 diff（幂等 LWW，
 * 与磁盘合并语义一致，顺序随节点/消息数组携带）。广播基线独立于落盘基线——远端已应用内容
 * 推进广播基线后不再被重发全房；applyRemoteCanvasPatch 路径不调本函数 → 无广播回环。
 * 节点拖动/resize 期间 schedulePersist 已被调用方门控（dragStop 才调），此处不做二次判断。
 */
function schedulePersist() {
  const st = useCanvasStore.getState();
  if (st.readOnly || !st.canvasId || !st.canvasFile) return;
  if (collabBroadcast) {
    const patch = computeCanvasCollabPatch({
      canvasId: st.canvasId,
      title: st.canvasTitle,
      nodes: st.nodes,
      edges: st.edges,
      messagesByConv: st.messagesByConv,
      lastSaved: {
        nodes: broadcastBaselineNodes,
        edges: broadcastBaselineEdges,
        messagesByConv: broadcastBaselineMessages,
        title: broadcastBaselineTitle,
      },
    });
    if (patch) collabBroadcast(st.canvasFile, patch);
  }
  persistCtl.schedule();
}

/** 边锚点自适应：按两节点中心相对方位重写 handle（与 ConnectionFrame 命名对齐）。 */
function withHandles<
  T extends {
    source: string;
    target: string;
    sourceHandle?: string | null;
    targetHandle?: string | null;
  },
>(edge: T, nodes: Node[]): T {
  const src = nodes.find((n) => n.id === edge.source);
  const tgt = nodes.find((n) => n.id === edge.target);
  if (!src || !tgt) return edge;
  const { sourceHandle, targetHandle } = pickEdgeHandles(
    rectOf(src),
    rectOf(tgt),
  );
  return { ...edge, sourceHandle, targetHandle };
}

/** 重算与指定节点相连的边锚点；有变化返回新 edges，无变化返回 null（避免无谓 set）。 */
function recalcEdgeHandles(
  edges: Edge[],
  nodes: Node[],
  nodeIds: Set<string>,
): Edge[] | null {
  const next = edges.map((e) => {
    if (!nodeIds.has(e.source) && !nodeIds.has(e.target)) return e;
    return withHandles(e, nodes);
  });
  return next.some(
    (e, i) =>
      e.sourceHandle !== edges[i].sourceHandle ||
      e.targetHandle !== edges[i].targetHandle,
  )
    ? next
    : null;
}

/** 各对话节点的 AbortController，模块级。 */
const abortControllers = new Map<string, AbortController>();

/**
 * 中止全部进行中的流（切画布/删节点/undo 用）。
 * abort() 后 streamChat 走 onDone（异步），其内部 cancelRaf + 清理 streamingByConv；
 * 此处同步清空 map/流式标志，避免 abort 与 load 的 set 之间的渲染残留。
 */
function abortAllStreams() {
  for (const controller of abortControllers.values()) controller.abort();
  abortControllers.clear();
}

/**
 * AI 产物节点公共脚手架（搜索节点/写笔记节点共用）：对话右侧 findFreeSpot 落点 +
 * 对话→产物边 + 静默落盘。
 * 走 silent set（不 pushUndo）：工具产物属业务操作（同 send 的影子节点），
 * 入 undo 栈会与 tool 回填消息撕裂（撤销节点但消息仍在）；但作废 redo：undo 后产物不得被 Ctrl+Y 抹除。
 * 返回新节点（调用方如需可继续填充，如写笔记后读正文回填）。
 */
function createProductNodeAfter(
  conversationId: string,
  makeNode: (id: string, spot: { x: number; y: number }) => Node,
  size: { w: number; h: number },
): Node | null {
  const store = useCanvasStore.getState();
  const convNode = store.nodes.find((n) => n.id === conversationId);
  if (!convNode) return null;
  touchRedo();
  const id = crypto.randomUUID();
  const spot = findFreeSpot(
    store.nodes,
    { x: convNode.position.x + 480, y: convNode.position.y },
    size,
  );
  const node = makeNode(id, spot);
  // withHandles 传入含新节点的列表：target（产物节点）能找到，边锚点自适应
  const nextNodes = [...store.nodes, node];
  const edge = withHandles(
    {
      id: crypto.randomUUID(),
      source: conversationId,
      target: id,
      sourceHandle: null,
      targetHandle: null,
    },
    nextNodes,
  );
  useCanvasStore.setState((s) => ({
    nodes: [...s.nodes, node],
    edges: [...s.edges, edge],
  }));
  schedulePersist();
  return node;
}

/** 搜索结果产物节点：对话右侧落点 + 对话→search 边（脚手架见 createProductNodeAfter）。 */
function createSearchNode(conversationId: string, query: string, data: SearchResultData) {
  createProductNodeAfter(
    conversationId,
    (id, spot) => ({
      id,
      type: "search",
      position: spot,
      data: { ...data, query },
    }),
    { w: 280, h: 220 },
  );
}

/** AI 写笔记产物节点：对话右侧落点 + 对话→text 边（笔记节点，file 引用新落盘的 .md）。 */
function createWriteNoteNode(
  conversationId: string,
  file: string,
  title: string,
) {
  const node = createProductNodeAfter(
    conversationId,
    (id, spot) => ({
      id,
      type: "text",
      position: spot,
      width: DEFAULT_TEXT_NODE_WIDTH,
      height: DEFAULT_TEXT_NODE_HEIGHT,
      data: { title, file, bodyMd: "" } as unknown as Node["data"],
    }),
    { w: DEFAULT_TEXT_NODE_WIDTH, h: DEFAULT_TEXT_NODE_HEIGHT },
  );
  if (!node) return;
  // 正文从磁盘读一次填充（刚写完的文件；失败标 fileMissing 降级，与 addTextNoteFromVault 同模式）
  readNote(file)
    .then((body) =>
      useCanvasStore.getState().updateNodeData(node.id, { bodyMd: body }),
    )
    .catch(() =>
      useCanvasStore.getState().updateNodeData(node.id, { fileMissing: true }),
    );
}

/** 节点引用的仓库路径（text/media/table 三类有 file 字段；物质化查重与 ConversationNode 的
 *  画布文件集/选择器路由共用，导出防三处各写一份类型分叉）。 */
export function nodeFileOf(n: Node): string | undefined {
  return n.type === "text"
    ? (n.data as unknown as TextData).file
    : n.type === "media"
      ? (n.data as unknown as MediaData).file
      : n.type === "table"
        ? (n.data as unknown as TableData).file
        : undefined;
}

/** 引用节点 data 构建（add*FromVault 与纯路径引用消费物质化共用）：磁盘读取失败统一降级标记，不抛错。 */
async function buildTextNoteData(file: string, title: string) {
  let bodyMd = "";
  let fileMissing = false;
  try {
    bodyMd = await readNote(file);
  } catch {
    fileMissing = true;
  }
  return { title, file, bodyMd, fileMissing };
}

async function buildMediaData(file: string, name: string): Promise<MediaData | null> {
  const isImage = /\.(png|jpe?g|webp|gif)$/i.test(name);
  if (isImage) {
    try {
      const thumb = await readAttachmentDataUrl(file);
      return { file, kind: "image", mime: inferImageMime(name), thumb, name };
    } catch (e) {
      console.error("读图片附件失败", e);
      return null;
    }
  }
  // 文本类附件：尝试读内容；二进制读失败标 parseFailed
  let body: string | undefined;
  let parseFailed = false;
  try {
    body = await readNote(file);
  } catch {
    parseFailed = true;
  }
  return { file, kind: "file", mime: "text/plain", name, body, parseFailed };
}

async function buildTableData(file: string, title: string) {
  let snapshot = "";
  let fileMissing = false;
  try {
    snapshot = tableToSnapshotText(await readTableVault(file));
  } catch {
    // 文件缺失/读取失败：标 fileMissing，TableNode 显示「文件缺失」降级
    fileMissing = true;
  }
  return { title, file, snapshot, fileMissing };
}

/** path 是否被本对话的纯路径引用覆盖（精确命中，或位于引用的目录内）。 */
function isRefCoveredPath(messages: Message[], path: string): boolean {
  return messages.some((m) =>
    (m.refs ?? []).some(
      (r) => r.file && (path === r.file || path.startsWith(`${r.file}/`)),
    ),
  );
}

/** 物质化在飞去重：同路径并发 read_file 只建一次节点（await 读盘窗口内二次触发直接跳过）。 */
const materializingPaths = new Set<string>();

/**
 * 纯路径引用的消费物质化：Agent 工具实际读取 @引用 的文件后，画布尚无对应节点 →
 * 按类型在对话左侧落引用节点（产出方位置，findFreeSpot 避让）并连 节点→对话 数据流边；
 * 同时把该对话历史中 `file:<path>` 引用改写为真实节点 id——引用即边：气泡 @chip 变为定位节点、
 * 数据流边随 isAssetConsumed 转实线。已有同文件节点只补边不重复建。
 * Agent 行为不入 Undo 栈（与工具产物节点同语义），仅作废 redo。
 */
async function materializeReferencedFile(conversationId: string, path: string): Promise<void> {
  if (materializingPaths.has(path)) return;
  materializingPaths.add(path);
  try {
    const convNode = useCanvasStore
      .getState()
      .nodes.find((n) => n.id === conversationId);
    if (!convNode) return;
    // await 读盘窗口内可能已切画布/删除对话节点：每次落节点/补边/改写前复查，防写脏新画布
    const stillOnCanvas = () =>
      useCanvasStore.getState().nodes.some((n) => n.id === conversationId);
    let nodeId = useCanvasStore.getState().nodes.find((n) => nodeFileOf(n) === path)?.id;
    if (!nodeId) {
      const lower = path.toLowerCase();
      const name = path.split("/").pop() ?? path;
      // 产出方位置：对话左侧（数据流 文件→对话），findFreeSpot 避让既有节点
      const spot = findFreeSpot(
        useCanvasStore.getState().nodes,
        { x: convNode.position.x - 310, y: convNode.position.y },
        { w: DEFAULT_TEXT_NODE_WIDTH, h: DEFAULT_TEXT_NODE_HEIGHT },
      );
      const id = crypto.randomUUID();
      if (lower.endsWith(".md")) {
        const data = await buildTextNoteData(path, noteTitleFromFile(path));
        if (!stillOnCanvas()) return;
        touchRedo();
        useCanvasStore.setState((s) => ({
          nodes: [
            ...s.nodes,
            {
              id,
              type: "text",
              position: spot,
              width: DEFAULT_TEXT_NODE_WIDTH,
              height: DEFAULT_TEXT_NODE_HEIGHT,
              data: data as unknown as Node["data"],
            },
          ],
        }));
        nodeId = id;
      } else if (lower.endsWith(".atb")) {
        const data = await buildTableData(path, tableTitleFromFile(path));
        if (!stillOnCanvas()) return;
        touchRedo();
        useCanvasStore.setState((s) => ({
          nodes: [
            ...s.nodes,
            {
              id,
              type: "table",
              position: spot,
              width: DEFAULT_TABLE_NODE_WIDTH,
              height: DEFAULT_TABLE_NODE_HEIGHT,
              data: data as unknown as Node["data"],
            },
          ],
        }));
        nodeId = id;
      } else {
        const data = await buildMediaData(path, name);
        if (!data || !stillOnCanvas()) return;
        touchRedo();
        useCanvasStore.setState((s) => ({
          nodes: [
            ...s.nodes,
            { id, type: "media", position: spot, data: data as unknown as Node["data"] },
          ],
        }));
        nodeId = id;
      }
    }
    if (!nodeId) return;
    if (!stillOnCanvas()) return;
    // 补边（引用即边；已连则跳过）+ 历史 `file:<path>` 引用改写为真实节点 id
    // （chip 定位节点 + isAssetConsumed 命中 → 边实线）。均为非入栈数据变更 → 作废 redo。
    const st = useCanvasStore.getState();
    const needEdge = !st.edges.some(
      (e) => e.source === nodeId && e.target === conversationId,
    );
    const needRewrite = (st.messagesByConv[conversationId] ?? []).some(
      (m) => m.refs?.some((r) => r.file === path && r.nodeId === `file:${path}`),
    );
    if (!needEdge && !needRewrite) return;
    touchRedo();
    if (needEdge) {
      useCanvasStore.setState((s) => ({
        edges: [
          ...s.edges,
          withHandles(
            {
              id: crypto.randomUUID(),
              source: nodeId,
              target: conversationId,
              sourceHandle: null,
              targetHandle: null,
            },
            s.nodes,
          ),
        ],
      }));
    }
    if (needRewrite) {
      useCanvasStore.setState((s) => ({
        messagesByConv: {
          ...s.messagesByConv,
          [conversationId]: (s.messagesByConv[conversationId] ?? []).map((m) =>
            m.refs?.some((r) => r.file === path && r.nodeId === `file:${path}`)
              ? {
                  ...m,
                  refs: m.refs.map((r) =>
                    r.file === path && r.nodeId === `file:${path}` ? { ...r, nodeId } : r,
                  ),
                }
              : m,
          ),
        },
      }));
    }
    schedulePersist();
  } finally {
    materializingPaths.delete(path);
  }
}

/** 画布对话节点实时查找（命名管线回调共用：延迟后/写回前重取，已删除/切画布返回 undefined）。 */
function findConversationNode(conversationId: string): Node | undefined {
  const node = useCanvasStore
    .getState()
    .nodes.find((n) => n.id === conversationId);
  return node && node.type === "conversation" ? node : undefined;
}

/**
 * 流式回调公共写路径：守卫节点已删除（迟到回调不重建 messagesByConv 键，防孤儿消息复活），
 * 定位占位 assistant 消息应用 patch；patch 返回 null = 移除该消息（空回复清理）。
 * 仅写 messagesByConv——streamingByConv 复位等状态由各回调自行 set。
 */
function patchAssistant(
  conversationId: string,
  asstId: string,
  patch: (m: Message) => Message | null,
): void {
  if (!findConversationNode(conversationId)) return;
  useCanvasStore.setState((state) => {
    const l = state.messagesByConv[conversationId] ?? [];
    return {
      messagesByConv: {
        ...state.messagesByConv,
        [conversationId]: l.flatMap((m) => {
          if (m.id !== asstId) return [m];
          const patched = patch(m);
          return patched ? [patched] : [];
        }),
      },
    };
  });
}

/**
 * LLM 话题自动命名：一轮对话完成后为对话节点生成话题标题。
 * - 仅对尚无 title 的对话节点命名（首轮完成后一次，之后不覆盖）
 * - 统一走 streaming.ts 的公共命名管线（模型解析/延迟/超时与面板共用）
 * - fire-and-forget：无可用模型/命名失败降级保留占位，不阻塞画布操作
 */
async function autoNameConversation(conversationId: string): Promise<void> {
  await runAutoNaming(
    {
      getMessages: () => {
        const node = findConversationNode(conversationId);
        if (!node) return [];
        const msgs = useCanvasStore.getState().messagesByConv[conversationId] ?? [];
        // 叙述-only 消息（content 为空、正文在 steps）回填正文，供话题命名摘要
        return msgs.map(fillAssistantReplyText);
      },
      isNamed: () => {
        const node = findConversationNode(conversationId);
        return !node || !!(node.data as Partial<ConversationData>).title;
      },
      applyTitle: (title) => {
        const node = findConversationNode(conversationId);
        if (!node) return;
        useCanvasStore.getState().updateNodeData(conversationId, { title });
      },
    },
    // key = 对话节点 id：发送新消息时只中止本节点的命名请求（不误伤其他对话）
    { key: conversationId },
  );
}

/**
 * 流式执行一轮对话：预建 assistant 消息 → 按注入语义组装 messages → SSE 流式写入。
 * send 与 regenerate 共用（流式输出/停止；引用固化进 user 消息）。
 *
 * 性能与状态正确性要点：
 * - onDelta 用 rAF 合并高频 token，避免每 token 一次 setState 卡 UI。
 * - abort/空回复：移除占位 assistant，避免残留空气泡（streamChat 在 abort 时走 onDone）。
 * - 错误：占位 assistant 写入 `[错误] …` 并随 .atlx 持久化；下次请求历史过滤此类消息，不污染上下文。
 * - 持久化：messages 嵌在对话节点 data 内，随 schedulePersist 增量补丁写 .atlx（仅变化实体）。
 */
async function runStream(conversationId: string): Promise<void> {
  const store = useCanvasStore;

  // 节点级 provider/model 优先，未指定则跟随仓库默认；解析失败（供应商已删/未配置）提示并中止，
  // 不静默回落默认——统一走 settingsStore.resolveChatTarget（与 AI 对话面板同源）
  const nodeData = store.getState().nodes.find((n) => n.id === conversationId)
    ?.data as Partial<ConversationData> | undefined;
  const resolved = useSettingsStore.getState().resolveChatTarget(
    nodeData?.providerId || nodeData?.model
      ? {
          providerId: nodeData.providerId || undefined,
          model: nodeData.model || undefined,
        }
      : null,
  );
  if (!resolved.ok) {
    store.setState({ error: resolved.error });
    return;
  }
  const { provider, model } = resolved;
  // 发送起始清上次错误（与 chatPanelStore.runExchange 对称：搜索源等已配置后不滞留旧横幅）
  store.setState({ error: null });
  // 推理等级为节点级独立覆盖（与 provider/model 正交，resolveChatTarget 不产 effort）；缺省 = 不指定（跟随默认，不下发 reasoning_effort）
  const reasoningEffort = nodeData?.reasoningEffort;

  // 预创建 assistant 消息（流式追加内容）
  const { id: asstId, ts: asstTs } = nowId();
  const list = store.getState().messagesByConv[conversationId] ?? [];
  store.setState({
    messagesByConv: {
      ...store.getState().messagesByConv,
      [conversationId]: [
        ...list,
        {
          id: asstId,
          role: "assistant",
          content: "",
          createdAt: asstTs,
        },
      ],
    },
    streamingByConv: {
      ...store.getState().streamingByConv,
      [conversationId]: true,
    },
  });

  const controller = new AbortController();
  abortControllers.set(conversationId, controller);

  // 引用已在 send 时固化进 user 消息 content（@引用 路径块 / 非文件节点全文注入），此处不再动态拼接
  // 叙述-only 消息（content 为空、正文在 steps 叙述步）先回填 content——
  // 否则空 content 会被下方过滤丢弃，造成多轮上下文断裂
  // 过滤 system 与错误占位 assistant（[错误] 不进 API 历史，避免污染上下文）；
  // 空占位 assistant（预建 content:"" 的流式占位）也不发送——部分端点对空 content 返回 400
  const history = store
    .getState()
    .messagesByConv[conversationId]
    .map(fillAssistantReplyText)
    .filter(
      (m) =>
        m.role !== "system" &&
        !(
          m.role === "assistant" &&
          (m.content.startsWith(ERROR_PREFIX) || m.content === "")
        ),
    );

  try {
    // 系统提示词 + 工具：按 Agent 实时解析（配置在 设置 → Agent，引用已注册提示词笔记实时读正文注入）。
    // 缺省（未选 Agent）= 预置「对话」；Agent 缺失（已删）降级为普通对话。
    const agentReq = await useSettingsStore
      .getState()
      .resolveAgentRequest(nodeData?.agentId);
    let tools: ToolSchema[] = [];
    if (agentReq) {
      tools = agentReq.tools;
      if (agentReq.skippedWebSearch) {
        store.setState({
          error: "未配置搜索源（设置 → 联网搜索），本次对话未启用联网搜索",
        });
      }
    }
    // 引用已在 send 时固化进 user 消息 content（@引用 路径块 / 非文件节点全文注入），此处不再动态拼接
    const apiMessages: LlmMessage[] = toLlmMessages(history);
    // 系统提示词注入：Agent 引用已注册提示词笔记实时读正文（外部编辑即时生效，读失败静默降级）；
    // 工具含 read_file 时追加「@引用 文件用 read_file 读取」引导。易变上下文（任务清单）走尾部块，
    // 不进系统提示词——系统前缀必须稳定以命中前缀缓存。
    const systemPrompt = assembleAgentSystemPrompt(agentReq?.systemPrompt, tools);
    if (systemPrompt) {
      apiMessages.unshift({ role: "system", text: systemPrompt });
    }
    // 当前任务清单（todo_write 开启时）以尾部 user 消息块带出：清单不变则逐字稳定 → 前缀缓存命中
    const todosBlock = tools.some((t) => t.name === "todo_write")
      ? currentTodosBlock(await readAgentTodos(conversationId))
      : "";
    if (todosBlock) {
      const last = apiMessages[apiMessages.length - 1];
      last.text += `\n\n${todosBlock}`;
    }
    await runStreamExchange({
      provider,
      model,
      ...(reasoningEffort ? { reasoningEffort } : {}),
      apiMessages,
      ...(tools.length ? { tools } : {}),
      signal: controller.signal,
      // 增量写入占位消息（引擎 rAF 合并后每帧调用）
      applyBatch: ({ content, reasoning }) => {
        patchAssistant(conversationId, asstId, (m) => ({
          ...m,
          ...(content ? { content: m.content + content } : {}),
          // 思考增量流入 steps（最后思考步拼接 / 工具轮之间自然分隔）
          ...(reasoning
            ? { steps: appendReasoning(m.steps ?? [], reasoning) }
            : {}),
        }));
      },
      onError: (err) => {
        // 节点已删除：abort 后回调迟到，不再写错误占位（messagesByConv 键已随删除清理）
        if (!findConversationNode(conversationId)) {
          abortControllers.delete(conversationId);
          return;
        }
        // 流式结果落盘属业务数据变更：作废 redo（undo 后流式回复不得被 Ctrl+Y 抹除）
        touchRedo();
        // 不静默降级：请求失败如实报错（[错误] 占位显示服务端具体信息，便于定位）
        patchAssistant(conversationId, asstId, (m) => ({
          ...m,
          content: m.content || `${ERROR_PREFIX} ${err.message}`,
        }));
        store.setState((state) => ({
          streamingByConv: { ...state.streamingByConv, [conversationId]: false },
        }));
        // 持久化错误占位（[错误] 前缀会在下次请求历史中被过滤，不污染上下文）
        schedulePersist();
        abortControllers.delete(conversationId);
      },
      onDone: ({ reasoning, timedOut, truncated, promoteNarration }) => {
        // 节点已删除：丢弃流收尾写入（防孤儿消息随补丁落盘），键已随删除清理
        if (!findConversationNode(conversationId)) {
          abortControllers.delete(conversationId);
          return;
        }
        // 流式结果落盘属业务数据变更：作废 redo（undo 后流式回复不得被 Ctrl+Y 抹除）
        touchRedo();
        // 空回复移除占位；超时且回答未产出写超时降级（保留思考）；否则正常复位。
        // 用占位消息的实际 content/steps 判定（叙述提升/工具步骤已在其内），而非引擎 totals
        const m = store
          .getState()
          .messagesByConv[conversationId]?.find((mm) => mm.id === asstId);
        if (!m) {
          // 流式中消息已被移除（极端竞态）：仅复位 streaming 态，不重建键
          store.setState((state) => ({
            streamingByConv: {
              ...state.streamingByConv,
              [conversationId]: false,
            },
          }));
          schedulePersist();
          abortControllers.delete(conversationId);
          return;
        }
        // onDone 最终化：最终回答轮叙述提升进 content + 输出上限截断提示（画布/面板共用）
        const finalized = finalizeReplyText({
          content: m.content,
          steps: m.steps ?? [],
          promoteNarration,
          truncated,
        });
        const decision = decideCleanup(
          finalized.content,
          reasoning,
          timedOut,
          finalized.steps.length > 0,
        );
        if (decision.kind === "remove") {
          // 空回复：移除占位 assistant，避免残留空气泡
          patchAssistant(conversationId, asstId, () => null);
        } else {
          patchAssistant(conversationId, asstId, (mm) => ({
            ...mm,
            content:
              decision.kind === "timeout-error"
                ? `${ERROR_PREFIX} ${TIMEOUT_ERROR_TEXT}`
                : finalized.content,
            steps: finalized.steps,
          }));
        }
        store.setState((state) => ({
          streamingByConv: { ...state.streamingByConv, [conversationId]: false },
        }));
        // messages 随 .atlx 增量补丁落盘（流式结束统一写，不逐 token 写）
        schedulePersist();
        abortControllers.delete(conversationId);
      },
      // 工具调用过程可视化：全量累积 runs 合并进占位消息 steps（思考→工具交错，随消息落 .atlx）
      onToolRuns: (runs) => {
        patchAssistant(conversationId, asstId, (m) => ({
          ...m,
          steps: mergeToolRuns(m.steps ?? [], runs),
        }));
      },
      // 工具轮叙述正文进 steps（渲染为该步的「思考行」）
      onNarration: (text) => {
        patchAssistant(conversationId, asstId, (m) => ({
          ...m,
          steps: appendNarration(m.steps ?? [], text),
        }));
      },
      executeTools: (calls) =>
        // 公共工具执行器（画布/面板共用）；差异仅产物节点：画布建搜索/写笔记节点，面板不建
        runAgentTools(
          calls,
          {
            signal: controller.signal,
            capabilities: {
              search: (query) => runSearch(useSettingsStore.getState().searchConfig, query),
              readFile: (path, opts) =>
                readVaultFileWindow(path, opts).then((res) => {
                  // @引用 的文件被 Agent 实际读取 → 消费物质化：画布落引用节点 + 连已消费边（幂等）
                  if (
                    isRefCoveredPath(
                      useCanvasStore.getState().messagesByConv[conversationId] ?? [],
                      path,
                    )
                  ) {
                    void materializeReferencedFile(conversationId, path);
                  }
                  return res;
                }),
              glob: (pattern, opts) => globVault(pattern, opts),
              grep: (pattern, opts) => grepVault(pattern, opts),
              listDir: (dir) => listVaultDir(dir),
              renameFile: (oldPath, newName) => useVaultStore.getState().renameFile(oldPath, newName),
              moveFile: (oldPath, targetDir) => useVaultStore.getState().moveFile(oldPath, targetDir),
              deleteFile: (path) => useVaultStore.getState().deleteFile(path),
              deleteDir: (dir, force) =>
                useVaultStore.getState().deleteFolder(dir, force).then((r) => ({
                  ok: r.deleted,
                  summary: r.deleted
                    ? `已删除目录「${dir}」`
                    : r.needsConfirm
                      ? `目录非空（${r.itemCount} 项）`
                      : "删除目录失败",
                  needsConfirm: r.needsConfirm,
                  itemCount: r.itemCount,
                })),
              readHistory: (path, opts) => readHistoryForAgent(path, opts),
              appendFile: (path, content) =>
                appendVaultFile(path, content).then((res) => {
                  if (res.ok) void recordAgentFileWrite(path);
                  return res;
                }),
              writeTodos: (todos) => writeAgentTodos(conversationId, todos),
              writeFile: (path, content) => writeVaultFile(path, content).then(() => {
                // Agent 协作历史：AI 写文件以 Agent 身份记入对应 kind 的历史（fire-and-forget）
                void recordAgentFileWrite(path, content);
                return { ok: true, summary: `已写入「${path}」` };
              }),
              editFile: (path, edits) => editVaultFile(path, edits).then((res) => {
                if (res.ok) void recordAgentFileWrite(path);
                return res;
              }),
              fetchUrl: fetchWeb,
            },
          },
          {
            onToolResult: (name, result) => {
              if (name === "web_search" && result.ok && result.data) {
                const d = result.data as SearchResultData;
                createSearchNode(conversationId, d.query, d);
              } else if (name === "write_file" && result.ok && result.data) {
                const { path } = result.data as { path: string };
                if (/\.md$/i.test(path)) {
                  const title = (path.split("/").pop() ?? path).replace(/\.md$/i, "");
                  createWriteNoteNode(conversationId, path, title);
                }
              }
            },
          },
        ),
    });
  } catch (e) {
    console.error("流式请求失败", e);
    // 节点已删除：不重建 streamingByConv 键（删除已清理），仅收掉 controller
    if (!findConversationNode(conversationId)) {
      abortControllers.delete(conversationId);
      return;
    }
    store.setState((state) => ({
      streamingByConv: { ...state.streamingByConv, [conversationId]: false },
    }));
    abortControllers.delete(conversationId);
  }
}

function nowId() {
  return { id: crypto.randomUUID(), ts: Date.now() };
}

/**
 * 节点 → AI 可注入内容（唯一权威映射，三处消费方共用——regenerate 重建 / 连边引用 /
 * send @提及 就地替换，防各写一份拷贝后行为分叉）：
 * - text：正文 bodyMd（画布内文本节点或 .md 笔记节点）
 * - search：结果摘要（勾选子集或全部，resultsToText）
 * - table：快照文本（snapshot 缺省回退标题）
 * - media 图片：文件名文本 + 图片附件（vision 发送用，文本位替换为文件名）
 * - media 文本类：解析出的正文 body
 * - 其余类型/无内容：null（不注入）
 */
function describeNodeAsInput(node: Node): { text: string; attach?: Attachment } | null {
  if (node.type === "text") {
    const d = node.data as unknown as TextData;
    return d.bodyMd ? { text: d.bodyMd } : null;
  }
  if (node.type === "search") {
    const text = resultsToText(node.data as unknown as SearchResultData);
    return text ? { text } : null;
  }
  if (node.type === "table") {
    const td = node.data as unknown as TableData;
    const snapshot = td.snapshot || td.title;
    return snapshot ? { text: snapshot } : null;
  }
  const md = node.data as unknown as MediaData;
  if (md.kind === "image" && md.thumb) {
    return {
      text: md.name ?? "",
      attach: {
        kind: "image",
        payload: md.thumb,
        mime: md.mime,
        filename: md.name,
      },
    };
  }
  if (md.body) return { text: md.body };
  return null;
}

/**
 * 引用拼装（send 注入与 regenerate 重建共用）：`[引用文件：…]` 路径块 + `[引用：…]` 前缀块
 * + 正文的统一叠放（文件块最前、引用块居中、正文垫底），块格式逐字一致。
 * fileRefs 为空不加文件块；prefixParts 为空不加引用块。
 */
function assembleContentWithRefs(
  content: string,
  fileRefs: string[],
  prefixParts: string[],
): string {
  // 目录引用加 / 后缀标注（引导见 FILE_REFERENCE_PROMPT 的目录句）；树中不存在按文件原样
  const pathKind = useVaultStore.getState().pathKind;
  const fileBlock = fileRefs.length
    ? `[引用文件：\n${fileRefs.map((f) => `- ${pathKind(f) === "dir" ? `${f}/` : f}`).join("\n")}]\n\n`
    : "";
  if (prefixParts.length)
    return `${fileBlock}[引用：${prefixParts.join("\n\n")}]\n\n${content}`;
  if (fileBlock) return `${fileBlock}${content}`;
  return content;
}

/**
 * 用最新资产状态重建最后一条 user 消息（regenerate 前调用，扩展）：
 * - .md 笔记 @引用（refs.label 出现在 displayContent）：保留 @标题 原位，最新文件路径重拼「引用文件」块
 * - 非文件 @提及：就地替换为最新内容——与 send 侧「首处子串替换」语义一致
 * - 正向连边引用（label 不在 displayContent）：重拼 `[引用：…]` 前缀
 * - 附件：画布媒体节点引用（sourceNodeId）替换为最新 thumb/body，临时附件保留
 * 源节点缺失/读不到内容时跳过（保持旧快照，不崩坏）。
 * 注意：调用方须保证 userMsg.displayContent 存在（重建以原始输入为基底，缺失时无法反推 @位置）。
 */
function rebuildUserContent(
  userMsg: Message,
  nodes: Node[],
): { content: string; attachments?: Attachment[] } {
  let content = userMsg.displayContent as string;
  const prefixParts: string[] = [];
  const attachByNode = new Map<string, Attachment>();
  const fileRefs: string[] = [];
  for (const ref of userMsg.refs ?? []) {
    // 纯路径引用（仓库文件/文件夹，画布无对应节点）：直接并入「引用文件」块（目录标注在拼装处统一做）
    if (ref.file) {
      if (!fileRefs.includes(ref.file)) fileRefs.push(ref.file);
      continue;
    }
    const node = nodes.find((n) => n.id === ref.nodeId);
    if (!node) continue;
    const tag = `@${ref.label}`;
    const inText = content.includes(tag);
    // .md 笔记节点：@引用 与连边引用统一走「引用文件」路径块——@标题 在正文则保留原位（不替换），
    // 取最新文件路径（笔记改名/移动后更新）；非文件节点（画布内文本/搜索/表格）落到下方整文注入分支
    if (node.type === "text" && (node.data as unknown as TextData).file) {
      const file = (node.data as unknown as TextData).file as string;
      if (!fileRefs.includes(file)) fileRefs.push(file);
      continue;
    }
    const latest = describeNodeAsInput(node);
    if (!latest) continue;
    if (inText) {
      // 函数形式替换：latest.text 若含 `$`/`&` 不会被 String.replace 当作替换模式解析
      content = content.replace(tag, () => latest.text);
      // 保留 sourceNodeId：附件来自画布媒体节点（@ 提及），供「已注入」检测与语义完整
      if (latest.attach)
        attachByNode.set(ref.nodeId, {
          ...latest.attach,
          sourceNodeId: ref.nodeId,
        });
    } else {
      prefixParts.push(latest.text);
    }
  }
  // 重建整体结构 = 文件块 + 连边引用前缀 + 正文（与 send 拼装顺序一致）
  content = assembleContentWithRefs(content, fileRefs, prefixParts);
  const attachments = (userMsg.attachments ?? []).map((a) =>
    a.sourceNodeId ? (attachByNode.get(a.sourceNodeId) ?? a) : a,
  );
  return { content, attachments: attachments.length ? attachments : undefined };
}

/**
 * silent 刷新公共骨架（refreshText/Media/TableContent 共用）：按 type + file 收集引用节点
 * （无引用直接返回，不触发读盘）→ produce 读盘并产出「节点 → data 补丁」函数 → silent set
 * （直接改 nodes，不调 schedulePersist——变更来自磁盘，写回会回环；补丁在 set 时按最新节点
 * 求值，await 读盘窗口内的其他编辑原样保留）→ 读失败统一 markFileMissing。
 * produce 返回 null = 无需更新（不 set，防无谓的全量节点数组替换）。
 */
async function refreshFileNodes(
  file: string,
  type: "text" | "media" | "table",
  produce: (
    targets: Node[],
  ) => Promise<((n: Node) => Record<string, unknown> | null) | null>,
): Promise<void> {
  const targets = useCanvasStore.getState().nodes.filter(
    (n) =>
      n.type === type &&
      (n.data as unknown as TextData | MediaData | TableData).file === file,
  );
  if (targets.length === 0) return;
  try {
    const patchOf = await produce(targets);
    if (!patchOf) return;
    useCanvasStore.setState((s) => ({
      nodes: s.nodes.map((n) => {
        const patch = patchOf(n);
        return patch
          ? { ...n, data: { ...n.data, ...patch } as unknown as Node["data"] }
          : n;
      }),
    }));
  } catch {
    useCanvasStore.getState().markFileMissing(file, type);
  }
}

export const useCanvasStore = create<CanvasState>((set, get) => ({
  canvasId: null,
  canvasFile: null,
  canvasTitle: "",
  nodes: [],
  edges: [],
  readOnly: false,
  messagesByConv: {},
  streamingByConv: {},
  lockedConversations: {},
  pendingMentionsByConv: {},
  error: null,
  selectedNodeId: null,
  loading: false,
  saving: false,
  dirty: false,
  conflictPending: false,
  baseUpdatedAt: 0,
  load: async (file) => {
    // 切换画布前先落盘旧画布未保存改动（防 debounce 窗口内丢改动；无脏不写）。
    // 仅 cancel 不够：500ms 窗口内的编辑会随切换丢失；flush 内部已清保存 timer，
    // 防残留 timer 用新画布状态写旧画布文件（P0 数据丢失）
    await get().flush();
    // 中止旧画布进行中的流：abort 后 onDone 只清流式标志，不再向新画布写增量
    abortAllStreams();
    // 中止旧画布进行中的命名请求（防其后台空转/误写；与面板 load 对称）
    abortAutoTitle();
    groupDragState = null;
    dragInProgress = false;
    set({ loading: true, error: null, selectedNodeId: null });
    try {
      // 外部白板格式（.canvas）走只读加载：映射为运行时节点 + 无向边，永不落盘
      const isWhiteboard = file.toLowerCase().endsWith(".canvas");
      const data = isWhiteboard
        ? await loadWhiteboardVault(file)
        : await loadCanvasVault(file);
      set({
        canvasId: data.id,
        canvasFile: file,
        canvasTitle: data.title,
        nodes: data.nodes,
        edges: data.edges,
        readOnly: isWhiteboard,
        messagesByConv: data.messagesByConv,
        // 乐观并发基准 = 磁盘版本；加载后本地与磁盘一致
        baseUpdatedAt: data.updatedAt,
        dirty: false,
        conflictPending: false,
        // 跨画布切换清空 undo/redo 与流式状态：快照含 messages，混用会串画布污染撤销
        streamingByConv: {},
        // 切画布释放本端独占编辑锁（旧画布锁不得带进新画布）
        lockedConversations: {},
        // 引用队列按对话节点 id 记键，跨画布无消费方，一并清空（防跨画布残留）
        pendingMentionsByConv: {},
        loading: false,
      });
      undoMgr.clear();
      // 已落盘基线 = 加载的磁盘状态（后续保存按引用 diff，未变实体不重写）
      syncLastSaved();
      // 恢复补命名：加载后对首个未命名对话节点重试（覆盖上次命名被中断/丢失的窗口；
      // 仅补一个防并发请求轰炸模型端点）；无 title 无消息的节点由消息检查自然跳过
      if (!isWhiteboard) {
        const unnamed = get().nodes.find(
          (n) =>
            n.type === "conversation" &&
            !(n.data as Partial<ConversationData>).title,
        );
        if (unnamed) void autoNameConversation(unnamed.id);
      }
    } catch (e) {
      console.error("加载画布失败", e);
      set({ loading: false, error: "加载画布失败，请重试" });
    }
  },
  renameCanvas: async (title) => {
    const { canvasId, canvasFile } = get();
    // 只读画布（外部白板格式）不支持重命名（文件保留不动）
    if (!canvasId || !canvasFile || get().readOnly) return;
    // 先按旧文件名落盘 pending 改动：重命名后 timer 回调的路径守卫会静默跳过保存，
    // 不 flush 会让 500ms 窗口内的编辑随改名一起丢失
    await get().flush();
    set({ canvasTitle: title });
    try {
      await renameCanvasVault(canvasFile, title);
      // 同目录改文件名：先算新路径再标记自写（旧路径删除 + 新路径创建两路事件一并抑制），
      // 并同步乐观锁基准防下次保存误冲突
      const newFile = siblingPath(
        canvasFile,
        `${sanitizeFilename(title)}.atlx`,
      );
      markSelfSave([canvasFile, newFile]);
      // 同步 canvasFile 到新路径（防下次保存写旧路径 → createdAt 重置/乐观锁失效/
      // 外部修改失配）；appStore.currentCanvasFile 同源（打开路径/文件面板高亮），一并同步
      set({ canvasFile: newFile });
      if (useAppStore.getState().currentCanvasFile === canvasFile) {
        useAppStore.setState({ currentCanvasFile: newFile });
      }
      // 文件已由 Rust 改名：同步标题基线，下次保存不再携带 title（无冗余重命名扫描）
      lastSavedTitle = title;
      await get().syncBaseUpdatedAt();
    } catch (e) {
      console.error("重命名失败", e);
    }
  },
  onNodesChange: (changes) => {
    // 只读画布：只保留选中态变化（禁拖拽/缩放产生的位置尺寸变更，防白板内容被改动）
    const changesNow = get().readOnly
      ? changes.filter((c) => c.type === "select")
      : changes;
    let nodes = applyNodeChanges(changesNow, get().nodes);
    // 分组拖拽联动：拖动中的组按位移同步平移组内成员（同帧合并防位置撕裂）。
    // 多选拖拽时成员自身也收到 position change（React Flow 按相同 delta 移动），
    // 此处覆盖结果一致，无叠加；成员未被选中时（仅拖组）平移由这里完成。
    if (groupDragState && !get().readOnly) {
      const groupChange = changesNow.find(
        (c) => c.type === "position" && c.id === groupDragState!.groupId,
      );
      if (groupChange?.type === "position" && groupChange.position) {
        const dx = groupChange.position.x - groupDragState.groupStart.x;
        const dy = groupChange.position.y - groupDragState.groupStart.y;
        const memberStart = new Map(
          groupDragState.members.map((m) => [m.id, m.start]),
        );
        nodes = nodes.map((n) => {
          const start = memberStart.get(n.id);
          return start
            ? { ...n, position: { x: start.x + dx, y: start.y + dy } }
            : n;
        });
      }
    }
    set({ nodes });
    // resize 结束（dimensions + resizing:false）：重算相连边锚点（与 onNodeDragStop 对称）
    const resized = new Set(
      changesNow
        .filter((c) => c.type === "dimensions")
        .filter((c) => c.resizing === false)
        .map((c) => c.id),
    );
    if (resized.size > 0) {
      const next = recalcEdgeHandles(get().edges, nodes, resized);
      if (next) {
        set({ edges: next });
        schedulePersist();
        return;
      }
    }
    // 纯选中变化（select）不落盘：点选/框选节点不应触发 .atlx 写入与「保存中」闪烁。
    // 拖拽进行中的 position 变更不落盘（dragStop 统一保存一次）；resize 进行中的
    // dimensions 变更不落盘（resizing:false 结束事件已在上方处理）——拖动不松手不再持续保存
    const draggingNow = dragInProgress;
    if (
      changesNow.some(
        (c) =>
          c.type !== "select" &&
          !(c.type === "position" && (c.dragging || draggingNow)) &&
          !(c.type === "dimensions" && c.resizing === true),
      )
    ) {
      schedulePersist();
    }
  },
  onEdgesChange: (changes) => {
    const edges = applyEdgeChanges(changes, get().edges);
    set({ edges });
    if (changes.some((c) => c.type !== "select")) schedulePersist();
  },
  onConnect: (connection: Connection) => {
    const { nodes, edges, messagesByConv } = get();
    const src = nodes.find((n) => n.id === connection.source);
    const tgt = nodes.find((n) => n.id === connection.target);
    // 拖线引用（text/media/search → conversation）：引用即边——未连接时立即建虚线边（未消费自动虚线）
    // + 输入框 @标签 队列（消费后由虚实判定自动转实线，取消引用=删 @标签断边）
    if (
      tgt?.type === "conversation" &&
      (src?.type === "text" || src?.type === "media" || src?.type === "search")
    ) {
      const alreadyConnected = edges.some(
        (e) => e.target === connection.target && e.source === connection.source,
      );
      if (alreadyConnected) {
        // 已连接：未消费（虚线待发送）→ 无效果（防同轮重复引用）；已消费（实线边）→ 再次注入——
        // 直接进输入框 @标签 队列（边已存在不可断开，发送时注入并保持实线）
        const injected = isAssetConsumed(
          messagesByConv[connection.target] ?? [],
          connection.source,
        );
        if (injected) {
          get().queueMention(connection.target, connection.source);
        }
        return;
      }
      // 未连接：立即建边（不入 undo 栈——取消引用 = 删 @标签自动断边，无需撤销）+ 输入框 @标签；
      // 属非入栈数据变更，必须作废 redo 栈（防 undo 后 Ctrl+Y 用旧快照抹掉本边）
      touchRedo();
      const nodesNow = get().nodes;
      const edge = withHandles({ ...connection }, nodesNow);
      set({ edges: rfAddEdge(edge, get().edges) });
      get().queueMention(connection.target, connection.source);
      schedulePersist();
      return;
    }
    // 自动分类：仅「对话 → 资产」是有向数据流产出边；其余连线（对话↔对话、link/group 参与、
    // 无对话组合）均为关联自由线（directed: false，无消费语义、可删除、箭头模式 linkMode）
    const isDataFlow =
      src?.type === "conversation" &&
      (tgt?.type === "text" || tgt?.type === "media" || tgt?.type === "search");
    get().pushUndo();
    const nodesNow = get().nodes;
    const edge = withHandles(
      {
        ...connection,
        ...(isDataFlow ? {} : { directed: false, linkMode: "none" as const }),
      },
      nodesNow,
    );
    const nextEdges = rfAddEdge(edge, get().edges);
    set({ edges: nextEdges });
    schedulePersist();
  },
  queueMention: (conversationId, nodeId) => {
    set((state) => ({
      pendingMentionsByConv: {
        ...state.pendingMentionsByConv,
        [conversationId]: [
          ...(state.pendingMentionsByConv[conversationId] ?? []),
          nodeId,
        ],
      },
    }));
  },
  clearPendingMentions: (conversationId) => {
    set((state) => {
      const next = { ...state.pendingMentionsByConv };
      delete next[conversationId];
      return { pendingMentionsByConv: next };
    });
  },
  onNodeDragStart: (_event, node) => {
    get().pushUndo();
    dragInProgress = true;
    // 组节点：快照组内成员（中心点在内，拖前位置一次判定），拖动期间按组位移联动平移
    groupDragState = null;
    if (get().readOnly || node.type !== "group") return;
    const gw = node.width ?? node.measured?.width ?? DEFAULT_GROUP_WIDTH;
    const gh = node.height ?? node.measured?.height ?? DEFAULT_GROUP_HEIGHT;
    const members = collectGroupMembers(get().nodes, node.id, {
      x: node.position.x,
      y: node.position.y,
      w: gw,
      h: gh,
    });
    if (members.length) {
      groupDragState = {
        groupId: node.id,
        groupStart: { x: node.position.x, y: node.position.y },
        members: members.map((m) => ({
          id: m.id,
          start: { x: m.position.x, y: m.position.y },
        })),
      };
    }
  },
  onNodeDragStop: (_event, node) => {
    // 组拖拽联动结束时重算组 + 全部成员的相连边锚点（位置自适应）
    const ids = new Set<string>([node.id]);
    if (groupDragState?.groupId === node.id) {
      for (const m of groupDragState.members) ids.add(m.id);
    }
    const next = recalcEdgeHandles(get().edges, get().nodes, ids);
    if (next) {
      set({ edges: next });
    }
    dragInProgress = false;
    groupDragState = null;
    // 拖拽期间 position 变更不落盘（见 onNodesChange），此处统一保存一次——
    // 未移动（点按即松）时补丁为空，自动跳过写盘
    schedulePersist();
  },
  copySelectedNodes: () => {
    const sel = get().nodes.filter((n) => n.selected);
    if (sel.length === 0) return false;
    clipboardNodes = sel.map((n) => ({
      type: n.type,
      position: { x: n.position.x, y: n.position.y },
      width: n.width,
      height: n.height,
      // 保留 group 的 -1：粘贴的分组同样垫底
      zIndex: n.zIndex,
      data: structuredClone(n.data),
    }));
    return true;
  },
  pasteNodes: (position) => {
    // 未打开画布（占位面板）时粘贴无意义：不写入 store（canvasId null 时 persist 虽不落盘，
    // 但节点会残留在内存态，打开画布前状态混乱）
    if (!get().canvasId || clipboardNodes.length === 0) return;
    get().pushUndo();
    // 整体中心对齐：以剪贴板包围盒中心为锚，粘贴件中心落在目标位置（多选复制保持相对排布）
    const xs = clipboardNodes.map((c) => c.position.x);
    const ys = clipboardNodes.map((c) => c.position.y);
    const centerX = (Math.min(...xs) + Math.max(...xs)) / 2;
    const centerY = (Math.min(...ys) + Math.max(...ys)) / 2;
    const pasted: Node[] = clipboardNodes.map((c) => ({
      id: crypto.randomUUID(),
      type: c.type,
      position: {
        x: position.x + (c.position.x - centerX),
        y: position.y + (c.position.y - centerY),
      },
      ...(c.width != null ? { width: c.width } : {}),
      ...(c.height != null ? { height: c.height } : {}),
      ...(c.zIndex != null ? { zIndex: c.zIndex } : {}),
      // 粘贴即选中（清除旧选中，避免旧节点被误当多选整体拖动）
      selected: true,
      data: structuredClone(c.data),
    }));
    set({
      nodes: [
        ...get().nodes.map((n) => (n.selected ? { ...n, selected: false } : n)),
        ...pasted,
      ],
    });
    schedulePersist();
  },
  addNode: (node) => {
    get().pushUndo();
    const nodes = [...get().nodes, node];
    set({ nodes });
    schedulePersist();
  },
  addTextNoteFromVault: async (file, title, position, exact = false) => {
    const data = await buildTextNoteData(file, title);
    // 拖拽落点精确（exact=true 跳过避让，保证节点落在鼠标位置）；其他入口走 findFreeSpot 避让
    const spot = exact
      ? position
      : findFreeSpot(get().nodes, position, { w: 320, h: 240 });
    get().addNode({
      id: crypto.randomUUID(),
      type: "text",
      position: spot,
      // 显式写入默认尺寸：React Flow 优先用节点存储的 width/height（TextNode 的 ?? 回退受测量机制干扰）
      width: DEFAULT_TEXT_NODE_WIDTH,
      height: DEFAULT_TEXT_NODE_HEIGHT,
      data: data as unknown as Node["data"],
    });
  },
  addMediaFromVault: async (file, name, position, exact = false) => {
    const data = await buildMediaData(file, name);
    if (!data) return;
    // 拖拽落点精确（exact=true 跳过避让）；其他入口走 findFreeSpot 避让
    const spot = exact
      ? position
      : findFreeSpot(get().nodes, position, { w: 260, h: 240 });
    get().addNode({
      id: crypto.randomUUID(),
      type: "media",
      position: spot,
      data: data as unknown as Node["data"],
    });
  },
  findTextNoteByFile: (file) => {
    const node = get().nodes.find(
      (n) => n.type === "text" && (n.data as unknown as TextData).file === file,
    );
    return node?.id ?? null;
  },
  findMediaNoteByFile: (file) => {
    const node = get().nodes.find(
      (n) =>
        n.type === "media" && (n.data as unknown as MediaData).file === file,
    );
    return node?.id ?? null;
  },
  addTableFromVault: async (file, title, position, exact = false) => {
    const data = await buildTableData(file, title);
    // 拖拽落点精确（exact=true 跳过避让）；其他入口走 findFreeSpot 避让
    const spot = exact
      ? position
      : findFreeSpot(get().nodes, position, { w: 320, h: 240 });
    get().addNode({
      id: crypto.randomUUID(),
      type: "table",
      position: spot,
      width: DEFAULT_TABLE_NODE_WIDTH,
      height: DEFAULT_TABLE_NODE_HEIGHT,
      data: data as unknown as Node["data"],
    });
  },
  refreshTableContent: (file, opts) =>
    refreshFileNodes(file, "table", async (targets) => {
      const snapshot = opts?.snapshot ?? tableToSnapshotText(await readTableVault(file));
      const ids = new Set(targets.map((t) => t.id));
      return (n) => (ids.has(n.id) ? { snapshot, fileMissing: false } : null);
    }),
  refreshTextContent: (file) =>
    refreshFileNodes(file, "text", async (targets) => {
      const bodyMd = await readNote(file);
      // 逐节点判定刷新/跳过（见 utils/noteRefresh#decideTextNodeRefresh）：
      // - 节点正文 == 磁盘 → 已一致跳过；
      // - 自上次落盘后改过正文（bodyMd ≠ lastSaved 同 id 节点）且磁盘仍停在节点基线 →
      //   保留本地编辑跳过，防「提交编辑 A → 保存写盘 → 回波到达前又提交编辑 B → 回波把 B 覆盖回 A」丢字；
      // - 未编辑过/新建未落盘 → 刷新到磁盘最新；
      // - 已编辑但磁盘已前进到节点基线之后（笔记编辑器/AI 写入）→ 外部最新者胜，刷新到磁盘，
      //   防陈旧基线上的编辑回写覆盖磁盘新内容（笔记编辑器静默回退根因，见 noteRefresh）。
      // 不能用 isKnownNoteDiskContent（lastWrittenMd）判回波：AI 文件写入也会登记基线，
      // 会把「磁盘新于节点内存」误判为自写回波而跳过刷新，节点保持陈旧——下次画布保存
      // 经 toFileNode 把旧正文回写覆盖 Agent 编辑。
      // 记录判定时刻的节点正文：set 时若正文已变（await 读盘窗口内用户又编辑了该节点），
      // 跳过刷新保留更新的本地编辑（与 keep 分支同语义，防把新输入覆盖回磁盘内容）。
      const staleDecisions = new Map<string, string>();
      for (const n of targets) {
        const cur = (n.data as unknown as TextData).bodyMd ?? "";
        if (cur === bodyMd) continue;
        const saved = lastSavedNodes.find((s) => s.id === n.id);
        const savedBody = saved
          ? (saved.data as unknown as TextData).bodyMd
          : undefined;
        if (decideTextNodeRefresh(cur, savedBody, bodyMd) === "refresh") {
          staleDecisions.set(n.id, cur);
        }
      }
      if (staleDecisions.size === 0) return null;
      // 同步磁盘基线（lastWrittenMd）：外部编辑刷新后用户「改回旧值」时脏检测能感知差异
      // （基线陈旧会导致回退被误判为「与上次写入一致」而跳过写盘，外部内容永久覆盖用户回退）
      recordNoteDiskContent(file, bodyMd);
      return (n) => {
        const decCur = staleDecisions.get(n.id);
        if (decCur === undefined) return null;
        if (((n.data as unknown as TextData).bodyMd ?? "") !== decCur) return null;
        return { bodyMd, fileMissing: false };
      };
    }),
  refreshMediaContent: (file) =>
    refreshFileNodes(file, "media", async (targets) => {
      const existing = targets[0]?.data as unknown as MediaData;
      const ids = new Set(targets.map((t) => t.id));
      if (existing.kind === "image") {
        const thumb = await readAttachmentDataUrl(file);
        return (n) => (ids.has(n.id) ? { thumb, fileMissing: false } : null);
      }
      const body = await readNote(file);
      return (n) =>
        ids.has(n.id) ? { body, parseFailed: false, fileMissing: false } : null;
    }),
  markFileMissing: (file, kind) => {
    set((s) => ({
      nodes: s.nodes.map((n) =>
        n.type === kind &&
        (n.data as unknown as TextData | MediaData | TableData).file === file
          ? {
              ...n,
              data: { ...n.data, fileMissing: true } as unknown as Node["data"],
            }
          : n,
      ),
    }));
  },
  reloadFromDisk: async () => {
    const { canvasFile } = get();
    if (!canvasFile) return;
    // 重载后与磁盘一致：清冲突与脏标记（本地未保存改动被丢弃，由用户确认后调用）
    set({ conflictPending: false, dirty: false });
    await get().load(canvasFile);
  },

  flush: async () => {
    const { readOnly, dirty, canvasId, canvasFile } = get();
    if (readOnly) return false;
    if (!dirty || !canvasId || !canvasFile) {
      // schedulePersist 曾置位 saving 但无改动可写（timer 未触发/无改动），复位防状态卡死
      set({ saving: false });
      return false;
    }
    await persistCtl.flush();
    return true;
  },

  mergeFromDisk: async () => {
    const { canvasFile } = get();
    if (!canvasFile) return;
    const localNodes = get().nodes;
    const localEdges = get().edges;
    const localMessages = get().messagesByConv;
    // 中止进行中的流与命名：合并整体替换消息状态，流继续写只会静默丢失（与 load 同策略）。
    // 不复用 load()——其内部 flush 会在合并前重触发冲突检测写盘、自动命名会发 LLM 请求，
    // 均属越界副作用；这里只做「读盘 + 同步基线 + 合并」。
    abortAllStreams();
    abortAutoTitle();
    groupDragState = null;
    dragInProgress = false;
    await mergeDiskIntoMemory({
      canvasFile,
      guardBasis: null,
      clearUndo: true,
      resetLoading: true,
      captureLocal: () => ({ nodes: localNodes, edges: localEdges, messagesByConv: localMessages }),
      onError: (e) => {
        console.error("合并磁盘失败", e);
        useCanvasStore.setState({ error: "合并磁盘失败，请重试" });
      },
    });
  },
  addEdge: (edge) => {
    get().pushUndo();
    const edges = [...get().edges, withHandles(edge, get().nodes)];
    set({ edges });
    schedulePersist();
  },
  updateNodeData: (nodeId, patch) => {
    // 协作独占锁守卫：对话节点被其他对端编辑时拒绝内容写（锁主为唯一写者；UI 已只读，此为竞态兜底）
    const target = get().nodes.find((n) => n.id === nodeId);
    if (target?.type === "conversation" && isConversationLockedByPeer(nodeId)) return;
    touchRedo();
    const nodes = get().nodes.map((n) =>
      n.id === nodeId ? { ...n, data: { ...n.data, ...patch } } : n,
    );
    set({ nodes });
    schedulePersist();
  },
  syncBaseUpdatedAt: async () => {
    const canvasFile = get().canvasFile;
    if (!canvasFile) return;
    try {
      const disk = await readCanvasVault(canvasFile);
      set({ baseUpdatedAt: disk.updatedAt });
    } catch {
      // 画布被外部删除等情况：保持现状，reload/切换路径会处理
    }
  },
  getReferencedInputs: (conversationId) => {
    const { nodes, edges } = get();
    // 跳过无向边（directed:false，白板连线无消费语义）
    const upstream = edges.filter(
      (e) => e.target === conversationId && e.directed !== false,
    );
    const inputs: ReferencedInput[] = [];
    for (const edge of upstream) {
      const sourceNode = nodes.find((n) => n.id === edge.source);
      if (!sourceNode) continue;
      // .md 笔记节点：连边引用与 @提及 统一走「引用文件」路径块（模型 read_file 读取，不整文注入）
      if (sourceNode.type === "text") {
        const d = sourceNode.data as unknown as TextData;
        if (d.file) {
          const label = d.title || prefix(d.bodyMd ?? "") || "文本";
          inputs.push({ nodeId: sourceNode.id, label, content: "", file: d.file });
          continue;
        }
      }
      const input = describeNodeAsInput(sourceNode);
      if (!input) continue;
      // 媒体图片（attach 形态）不走连边注入：连边引用是文本语义，图片经 @提及 以附件发送（vision）；
      // 文本类媒体（解析出 body）与文本/搜索/表格一致注入正文
      if (input.attach) continue;
      // label 语义各类型不同：文本取正文前缀、search 取搜索词、table 取标题（与 @提及 显示名一致）
      const label =
        sourceNode.type === "search"
          ? prefix((sourceNode.data as unknown as SearchResultData).query) || "搜索"
          : sourceNode.type === "table"
            ? (sourceNode.data as unknown as TableData).title || "表格"
            : prefix(input.text) || "文本";
      inputs.push({
        nodeId: sourceNode.id,
        label,
        content: input.text,
      });
    }
    return inputs;
  },
  clearError: () => set({ error: null }),
  selectNode: (nodeId) => {
    set({ selectedNodeId: nodeId });
  },
  setEdgeLinkMode: (edgeId, linkMode) => {
    // 只读白板禁编辑；样式切换只改边数据，随画布 debounce 落盘（不入 undo 栈，但作废 redo）
    if (get().readOnly) return;
    touchRedo();
    set((s) => ({
      edges: s.edges.map((e) => (e.id === edgeId ? { ...e, linkMode } : e)),
    }));
    schedulePersist();
  },

  pushUndo: () => {
    undoMgr.push();
  },

  send: async (conversationId, content, attachments = [], mentions = [], fileMentions = []) => {
    const { canvasId, messagesByConv, nodes } = get();
    if (!canvasId) return;
    // 协作独占锁守卫：对话节点被其他对端独占编辑（确定性锁主非本端）时拒绝发送（竞态兜底）
    if (isConversationLockedByPeer(conversationId)) return;

    // AI 消息发送不进 Undo 栈（业务操作），但作废 redo：undo 后发送的新消息不得被 Ctrl+Y 抹除
    touchRedo();

    // 让路：中止本对话的自动命名请求（防其占用后端槽位与新消息排队；不误伤其他对话的命名）
    abortAutoTitle(conversationId);

    // 发送时自动连线（防御）：输入框 @提及 但尚未建边的引用此刻建立边（正常路径拖线/@picker 已立即建边，
    // 此处仅兜底异常路径）；源节点已被删除的提及不再建边（否则产生悬空边），该引用随之下沉丢弃
    const edgesNow = get().edges;
    const missing = mentions.filter(
      (m) =>
        !edgesNow.some(
          (e) => e.target === conversationId && e.source === m.nodeId,
        ) && nodes.some((n) => n.id === m.nodeId),
    );
    if (missing.length) {
      set({
        edges: [
          ...edgesNow,
          ...missing.map((m) =>
            withHandles(
              {
                id: crypto.randomUUID(),
                source: m.nodeId,
                target: conversationId,
                sourceHandle: null,
                targetHandle: null,
              },
              nodes,
            ),
          ),
        ],
      });
      schedulePersist();
    }

    // @提及 → 引用：文本节点正文 / 媒体附件就地替换注入；.md 笔记节点改为只发文件路径
    // （消息顶部「引用文件」块，模型用 read_file 读取正文），@标题 文本位保留不动。
    // 用 scanMentionHits 按命中实例精确处理（重复 @提及 时不错位），文本替换从后往前避免位置漂移。
    let finalContent = content;
    const mentionRefs: ReferencedInput[] = [];
    const mentionAtts: PendingAttachment[] = [];
    const fileRefs: Array<{ nodeId: string; label: string; file: string }> = [];
    const hits = scanMentionHits(content, mentions);
    for (let i = hits.length - 1; i >= 0; i--) {
      const { start, end, mention: m } = hits[i];
      const node = nodes.find((n) => n.id === m.nodeId);
      if (!node) continue;
      const label = m.text.slice(1);
      // .md 笔记节点：@引用 只发文件路径（保留 @标题 原位），模型按需 read_file 读取；
      // 仍记入 mentionRefs（气泡 @chip + 防连边引用重复注入）
      if (node.type === "text") {
        const d = node.data as unknown as TextData;
        if (d.file) {
          fileRefs.push({ nodeId: node.id, label, file: d.file });
          mentionRefs.push({ nodeId: node.id, label, content: "" });
          continue;
        }
      }
      const input = describeNodeAsInput(node);
      if (!input) continue;
      if (input.attach) {
        // 图片：文本位替换为文件名，图片本体随附件（vision）发送。
        // 引用此节点的附件已在托盘（拖线/@picker 均同时进托盘 + 输入框 @标记），
        // 这里只做文本位替换，不重复 push——否则同一图片会发两张。
        finalContent =
          finalContent.slice(0, start) + input.text + finalContent.slice(end);
        if (!attachments.some((a) => a.sourceNodeId === node.id)) {
          mentionAtts.push({
            id: crypto.randomUUID(),
            ...input.attach,
            sourceNodeId: node.id,
          });
        }
      } else {
        finalContent =
          finalContent.slice(0, start) + input.text + finalContent.slice(end);
        mentionRefs.push({ nodeId: node.id, label, content: input.text });
      }
    }
    mentionRefs.reverse(); // 从后往前处理后恢复按出现顺序
    mentionAtts.reverse(); // 同 mentionRefs：多图片附件保持 @提及 出现顺序

    // 连边通道：与 @提及 通道遍历的是同一引用集合（「引用即边」——每个 @提及 都建边），
    // 是注入的两半——@提及 通道按 @标签 位置原位注入；这里只补「有边但当前消息无 @标签 且从未消费」的引用。
    // 交互建的边首轮发送即被 @提及 消费，不会走到此分支；仅导入/旧画布/协作遗留的孤儿边命中。
    const edgeRefs = get().getReferencedInputs(conversationId);
    const already = new Set(mentionRefs.map((r) => r.nodeId));
    // 已消费（历史消息已注入过）的引用不重复注入——边消费后保留为实线，若不过滤，
    // 后续每条消息都会重拼 [引用：…] 前缀并在气泡里累计同样的 @chip
    const historyMsgs = messagesByConv[conversationId] ?? [];
    const extraRefs = edgeRefs.filter(
      (r) => !already.has(r.nodeId) && !isAssetConsumed(historyMsgs, r.nodeId),
    );
    // 连边引用分区：.md 笔记（file）并入「引用文件」路径块；非文件节点（画布内文本/搜索/表格）整文注入
    const edgeFileRefs: Array<{ nodeId: string; label: string; file: string }> = [];
    const contentRefs: ReferencedInput[] = [];
    for (const r of extraRefs) {
      if (r.file) edgeFileRefs.push({ nodeId: r.nodeId, label: r.label, file: r.file });
      else contentRefs.push(r);
    }
    const refText = contentRefs
      .map((r) => r.content)
      .filter(Boolean)
      .join("\n\n");

    // 「引用文件」路径块：@提及 .md（已恢复 @出现顺序）+ 连边 .md + 纯路径引用合并，同路径去重——
    // 与 FILE_REFERENCE_PROMPT 引导对应，模型据此用 read_file 读取正文（目录标注由拼装处统一加）
    const seenNodeIds = new Set<string>();
    const uniqueFileRefs = [...fileRefs.reverse(), ...edgeFileRefs].filter((r) => {
      if (seenNodeIds.has(r.nodeId)) return false;
      seenNodeIds.add(r.nodeId);
      return true;
    });
    const seenPaths = new Set<string>();
    const refPaths = [...uniqueFileRefs.map((r) => r.file), ...fileMentions.map((fm) => fm.file)].filter(
      (p) => {
        if (seenPaths.has(p)) return false;
        seenPaths.add(p);
        return true;
      },
    );
    finalContent = assembleContentWithRefs(finalContent, refPaths, refText ? [refText] : []);
    const refs = [...extraRefs, ...mentionRefs];
    const allAttachments = [...attachments, ...mentionAtts];

    // 影子节点预分配：无来源（临时附件，非画布节点引用）的附件发送后归档为画布媒体节点。
    // 提前分配节点 id 并写回消息附件的 sourceNodeId——DataFlowEdge 按
    // attachments.sourceNodeId 反推「已消费」命中（附件已随本消息注入），影子节点边显示实线。
    const shadowIds = new Map<PendingAttachment, string>();
    for (const a of allAttachments) {
      if (!a.sourceNodeId) shadowIds.set(a, crypto.randomUUID());
    }

    // 追加 user 消息
    const { id: userId, ts: userTs } = nowId();
    const userMsg: Message = {
      id: userId,
      role: "user",
      content: finalContent,
      // 气泡显示原始输入（含 @提及 标记），避免展示就地替换后的一大篇正文
      displayContent: content,
      createdAt: userTs,
      attachments: allAttachments.length
        ? allAttachments.map((a) => {
            const { kind, payload, mime, filename, sourceNodeId } = a;
            return {
              kind,
              payload,
              mime,
              filename,
              sourceNodeId: sourceNodeId ?? shadowIds.get(a),
            };
          })
        : undefined,
      refs: refs.length || fileMentions.length
        ? [
            ...refs.map((r) => ({ nodeId: r.nodeId, label: r.label })),
            // 纯路径引用：nodeId 用 file: 前缀合成（气泡 chip 去重键），file 供点击按类型打开；
            // 路径已被节点引用覆盖的跳过（同一引用不出双 chip，物质化改写后两键相同）
            ...fileMentions
              .filter((fm) => !uniqueFileRefs.some((r) => r.file === fm.file))
              .map((fm) => ({
                nodeId: `file:${fm.file}`,
                label: fm.label,
                file: fm.file,
              })),
          ]
        : undefined,
    };
    set({
      messagesByConv: {
        ...messagesByConv,
        [conversationId]: [...(messagesByConv[conversationId] ?? []), userMsg],
      },
    });

    // 持久化 user 消息（随画布自动保存增量落盘）
    schedulePersist();

    // 影子节点：无来源的附件发送后静默归档为媒体节点，
    // 属业务操作，不进 Undo 栈，随画布自动保存落库。
    // 生成位置：对话节点左侧，用 findFreeSpot 自适应避开已有节点（含本批已添加的）。
    const shadows = allAttachments.filter((a) => !a.sourceNodeId);
    if (shadows.length) {
      const convNode = get().nodes.find((n) => n.id === conversationId);
      if (convNode) {
        const baseX = convNode.position.x - 310;
        const newNodes = [...get().nodes];
        const newEdges = [...get().edges];
        shadows.forEach((a) => {
          // 用消息附件里已写回的 shadowIds（保证 id 一致，消费反推能命中）
          const mediaId = shadowIds.get(a) ?? crypto.randomUUID();
          const spot = findFreeSpot(
            newNodes,
            { x: baseX, y: convNode.position.y },
            { w: 260, h: 240 },
          );
          newNodes.push({
            id: mediaId,
            type: "media",
            position: spot,
            data: {
              mime: a.mime,
              kind: a.kind,
              name: a.filename,
              thumb: a.kind === "image" ? a.payload : undefined,
              body: a.kind === "file" ? a.payload : undefined,
              parseFailed: a.parseFailed,
            },
          });
          newEdges.push(
            withHandles(
              {
                id: crypto.randomUUID(),
                source: mediaId,
                target: conversationId,
                sourceHandle: null,
                targetHandle: null,
              },
              newNodes,
            ),
          );
        });
        set({ nodes: newNodes, edges: newEdges });
        schedulePersist();
      }
    }

    await runStream(conversationId);
    // LLM 话题自动命名（首轮完成；fire-and-forget 不阻塞发送返回）
    void autoNameConversation(conversationId);
  },

  regenerate: async (conversationId) => {
    const list = get().messagesByConv[conversationId] ?? [];
    // 协作独占锁守卫：同 send（竞态兜底）
    if (isConversationLockedByPeer(conversationId)) return;
    // 重新生成不入 Undo 栈（业务操作），但作废 redo（同 send 语义）
    touchRedo();
    let lastUserIdx = -1;
    let lastAsstIdx = -1;
    for (let i = list.length - 1; i >= 0; i--) {
      if (lastAsstIdx < 0 && list[i].role === "assistant") lastAsstIdx = i;
      if (lastUserIdx < 0 && list[i].role === "user") lastUserIdx = i;
      if (lastUserIdx >= 0 && lastAsstIdx >= 0) break;
    }
    if (lastUserIdx < 0) return;

    // 最后一条 user 消息若引用了资产，用最新状态重建（文件路径/文本/媒体内容与附件都可能已被
    // 编辑或外部修改，重新生成应基于最新上下文而非发送时的快照）。重建后随 .atlx 持久化。
    // displayContent 是重建基底（原始输入含 @标记）；旧消息无该字段时跳过（无法反推 @位置）。
    let nextList = list;
    const userMsg = list[lastUserIdx];
    if (userMsg.displayContent && userMsg.refs?.length) {
      const rebuilt = rebuildUserContent(userMsg, get().nodes);
      const changed =
        rebuilt.content !== userMsg.content ||
        JSON.stringify(rebuilt.attachments) !==
          JSON.stringify(userMsg.attachments);
      if (changed) {
        nextList = nextList.map((m) =>
          m.id === userMsg.id
            ? {
                ...m,
                content: rebuilt.content,
                attachments: rebuilt.attachments,
              }
            : m,
        );
      }
    }

    // 移除最后一条 assistant（内存 + 随 .atlx 持久化），随后重发最后一条 user。
    // 注意：重建与移除必须在同一份 nextList 上完成后再一次性 set——
    // 若各自基于旧 list 两次 set，第二次会覆盖重建结果（AI 仍用旧引用内容）。
    if (lastAsstIdx > lastUserIdx) {
      const asst = nextList[lastAsstIdx];
      nextList = nextList.filter((m) => m.id !== asst.id);
    }
    if (nextList !== list) {
      set({
        messagesByConv: {
          ...get().messagesByConv,
          [conversationId]: nextList,
        },
      });
      schedulePersist();
    }
    await runStream(conversationId);
  },

  rollbackTo: (conversationId, messageId) => {
    const list = get().messagesByConv[conversationId] ?? [];
    const idx = list.findIndex((m) => m.id === messageId);
    if (idx < 0 || idx === list.length - 1) return;
    get().pushUndo();
    set((state) => ({
      messagesByConv: {
        ...state.messagesByConv,
        [conversationId]: list.slice(0, idx + 1),
      },
    }));
    schedulePersist();
  },

  branchFrom: async (conversationId, upToMessageId, position) => {
    const { nodes, edges, messagesByConv } = get();
    const parent = nodes.find((n) => n.id === conversationId);
    if (!parent) return;
    const parentData = parent.data as Partial<ConversationData>;
    const src = messagesByConv[conversationId] ?? [];

    // 以目标 AI 回复（含）为止的全部完整状态创建分支：排除空占位，保留仅含附件无文本的消息
    const upToIdx = src.findIndex((m) => m.id === upToMessageId);
    if (upToIdx < 0) return;
    const copied = src
      .slice(0, upToIdx + 1)
      .filter(
        (m) => assistantReplyText(m).trim() !== "" || (m.attachments?.length ?? 0) > 0,
      );

    // 单次 undo 事务：节点 + 边 + 消息复制
    get().pushUndo();

    const childId = crypto.randomUUID();
    const baseTs = Date.now();
    const childMsgs = copied.map((m, i) => ({
      id: crypto.randomUUID(),
      role: m.role,
      content: m.content,
      // 分支保留完整步骤（叙述/工具/思考）：叙述-only 消息正文在 steps，缺省则分支显示为空
      steps: m.steps,
      attachments: m.attachments,
      refs: m.refs,
      createdAt: baseTs + i,
    }));

    const childNode = {
      id: childId,
      type: "conversation",
      position,
      // 分支节点继承默认对话尺寸（与新节点一致），用户可 resize 覆盖
      width: DEFAULT_CONVERSATION_WIDTH,
      height: DEFAULT_CONVERSATION_HEIGHT,
      data: {
        providerId: parentData.providerId ?? "",
        model: parentData.model ?? "",
        // 分支继承 Agent（子节点独立演化，改动不影响父节点）
        agentId: parentData.agentId,
        // 分支继承节点级推理等级（与模型同语义；子节点可独立改，不影响父节点）
        reasoningEffort: parentData.reasoningEffort,
      },
    };
    // 父→子有向边仅表分支血缘：对话→对话边不进 getReferencedInputs、不生成 @chip，故无数据交互
    const childEdge = withHandles(
      {
        id: crypto.randomUUID(),
        source: conversationId,
        target: childId,
        sourceHandle: null,
        targetHandle: null,
      },
      nodes,
    );

    set({
      nodes: [...nodes, childNode],
      edges: [...edges, childEdge],
      messagesByConv: { ...messagesByConv, [childId]: childMsgs },
    });

    // 文件化后 messages 嵌对话节点 data，无 FK 约束，随画布自动保存增量落盘
    schedulePersist();

    // 分支继承父对话的任务清单（分支 = 同一线程的延续，模型在分支上可见父侧已规划进度；
    // 写失败静默降级，不阻塞建分支）
    void readAgentTodos(conversationId)
      .then((todos) => writeAgentTodos(childId, todos))
      .catch(() => {});
  },
  abort: (conversationId) => {
    abortControllers.get(conversationId)?.abort();
  },
  resetCanvasState: () => {
    // 删除当前画布：取消未落盘保存定时器 + 中止流，再复位全部画布态——
    // 否则残留 saveTimer 会重写已删的 .atlx、watcher 事件匹配旧 id 产生误导 reload
    persistCtl.cancel();
    abortAllStreams();
    groupDragState = null;
    dragInProgress = false;
    set({
      canvasId: null,
      canvasFile: null,
      canvasTitle: "",
      nodes: [],
      edges: [],
      readOnly: false,
      messagesByConv: {},
      streamingByConv: {},
      lockedConversations: {},
      pendingMentionsByConv: {},
      error: null,
      selectedNodeId: null,
      saving: false,
      dirty: false,
      conflictPending: false,
      baseUpdatedAt: 0,
    });
    undoMgr.clear();
    syncLastSaved();
  },
  canvasHistoryLoad: (file) => loadCanvasHistory("canvas", file),

  canvasHistoryRollback: async (file, seq) => {
    // 仅当前打开画布可回滚（随后重载内存态）；file 须匹配当前画布
    if (get().canvasFile !== file) return null;
    const versions = await loadCanvasHistory("canvas", file);
    const content = versionContentAt(versions, seq);
    if (content == null) return null;
    try {
      const snapshot = JSON.parse(content) as CanvasFile;
      if (!snapshot || snapshot.schema !== CANVAS_SCHEMA) return null;
      // title 即文件名：以当前文件名归一化快照 title（防回滚旧版本触发文件名回跳/漂移）
      const currentTitle =
        file.split("/").pop()?.replace(/\.atlx$/i, "") ?? snapshot.title;
      // 取消挂起的防抖保存：回滚写盘后旧 timer 不得把恢复前内容写回
      persistCtl.cancel();
      // 全量写回快照（baseUpdatedAt 缺省 = 绕过乐观锁——回滚是显式覆盖，显式用户意图）
      await writeCanvasVault({ ...snapshot, title: currentTitle }, file, undefined);
      // 抑制 watcher 回波，然后重载内存（含乐观锁基准与撤销栈重置，同冲突「重载」语义）
      markSelfSave(file);
      set({ dirty: false });
      await get().reloadFromDisk();
      // 回滚记一条 restore 版本（滚动恢复点 + 审计「何时回滚到哪」）
      await recordHistoryVersion("canvas", file, {
        content,
        action: "restore",
        summarize: summarizeCanvasSnapshot,
      });
      return content;
    } catch (e) {
      console.error("画布回滚失败", e);
      return null;
    }
  },
  retrySearch: async (nodeId, query) => {
    const settings = useSettingsStore.getState();
    const data = await runSearch(settings.searchConfig, query);
    // runSearch 不抛异常（失败降级为 error 字段），直接覆盖节点 data 并落盘
    get().updateNodeData(nodeId, { ...data, query });
  },
  deleteSelected: () => {
    // 只读画布（外部白板格式）禁止删除节点
    if (get().readOnly) return;
    const { nodes } = get();
    const selectedNodeIds = new Set(
      // 协作：被其他对端独占编辑的对话节点禁删（锁作用范围，防误删进行中的编辑）
      nodes
        .filter((n) => n.selected && !(n.type === "conversation" && isConversationLockedByPeer(n.id)))
        .map((n) => n.id),
    );
    // 连接后不可手动断开：Delete 只删节点，不再删除单独选中的边（连到被删节点的边随节点删除）。
    // 例外：无向关联边可单独删除（选中边后 Delete，关联线不表达数据流，无引用语义）
    const selectedUndirectedEdgeIds = get()
      .edges.filter((e) => e.selected && e.directed === false)
      .map((e) => e.id);
    if (selectedNodeIds.size === 0 && selectedUndirectedEdgeIds.length === 0) return;
    get().deleteNodes([...selectedNodeIds], selectedUndirectedEdgeIds);
  },

  deleteNodes: (ids, extraEdgeIds = []) => {
    // 只读画布（外部白板格式）禁止删除节点
    if (get().readOnly) return;
    const { nodes, edges, onNodesChange, onEdgesChange } = get();
    // 协作：被其他对端独占编辑的对话节点禁删（锁作用范围，防误删进行中的编辑）
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const nodeIds = new Set(
      ids.filter((id) => {
        const n = byId.get(id);
        return !!n && !(n.type === "conversation" && isConversationLockedByPeer(id));
      }),
    );
    // 连到被删节点的边随节点删除（与 deleteSelected 同语义）；extraEdgeIds 为调用方指定删除的边
    const orphanEdgeIds = edges
      .filter((e) => nodeIds.has(e.source) || nodeIds.has(e.target))
      .map((e) => e.id);
    const allEdgeIds = new Set([...orphanEdgeIds, ...extraEdgeIds]);
    if (nodeIds.size === 0 && allEdgeIds.size === 0) return;

    // 记录被删的对话节点 id，删除后清理其消息内存
    // （持久化层：messages 嵌对话节点，随画布自动保存增量落盘，删节点即删其消息）
    const deletedConvIds = nodes
      .filter((n) => nodeIds.has(n.id) && n.type === "conversation")
      .map((n) => n.id);
    // 注意：不清理对话节点的任务清单侧车——本删除可 undo（pushUndo 恢复节点/消息），
    // 若删侧车则 Ctrl+Z 后对话回来但清单永久丢失；孤儿侧车隐藏且无 watcher 回波，可接受。

    // 删除流式中的对话节点：先 abort（否则流无法再被中止，onDone 还会往已删节点写增量）
    if (deletedConvIds.length) {
      for (const cid of deletedConvIds) abortControllers.get(cid)?.abort();
      set((state) => {
        const next = { ...state.streamingByConv };
        for (const cid of deletedConvIds) delete next[cid];
        return { streamingByConv: next };
      });
    }

    get().pushUndo();

    if (nodeIds.size > 0) {
      onNodesChange([...nodeIds].map((id) => ({ type: "remove" as const, id })));
    }
    if (allEdgeIds.size > 0) {
      onEdgesChange([...allEdgeIds].map((id) => ({ type: "remove" as const, id })));
    }
    if (deletedConvIds.length) {
      set((state) => {
        const nextMessages = { ...state.messagesByConv };
        const nextMentions = { ...state.pendingMentionsByConv };
        for (const id of deletedConvIds) {
          delete nextMessages[id];
          // 未消费的引用队列一并清理（节点删除后无消费方，防陈旧队列残留）
          delete nextMentions[id];
        }
        return {
          messagesByConv: nextMessages,
          pendingMentionsByConv: nextMentions,
        };
      });
    }
  },

  // ===== Undo / Redo =====

  undo: () => {
    if (!undoMgr.canUndo) return;
    // 流式期间 undo 会撕裂消息状态（占位消息不在旧快照中，后续 delta 丢失）→ 先中止流
    abortAllStreams();
    undoMgr.undo();
    schedulePersist();
  },
  redo: () => {
    if (!undoMgr.canRedo) return;
    abortAllStreams();
    undoMgr.redo();
    schedulePersist();
  },

  // ===== 多人实时协作（presence + canvas-patch 补丁）=====

  acquireConversationLock: (conversationId) => {
    if (get().lockedConversations[conversationId] !== undefined) return;
    set((st) => ({
      lockedConversations: {
        ...st.lockedConversations,
        [conversationId]: Date.now(),
      },
    }));
  },

  releaseConversationLock: (conversationId) => {
    if (get().lockedConversations[conversationId] === undefined) return;
    set((st) => {
      const next = { ...st.lockedConversations };
      delete next[conversationId];
      return { lockedConversations: next };
    });
  },

  clearConversationLocks: () => {
    if (Object.keys(get().lockedConversations).length === 0) return;
    set({ lockedConversations: {} });
  },

  setCollabBroadcast: (fn) => {
    collabBroadcast = fn;
  },

  applyRemoteCanvasPatch: (file, patch) => {
    // 只应用当前打开的画布；补丁画布 id 不符（陈旧/串文件）拒绝——防污染本端状态
    const s = get();
    // 只读画布（外部白板格式）不接受协作补丁（内容来自原 .canvas，非共享 .atlx）
    if (file !== s.canvasFile || patch.id !== s.canvasId || s.readOnly) return;
    const removedNodeIds = new Set(patch.removedNodeIds);
    const removedEdgeIds = new Set(patch.removedEdgeIds);
    // 反解远端节点（conversation 提取 messages / text 标记补读正文 / group 补 zIndex）
    const deserialized = patch.upsertNodes.map(deserializeNodeForCollab);
    // 重命名（title 变化）：同步 canvasTitle + 路径漂移（canvasFile/appStore.currentCanvasFile）+
    // 登记协作重命名抑制（watcher 旧 delete/新 create 事件跳过 reload/conflict，防本地脏编辑被重载打断）
    let renameNewFile: string | null = null;
    if (patch.title && patch.title !== s.canvasTitle && s.canvasFile) {
      renameNewFile = siblingPath(s.canvasFile, `${sanitizeFilename(patch.title)}.atlx`);
      markCollabCanvasRename([s.canvasFile, renameNewFile]);
    }
    set((st) => {
      // 与 Rust patch_canvas_vault 同语义：removed 过滤 → upsert 按 id 覆盖/追加
      const nodes = st.nodes.filter((n) => !removedNodeIds.has(n.id));
      for (const { node } of deserialized) {
        const i = nodes.findIndex((x) => x.id === node.id);
        if (i >= 0) {
          // 远端结构补丁省略、由接收端自行补读的内容，覆盖前保留本端既有值防瞬时空白：
          // - 文件型 text 节点：正文在共享盘 `.md`，补丁只带 { title, file } 不带 bodyMd。
          //   直接剥离会让接收端节点空白，且 stripped 空值会被 refreshTextContent 的 keep
          //   判定误认为「用户清空过正文」而跳过刷新（持久空白）；保留后 refreshTextContent
          //   以「补丁前正文」与磁盘比对：一致跳过 / 磁盘更新则刷新。
          // - table 节点：快照摘要不在补丁（在共享盘 `.atb`），保留本端既有 snapshot 防画布
          //   节点空白，后续由表格 watcher 分支 refreshTableContent 补读最新。
          if (node.type === "text" || node.type === "table") {
            const nodeData = node.data as Record<string, unknown>;
            const localData = nodes[i].data as Record<string, unknown>;
            const field = node.type === "text" ? "bodyMd" : "snapshot";
            if (nodeData[field] === undefined && localData[field] !== undefined) {
              node.data = { ...nodeData, [field]: localData[field] } as unknown as Node["data"];
            }
          }
          nodes[i] = node;
        } else {
          nodes.push(node);
        }
      }
      const edges = st.edges.filter((e) => !removedEdgeIds.has(e.id));
      for (const e of patch.upsertEdges) {
        const runtimeEdge: CanvasEdge = {
          id: e.id,
          source: e.source,
          target: e.target,
          sourceHandle: e.sourceHandle ?? null,
          targetHandle: e.targetHandle ?? null,
          directed: e.directed,
          linkMode: e.linkMode,
        };
        const i = edges.findIndex((x) => x.id === e.id);
        if (i >= 0) edges[i] = runtimeEdge;
        else edges.push(runtimeEdge);
      }
      // 消息：conversation 节点消息按 id 合并（远端基底 + 本地独有补入），保护本端进行中/流式
      // 消息不被对端陈旧节点快照覆盖（锁模型下对端不并发写消息，此为兜底）；
      // 远端删除对话节点 → 清理对应消息键
      const messagesByConv = { ...st.messagesByConv };
      for (const id of removedNodeIds) delete messagesByConv[id];
      for (const { node, messages } of deserialized) {
        if (node.type !== "conversation" || messages === undefined) continue;
        const merged = mergeMessages(messages, messagesByConv[node.id] ?? []);
        if (merged !== messagesByConv[node.id]) messagesByConv[node.id] = merged;
      }
      // 对端删除使本端选中失效：清理（防高亮残留，与表格同策略）
      const selectedNodeId =
        st.selectedNodeId && removedNodeIds.has(st.selectedNodeId) ? null : st.selectedNodeId;
      return {
        nodes,
        edges,
        messagesByConv,
        selectedNodeId,
        ...(renameNewFile ? { canvasTitle: patch.title, canvasFile: renameNewFile } : {}),
      };
    });
    if (renameNewFile && useAppStore.getState().currentCanvasFile === s.canvasFile) {
      useAppStore.setState({ currentCanvasFile: renameNewFile });
    }
    // 对端删除本端持锁的节点（锁模型下不应发生，防御性释放）+ 远端 text 节点正文补读（共享盘 .md）
    for (const id of removedNodeIds) {
      if (s.lockedConversations[id] !== undefined) get().releaseConversationLock(id);
    }
    for (const { refreshBodyMdFile } of deserialized) {
      if (refreshBodyMdFile) void get().refreshTextContent(refreshBodyMdFile);
    }
    // 核心：远端已应用内容对房间已知，推进广播基线，避免被当作本地增量重发全房
    syncBroadcastBaseline();
  },
}));
