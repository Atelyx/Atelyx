import { Loader2, Lock, Plus, RefreshCw, Scissors, X } from "lucide-react";
import { useEffect, useCallback, useMemo, useRef, useState } from "react";
import { useReactFlow, type NodeProps } from "@xyflow/react";
import { useShallow } from "zustand/react/shallow";
import { ResizeHandle } from "./ResizeHandle";
import { useCanvasStore, nodeFileOf } from "@/stores/canvasStore";
import { useCollabStore } from "@/stores/collabStore";
import { useSettingsStore, selectDefaultModelDisplay } from "@/stores/settingsStore";
import { useNodeCollab } from "@/hooks/useNodeCollab";
import { resolveLockState } from "@/utils/canvasCollab";
import { useAutoScrollFollow } from "@/hooks/useAutoScrollFollow";
import { DEFAULT_CONVERSATION_WIDTH,
  DEFAULT_CONVERSATION_HEIGHT,
  DEFAULT_TEXT_NODE_WIDTH,
  DEFAULT_TEXT_NODE_HEIGHT,
} from "@/constants/canvas";
import { ERROR_PREFIX } from "@/constants/chat";
import { isAssetConsumed } from "@/utils/consumed";
import { findFreeSpot } from "@/utils/layout";
import { useNotificationStore } from "@/stores/notificationStore";
import {
  mentionTextOf,
  prefix,
  splitMentions,
  type MentionSeg,
} from "@/utils/text";
import type {
  ConversationData,
  MediaData,
  Message,
  PendingAttachment,
  Attachment,
} from "@/types";
import type { Node as FlowNode } from "@xyflow/react";
import { VaultAtPicker, type VaultPickTarget } from "@/components/common/VaultAtPicker";
import { openVaultPath } from "@/components/common/FileKindIcon";
import { noteTitleFromFile } from "@/utils/filename";
import { ConversationAttachmentTray } from "./ConversationAttachmentTray";
import { ConnectionFrame } from "./ConnectionFrame";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { ModelSelect } from "@/components/common/ModelSelect";
import { Menu, MenuItem } from "@/components/common/Menu";
import { ChatMessageBubble } from "@/components/common/ChatMessageBubble";
import type { MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";
import { MentionTextarea } from "@/components/common/MentionTextarea";
import { JumpToBottomButton } from "@/components/common/JumpToBottomButton";
import { useInlineEdit } from "@/hooks/useInlineEdit";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import { useWikiNodeLocate } from "@/hooks/useWikiNodeLocate";
import { assistantReplyText } from "@/utils/agentSteps";

/** 模块级空消息数组，避免 selector 每次返回新引用导致 React 无限循环。 */
const EMPTY_MESSAGES: Message[] = [];
const FALSE = false as const;
/** 拖线引用队列的空数组占位（selector 稳定引用）。 */
const EMPTY_PENDING: string[] = [];

/** 附件 → 媒体节点 data（影子节点固定 / 历史附件拉出共用）；参数为结构化最小集，PendingAttachment / Attachment 同构皆可传入。
 *  只带 `file` 引用：图片/文本内容由节点渲染按引用读回（内嵌 base64 会让画布文件膨胀）。 */
function toMediaData(att: {
  mime: string;
  kind: "image" | "file";
  file?: string;
  payload?: string;
  filename?: string;
  parseFailed?: boolean;
}): MediaData & Record<string, unknown> {
  return {
    file: att.file,
    mime: att.mime,
    kind: att.kind,
    name: att.filename,
    // 内容缓存按引用读回的现有值透传（落盘时被剥离）：无引用（历史内嵌附件）时它就是唯一内容来源
    thumb: att.kind === "image" ? att.payload : undefined,
    body: att.kind === "file" ? att.payload : undefined,
    parseFailed: att.parseFailed,
  };
}

/**
 * 画布媒体节点 → 待发送托盘附件（只带 `file` 引用 + 已读到的内容缓存）。
 * sourceNodeId 供 DataFlowEdge「已消费」反推与发送后归档影子节点；三处进托盘通道
 * （媒体源连边 / 拖线 / @picker 选中）共用，防各自手写拷贝后行为分叉。
 */
function mediaAttachmentFrom(n: FlowNode): PendingAttachment {
  const md = n.data as unknown as MediaData;
  return {
    id: crypto.randomUUID(),
    kind: md.kind,
    payload: md.kind === "image" ? (md.thumb ?? "") : (md.body ?? ""),
    ...(md.file ? { file: md.file } : {}),
    mime: md.mime ?? "",
    filename: md.name,
    sourceNodeId: n.id,
    parseFailed: md.parseFailed,
  };
}

/** 同源节点已在托盘则不重复进（拖线/picker/连边多通道可能重复触发同一节点）。 */
function appendMediaAttachment(
  prev: PendingAttachment[],
  node: FlowNode,
): PendingAttachment[] {
  if (prev.some((a) => a.sourceNodeId === node.id)) return prev;
  return [...prev, mediaAttachmentFrom(node)];
}

/** 画布消息 refs → chip 去重键（nodeId）：模块级稳定函数，供 ChatMessageBubble memo 生效 */
const refKeyOfNodeRef = (r: { label: string }) =>
  (r as unknown as { nodeId: string }).nodeId;

/**
 * 对话节点。
 * 消息列表 + 输入框 + 流式渲染 + Markdown。
 * - 输入框支持粘贴/拖拽附件 → 待发送托盘
 * - @ 提及引用画布资产：@chips 常驻显示入边引用
 * - 连接边框：鼠标移到节点边缘渐显连接圆点，从边缘拉线接入引用 / 拖线引用（发送时自动连线）
 */
export function ConversationNode({ id, width, height, selected }: NodeProps) {
  const hasFixedHeight = height != null;
  const messages = useCanvasStore(
    (s) => s.messagesByConv[id] ?? EMPTY_MESSAGES,
  );
  const streaming = useCanvasStore((s) => s.streamingByConv[id] ?? FALSE);
  // 协作：本节点远端选中/独占锁主/生成中（锁主判定见 useNodeCollab）
  const { lockedByPeer, streamingPeers, iOwnLock } = useNodeCollab(id);
  const collabStreamingPeer = streamingPeers[0];
  const send = useCanvasStore((s) => s.send);
  const regenerate = useCanvasStore((s) => s.regenerate);
  const rollbackTo = useCanvasStore((s) => s.rollbackTo);
  const abort = useCanvasStore((s) => s.abort);
  const branchFrom = useCanvasStore((s) => s.branchFrom);
  const error = useCanvasStore((s) => s.error);
  const clearError = useCanvasStore((s) => s.clearError);
  const addNode = useCanvasStore((s) => s.addNode);
  const addEdge = useCanvasStore((s) => s.addEdge);
  const updateNodeData = useCanvasStore((s) => s.updateNodeData);
  const { fitView } = useReactFlow();
  const providers = useSettingsStore((s) => s.config.providers);
  const defaultModelDisplay = useSettingsStore(selectDefaultModelDisplay);
  const nodeData = useCanvasStore(
    (s) => s.nodes.find((n) => n.id === id)?.data,
  ) as Partial<ConversationData> | undefined;

  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<PendingAttachment[]>([]);
  // 标题：LLM 自动命名 → 回退首条 user 消息前缀 → 「对话」；双击 inline 编辑（空提交 = 清除回退显示）
  const firstUserMsg = messages.find((m) => m.role === "user");
  const displayTitle =
    nodeData?.title ||
    prefix(firstUserMsg?.displayContent ?? firstUserMsg?.content ?? "", 12) ||
    "对话";
  const titleEdit = useInlineEdit({
    value: displayTitle,
    onCommit: (v) => {
      // 协作：节点被其他对端独占编辑时标题属内容（锁作用范围），拒绝提交
      if (lockedByPeer) return;
      const t = v.trim();
      if (t === displayTitle) return;
      updateNodeData(id, { title: t || undefined });
    },
  });
  const [picker, setPicker] = useState<{
    x: number;
    y: number;
    openUp: boolean;
    yBottom: number;
    query: string;
  } | null>(null);
  // 记录 @ 触发时光标位置（@ 尚未插入，插入后 @ 即在该索引），用于精确删除而非只删末尾
  const [atIdx, setAtIdx] = useState(-1);
  // @ 提及映射：输入框内可见的 @显示名 → 源节点 id，发送时按文本就地替换为引用内容
  const [mentions, setMentions] = useState<{ nodeId: string; text: string }[]>(
    [],
  );
  // 纯路径引用：@ 选择的仓库文件/文件夹（画布无对应节点），发送时只并路径进「引用文件」块（nodeId = file:<path>）
  const [fileMentions, setFileMentions] = useState<{ file: string; label: string }[]>([]);
  // 仅订阅「本节点入边的 media 源节点」派生数据（useShallow 保证引用稳定）：
  // 拖拽其他节点（nodes 数组变化但未变节点对象引用保留）时不触发本组件重渲染/重跑 effect
  const mediaSources = useCanvasStore(
    useShallow((s) =>
      s.edges
        .filter((e) => e.target === id)
        .map((e) => s.nodes.find((n) => n.id === e.source))
        .filter((n): n is FlowNode => !!n && n.type === "media"),
    ),
  );

  // 当前画布上有节点引用的文件路径（@ 选择器排序：画布内文件排最前，选中走节点引用流）；
  // useShallow 数组逐项比较，路径集合不变不重渲染
  const canvasFilePaths = useCanvasStore(
    useShallow((s) => {
      const paths: string[] = [];
      for (const n of s.nodes) {
        const f = nodeFileOf(n);
        if (f) paths.push(f);
      }
      return paths;
    }),
  );
  const canvasFiles = useMemo(() => new Set(canvasFilePaths), [canvasFilePaths]);

  // ===== Agent 选择：配置在 设置 → Agent（settingsStore.agents，仓库级 .atelyx/agents.json）；
  // 发送时实时解析系统提示词与工具（canvasStore.runStream 按 agentId 解析）=====
  const agents = useSettingsStore((s) => s.agents);

  const scrollRef = useRef<HTMLDivElement>(null);
  const nodeRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  // ===== 协作独占编辑锁生命周期（锁模型：首次真实输入占锁，失焦无输入/流式结束释放）=====
  const acquireLock = useCallback(() => {
    useCanvasStore.getState().acquireConversationLock(id);
  }, [id]);
  const releaseLock = useCallback(() => {
    useCanvasStore.getState().releaseConversationLock(id);
  }, [id]);
  // 锁释放判定上下文：blur/流式结束用最新 draft/附件/流式态（ref 避免每按键重挂监听）
  const lockCtxRef = useRef({ draft: "", attachments: 0, streaming: false });
  lockCtxRef.current = { draft: input, attachments: attachments.length, streaming };
  // 输入区失焦且无输入/附件且未流式 → 释放锁（「失焦无输入」释放语义）。
  // 用容器合成 onBlur（focusout 冒泡，见下方输入行 div）而非 textareaRef 原生监听——
  // 只读条（lockedByPeer）切回输入行时 textarea 重新挂载，原生监听不随挂载重建会失效 → 锁泄漏
  const handleInputRowBlur = useCallback(() => {
    const ctx = lockCtxRef.current;
    if (ctx.draft === "" && ctx.attachments === 0 && !ctx.streaming) releaseLock();
  }, [releaseLock]);
  // 流式结束（onDone）且无输入/附件 → 释放锁（发送后流式期间持续持有）
  const prevStreamingRef = useRef(streaming);
  useEffect(() => {
    if (prevStreamingRef.current && !streaming) {
      const ctx = lockCtxRef.current;
      if (ctx.draft === "" && ctx.attachments === 0) releaseLock();
    }
    prevStreamingRef.current = streaming;
  }, [streaming, releaseLock]);
  // 节点卸载（删除/切画布/视图切换）→ 释放锁
  useEffect(() => () => releaseLock(), [releaseLock]);

  // ===== 智能滚动跟随：贴底时自动跟随新消息；用户上翻看历史时不被拉走，显示「新消息」回底按钮（与 AI 对话面板共用 hook） =====
  const { handleScroll, jumpToBottom, showJumpToBottom } = useAutoScrollFollow(
    scrollRef,
    [messages],
  );

  // 阻止滚轮事件冒泡到 React Flow（防止画布缩放）
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => e.stopPropagation();
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, []);

  // 画布媒体节点新连入 → 自动进待发送托盘（画布媒体节点通道）。
  // 已通过该对话发送过的附件（含发送时自动归档的影子节点）不再重复进托盘：按**引用**判定——
  // 附件内容不随消息持久化（`payload` 落盘被剥离），引用是重载后仍然稳定的身份。
  // 消息附件里来自媒体节点的还带 `sourceNodeId`：节点「保存到仓库」会把引用从临时区换成仓库路径，
  // 只比引用会让同一张图再进一次托盘（源节点身份也要收进判定集合）。
  useEffect(() => {
    setAttachments((prev) => {
      const sentRefs = new Set(
        (useCanvasStore.getState().messagesByConv[id] ?? []).flatMap((m) =>
          (m.attachments ?? []).flatMap((a) => [a.file, a.sourceNodeId].filter(Boolean)),
        ),
      );
      const added: PendingAttachment[] = [];
      for (const n of mediaSources) {
        if (prev.some((a) => a.sourceNodeId === n.id)) continue;
        // 源节点身份优先：该节点发过就不再进托盘（引用可能已被「保存到仓库」换过形态）
        if (sentRefs.has(n.id)) continue;
        const att = mediaAttachmentFrom(n);
        if (att.file && sentRefs.has(att.file)) continue;
        added.push(att);
      }
      return added.length ? [...prev, ...added] : prev;
    });
  }, [id, mediaSources]);

  // 拖线引用消费：text/media 节点拖线到本对话 → 输入框出现 @标签（媒体同步进托盘），边在发送时自动建立
  const pendingMentions = useCanvasStore(
    (s) => s.pendingMentionsByConv[id] ?? EMPTY_PENDING,
  );
  useEffect(() => {
    if (pendingMentions.length === 0) return;
    const store = useCanvasStore.getState();
    for (const nodeId of pendingMentions) {
      const node = store.nodes.find((n) => n.id === nodeId);
      if (!node) continue;
      // 媒体（图片/文件）连线：只进待发送托盘，**不在输入框出现 @标签**（图片靠托盘附件注入，
      // 无文本占位；text/search 引用才用 @标签 就地替换）
      if (node.type === "media") {
        // 同源节点已在托盘则不重复进（拖线/picker 可能重复触发同一节点）
        setAttachments((prev) => appendMediaAttachment(prev, node));
        continue;
      }
      const mentionText = `@${mentionTextOf(node)}`;
      setInput((prev) => (prev ? prev + " " : "") + mentionText + " ");
      setMentions((prev) => [...prev, { nodeId, text: mentionText }]);
    }
    store.clearPendingMentions(id);
  }, [pendingMentions, id]);

  const clearAttachments = () => setAttachments([]);

  /** 协作锁主校验（读最新 store 状态）：本端是否仍是本节点的确定性锁主。发送前必查——若对端
   * 在锁传播窗口内抢占成功，此时发送会并发写 → 数据丢失（store.send 亦有兜底守卫，但组件
   * 需在清空草稿前拦截，防已输入内容被吞）。无对端声明（含未接入协作）或本端即锁主 → 通过。 */
  const isOwnLockActive = useCallback((): boolean => {
    const { lockedConversations } = useCanvasStore.getState();
    const { peers, myPeerId } = useCollabStore.getState();
    const { owner, lockedByMe } = resolveLockState(
      id,
      lockedConversations[id],
      myPeerId,
      peers,
    );
    return owner === null || lockedByMe;
  }, [id]);

  const handleSend = () => {
    const text = input.trim();
    if ((!text && attachments.length === 0) || streaming) return;
    // 协作锁主校验：非锁主不发送（保留草稿，等锁释放后可继续）
    if (!isOwnLockActive()) return;
    // 持锁发送：流式期间锁持续持有（acquire 幂等，已有锁不刷新 since）
    acquireLock();
    setInput("");
    const atts = attachments;
    const mts = mentions;
    const fms = fileMentions;
    clearAttachments();
    setMentions([]);
    setFileMentions([]);
    void send(id, text, atts, mts, fms);
  };

  // ===== 附件输入：粘贴 / 拖拽 / 选择文件（未入库附件通道） =====

  /** 单文件进托盘：字节落仓库内隐藏临时区（`.atelyx/temp/<canvasKey>/`），托盘只留引用 + 内容缓存。
   *  不内嵌 base64——图片直接进 `.atlx` 会让画布文件涨到几十 MB。
   *  读/写失败必须可见：附件是用户显式动作，静默丢弃等于点击无反应。
   *  内容不是文本（PDF/zip 等）不算失败：空载荷 + `parseFailed` 进托盘（可引用、不注入模型）。 */
  const addFile = (file: File) => {
    // 加附件 = 编辑意图 → 占锁（协作）
    acquireLock();
    const isImage = file.type.startsWith("image/");
    void (async () => {
      try {
        const { file: ref, payload, parseFailed } = await useCanvasStore
          .getState()
          .attachFileFromDisk(file);
        setAttachments((prev) => [
          ...prev,
          {
            id: crypto.randomUUID(),
            kind: isImage ? "image" : "file",
            payload,
            file: ref,
            mime: file.type,
            filename: file.name,
            ...(parseFailed ? { parseFailed: true } : {}),
          },
        ]);
      } catch (e) {
        console.error("添加附件失败", e);
        // 带上具体原因（如「画布未加载」）：固定文案会让用户不知道能不能重试
        const reason = e instanceof Error && e.message ? `：${e.message}` : "";
        useNotificationStore.getState().notify({
          level: "error",
          message: `附件「${file.name}」添加失败${reason}`,
        });
      }
    })();
  };

  /** 多文件批量进托盘（拖拽 / 文件选择共用）。 */
  const addFiles = (files: File[]) => {
    for (const file of files) addFile(file);
  };

  const handlePaste = (e: React.ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of items) {
      if (item.type.startsWith("image/")) {
        e.preventDefault();
        const file = item.getAsFile();
        if (file) addFile(file);
      }
    }
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // 按文件类型分支读法在 addFile 内部完成（图片/文本类）
    addFiles(Array.from(e.dataTransfer?.files ?? []));
  };

  const handleAttachmentRemove = (attId: string) => {
    const att = attachments.find((a) => a.id === attId);
    if (!att) return;
    if (att.sourceNodeId) {
      // 取消引用 = 断开边
      const edge = useCanvasStore
        .getState()
        .edges.find((e) => e.target === id && e.source === att.sourceNodeId);
      if (edge)
        useCanvasStore
          .getState()
          .onEdgesChange([{ type: "remove", id: edge.id }]);
    }
    setAttachments((prev) => prev.filter((a) => a.id !== attId));
  };

  const handlePin = (att: PendingAttachment) => {
    if (att.sourceNodeId) return;
    const { nodes } = useCanvasStore.getState();
    const convNode = nodes.find((n) => n.id === id);
    if (!convNode) return;
    const mediaId = crypto.randomUUID();
    // 对话节点左侧，自适应避开已有节点（避免与既有媒体节点重叠）
    const spot = findFreeSpot(
      nodes,
      { x: convNode.position.x - 310, y: convNode.position.y },
      { w: 260, h: 240 },
    );
    addNode({
      id: mediaId,
      type: "media",
      position: spot,
      data: toMediaData(att),
    });
    addEdge({
      id: crypto.randomUUID(),
      source: mediaId,
      target: id,
      sourceHandle: null,
      targetHandle: null,
    });
    setAttachments((prev) => prev.filter((a) => a.id !== att.id));
  };

  // ===== @ 提及（反向：手动 @ → 自动建边） =====

  /** @标签 原位插入（各插入路径共用同一语义：@ 到光标间过滤词替换、分隔空格、尾随空格、
   *  光标复位到尾随空格后、关闭选择器）；record 回调登记引用映射（节点 mentions / 纯路径 fileMentions）。
   *  插入位置在 `setInput(prev => …)` 内按 `prev` 计算：同一 tick 多条引用入队时（多选拖拽一次引用多个文件）
   *  后一条必须看到前一条已插入后的文本，用渲染期闭包的 `input` 会让后写覆盖先写、@标签丢失。
   *  `atIdx`/光标是「待替换区间」的渲染期事实，同 tick 多次插入不会改变它们（每次插入后都会复位选择器）。 */
  const insertMentionLabel = (mentionText: string, record: () => void) => {
    const caret = textareaRef.current?.selectionStart ?? input.length;
    const insertAt = Math.min(Math.max(atIdx, 0), input.length);
    const end = Math.max(caret, insertAt);
    let caretAfter = 0;
    setInput((prev) => {
      const from = Math.min(Math.max(atIdx, 0), prev.length);
      const to = Math.min(Math.max(end, from), prev.length);
      // 前文非空且不以空白结尾时补分隔空格，标签后恒带一个尾随空格——
      // 保证胶囊前后为空白区，胶囊背景外扩（.mention-capsule）不遮相邻字符
      const before = prev.slice(0, from);
      const sep = before && !/\s$/.test(before) ? " " : "";
      caretAfter = from + sep.length + mentionText.length + 1;
      return prev.slice(0, from) + sep + mentionText + " " + prev.slice(to);
    });
    record();
    // 光标移到尾随空格之后（继续输入不紧贴胶囊）
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) {
        ta.focus();
        ta.setSelectionRange(caretAfter, caretAfter);
      }
    });
    setPicker(null);
    setAtIdx(-1);
  };

  const handlePickerPick = (node: FlowNode) => {
    if (node.type === "media") {
      // 同源节点已在托盘则不重复进（picker 选中后 useEffect 按边再进会重复）
      setAttachments((prev) => appendMediaAttachment(prev, node));
    }
    // 仓库选择器全量列出候选，已连边（同源同向）不重复建边；@标签 仍插入 = 再次引用语义
    const connected = useCanvasStore
      .getState()
      .edges.some((e) => e.source === node.id && e.target === id);
    if (!connected) {
      addEdge({
        id: crypto.randomUUID(),
        source: node.id,
        target: id,
        sourceHandle: null,
        targetHandle: null,
      });
    }
    // 输入框插入可见 @显示名，并记录提及映射供发送时就地替换
    const mentionText = `@${mentionTextOf(node)}`;
    insertMentionLabel(mentionText, () =>
      setMentions((prev) => [...prev, { nodeId: node.id, text: mentionText }]),
    );
  };

  /** 仓库选择器选中：画布上已有节点的文件路由到既有节点引用流（建边/托盘/快照，引用即边）；
   * 无节点的文件与文件夹 → 纯路径引用（@标签 原位插入 + fileMentions，发送只并路径块）。 */
  const handleVaultPick = (t: VaultPickTarget) => {
    if (!t.isDir) {
      const node = useCanvasStore
        .getState()
        .nodes.find((n) => nodeFileOf(n) === t.path);
      if (node) {
        handlePickerPick(node);
        return;
      }
    }
    const label = t.name.toLowerCase().endsWith(".md") ? noteTitleFromFile(t.path) : t.name;
    insertMentionLabel(`@${label}`, () =>
      setFileMentions((prev) => [...prev, { file: t.path, label }]),
    );
  };

  // 胶囊被移除（MentionTextarea 已删文本 + 复位光标）→ 引用层清理：
  // 仅未消费（虚线待发送）的引用边自动断开；已消费（历史实线边，如「再次注入」）
  // 不断边（连接后不可手动断开）；media 附件同步从托盘移除
  const removeMention = (seg: MentionSeg) => {
    const nodeId = seg.mention?.nodeId;
    // 纯路径引用：只清 fileMentions（无节点边/托盘语义）；同路径多枚胶囊按实例删第一处
    if (nodeId?.startsWith("file:")) {
      const path = nodeId.slice(5);
      setFileMentions((prev) => {
        const idx = prev.findIndex((x) => x.file === path);
        if (idx < 0) return prev;
        return prev.filter((_, i) => i !== idx);
      });
      return;
    }
    setMentions((prev) => prev.filter((x) => x !== seg.mention));
    if (!nodeId) return;
    const store = useCanvasStore.getState();
    const consumed = isAssetConsumed(store.messagesByConv[id] ?? [], nodeId);
    if (consumed) return; // 已消费边不可断开（仅移除 @标签 文本）
    const edge = store.edges.find(
      (e) => e.target === id && e.source === nodeId,
    );
    if (edge) store.onEdgesChange([{ type: "remove", id: edge.id }]);
    // media 引用：托盘附件一并移除（取消引用 = 完全取消，与 handleAttachmentRemove 语义对称）
    setAttachments((prev) => prev.filter((a) => a.sourceNodeId !== nodeId));
  };

  // ===== 文本提取（划词右键） =====

  /** 划词菜单（右键时的视口坐标 + 选中文本）；null = 关闭。 */
  const [selMenu, setSelMenu] = useState<{
    x: number;
    y: number;
    text: string;
  } | null>(null);

  const handleMessagesCtx = (e: React.MouseEvent) => {
    const sel = window.getSelection();
    const text = sel?.toString().trim();
    if (!text) return;
    e.preventDefault();
    e.stopPropagation();
    // 视口坐标（Menu 经 PopupLayer portal 到 body 固定定位，无需转节点相对坐标）
    setSelMenu({ x: e.clientX, y: e.clientY, text });
  };

  const extractToTextNode = () => {
    if (!selMenu) return;
    const { nodes } = useCanvasStore.getState();
    const convNode = nodes.find((n) => n.id === id);
    if (!convNode) return;
    const textNodeId = crypto.randomUUID();
    const title = prefix(selMenu.text, 16) || "文本";
    // 画布内文本节点：不落 `.md`，正文随 .atlx 内嵌（右键「保存为笔记」才生成仓库笔记文件）
    // 对话节点右侧，自适应避开已有节点
    const spot = findFreeSpot(
      nodes,
      { x: convNode.position.x + 480, y: convNode.position.y },
      { w: 320, h: 240 },
    );
    addNode({
      id: textNodeId,
      type: "text",
      position: spot,
      // 显式写入默认尺寸（与 addTextNoteFromVault 一致，避免依赖 TextNode 的 ?? 回退）
      width: DEFAULT_TEXT_NODE_WIDTH,
      height: DEFAULT_TEXT_NODE_HEIGHT,
      data: { title, bodyMd: selMenu.text },
    });
    addEdge({
      id: crypto.randomUUID(),
      source: id,
      target: textNodeId,
      sourceHandle: null,
      targetHandle: null,
    });
    setSelMenu(null);
  };

  // ===== 历史图片拉出为媒体节点（与文本提取对称） =====

  const extractToMediaNode = useCallback(
    (att: Attachment) => {
      const { nodes } = useCanvasStore.getState();
      const convNode = nodes.find((n) => n.id === id);
      if (!convNode) return;
      const mediaId = crypto.randomUUID();
      // 对话节点右侧，自适应避开已有节点
      const spot = findFreeSpot(
        nodes,
        { x: convNode.position.x + 480, y: convNode.position.y },
        { w: 260, h: 240 },
      );
      addNode({
        id: mediaId,
        type: "media",
        position: spot,
        data: toMediaData(att),
      });
      addEdge({
        id: crypto.randomUUID(),
        source: id,
        target: mediaId,
        sourceHandle: null,
        targetHandle: null,
      });
    },
    [id, addNode, addEdge],
  );

  // ===== 分支：以点击消息（含）之前的全部完整状态创建子对话节点 =====
  // 分支按钮挂在每条消息气泡下方；父子间仅一条血缘边，无数据交互。
  const handleBranch = useCallback(
    (messageId: string) => {
      const { nodes } = useCanvasStore.getState();
      const convNode = nodes.find((n) => n.id === id);
      if (!convNode) return;
      // 子节点落在父节点右侧，自适应避开已有节点（对话节点默认宽 ~480）
      const spot = findFreeSpot(
        nodes,
        {
          x: convNode.position.x + DEFAULT_CONVERSATION_WIDTH + 40,
          y: convNode.position.y,
        },
        { w: DEFAULT_CONVERSATION_WIDTH, h: DEFAULT_CONVERSATION_HEIGHT },
      );
      void branchFrom(id, messageId, spot);
      // 聚焦到新节点，避免落在视野外
      setTimeout(() => fitView({ duration: 200, padding: 0.2 }), 60);
    },
    [id, branchFrom, fitView],
  );

  // 点击 user 消息气泡里的 @chip → 定位视图到引用的源节点；纯路径引用（file:<path>）画布无节点，按类型打开文件
  const handleLocateRef = useCallback(
    (nodeId: string) => {
      if (nodeId.startsWith("file:")) {
        openVaultPath(nodeId.slice(5));
        return;
      }
      fitView({ nodes: [{ id: nodeId }], duration: 200, padding: 0.2 });
    },
    [fitView],
  );

  // [[wiki 链接]] 定位 + 笔记链接打开/新建（公共接线簇，见 hooks/useWikiNodeLocate、useVaultLinkHandlers）
  const { isWikiLocatable, handleLocateWiki } = useWikiNodeLocate();
  const {
    handleOpenWikiNote,
    isVaultPathNote,
    handleOpenVaultPathNote,
    handleCreateNote,
  } = useVaultLinkHandlers();

  // ===== 渲染 =====

  // assistant 消息的链接/定位回调：useMemo 稳定化（气泡 memo 生效前提，流式期间历史消息不重渲染）
  const messageMarkdownLinks = useMemo<MarkdownEditorLinks>(
    () => ({
      onOpenNote: handleOpenWikiNote,
      isVaultPathNote,
      onOpenVaultPathNote: handleOpenVaultPathNote,
      onCreateNote: handleCreateNote,
      isLocatable: isWikiLocatable,
      onLocate: handleLocateWiki,
    }),
    [handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote, isWikiLocatable, handleLocateWiki],
  );
  const handleRollback = useCallback(
    (messageId: string) => rollbackTo(id, messageId),
    [id, rollbackTo],
  );

  const last = messages[messages.length - 1];
  const canRegenerate =
    !!last &&
    last.role === "assistant" &&
    !streaming &&
    !last.content.startsWith(ERROR_PREFIX);
  // 输入框 overlay 分段：@提及 → 圆角标签段（可删除），其余普通文本段（含纯路径引用 file:<path>）
  const segments = splitMentions(input, [
    ...mentions,
    ...fileMentions.map((fm) => ({ nodeId: `file:${fm.file}`, text: `@${fm.label}` })),
  ]);

  return (
    <div
      ref={nodeRef}
      className="rounded-lg shadow-lg border flex flex-col text-sm"
      style={{
        width: width ?? 420,
        height: height ?? undefined,
        minWidth: 280,
        minHeight: 150,
        background: "var(--bg-card)",
        borderColor: selected ? "var(--accent)" : "var(--border)",
        cursor: "default",
        position: "relative",
      }}
    >
      <ConnectionFrame topType="target" selected={selected} />

      <header
        className="px-3 py-2 border-b rounded-t-lg flex items-center gap-2"
        style={{
          cursor: "grab",
          borderColor: "var(--border)",
          background: "var(--bg-card)",
        }}
      >
        {/* 标题：双击 inline 编辑（nodrag + stopPropagation 防触发节点拖拽） */}
        {titleEdit.editing ? (
          <input
            {...titleEdit.inputProps}
            autoFocus
            onPointerDown={(e) => e.stopPropagation()}
            placeholder="对话"
            className="nodrag font-medium text-sm min-w-0 w-32 bg-transparent border-b border-[var(--accent)] outline-none"
            style={{ color: "var(--text-primary)" }}
          />
        ) : (
          <span
            onDoubleClick={(e) => {
              e.stopPropagation();
              titleEdit.start();
            }}
            title="双击重命名"
            className="font-medium truncate max-w-[150px] min-w-0 flex-shrink cursor-text"
            style={{ color: "var(--text-primary)" }}
          >
            {displayTitle}
          </span>
        )}
        {streaming && (
          <span className="text-xs text-[var(--accent)] flex-shrink-0 inline-flex items-center gap-1">
            <Loader2 size={12} className="animate-spin flex-shrink-0" />
            生成中
          </span>
        )}

        {/* 供应商·模型选择：节点指定优先（一步定下 provider + model），留空 = 跟随仓库默认 */}
        <div
          className="ml-auto flex items-center gap-1 nodrag"
          onClick={(e) => e.stopPropagation()}
        >
          {/* Agent 选择：选中的 Agent 提供系统提示词与工具（发送时实时解析）；缺省「对话」= 普通对话 */}
          <DropdownSelect
            value={nodeData?.agentId ?? ""}
            onChange={(v) => updateNodeData(id, { agentId: v || undefined })}
            options={agents.map((a) => ({ value: a.id, label: a.name }))}
            // 未选择（旧数据/清空）= 缺省「对话」：占位显示对话、运行时按「对话」解析
            placeholder="对话"
            emptyText="暂无 Agent（设置 → Agent 新建）"
            className="nodrag text-xs rounded px-1 py-0.5 w-24"
            style={{
              color: "var(--text-secondary)",
              background: "var(--input-bg)",
              border: "1px solid var(--input-border)",
            }}
            title="选择 Agent（系统提示词与工具在 设置 → Agent 中配置；缺省「对话」= 普通对话）"
          />
          <ModelSelect
            providers={providers}
            providerId={nodeData?.providerId}
            model={nodeData?.model}
            effort={nodeData?.reasoningEffort}
            onSelectModel={(sel) => {
              if (!sel) {
                // 留空 = 全部跟随仓库默认（清空节点级指定）
                updateNodeData(id, { providerId: "", model: "" });
                return;
              }
              updateNodeData(id, {
                providerId: sel.providerId,
                model: sel.model,
              });
            }}
            onSelectEffort={(effort) =>
              updateNodeData(id, { reasoningEffort: effort })
            }
            defaultModelDisplay={defaultModelDisplay}
            className="nodrag text-xs rounded px-1 py-0.5 w-24"
            style={{
              color: "var(--text-secondary)",
              background: "var(--input-bg)",
              border: "1px solid var(--input-border)",
            }}
            title="选择供应商与模型，或单独设置推理等级（留空 = 跟随默认）"
          />
        </div>
      </header>

      {error && (
        <div
          className="nodrag px-3 py-1.5 text-xs flex items-center justify-between"
          style={{
            background: "#7f1d1d",
            color: "#fca5a5",
            userSelect: "text",
            WebkitUserSelect: "text",
          }}
        >
          <span>{error}</span>
          <button onClick={clearError} className="ml-2 hover:opacity-80">
            <X size={12} />
          </button>
        </div>
      )}

      <div
        className={`relative ${hasFixedHeight ? "flex-1 min-h-0 flex flex-col" : ""}`}
      >
        <div
          ref={scrollRef}
          className={`nodrag nowheel overflow-auto px-3 pt-2 pb-6 space-y-3 ${hasFixedHeight ? "flex-1 min-h-0" : "max-h-[300px]"}`}
          onScroll={handleScroll}
          onContextMenu={handleMessagesCtx}
        >
          {messages.length === 0 ? (
            <p className="text-xs mt-2" style={{ color: "var(--text-muted)" }}>
              输入消息开始对话
            </p>
          ) : (
            messages.map((m, i) => {
              // 进行中的最后一条消息（流式占位/未完成）不显示分支按钮：「完整状态」语义
              const isStreamingMsg = streaming && i === messages.length - 1;
              // 分支按钮仅在 AI 回复处显示（用户消息不分支）：以该 AI 回复（含）之前的完整状态创建子节点
              const canBranch =
                m.role === "assistant" &&
                assistantReplyText(m).trim() !== "" &&
                !isStreamingMsg;
              return (
                <ChatMessageBubble
                  key={m.id}
                  role={m.role === "user" ? "user" : "assistant"}
                  displayContent={
                    m.role === "user"
                      ? (m.displayContent ?? m.content)
                      : undefined
                  }
                  refs={m.refs}
                  refKeyOf={refKeyOfNodeRef}
                  onRefChipClick={handleLocateRef}
                  content={m.content}
                  steps={m.steps}
                  isStreaming={isStreamingMsg}
                  attachments={m.attachments}
                  onMediaExtract={extractToMediaNode}
                  markdownLinks={messageMarkdownLinks}
                  copyText={
                    m.role === "user"
                      ? (m.displayContent ?? m.content)
                      : assistantReplyText(m)
                  }
                  messageId={m.id}
                  canRollback={canBranch}
                  onRollback={handleRollback}
                  onBranch={canBranch ? handleBranch : undefined}
                  stopPropagation
                />
              );
            })
          )}

          {canRegenerate && (
            <div className="flex justify-end">
              <button
                onClick={() => void regenerate(id)}
                className="text-xs px-2 py-0.5 rounded border hover:opacity-80 inline-flex items-center gap-1"
                style={{
                  color: "var(--text-secondary)",
                  borderColor: "var(--border)",
                  background: "var(--bg-tertiary)",
                }}
                title="重新生成最后一条回复"
              >
                <RefreshCw size={12} className="flex-shrink-0" /> 重新生成
              </button>
            </div>
          )}
        </div>
        {showJumpToBottom && (
          <JumpToBottomButton onClick={jumpToBottom} className="nodrag" />
        )}
      </div>

      {/* 划词浮动菜单（文本提取）：统一 Menu（PopupLayer 壳：视口钳制 + Esc/外点关闭） */}
      {selMenu && (
        <Menu
          x={selMenu.x}
          y={selMenu.y}
          widthClass="w-44"
          stopPointerDown
          onClose={() => setSelMenu(null)}
        >
          <MenuItem onClick={extractToTextNode}>
            <Scissors size={14} className="flex-shrink-0" /> 拉出为文本节点
          </MenuItem>
          <MenuItem onClick={() => setSelMenu(null)}>取消</MenuItem>
        </Menu>
      )}

      {/* @ 提及选择器（全仓库文件/文件夹，画布内文件排最前） */}
      {picker && (
        <VaultAtPicker
          x={picker.x}
          y={picker.y}
          openUp={picker.openUp}
          yBottom={picker.yBottom}
          query={picker.query}
          canvasFiles={canvasFiles}
          onPick={handleVaultPick}
          onClose={() => setPicker(null)}
        />
      )}

      <ConversationAttachmentTray
        attachments={attachments}
        onRemove={handleAttachmentRemove}
        onPin={handlePin}
      />

      {lockedByPeer ? (
        /* 协作：对话节点被其他对端独占编辑 → 内容只读条（输入/附件/发送禁用） */
        <div
          className="nodrag border-t px-3 py-2 flex items-center gap-1.5 text-xs"
          style={{ borderColor: "var(--border)", color: lockedByPeer.color }}
        >
          <Lock size={13} className="flex-shrink-0" />
          <span className="truncate">
            {collabStreamingPeer
              ? `${lockedByPeer.nickname} 正在生成…（只读）`
              : `${lockedByPeer.nickname} 正在编辑…（只读）`}
          </span>
        </div>
      ) : (
      <div
        className="nodrag border-t p-2 flex gap-2"
        style={{ borderColor: "var(--border)" }}
        onBlur={handleInputRowBlur}
        onDragOver={(e) => e.preventDefault()}
        onDrop={handleDrop}
      >
        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => {
            addFiles(Array.from(e.target.files ?? []));
            e.target.value = "";
          }}
        />
        <button
          onClick={() => fileInputRef.current?.click()}
          className="px-2 rounded text-sm nodrag hover:opacity-80"
          style={{
            color: "var(--text-secondary)",
            background: "var(--bg-tertiary)",
          }}
          title="添加附件（Ctrl+V 粘贴图片 / 拖拽文件）"
        >
          <Plus size={16} />
        </button>
        {iOwnLock && (
          <span
            className="self-center w-1.5 h-1.5 rounded-full flex-shrink-0"
            style={{ background: "var(--accent)" }}
            title="正在独占编辑（他人只读）"
          />
        )}
        <div className="relative flex-1 min-w-0 overflow-hidden">
          <MentionTextarea
            textareaRef={textareaRef}
            value={input}
            onChange={(v) => {
              // 首次真实输入占锁（协作独占锁：草稿空 → 非空）
              if (input === "" && v !== "") acquireLock();
              setInput(v);
              // @ 后继续输入 → 实时过滤候选（query = @ 位置之后的内容）；
              // @ 锚字符已被删（退格/整体替换）→ 关闭选择器，防陈旧 atIdx 错位插入
              if (picker && atIdx >= 0) {
                if (v[atIdx] !== "@") {
                  setPicker(null);
                  setAtIdx(-1);
                } else {
                  setPicker((p) => (p ? { ...p, query: v.slice(atIdx + 1) } : p));
                }
              }
            }}
            segments={segments}
            onRemoveMention={removeMention}
            onKeyDown={(e) => {
              if (e.key === "@") {
                setAtIdx(textareaRef.current?.selectionStart ?? 0);
                const taRect = textareaRef.current?.getBoundingClientRect();
                const nodeRect = nodeRef.current?.getBoundingClientRect();
                // 节点内相对坐标（菜单 absolute 定位，避免 React Flow transform 容器下 fixed 漂移）
                const x = (taRect?.left ?? 0) - (nodeRect?.left ?? 0);
                const y = (taRect?.bottom ?? 0) - (nodeRect?.top ?? 0);
                const yBottom = (nodeRect?.bottom ?? 0) - (taRect?.bottom ?? 0);
                // 下方视口空间不足（估算菜单高 ~264）→ 向上弹出
                const openUp = window.innerHeight - (taRect?.bottom ?? 0) < 264;
                setPicker({ x, y, openUp, yBottom, query: "" });
              }
              // IME 组合期间 Enter 是「上屏候选词」而非发送（中文输入法必踩，防半成品拼音发送）
              if (
                e.key === "Enter" &&
                !e.shiftKey &&
                !e.nativeEvent.isComposing
              ) {
                e.preventDefault();
                handleSend();
              }
            }}
            onPaste={handlePaste}
            placeholder="输入消息…（@ 引用仓库文件/文件夹，Shift+Enter 换行）"
            rows={2}
            backgroundLayer={
              <div
                className="absolute inset-0 rounded"
                style={{ background: "var(--input-bg)" }}
              />
            }
            overlayClassName="z-10 rounded px-2 py-1 text-sm"
            textareaClassName="relative w-full h-full rounded px-2 py-1 text-sm focus:outline-none focus:ring-1 focus:ring-[var(--accent)] nodrag nowheel overflow-y-auto"
          />
        </div>
        {streaming ? (
          <button
            onClick={() => abort(id)}
            className="px-3 rounded text-xs nodrag"
            style={{
              background: "var(--bg-tertiary)",
              color: "var(--text-secondary)",
            }}
          >
            停止
          </button>
        ) : (
          <button
            onClick={handleSend}
            className="px-3 rounded bg-[var(--accent)] text-[var(--accent-fg)] text-xs hover:bg-[var(--accent-hover)] nodrag"
          >
            发送
          </button>
        )}
      </div>
      )}

      <ResizeHandle />
    </div>
  );
}
