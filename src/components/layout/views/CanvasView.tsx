/**
 * 画布视图面板：ReactFlow 画布完整渲染（节点/边/悬浮控件/小地图/状态栏）。
 *
 * 内聚职责：视口缓存（面板卸载重挂恢复）、画布右键菜单、节点右键菜单、面板拖拽落点、只读白板横幅。
 * 画布快捷键仅在**本面板聚焦**时生效（focusedPanelId 门控）。
 */
import {
  FileOutput,
  FileText,
  Grid3x3,
  LayoutDashboard,
  Link2,
  Magnet,
  Maximize,
  MessageSquarePlus,
  Minus,
  MoreHorizontal,
  Palette,
  Plus,
} from "lucide-react";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Edge, Node } from "@xyflow/react";
import {
  Background,
  ConnectionMode,
  ControlButton,
  MiniMap,
  Panel as FlowPanel,
  ReactFlow,
  useReactFlow,
  useViewport,
  type Connection,
} from "@xyflow/react";
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import {
  cacheCanvasViewport,
  getCachedCanvasViewport,
  onCanvasViewportHandoff,
} from "@/stores/panelStore";
import { useCanvasHotkeys } from "@/hooks/useCanvasHotkeys";
import { useNoteUndoRouting } from "@/hooks/useNoteUndoRouting";
import {
  DEFAULT_CONVERSATION_HEIGHT,
  DEFAULT_CONVERSATION_WIDTH,
  DEFAULT_GROUP_HEIGHT,
  DEFAULT_GROUP_WIDTH,
  DEFAULT_LINK_HEIGHT,
  DEFAULT_LINK_WIDTH,
  DEFAULT_TEXT_NODE_HEIGHT,
  DEFAULT_TEXT_NODE_WIDTH,
} from "@/constants/canvas";
import { BUILTIN_AGENT_CHAT_ID } from "@/constants/agents";
import { ConversationNode } from "@/components/canvas/nodes/ConversationNode";
import { TextNode } from "@/components/canvas/nodes/TextNode";
import { MediaNode } from "@/components/canvas/nodes/MediaNode";
import { SearchResultNode } from "@/components/canvas/nodes/SearchResultNode";
import { GroupNode } from "@/components/canvas/nodes/GroupNode";
import { LinkNode } from "@/components/canvas/nodes/LinkNode";
import { withCollab } from "@/components/canvas/nodes/withCollab";
import { useCollabStore } from "@/stores/collabStore";
import { usePluginStore } from "@/stores/pluginStore";
import {
  OPEN_TABLE_EVENT,
  TableNode,
} from "@/components/canvas/nodes/TableNode";
import { NodeContextMenu } from "@/components/canvas/panels/NodeContextMenu";
import { DataFlowEdge } from "@/components/canvas/edges/DataFlowEdge";
import {
  ATELYX_FILE_MIME,
  type AtelyxFilePayload,
} from "@/components/canvas/panels/FileExplorerPanel";
import { PanelPlaceholder } from "@/components/layout/PanelPlaceholder";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import { PopupLayer } from "@/components/common/PopupLayer";
import { HistoryModal } from "@/components/history/HistoryModal";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { MenuSlotList } from "@/components/plugins/MenuSlot";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";

const builtinNodeTypes = {
  conversation: withCollab(ConversationNode),
  text: withCollab(TextNode),
  media: withCollab(MediaNode),
  search: withCollab(SearchResultNode),
  group: withCollab(GroupNode),
  link: withCollab(LinkNode),
  table: withCollab(TableNode),
};
const builtinEdgeTypes = { default: DataFlowEdge };

/** 视口恢复延迟（ms）：等刚装载的画布节点完成首帧布局后再设视口，防止被装载触发的渲染覆盖。 */
const VIEWPORT_RESTORE_DELAY_MS = 50;

