/**
 * 设置 → Agent 面板：Agent 配置列表 + 编辑器。
 *
 * Agent = 可复用的对话预设（名称 + 系统提示词 + 工具），对话节点 / AI 对话面板
 * 按 id 引用、发送时实时解析（改 Agent 即改行为，无需改引用处）。
 * 系统提示词 = 引用已注册提示词笔记（右键笔记「注册为提示词」，发送时实时读正文、
 * 外部编辑即时生效）；工具 = 全部工具按分类折叠分组勾选，勾选即赋予能力、取消即移除（默认全开）。
 *
 * 预置 Agent（builtin 标记）默认随仓库出现、
 * 可编辑但不可删除（列表不显示删除按钮，store 侧另有兜底）；副本为普通 Agent 可删除。
 *
 * 编辑即时生效（同设置页其他面板）：下拉/工具勾选即改即存；名称用本地草稿 + blur 提交
 * （避免每键一次 agents.json 原子写，空名回退）。删除走 ConfirmDialog（同设置页惯例）。
 */
import { ChevronDown, ChevronRight, Copy, Plus, Sparkles, Trash2 } from "lucide-react";
import { useLayoutEffect, useEffect, useRef, useState } from "react";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePluginStore } from "@/stores/pluginStore";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import {
  AGENT_TOOLS_META,
  AGENT_TOOL_CATEGORIES,
  type AgentToolCategory,
  type AgentToolMeta,
} from "@/constants/tools";
import { noteTitleFromFile } from "@/utils/filename";
import type { AgentConfig } from "@/types";

