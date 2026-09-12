/**
 * 右侧边栏属性面板：不只画布专属，也显示笔记属性。
 *
 * 上下文（焦点优先，其次选中）：
 * - 焦点在笔记编辑器面板 → 显示当前笔记的 frontmatter 属性（NotePropertiesView 复用，增删改/切类型，编辑即写盘）
 * - 选中笔记节点（text + file）→ 显示该笔记的属性
 * - 焦点在画布/选中其他节点 → 节点属性（对话：Agent + 来源/资产列表；文本/媒体：基本信息 + 来源/消费方）
 * - 其余（无选中、非笔记焦点）→ 空面板（无占位提示）
 * 资产列表项点击 → setCenter 定位到对应节点（与 @chip 点击定位一致）。
 * 分层：走 canvasStore / appStore / uiStateStore / panelStore / noteStore / settingsStore，不直调 service。
 */
import {
  Bot,
  FileText,
  GitBranch,
  Image,
  LayoutDashboard,
  Link2,
  MessageSquare,
  Network,
  Table as TableIcon,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useReactFlow, type Node as FlowNode } from "@xyflow/react";
import { useShallow } from "zustand/react/shallow";
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { usePanelStore } from "@/stores/panelStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { BUILTIN_AGENT_CHAT_ID } from "@/constants/agents";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { NotePropertiesView } from "@/components/editor/NotePropertiesView";
import { useNoteBodySession, useNoteSurface } from "@/hooks/useNoteBodySession";
import { useVaultTagCandidates } from "@/hooks/useVaultTagCandidates";
import { parseFrontmatter, stringifyFrontmatter } from "@/utils/frontmatter";
import { noteTitleFromFile } from "@/utils/filename";
import { activeTabOf, findPanel } from "@/utils/workspaceLayout";
import { mentionTextOf, prefix } from "@/utils/text";
import type {
  ConversationData,
  DetachedWindow,
  LayoutNode,
  MediaData,
  TableData,
  TextData,
  ViewKind,
} from "@/types";

const NODE_TYPE_LABEL: Record<string, string> = {
  conversation: "对话",
  text: "文本",
  media: "媒体",
  search: "搜索",
  group: "分组",
  link: "链接",
  table: "表格",
};

/** 无入边/出边时的空派生（模块级常量，useShallow 数组比较依赖元素引用稳定）。 */
const EMPTY_NODES: FlowNode[] = [];

/** 属性面板展示上下文：笔记（file）/ 节点（nodeId）/ 空。 */
type InspectorContext = { kind: "note"; file: string } | { kind: "node"; nodeId: string } | null;

/** 焦点宿主 → 当前激活标签视图（主窗口面板或撕裂窗口；null = 无焦点/无法解析）。
 *  先查撕裂窗口再查面板树：撕裂窗口 activeTree 是 id 恰等于窗口 id 的空根面板（panelStore
 *  applyPanelInit），先查树会命中空面板导致窗口内聚焦判定失效（直到下一次布局广播）。 */
function focusedHostViewOf(
  focusedPanelId: string | null,
  mirror: { activeTree: LayoutNode; detachedWindows: DetachedWindow[] } | null,
): ViewKind | null {
  if (!focusedPanelId || !mirror) return null;
  const win = mirror.detachedWindows.find((w) => w.id === focusedPanelId);
  if (win) {
    return (
      win.tabs.find((t) => t.id === win.activeTabId)?.view ?? win.tabs[0]?.view ?? null
    );
  }
  const panel = findPanel(mirror.activeTree, focusedPanelId);
  if (panel) return activeTabOf(panel)?.view ?? null;
  return null;
}

/**
 * 上下文解析：焦点在笔记编辑器 → 当前笔记；焦点在画布等 → 跟随选中；
 * 焦点在属性面板自身 → 返回 null（由调用方保留既有上下文，面板内操作不闪走）。
 */
function resolveContext(
  focusedHostView: ViewKind | null,
  currentNoteFile: string | null,
  selectedNodeId: string | null,
): InspectorContext {
  if (focusedHostView === "note") {
    return currentNoteFile ? { kind: "note", file: currentNoteFile } : null;
  }
  if (focusedHostView !== "inspector") {
    return selectedNodeId ? { kind: "node", nodeId: selectedNodeId } : null;
  }
  return null;
}

/**
 * 对话节点显示名：优先 LLM 自动命名的话题标题（data.title，首轮对话完成后自动命名），
 * 未命名时回退首条 user 消息前缀，无消息时「对话节点」。
 * 入参是已取出的「首条 user 消息正文」而非消息数组——调用方据此只订阅该字符串，
 * 父对话流式增量不会让本面板每帧重渲染。
 */