// 连线合法性（边类型按两端节点自动分类）：
// - 资产（text/media/search/table）→ 对话：数据流引用边，放行（重复由 onConnect「再次注入」处理）
// - 对话 → 资产：数据流产出边，同对已有边拦截（防重复）
// - 其余（对话↔对话、link/group 参与、无对话组合）：关联自由线，同对已有边拦截
function isValidConnection(connection: Edge | Connection) {
  if (connection.source === connection.target) return false;
  const state = useCanvasStore.getState();
  const nodes = state.nodes;
  const src = nodes.find((n) => n.id === connection.source);
  const tgt = nodes.find((n) => n.id === connection.target);
  if (!src || !tgt) return false;
  const isAsset = (t: string | undefined) =>
    t === "text" || t === "media" || t === "search" || t === "table";
  const isAssetToConv = isAsset(src.type) && tgt.type === "conversation";
  // 同对已有任意边（数据流产出/关联）→ 拦截；资产→对话例外（onConnect 处理「再次注入」）
  if (!isAssetToConv) {
    const already = state.edges.some(
      (e) => e.source === connection.source && e.target === connection.target,
    );
    if (already) return false;
  }
  return true;
}

/** 画布面板 props：panelId 用于聚焦判定（画布快捷键门控）。 */
export const CanvasView = memo(function CanvasView({
  panelId,
}: {
  panelId: string;
}) {
  const nodes = useCanvasStore((s) => s.nodes);
  const edges = useCanvasStore((s) => s.edges);
  const readOnly = useCanvasStore((s) => s.readOnly);
  const onNodesChange = useCanvasStore((s) => s.onNodesChange);
  const onEdgesChange = useCanvasStore((s) => s.onEdgesChange);
  const onConnect = useCanvasStore((s) => s.onConnect);
  const load = useCanvasStore((s) => s.load);
  const selectNode = useCanvasStore((s) => s.selectNode);
  const onNodeDragStart = useCanvasStore((s) => s.onNodeDragStart);
  const onNodeDragStop = useCanvasStore((s) => s.onNodeDragStop);
  const pasteNodes = useCanvasStore((s) => s.pasteNodes);
  const addNode = useCanvasStore((s) => s.addNode);
  const addTextNoteFromVault = useCanvasStore((s) => s.addTextNoteFromVault);
  const addMediaFromVault = useCanvasStore((s) => s.addMediaFromVault);
  const canvasFile = useAppStore((s) => s.currentCanvasFile);
  const openTable = useAppStore((s) => s.openTable);
  const convertWhiteboard = useAppStore((s) => s.convertWhiteboard);
  const focusedPanelId = useUiStateStore((s) => s.focusedPanelId);
  // 节点组件表 = 内建 + 插件注册（插件节点：注册的组件渲染；插件停用后该类型节点按 React Flow
  // 未注册类型处理为空节点占位，不崩溃）。
  const uiRevision = usePluginStore((s) => s.uiRevision);
  const mergedNodeTypes = useMemo(
    () => ({ ...builtinNodeTypes, ...usePluginStore.getState().pluginNodeTypes() }),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- uiRevision 是插件注册的响应信号（节点表在服务层非响应式），必须作重算依赖
    [uiRevision],
  );
  // 边类型表 = 内建 + 插件注册（与节点同 last-wins 语义：插件注册同名 type 覆盖内置）。
  const mergedEdgeTypes = useMemo(
    () => ({ ...builtinEdgeTypes, ...usePluginStore.getState().pluginEdgeTypes() }),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 同上：uiRevision 是插件注册响应信号
    [uiRevision],
  );
  // 协作：同看本画布的在线用户（presence.file 命中 + view=canvas；断开连接自动消失）
  const collabPeers = useCollabStore((s) => s.peers);
  const canvasPeers = useMemo(
    () =>
      canvasFile
        ? collabPeers.filter(
            (p) => p.presence?.file === canvasFile && p.presence?.view === "canvas",
          )
        : [],
    [canvasFile, collabPeers],
  );
  const {
    screenToFlowPosition,
    fitView,
    zoomIn,
    zoomOut,
    setCenter,
    setViewport,
  } = useReactFlow();

  const [showGrid, setShowGrid] = useState(true);
  // 网格吸附开关（左下角按钮组磁铁按钮切换，默认开）
  const [snapEnabled, setSnapEnabled] = useState(true);

  // 画布右键菜单（空白处）
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    flowX: number;
    flowY: number;
    linkMode?: boolean;
  } | null>(null);
  const closeMenu = useCallback(() => setMenu(null), []);
  // 节点右键菜单
  const [nodeMenu, setNodeMenu] = useState<{
    nodeId: string;
    x: number;
    y: number;
  } | null>(null);
  const closeNodeMenu = useCallback(() => setNodeMenu(null), []);

  // 工具栏右上角「···」菜单（历史记录入口；统一 usePopupAnchor + PopupLayer 浮层）
  const moreTriggerRef = useRef<HTMLButtonElement>(null);
  const moreMenu = usePopupAnchor(moreTriggerRef);
  /** 画布历史面板开关（「···」→ 历史记录）。 */
  const [historyOpen, setHistoryOpen] = useState(false);

  // 加载画布：store 已持有该画布（面板重挂，未保存改动还在内存）时不重载；
  // 首次打开/切换画布（store.canvasFile 与目标不一致）才读盘
  useEffect(() => {
    if (canvasFile && useCanvasStore.getState().canvasFile !== canvasFile)
      load(canvasFile);
  }, [canvasFile, load]);

  // 画布视口恢复（与 onInit 分工，互不重叠）：
  // - 本 effect 只响应真实加载转换（loading true→false，首次打开/切换画布）：画布数据刚落位，
  //   延迟到首帧布局完成后重设视口（无缓存视口如重启后首次打开 → 自动适应视图）。
  //   fitView prop 仅初次挂载生效，切画布不重挂实例，需手动触发。
  // - onInit 只处理「挂载时画布已就绪」（面板重挂），见组件尾部。
  const canvasLoading = useCanvasStore((s) => s.loading);
  // 以挂载时加载态为基线：仅经 loading=true 置位过的 prevLoadingRef 才算真实转换，
  // 挂载即已加载的画布由此跳过本分支（交给 onInit），消除双路径重复恢复
  const prevLoadingRef = useRef(canvasLoading);
  useEffect(() => {
    if (canvasLoading) {
      prevLoadingRef.current = true;
      return;
    }
    if (prevLoadingRef.current) {
      prevLoadingRef.current = false;
      // 占位面板（无画布）没有视口可恢复/适应，跳过
      if (!canvasFile) return;
      const t = setTimeout(() => {
        const vp = getCachedCanvasViewport(canvasFile);
        if (vp) setViewport(vp);
        else fitView({ duration: 200, padding: 0.15 });
      }, VIEWPORT_RESTORE_DELAY_MS);
      return () => clearTimeout(t);
    }
  }, [canvasLoading, fitView, setViewport, canvasFile]);

  // 画布视口跨窗口交接补恢复：视图从别的窗口移入本窗口、交接事件晚于挂载到达时，
  // 挂载时的 fitView 已被覆盖（画布刚加载），此处按到达的交接视口重设
  useEffect(() => {
    return onCanvasViewportHandoff((file, vp) => {
      if (file === canvasFile) setViewport(vp);
    });
  }, [canvasFile, setViewport]);

  // ReactFlow 容器 ref：取画布区中心坐标（底部工具栏/文件面板建节点时落点）
  const flowWrapperRef = useRef<HTMLDivElement>(null);

  const canvasCenter = useCallback(() => {
    const rect = flowWrapperRef.current?.getBoundingClientRect();
    const center = rect
      ? { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
      : { x: window.innerWidth / 2, y: window.innerHeight / 2 };
    return screenToFlowPosition(center);
  }, [screenToFlowPosition]);

  // 画布快捷键仅在画布面板聚焦时启用（其他面板激活时 Delete/Ctrl+Z/Ctrl+A 不误操作画布）；
  // 只读白板（外部白板格式）不提供编辑快捷键；Ctrl+V 粘贴到画布视口中心（坐标由 UI 层提供，store 不依赖 ReactFlow）
  const handlePaste = useCallback(
    () => pasteNodes(canvasCenter()),
    [pasteNodes, canvasCenter],
  );
  const focused = focusedPanelId === panelId;
  useCanvasHotkeys(
    menu ? closeMenu : undefined,
    focused && !readOnly,
    handlePaste,
  );
  // 画布上的笔记节点编辑面参与笔记撤销路由（面板未打开时也要能 Ctrl+Z）
  useNoteUndoRouting();

  const addConversationAt = useCallback(
    (x: number, y: number) => {
      const newNode: Node = {
        id: crypto.randomUUID(),
        type: "conversation",
        position: { x, y },
        // 初始尺寸大一点便于多轮对话；用户可 resize 覆盖（持久化到 .atlx）。
        // 新对话节点默认选中预置「对话」Agent（用户可改选/清除）
        width: DEFAULT_CONVERSATION_WIDTH,
        height: DEFAULT_CONVERSATION_HEIGHT,
        data: { providerId: "", model: "", agentId: BUILTIN_AGENT_CHAT_ID },
      };
      addNode(newNode);
    },
    [addNode],
  );

  /** 画布空白处右键/新建：画布内文本节点（无 file，正文空，随 .atlx 内嵌；右键「保存为笔记」才落盘）。 */
  const addTextNodeAt = useCallback(
    (x: number, y: number) => {
      const newNode: Node = {
        id: crypto.randomUUID(),
        type: "text",
        position: { x, y },
        width: DEFAULT_TEXT_NODE_WIDTH,
        height: DEFAULT_TEXT_NODE_HEIGHT,
        data: { title: "文本", bodyMd: "" },
      };
      addNode(newNode);
    },
    [addNode],
  );

  /** 画布空白处右键「添加分组」：背景矩形容器（低 zIndex，色板默认中性色）。 */
  const addGroupAt = useCallback(
    (x: number, y: number) => {
      const newNode: Node = {
        id: crypto.randomUUID(),
        type: "group",
        position: { x, y },
        zIndex: -1,
        width: DEFAULT_GROUP_WIDTH,
        height: DEFAULT_GROUP_HEIGHT,
        data: { label: "分组" },
      };
      addNode(newNode);
    },
    [addNode],
  );

  /** 画布空白处右键「添加链接」：URL 卡片节点。 */
  const addLinkAt = useCallback(
    (x: number, y: number, url: string) => {
      const newNode: Node = {
        id: crypto.randomUUID(),
        type: "link",
        position: { x, y },
        width: DEFAULT_LINK_WIDTH,
        height: DEFAULT_LINK_HEIGHT,
        data: { url },
      };
      addNode(newNode);
    },
    [addNode],
  );

  /**
   * 画布容器原生监听 dragover/drop：绕开 React 合成事件委托（WebView2 中合成事件对
   * HTML5 DnD 的 preventDefault 不可靠 → drop 不触发 + 禁止光标）。仅消费面板拖拽（自定义
   * MIME），非面板来源（如节点输入框拖文件）放行给节点自身的 drop 处理。
   */
  useEffect(() => {
    const el = flowWrapperRef.current;
    if (!el) return;
    const onDragOver = (e: DragEvent) => e.preventDefault();
    const onDrop = (e: DragEvent) => {
      const raw = e.dataTransfer?.getData(ATELYX_FILE_MIME);
      if (!raw) return; // 非面板来源，放行给节点
      e.preventDefault();
      e.stopPropagation();
      let payload: AtelyxFilePayload;
      try {
        payload = JSON.parse(raw) as AtelyxFilePayload;
      } catch {
        return;
      }
      const pos = screenToFlowPosition({ x: e.clientX, y: e.clientY });
      if (payload.kind === "note") {
        void addTextNoteFromVault(
          payload.file,
          payload.title ?? payload.name,
          pos,
          true,
        );
      } else {
        void addMediaFromVault(payload.file, payload.name, pos, true);
      }
    };
    el.addEventListener("dragover", onDragOver);
    el.addEventListener("drop", onDrop);
    return () => {
      el.removeEventListener("dragover", onDragOver);
      el.removeEventListener("drop", onDrop);
    };
  }, [
    canvasFile,
    screenToFlowPosition,
    addTextNoteFromVault,
    addMediaFromVault,
  ]);

  const onPaneContextMenu = useCallback(
    (event: React.MouseEvent) => {
      event.preventDefault();
      // 只读白板（外部白板格式）：无新建入口
      if (useCanvasStore.getState().readOnly) return;
      // 右键空白 = 取消节点选中（与左键 onPaneClick 一致）
      selectNode(null);
      const pos = screenToFlowPosition({ x: event.clientX, y: event.clientY });
      setMenu({
        x: event.clientX,
        y: event.clientY,
        flowX: pos.x,
        flowY: pos.y,
      });
    },
    [screenToFlowPosition, selectNode],
  );

  const onNodeContextMenuInternal = useCallback(
    (event: React.MouseEvent, node: Node) => {
      event.preventDefault();
      // 只读白板（外部白板格式）：节点不可编辑/删除，不提供右键菜单
      if (useCanvasStore.getState().readOnly) return;
      // 右键节点同步选中到属性面板（右键菜单操作对象 = 面板展示对象）；与左键对称聚焦画布面板——
      // 否则焦点停留在属性面板时其「焦点优先」上下文不会跟随本次选中
      selectNode(node.id);
      useUiStateStore.getState().setFocusedPanel(panelId);
      setNodeMenu({ nodeId: node.id, x: event.clientX, y: event.clientY });
    },
    [selectNode, panelId],
  );

  // 属性面板选中：左键单击节点选中、单击空白清空（与右键行为对称）
  const handleNodeClick = useCallback(
    (_event: React.MouseEvent, node: Node) => {
      selectNode(node.id);
    },
    [selectNode],
  );
  const handlePaneClick = useCallback(() => {
    selectNode(null);
  }, [selectNode]);

  // 画布表格节点「打开表格」按钮 → 打开表格面板（ReactFlow 节点无法经 props 回调，走事件桥接）
  useEffect(() => {
    const onOpenTable = (e: Event) => {
      const detail = (e as CustomEvent).detail as
        { file: string; title: string } | undefined;
      if (detail?.file) openTable(detail.file, detail.title ?? "表格");
    };
    window.addEventListener(OPEN_TABLE_EVENT, onOpenTable);
    return () => window.removeEventListener(OPEN_TABLE_EVENT, onOpenTable);
  }, [openTable]);

  if (!canvasFile) {
    return (
      <PanelPlaceholder
        icon={<Palette size={64} strokeWidth={1.5} />}
        title="打开画布"
        description="从左侧文件面板或搜索面板单击一个 .atlx 画布开始编辑。"
      />
    );
  }

  return (
    <div
      className="h-full w-full relative flex flex-col"
      onClick={() => {
        // 点击菜单外任意处关闭右键菜单（菜单容器自身 stopPropagation，防点菜单被抢先关闭）
        closeMenu();
        closeNodeMenu();
      }}
    >
      {/* 工具栏：与笔记/表格统一（高度 py-1.5）——只读白板横幅（左）+ 协作者胶囊 + 右上角「···」历史菜单 */}
      <div
        className="flex items-center gap-2 px-3 py-1.5 border-b flex-shrink-0 text-xs select-none"
        style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
      >
        {/* 只读白板横幅：外部白板格式只读查看 + 转换为画布入口（原文件保留，单向转换） */}
        {readOnly && (
          <span className="flex items-center gap-2 flex-shrink-0">
            <span
              className="flex items-center gap-1 px-1.5 py-0.5 rounded border"
              style={{
                background: "var(--bg-tertiary)",
                borderColor: "var(--border)",
                color: "var(--text-secondary)",
              }}
            >
              <FileOutput size={12} /> 只读查看 · 外部白板格式
            </span>
            <button
              onClick={() => void convertWhiteboard(canvasFile)}
              className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded transition-colors"
              style={{
                background: "color-mix(in srgb, var(--accent) 15%, transparent)",
                color: "var(--accent)",
              }}
              title="生成同目录 .atlx 画布副本并打开（原文件保留）"
            >
              <FileOutput size={12} /> 转换为画布
            </button>
          </span>
        )}
        <span className="flex-1" />
        {/* 协作：同看本画布的在线用户胶囊（用户色点 + 昵称；断开连接自动消失） */}
        {canvasPeers.length > 0 && (
          <div className="flex items-center gap-1 flex-shrink-0">
            {canvasPeers.map((p) => (
              <span
                key={p.peerId}
                className="flex items-center gap-1.5 px-1.5 py-0.5 rounded border text-[11px]"
                style={{
                  background: "var(--bg-tertiary)",
                  borderColor: "var(--border)",
                  color: "var(--text-secondary)",
                }}
                title={`${p.nickname}${p.deviceName ? `（${p.deviceName}）` : ""} · 正在查看本画布`}
              >
                <span
                  className="w-2 h-2 rounded-full flex-shrink-0"
                  style={{ background: p.color }}
                />
                <span className="max-w-24 truncate">{p.nickname}</span>
              </span>
            ))}
          </div>
        )}
        {/* 「···」更多选项：历史记录入口（统一 usePopupAnchor + PopupLayer 浮层） */}
        <span className="flex-shrink-0">
          <button
            ref={moreTriggerRef}
            onClick={() => moreMenu.toggle()}
            title="更多选项"
            className="p-0.5 rounded hover:opacity-80"
            style={{ color: moreMenu.anchor ? "var(--accent)" : "var(--text-muted)" }}
          >
            <MoreHorizontal size={15} />
          </button>
          <PopupLayer
            anchor={moreMenu.anchor}
            onClose={moreMenu.close}
            triggerRef={moreTriggerRef}
            widthClass="w-36"
          >
            <button
              className="w-full flex items-center gap-2 px-2 py-1.5 text-xs hover:opacity-80 disabled:cursor-not-allowed disabled:opacity-40"
              style={{ color: "var(--text-primary)" }}
              onClick={() => {
                moreMenu.close();
                setHistoryOpen(true);
              }}
              disabled={readOnly}
              title={readOnly ? "只读白板（外部白板格式）无历史记录" : "查看本画布的历史版本并回滚"}
            >
              <span className="w-3.5 flex-shrink-0" />
              历史记录
            </button>
          </PopupLayer>
        </span>
      </div>

      <div
        ref={flowWrapperRef}
        className="flex-1 min-h-0 w-full"
        onContextMenu={(e) => {
          if ((e.target as HTMLElement).closest(".react-flow__node")) return;
          onPaneContextMenu(e);
        }}
      >
        <ReactFlow
          nodes={nodes}
          edges={edges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          onConnect={onConnect}
          onNodeDragStart={onNodeDragStart}
          onNodeDragStop={onNodeDragStop}
          onNodeClick={handleNodeClick}
          onPaneClick={handlePaneClick}
          onNodeContextMenu={onNodeContextMenuInternal}
          onMoveEnd={(_event, viewport) => {
            // 记录当前画布视口，面板卸载（布局切换/关闭）重挂时恢复；跨窗口交接时经 viewHandoff 转移
            if (canvasFile) cacheCanvasViewport(canvasFile, viewport);
          }}
          onInit={(instance) => {
            // 挂载时画布已就绪（未在加载）→ 同步恢复缓存视口：pane 几何已定型、数据已就位，
            // 立即重设无闪烁；挂载时仍在加载/后续切换画布由上面的 loading-effect 统一负责
            if (canvasFile && !useCanvasStore.getState().loading) {
              const vp = getCachedCanvasViewport(canvasFile);
              if (vp) instance.setViewport(vp);
            }
          }}
          isValidConnection={isValidConnection}
          nodeTypes={mergedNodeTypes}
          edgeTypes={mergedEdgeTypes}
          fitView
          minZoom={0.1}
          maxZoom={2}
          panOnDrag={[1, 2]}
          panActivationKeyCode="Space"
          // 左键拖拽空白 = 框选（React Flow 默认 selectionOnDrag=false 须显式开启；
          // panOnDrag 数组不含左键 0，两者不冲突：中键/右键平移、左键框选）
          selectionOnDrag
          snapToGrid={snapEnabled}
          snapGrid={[16, 16]}
          // 只读白板（外部白板格式）：禁拖拽/连线；关联边需任意 handle 组合（Loose，
          // 边类型由两端节点自动分类，方向仍由拖出端决定）
          nodesDraggable={!readOnly}
          nodesConnectable={!readOnly}
          edgesReconnectable={!readOnly}
          connectionMode={ConnectionMode.Loose}
        >
          {showGrid && <Background gap={16} />}
          {/* 画布右侧中部悬浮按钮组：网格 / 磁铁吸附（竖排，react-flow__controls 样式） */}
          <FlowPanel position="center-right">
            <div className="react-flow__controls">
              <ControlButton
                title="显示网格"
                aria-label="显示网格"
                onClick={() => setShowGrid((v) => !v)}
                style={{
                  width: 36,
                  height: 36,
                  color: showGrid ? "var(--accent)" : undefined,
                }}
              >
                <Grid3x3 size={18} />
              </ControlButton>
              <ControlButton
                title={snapEnabled ? "关闭网格吸附" : "开启网格吸附"}
                aria-label="网格吸附"
                onClick={() => setSnapEnabled((v) => !v)}
                style={{
                  width: 36,
                  height: 36,
                  color: snapEnabled ? "var(--accent)" : undefined,
                }}
              >
                <Magnet size={18} />
              </ControlButton>
            </div>
          </FlowPanel>
          {/* 左下角按钮组：放大/缩小/适应视图（bottom 上调避开状态栏统计） */}
          <FlowPanel position="bottom-left" style={{ bottom: 32 }}>
            <div className="react-flow__controls">
              <ControlButton
                title="放大 (+)"
                onClick={() => zoomIn({ duration: 150 })}
              >
                <Plus size={16} />
              </ControlButton>
              <ControlButton
                title="缩小 (-)"
                onClick={() => zoomOut({ duration: 150 })}
              >
                <Minus size={16} />
              </ControlButton>
              <ControlButton
                title="适应视图"
                onClick={() => fitView({ duration: 200, padding: 0.15 })}
              >
                <Maximize size={16} />
              </ControlButton>
            </div>
          </FlowPanel>
          {/* 点击小地图任意处 → 视口中心快速跳转到该画布位置（React Flow 12 默认不跳转，需自定义 onClick） */}
          <MiniMap
            pannable
            zoomable
            nodeBorderRadius={4}
            onClick={(_, position) =>
              setCenter(position.x, position.y, { duration: 200 })
            }
            style={{ bottom: 16, right: 16, width: 170, height: 130 }}
          />
        </ReactFlow>

        {/* 左下角状态栏：统计（右下角让位 MiniMap；网格按钮已并入左下角按钮组） */}
        <div
          className="absolute bottom-1.5 left-3 z-10 text-[11px] select-none inline-flex items-center gap-1.5"
          style={{ color: "var(--text-muted)" }}
        >
          <span>
            {nodes.length} 个节点 · {edges.length} 条连线
          </span>
          <ZoomBadge />
          {/* 插件贡献区：画布状态条（list 槽，priority 降序） */}
          <SlotListMount slot="statusbar/canvas" />
        </div>
      </div>

      {/* 画布空白处右键菜单 */}
      {menu && (
        <Menu
          x={menu.x}
          y={menu.y}
          onClose={closeMenu}
          widthClass="w-40"
          stopPointerDown
        >
          {menu.linkMode ? (
            /* 添加链接：菜单内 inline 输入 URL（Enter 创建，Esc 关闭） */
            <div className="px-3 py-1.5">
              <input
                autoFocus
                placeholder="https://…"
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    const url = (e.target as HTMLInputElement).value.trim();
                    if (url) addLinkAt(menu.flowX, menu.flowY, url);
                    closeMenu();
                  } else if (e.key === "Escape") {
                    closeMenu();
                  }
                }}
                onBlur={closeMenu}
                className="w-full bg-transparent border-b border-[var(--accent)] outline-none text-xs"
                style={{ color: "var(--text-primary)" }}
              />
            </div>
          ) : (
            <>
              {/* 插件菜单项：画布空白右键菜单（contextmenu/canvas） */}
              <MenuSlotList target="canvas" />
              <MenuItem
                onClick={() => {
                  addConversationAt(menu.flowX, menu.flowY);
                  closeMenu();
                  setTimeout(
                    () => fitView({ duration: 200, padding: 0.2 }),
                    50,
                  );
                }}
              >
                <span className="inline-flex items-center gap-1.5">
                  <MessageSquarePlus size={14} /> 添加对话节点
                </span>
              </MenuItem>
              <MenuItem
                onClick={() => {
                  addTextNodeAt(menu.flowX, menu.flowY);
                  closeMenu();
                }}
              >
                <span className="inline-flex items-center gap-1.5">
                  <FileText size={14} /> 添加文本节点
                </span>
              </MenuItem>
              <MenuDivider />
              <MenuItem
                onClick={() => {
                  addGroupAt(menu.flowX, menu.flowY);
                  closeMenu();
                }}
              >
                <span className="inline-flex items-center gap-1.5">
                  <LayoutDashboard size={14} /> 添加分组
                </span>
              </MenuItem>
              <MenuItem
                onClick={() =>
                  setMenu((m) => (m ? { ...m, linkMode: true } : m))
                }
              >
                <span className="inline-flex items-center gap-1.5">
                  <Link2 size={14} /> 添加链接
                </span>
              </MenuItem>
            </>
          )}
        </Menu>
      )}

      {/* 节点右键菜单 */}
      {nodeMenu && (
        <NodeContextMenu
          nodeId={nodeMenu.nodeId}
          x={nodeMenu.x}
          y={nodeMenu.y}
          onClose={closeNodeMenu}
        />
      )}

      {/* 画布历史面板（工具栏「···」→ 历史记录；readOnly 白板禁用入口不打开） */}
      <HistoryModal
        kind="canvas"
        file={canvasFile}
        open={historyOpen}
        onClose={() => setHistoryOpen(false)}
      />
    </div>
  );
});

/** 缩放百分比指示（独立小组件订阅 viewport，避免平移/缩放导致整个页面重渲染）。 */
function ZoomBadge() {
  const { zoom } = useViewport();
  return <span>{Math.round(zoom * 100)}%</span>;
}