/** 单个工具分类的折叠分组：头部 = 折叠箭头 + 分类名 + 三态复选框（全选/部分/全不选）；展开后逐项勾选。 */
function ToolCategoryGroup({
  cat,
  tools,
  enabled,
  searchReady,
  collapsed,
  onToggle,
  onToggleAll,
  onToggleCollapsed,
}: {
  cat: { key: AgentToolCategory; label: string };
  tools: AgentToolMeta[];
  enabled: Set<string>;
  searchReady: boolean;
  collapsed: boolean;
  onToggle: (id: string) => void;
  onToggleAll: () => void;
  onToggleCollapsed: () => void;
}) {
  const allOn = tools.every((t) => enabled.has(t.id));
  const noneOn = tools.every((t) => !enabled.has(t.id));
  const partial = !allOn && !noneOn;
  const checkboxRef = useRef<HTMLInputElement>(null);
  // indeterminate 是 DOM 属性（无 JSX 声明式写法）：部分勾选时呈短横态；
  // 用 useLayoutEffect 同步置位，避免部分勾选态渲染到 paint 之间闪一帧未勾选
  useLayoutEffect(() => {
    if (checkboxRef.current) checkboxRef.current.indeterminate = partial;
  }, [partial]);
  return (
    <div
      className="rounded border overflow-hidden"
      style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
    >
      <div
        className="flex items-center gap-1.5 px-2 py-1.5 cursor-pointer select-none"
        onClick={onToggleCollapsed}
      >
        {collapsed ? (
          <ChevronRight size={12} style={{ color: "var(--text-muted)" }} />
        ) : (
          <ChevronDown size={12} style={{ color: "var(--text-muted)" }} />
        )}
        <span className="text-xs font-medium" style={{ color: "var(--text-primary)" }}>
          {cat.label}
        </span>
        <input
          type="checkbox"
          ref={checkboxRef}
          checked={allOn}
          onChange={onToggleAll}
          // 点击复选框只切换该类全选/全不选，不触发头部折叠（stopPropagation）
          onClick={(e) => e.stopPropagation()}
          className="ml-auto w-3.5 h-3.5 flex-shrink-0 accent-[var(--accent)] cursor-pointer"
          title="全选/全不选"
        />
      </div>
      {!collapsed && (
        <div
          className="px-2 pb-2 pt-1 flex flex-col gap-1 border-t"
          style={{ borderColor: "var(--border)" }}
        >
          {tools.map((t) => {
            const on = enabled.has(t.id);
            return (
              // 行用 label 承载点击（整行命中切换单一 onChange；原生 checkbox 内嵌 button 会双触发）
              <label
                key={t.id}
                className="flex items-center gap-2 px-1 py-0.5 text-xs rounded hover:bg-[var(--hover)]"
                style={{
                  color: on ? "var(--text-primary)" : "var(--text-secondary)",
                }}
              >
                <input
                  type="checkbox"
                  checked={on}
                  onChange={() => onToggle(t.id)}
                  className="w-3.5 h-3.5 flex-shrink-0 accent-[var(--accent)]"
                />
                <span className="flex items-center gap-1.5">
                  {t.label}
                  {t.needsSearch && !searchReady && (
                    <span className="text-[10px]" style={{ color: "var(--text-muted)" }}>
                      （未配置搜索源，发送时自动降级）
                    </span>
                  )}
                </span>
              </label>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function AgentSettingsSection() {
  const agents = useSettingsStore((s) => s.agents);
  const addAgent = useSettingsStore((s) => s.addAgent);
  const updateAgent = useSettingsStore((s) => s.updateAgent);
  const removeAgent = useSettingsStore((s) => s.removeAgent);
  const duplicateAgent = useSettingsStore((s) => s.duplicateAgent);
  const promptNotes = useSettingsStore((s) => s.promptNotes);
  // 订阅插件运行时：插件启停/卸载变化触发本组件重渲染（插件工具表在服务层，非响应式，
  // 靠 pluginStore 收敛驱动重算；插件工具注册也经 pluginStore 的 UI 注册变更通知驱动重渲染）。
  usePluginStore((s) => s.plugins);
  const allToolsMeta: AgentToolMeta[] = [
    ...AGENT_TOOLS_META,
    ...usePluginStore.getState().pluginToolMetas(),
  ];
  // 搜索源就绪状态（订阅字段而非 isSearchConfigured 函数引用，配置变化即时刷新提示）
  const searchConfig = useSettingsStore((s) => s.searchConfig);
  const tavilyKey = useSettingsStore((s) => s.tavilyKey);
  const searchReady =
    searchConfig.provider === "tavily"
      ? !!tavilyKey
      : !!searchConfig.searxngUrl;
  // 当前选中 Agent id（null = 未选；列表为空时自动清空）
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  // 名称草稿：即时生效但名称走 blur 提交（避免每键一次 agents.json 原子写，同设置页其他面板）
  const [nameDraft, setNameDraft] = useState("");
  // 工具分类折叠状态（默认全折叠；不持久化，每次打开设置重置）
  const [collapsedCats, setCollapsedCats] = useState<Record<AgentToolCategory, boolean>>(() =>
    Object.fromEntries(AGENT_TOOL_CATEGORIES.map((c) => [c.key, true])) as Record<
      AgentToolCategory,
      boolean
    >,
  );

  const selected = agents.find((a) => a.id === selectedId) ?? null;

  // 选中变化/外部删除 → 同步名称草稿；选中项已被删除/不存在时清空选择
  useEffect(() => {
    if (selected) {
      setNameDraft(selected.name);
    } else if (selectedId !== null) {
      setSelectedId(null);
    }
  }, [selected?.id, selectedId]); // eslint-disable-line react-hooks/exhaustive-deps

  const commitName = () => {
    if (!selected) return;
    const trimmed = nameDraft.trim();
    if (trimmed) {
      if (trimmed !== selected.name) void updateAgent(selected.id, { name: trimmed });
    } else {
      // 空名不落盘、回退显示原名
      setNameDraft(selected.name);
    }
  };

  // 切换选中前先提交未 blur 的名称草稿（即时生效语义下防改名静默丢失），再切换
  const handleSelect = (id: string) => {
    commitName();
    setSelectedId(id);
  };

  // 勾选/全选从 store 最新态读 tools 计算整表替换（避免渲染闭包过期导致连续切换丢勾选）
  const currentAgentTools = () =>
    useSettingsStore.getState().agents.find((a) => a.id === selectedId)?.tools ?? [];

  const toggleTool = (id: string) => {
    const cur = currentAgentTools();
    const on = cur.includes(id);
    if (selectedId) {
      void updateAgent(selectedId, { tools: on ? cur.filter((x) => x !== id) : [...cur, id] });
    }
  };

  const toggleCategoryAll = (cat: AgentToolCategory) => {
    const ids = allToolsMeta.filter((t) => t.category === cat).map((t) => t.id);
    const cur = new Set(currentAgentTools());
    const allOn = ids.every((id) => cur.has(id));
    const next = allOn
      ? [...cur].filter((id) => !ids.includes(id))
      : Array.from(new Set([...cur, ...ids]));
    if (selectedId) void updateAgent(selectedId, { tools: next });
  };

  const handleAdd = async () => {
    const id = await addAgent();
    setSelectedId(id);
  };

  const summary = (a: AgentConfig): string =>
    a.systemPromptFile
      ? `已注册提示词：${noteTitleFromFile(a.systemPromptFile)}`
      : "无提示词";

  return (
    <section className="flex-1 min-h-0 p-5 flex flex-col">
      <div className="flex items-center justify-between mb-3">
        <div>
          <div
            className="text-sm font-medium flex items-center gap-1.5"
            style={{ color: "var(--text-primary)" }}
          >
            <Sparkles size={14} style={{ color: "var(--accent)" }} />
            Agent
          </div>
        </div>
        <button
          onClick={() => void handleAdd()}
          className="flex items-center gap-1 text-xs rounded px-2.5 py-1.5 flex-shrink-0 hover:opacity-80"
          style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
        >
          <Plus size={12} />
          新建 Agent
        </button>
      </div>

      {/* 列表恒非空：预置 Agent 在进仓库时种子补齐（settingsStore.loadVaultConfig），无需空态 */}
      <div className="flex-1 min-h-0 flex gap-3">
        {/* 左侧：Agent 列表（预置 Agent 不显示删除按钮） */}
        <div
          className="w-52 shrink-0 rounded-lg border overflow-auto"
          style={{
            borderColor: "var(--border)",
            background: "var(--bg-primary)",
          }}
        >
          {agents.map((a) => (
            <div
              key={a.id}
              className={`group flex items-center gap-1 px-2.5 py-2 cursor-pointer border-b last:border-b-0 ${
                selectedId === a.id ? "" : "hover:bg-[var(--hover)]"
              }`}
              style={{
                background:
                  selectedId === a.id ? "var(--bg-tertiary)" : undefined,
                borderColor: "var(--border)",
              }}
              onClick={() => handleSelect(a.id)}
            >
              <div className="flex-1 min-w-0">
                <div
                  className="text-xs font-medium truncate flex items-center gap-1"
                  style={{ color: "var(--text-primary)" }}
                  title={a.name}
                >
                  {a.name}
                  {a.builtin && (
                    <span
                      className="text-[9px] px-1 rounded flex-shrink-0"
                      style={{
                        color: "var(--text-muted)",
                        background: "var(--bg-tertiary)",
                        border: "1px solid var(--border)",
                      }}
                    >
                      预置
                    </span>
                  )}
                </div>
                <div
                  className="text-[11px] truncate mt-0.5"
                  style={{ color: "var(--text-muted)" }}
                  title={summary(a)}
                >
                  {summary(a)}
                </div>
              </div>
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  void duplicateAgent(a.id);
                }}
                title="复制 Agent"
                className="p-1 rounded hover:opacity-70 flex-shrink-0 opacity-0 group-hover:opacity-100"
                style={{ color: "var(--text-muted)" }}
              >
                <Copy size={12} />
              </button>
              {!a.builtin && (
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    handleSelect(a.id);
                    setConfirmDelete(true);
                  }}
                  title="删除 Agent"
                  className="p-1 rounded hover:opacity-70 flex-shrink-0 opacity-0 group-hover:opacity-100"
                  style={{ color: "#f87171" }}
                >
                  <Trash2 size={12} />
                </button>
              )}
            </div>
          ))}
        </div>

        {/* 右侧：编辑器（即时生效；名称 blur 提交） */}
        {selected && (
          <div
            className="flex-1 min-w-0 rounded-lg border p-4 overflow-auto"
            style={{
              borderColor: "var(--border)",
              background: "var(--bg-primary)",
            }}
          >
            {/* 名称 */}
            <div className="mb-4">
              <div
                className="text-xs mb-1.5"
                style={{ color: "var(--text-secondary)" }}
              >
                名称
              </div>
              <input
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                onBlur={commitName}
                placeholder="如：写作助手"
                className="w-full max-w-[280px] text-sm rounded px-2 py-1 outline-none"
                style={{
                  color: "var(--text-primary)",
                  background: "var(--input-bg)",
                  border: "1px solid var(--input-border)",
                }}
              />
            </div>

            {/* 系统提示词：一级下拉直接选择已注册提示词笔记（发送时实时读正文注入，外部编辑即时生效） */}
            <div className="mb-4">
              <div
                className="text-xs mb-1.5"
                style={{ color: "var(--text-secondary)" }}
              >
                系统提示词
              </div>
              <DropdownSelect
                value={selected?.systemPromptFile ?? ""}
                onChange={(v) => {
                  if (selected) void updateAgent(selected.id, { systemPromptFile: v || undefined });
                }}
                options={[
                  { value: "", label: "不使用" },
                  ...promptNotes.map((f) => ({
                    value: f,
                    label: noteTitleFromFile(f),
                  })),
                ]}
                emptyText="暂无已注册提示词（在文件面板右键笔记 → 注册为提示词）"
                placeholder="选择提示词笔记"
                className="w-full text-sm rounded px-2 py-1"
                style={{
                  color: "var(--text-secondary)",
                  background: "var(--input-bg)",
                  border: "1px solid var(--input-border)",
                }}
              />
              <p
                className="text-[11px] mt-1"
                style={{ color: "var(--text-muted)" }}
              >
                文件面板右键 .md 可注册为系统提示词
              </p>
            </div>

            {/* 工具：全部工具按分类折叠分组、可勾选生效；分类头部三态复选框（全选/部分/全不选） */}
            <div className="mb-4">
              <div
                className="text-xs mb-1.5"
                style={{ color: "var(--text-secondary)" }}
              >
                工具
              </div>
              <div className="flex flex-col gap-2">
                {AGENT_TOOL_CATEGORIES.map((cat) => (
                  <ToolCategoryGroup
                    key={cat.key}
                    cat={cat}
                    tools={allToolsMeta.filter((t) => t.category === cat.key)}
                    enabled={new Set(selected.tools)}
                    searchReady={searchReady}
                    collapsed={collapsedCats[cat.key]}
                    onToggle={toggleTool}
                    onToggleAll={() => toggleCategoryAll(cat.key)}
                    onToggleCollapsed={() =>
                      setCollapsedCats((prev) => ({
                        ...prev,
                        [cat.key]: !prev[cat.key],
                      }))
                    }
                  />
                ))}
              </div>
            </div>
          </div>
        )}
      </div>

      {confirmDelete && (
        <ConfirmDialog
          title="删除 Agent"
          confirmText="删除"
          onConfirm={() => {
            if (selectedId) void removeAgent(selectedId);
            setSelectedId(null);
            setConfirmDelete(false);
          }}
          onCancel={() => setConfirmDelete(false)}
        />
      )}
    </section>
  );
}