function conversationDisplayName(node: FlowNode, firstUserContent: string | undefined): string {
  const data = node.data as Partial<ConversationData>;
  if (data.title) return data.title;
  return firstUserContent ? prefix(firstUserContent, 16) : "对话节点";
}

/**
 * 无入边时的来源降级文案：笔记节点显示仓库路径，画布内文本/媒体显示「手动创建」，
 * 搜索节点保持「暂无」（其来源必为对话自动建边，孤立仅因源节点被删）。
 */
function sourceFallback(node: FlowNode): string {
  if (node.type === "text") {
    return (node.data as unknown as TextData).file ?? "手动创建";
  }
  if (node.type === "table") {
    return (node.data as unknown as TableData).file ?? "手动创建";
  }
  if (node.type === "media") return "手动创建";
  return "暂无";
}

/** 资产列表行：节点类型图标 + 显示名（对话行标注「分支」血缘）。 */
function AssetRow({ node, onLocate }: { node: FlowNode; onLocate: (id: string) => void }) {
  const icon =
    node.type === "conversation" ? (
      <MessageSquare size={13} className="flex-shrink-0" />
    ) : node.type === "text" ? (
      <FileText size={13} className="flex-shrink-0" />
    ) : node.type === "table" ? (
      <TableIcon size={13} className="flex-shrink-0" />
    ) : node.type === "group" ? (
      <LayoutDashboard size={13} className="flex-shrink-0" />
    ) : node.type === "link" ? (
      <Link2 size={13} className="flex-shrink-0" />
    ) : (
      <Image size={13} className="flex-shrink-0" />
    );
  return (
    <button
      onClick={() => onLocate(node.id)}
      className="w-full text-left px-2 py-1.5 rounded flex items-center gap-2 hover:opacity-80"
      style={{ background: "var(--bg-tertiary)", color: "var(--text-primary)" }}
      title="点击定位到节点"
    >
      {icon}
      <span className="truncate text-xs">{node.type === "conversation" ? "对话" : mentionTextOf(node)}</span>
      {node.type === "conversation" && (
        <span className="ml-auto text-[10px] flex-shrink-0" style={{ color: "var(--text-muted)" }}>
          分支
        </span>
      )}
    </button>
  );
}

/** 分组小标题。 */
function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="text-[11px] mb-1 flex items-center gap-1" style={{ color: "var(--text-muted)" }}>
      {children}
    </h3>
  );
}

/** 面板标题栏：左「属性」+ 右类型徽章（笔记模式与节点模式共用）。 */
function InspectorHeader({ badge }: { badge: string }) {
  return (
    <div
      className="px-3 py-2 border-b flex items-center gap-2 flex-shrink-0"
      style={{ borderColor: "var(--border)" }}
      data-tauri-drag-region
    >
      <span className="font-medium text-xs">属性</span>
      <span
        className="ml-auto text-xs px-1.5 py-0.5 rounded flex-shrink-0"
        style={{ background: "var(--bg-tertiary)", color: "var(--text-secondary)" }}
      >
        {badge}
      </span>
    </div>
  );
}

/**
 * 相邻节点订阅：入向来源（in）/ 出向消费方（out）/ 无向关联端点（assoc）。
 * find 节点 → 过滤邻边 → 映射回节点 → filter(Boolean)；assoc 双向匹配并按对端 id 去重
 * （自由线同一对端可有多条连线）。
 *
 * useShallow 返回「数组」：元素是稳定节点引用，浅比较逐元素有效，未变不触发重渲染；
 * 空态必须返回 EMPTY_NODES 模块常量——返回新建数组/含新建数组字段的对象时 useShallow
 * 判等恒失效，触发 React「getSnapshot should be cached」+ 无限重渲染。
 */
function useAdjacentNodes(nodeId: string | null, dir: "in" | "out" | "assoc"): FlowNode[] {
  return useCanvasStore(
    useShallow((s) => {
      const n = nodeId ? s.nodes.find((x) => x.id === nodeId) : undefined;
      if (!n) return EMPTY_NODES;
      // 无向关联边双向匹配端点，按对端 id 去重
      if (dir === "assoc") {
        const ids = new Set<string>();
        for (const e of s.edges) {
          if (e.directed !== false) continue;
          if (e.source === n.id) ids.add(e.target);
          else if (e.target === n.id) ids.add(e.source);
        }
        return [...ids]
          .map((id) => s.nodes.find((x) => x.id === id))
          .filter((x): x is FlowNode => !!x);
      }
      // 只含有向数据流边：无向关联边不表达消费/来源（独立「关联」分区展示）；
      // in = 入边取 source（来源），out = 出边取 target（消费方），均排除自环
      return s.edges
        .filter(
          (e) =>
            e.directed !== false &&
            (dir === "in"
              ? e.target === n.id && e.source !== n.id
              : e.source === n.id && e.target !== n.id),
        )
        .map((e) => s.nodes.find((x) => x.id === (dir === "in" ? e.source : e.target)))
        .filter((x): x is FlowNode => !!x);
    })
  );
}

export function InspectorPanel() {
  // 焦点宿主视图（笔记/画布/属性/其他）：决定笔记模式 vs 跟随选中。
  // selector 返回原始值（string|null）：layoutMirror 每次布局变更都是新对象，派生值未变不重渲染
  // （resize 拖拽/展开目录等无关变更不再拖着重面板重渲染）
  const focusedPanelId = useUiStateStore((s) => s.focusedPanelId);
  const focusedHostView = usePanelStore((s) => focusedHostViewOf(focusedPanelId, s.layoutMirror));
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const selectedNodeId = useCanvasStore((s) => s.selectedNodeId);
  /** 面板展示上下文：笔记（file）/ 节点（nodeId）/ 空；焦点在属性面板自身时保留既有上下文。 */
  const [context, setContext] = useState<InspectorContext>(() =>
    resolveContext(
      focusedHostViewOf(
        useUiStateStore.getState().focusedPanelId,
        usePanelStore.getState().layoutMirror,
      ),
      useAppStore.getState().currentNoteFile,
      useCanvasStore.getState().selectedNodeId,
    ),
  );
  useEffect(() => {
    if (focusedHostView !== "inspector") {
      setContext(resolveContext(focusedHostView, currentNoteFile, selectedNodeId));
    }
    // focusedHostView === "inspector"：焦点在本面板，保留既有上下文（面板内操作不闪走）
  }, [focusedHostView, currentNoteFile, selectedNodeId]);

  // 窄化订阅：节点引用（未变时稳定）+ 相邻节点数组（useAdjacentNodes 内 useShallow 比较）
  const nodeId = context?.kind === "node" ? context.nodeId : null;
  const node = useCanvasStore((s) => (nodeId ? s.nodes.find((x) => x.id === nodeId) : undefined));
  const sources = useAdjacentNodes(nodeId, "in");
  const targets = useAdjacentNodes(nodeId, "out");
  // 无向关联边端点（去重）：关联是自由线，不记入消费/来源列表
  const associations = useAdjacentNodes(nodeId, "assoc");
  // 只订阅选中对话的消息数（number，流式期间长度不变不重渲染；全量 messagesByConv 会每帧刷新面板）
  const msgCount = useCanvasStore((s) => (nodeId ? (s.messagesByConv[nodeId]?.length ?? 0) : 0));
  const updateNodeData = useCanvasStore((s) => s.updateNodeData);
  // Agent 候选（配置在 设置 → Agent，仓库级 .atelyx/agents.json；发送时实时解析系统提示词/工具）
  const agents = useSettingsStore((s) => s.agents);
  // 分支来源：入边中 type 为 conversation 的父节点（血缘边对话→对话），无则手动创建。
  // 只订阅父对话「首条 user 消息正文」（字符串，流式增量不改它 → 不重渲染；订阅整个
  // messagesByConv 数组会每帧刷新面板），用于来源显示名回退
  const parentConv = sources.find((n) => n.type === "conversation");
  const parentFirstUser = useCanvasStore((s) =>
    parentConv
      ? s.messagesByConv[parentConv.id]?.find((m) => m.role === "user")?.content
      : undefined,
  );
  const { setCenter } = useReactFlow();

  // 笔记模式数据：焦点当前笔记（优先级）或选中笔记节点（text + file）
  const selectedNoteFile =
    node && node.type === "text" ? (node.data as unknown as TextData).file ?? null : null;
  const targetNoteFile = context?.kind === "note" ? context.file : selectedNoteFile;
  // 笔记正文会话：属性读写与笔记面板/画布节点共用同一条保存链（同一篇笔记只有一个写者）
  const { session: noteSession, view: noteView } = useNoteBodySession(targetNoteFile);
  /** 笔记能力是否可用：会话/属性尚在载入时与「能力缺席」是两回事，降级文案只对后者 */
  const noteSurface = useNoteSurface();
  const parsed = useMemo(
    () => (noteView ? parseFrontmatter(noteView.content) : null),
    [noteView],
  );
  // 落盘失败由会话写入会话状态（面板 header 展示）；本面板独立成面板时也要可见
  const noteSaveFailed = !!noteView?.error;
  const { tagCandidates, requestTagCandidates } = useVaultTagCandidates();
  // 属性提交失败：内联错误提示 + 重试（stringify 失败等本端错误；写盘失败见 noteSaveFailed）
  const [saveError, setSaveError] = useState(false);
  const lastFailedDataRef = useRef<Record<string, unknown> | null>(null);

  const locateNode = (id: string) => {
    // 点击定位时读最新位置（不订阅，避免位置变化触发面板重渲染）
    const n = useCanvasStore.getState().nodes.find((x) => x.id === id);
    if (!n) return;
    const w = n.measured?.width ?? n.width ?? 200;
    const h = n.measured?.height ?? n.height ?? 100;
    setCenter(n.position.x + w / 2, n.position.y + h / 2, { zoom: 1, duration: 300 });
  };

  // 笔记模式：frontmatter 属性（与编辑器属性区同一组件，改动经会话保存链落盘）
  if (targetNoteFile) {
    const title = noteTitleFromFile(targetNoteFile);
    const handleNotePropsUpdate = (next: Record<string, unknown>) => {
      if (!parsed || !noteSession) return;
      lastFailedDataRef.current = next;
      try {
        noteSession.commitContent(stringifyFrontmatter(next, parsed.body));
        setSaveError(false);
      } catch (e) {
        console.error("[frontmatter] stringify error:", e, next);
        setSaveError(true);
      }
    };
    return (
      <div
        className="h-full flex flex-col overflow-hidden"
        style={{ background: "var(--bg-secondary)", color: "var(--text-primary)" }}
      >
        <InspectorHeader badge="笔记" />

        <div className="flex-1 overflow-auto px-3 py-2 flex flex-col gap-3">
          <div>
            <div className="text-sm font-medium truncate">{title}</div>
            <div
              className="text-[11px] truncate"
              style={{ color: "var(--text-muted)" }}
              title={targetNoteFile}
            >
              {targetNoteFile}
            </div>
          </div>
          {parsed ? (
            <NotePropertiesView
              key={targetNoteFile}
              data={parsed.data}
              parseError={!parsed.ok}
              onUpdate={handleNotePropsUpdate}
              onOpenSource={() => useAppStore.getState().openNote(targetNoteFile, title)}
              tagCandidates={tagCandidates}
              onRequestTagCandidates={requestTagCandidates}
            />
          ) : !noteSurface ? (
            /* 笔记能力缺席（笔记插件停用）：属性读写无提供者，明示降级而不是静默少一块 */
            <div className="text-xs" style={{ color: "#f59e0b" }}>
              笔记能力未启用，属性不可编辑
            </div>
          ) : null}
          {(saveError || noteSaveFailed) && (
            <div className="text-xs flex items-center gap-2" style={{ color: "#f87171" }}>
              <span>属性保存失败</span>
              <button
                className="px-1.5 py-0.5 rounded border hover:opacity-80 flex-shrink-0"
                style={{ borderColor: "#f87171" }}
                onClick={() => {
                  const last = lastFailedDataRef.current;
                  if (last) handleNotePropsUpdate(last);
                }}
              >
                重试
              </button>
            </div>
          )}
        </div>
      </div>
    );
  }

  // 无上下文（无选中节点、焦点非笔记编辑器）：空面板，无占位提示
  if (!node) {
    return <div className="h-full" style={{ background: "var(--bg-secondary)" }} />;
  }

  const isConv = node.type === "conversation";

  let title = NODE_TYPE_LABEL[node.type ?? ""] ?? node.type ?? "节点";
  let sub: string | undefined;
  if (isConv) {
    sub = `${msgCount} 条消息`;
  } else if (node.type === "text") {
    const d = node.data as unknown as TextData;
    title = d.title || "未命名文本";
    // 无 file = 画布内文本节点（未落盘），与笔记节点（显示仓库路径）区分
    sub = d.file ?? "画布内文本（未保存为笔记）";
  } else if (node.type === "media") {
    const d = node.data as unknown as MediaData;
    title = d.name || "未命名媒体";
    sub = [d.file, d.kind].filter(Boolean).join(" · ");
  } else if (node.type === "group") {
    const d = node.data as unknown as { label?: string };
    title = d.label || "未命名分组";
    sub = "分组容器（节点放入 = 坐标重叠，颜色可切换）";
  } else if (node.type === "link") {
    const d = node.data as unknown as { url?: string };
    title = d.url || "未命名链接";
    sub = "链接节点（单击在浏览器打开）";
  } else if (node.type === "table") {
    const d = node.data as unknown as TableData;
    title = d.title || "未命名表格";
    sub = d.file ?? "表格";
  }

  const agentId = isConv
    ? (node.data as unknown as Partial<ConversationData>).agentId
    : undefined;
  // 未选择（旧数据/清空）= 缺省「对话」：摘要按「对话」展示、运行时按「对话」解析
  const selectedAgent =
    agents.find((a) => a.id === (agentId || BUILTIN_AGENT_CHAT_ID)) ?? null;

  return (
    <div
      className="h-full flex flex-col overflow-hidden"
      style={{ background: "var(--bg-secondary)", color: "var(--text-primary)" }}
    >
      <InspectorHeader badge={NODE_TYPE_LABEL[node.type ?? ""] ?? node.type ?? "节点"} />

      <div className="flex-1 overflow-auto px-3 py-2 flex flex-col gap-3">
        <div>
          <div className="text-sm font-medium truncate">{title}</div>
          {sub && (
            <div className="text-[11px] truncate" style={{ color: "var(--text-muted)" }} title={sub}>
              {sub}
            </div>
          )}
        </div>

        {isConv && (
          <>
          <section>
            <SectionTitle>
              <GitBranch size={12} /> 来源
            </SectionTitle>
            {/* 分支血缘边（对话→对话）的父节点 = 分支来源；无则手动创建 */}
            <div className="text-xs" style={{ color: "var(--text-secondary)" }}>
              {parentConv ? conversationDisplayName(parentConv, parentFirstUser) : "手动创建"}
            </div>
          </section>

          <section>
            <SectionTitle>
              <Bot size={12} /> Agent
            </SectionTitle>
            <DropdownSelect
              value={agentId ?? ""}
              onChange={(v) => updateNodeData(node.id, { agentId: v || undefined })}
              options={agents.map((a) => ({ value: a.id, label: a.name }))}
              placeholder="对话"
              emptyText="暂无 Agent（设置 → Agent 新建）"
              className="w-full text-xs rounded px-1.5 py-1"
              style={{
                color: "var(--text-secondary)",
                background: "var(--input-bg)",
                border: "1px solid var(--input-border)",
              }}
              title="选中的 Agent 提供系统提示词与工具（发送时实时解析；缺省「对话」= 普通对话；配置在 设置 → Agent）"
            />
            {selectedAgent && (
              <p
                className="text-[11px] mt-1.5 leading-relaxed"
                style={{ color: "var(--text-muted)" }}
                title={
                  selectedAgent.systemPromptFile
                    ? `系统提示词：已注册提示词（${selectedAgent.systemPromptFile}）`
                    : undefined
                }
              >
                {selectedAgent.systemPromptFile
                  ? `系统提示词：已注册提示词（${selectedAgent.systemPromptFile}）`
                  : "系统提示词：未设置"}
              </p>
            )}
          </section>
          </>
        )}

        <section>
          <SectionTitle>{isConv ? "被消费的资产" : "来源"}</SectionTitle>
          {sources.length === 0 ? (
            <p className="text-[11px]" style={{ color: "var(--text-muted)" }}>
              {sourceFallback(node)}
            </p>
          ) : (
            <div className="flex flex-col gap-1">
              {sources.map((src) => (
                <AssetRow key={src.id} node={src} onLocate={locateNode} />
              ))}
            </div>
          )}
        </section>

        <section>
          <SectionTitle>{isConv ? "产出的资产" : "消费方"}</SectionTitle>
          {targets.length === 0 ? (
            <p className="text-[11px]" style={{ color: "var(--text-muted)" }}>
              暂无
            </p>
          ) : (
            <div className="flex flex-col gap-1">
              {targets.map((tgt) => (
                <AssetRow key={tgt.id} node={tgt} onLocate={locateNode} />
              ))}
            </div>
          )}
        </section>

        {/* 无向关联边端点：自由线不表达数据流，独立分区展示（不记入消费/产出） */}
        {associations.length > 0 && (
          <section>
            <SectionTitle>
              <Network size={12} /> 关联
            </SectionTitle>
            <div className="flex flex-col gap-1">
              {associations.map((assoc) => (
                <AssetRow key={assoc.id} node={assoc} onLocate={locateNode} />
              ))}
            </div>
          </section>
        )}
      </div>
    </div>
  );
}
