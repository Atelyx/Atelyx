/**
 * 插件市场浏览：官方索引（CDN）搜索/筛选/安装。
 *
 * - 搜索：名称/id/描述/仓库全文匹配；类型筛选（全部/各类型）；徽标展示（官方/精选）
 * - 安装 = 按 repo 取源码（git clone，无 git 回退 GitHub 源码包），默认未启用，
 *   由「已安装」tab 确认启用；安装前提示社区插件未受官方审查、可访问本地数据
 * - 顶部下拉（类型筛选/安装作用域）用统一 DropdownSelect 组件（自绘弹层，非原生 select）
 * 分层：只经 pluginStore 触达插件能力。
 */
import { useEffect, useMemo, useState } from "react";
import { Download, RefreshCw, Star } from "lucide-react";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { usePluginStore } from "@/stores/pluginStore";
import { PLUGIN_BADGE_LABELS, PLUGIN_SOURCE_LABELS, PLUGIN_TYPE_LABELS } from "@/constants/plugins";
import type { PluginIndexEntry, PluginScope, PluginType } from "@/types";

const TYPE_FILTERS: { value: PluginType | "all"; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "tool", label: "AI 工具" },
  { value: "panel", label: "面板" },
  { value: "app", label: "应用页面" },
  { value: "node", label: "画布节点" },
  { value: "theme", label: "皮肤" },
  { value: "setting", label: "设置项" },
  { value: "command", label: "命令" },
  { value: "background", label: "后台服务" },
  { value: "tableview", label: "表格视图" },
];

const SCOPE_OPTIONS: { value: PluginScope; label: string }[] = [
  { value: "app", label: "本机" },
  { value: "vault", label: "随仓库共享" },
];

export function MarketplaceSection() {
  const marketItems = usePluginStore((s) => s.marketItems);
  const marketLoaded = usePluginStore((s) => s.marketLoaded);
  const marketLoading = usePluginStore((s) => s.marketLoading);
  const marketError = usePluginStore((s) => s.marketError);
  const loadMarket = usePluginStore((s) => s.loadMarket);
  const install = usePluginStore((s) => s.install);
  const plugins = usePluginStore((s) => s.plugins);

  const [query, setQuery] = useState("");
  const [typeFilter, setTypeFilter] = useState<PluginType | "all">("all");
  const [scope, setScope] = useState<PluginScope>("app");
  const [installingRepo, setInstallingRepo] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [confirming, setConfirming] = useState<PluginIndexEntry | null>(null);

  useEffect(() => {
    if (!marketLoaded) void loadMarket();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 仅挂载时加载一次（store 内幂等）
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return marketItems.filter((it) => {
      if (typeFilter !== "all" && it.type !== typeFilter) return false;
      if (q.length === 0) return true;
      return [it.name, it.id, it.repo, it.tagline ?? "", it.description ?? ""]
        .join(" ")
        .toLowerCase()
        .includes(q);
    });
  }, [marketItems, query, typeFilter]);

  /** 安装统一入口：按包内实际 id 如实提示（替换了哪一行由落位结果判定，不认索引自报 id）。 */
  const doInstall = async (entry: PluginIndexEntry): Promise<void> => {
    const repo = entry.repo;
    if (installingRepo) return;
    setInstallingRepo(repo);
    setNotice(null);
    try {
      const result = await install(repo, scope);
      const idNote = result.id === entry.id ? "" : `（包内 id 为 ${result.id}，与索引 id ${entry.id} 不同）`;
      const replacedNote = result.replaced
        ? "已替代同名行，该行现为停用状态，到「已安装」tab 启用"
        : "默认未启用，到「已安装」tab 启用";
      setNotice({ kind: "ok", text: `已安装 ${repo}${idNote}：${replacedNote}` });
    } catch (e) {
      setNotice({ kind: "error", text: `安装失败：${e instanceof Error ? e.message : String(e)}` });
    } finally {
      setInstallingRepo(null);
    }
  };

  return (
    <div className="flex flex-col gap-3 min-h-0">
      {/* 搜索 / 筛选 / 安装作用域 / 刷新 */}
      <div className="flex gap-2 flex-wrap">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索插件（名称 / id / 描述 / 仓库）"
          className="flex-1 min-w-[180px] px-2.5 py-1.5 rounded text-xs border outline-none"
          style={{ background: "var(--bg-primary)", borderColor: "var(--border)", color: "var(--text-primary)" }}
        />
        <DropdownSelect
          value={typeFilter}
          onChange={(v) => setTypeFilter(v as PluginType | "all")}
          options={TYPE_FILTERS.map((f) => ({ value: f.value, label: f.label }))}
          title="类型筛选"
          className="text-xs rounded px-2 py-1.5"
          style={{
            color: "var(--text-primary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
        <DropdownSelect
          value={scope}
          onChange={(v) => setScope(v as PluginScope)}
          options={SCOPE_OPTIONS.map((o) => ({ value: o.value, label: o.label }))}
          title="安装作用域"
          className="text-xs rounded px-2 py-1.5"
          style={{
            color: "var(--text-primary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
        <button
          onClick={() => void loadMarket(true)}
          title="刷新市场索引"
          className="px-2 py-1.5 rounded border hover:bg-[var(--hover)]"
          style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
        >
          <RefreshCw size={13} />
        </button>
      </div>

      {marketError && (
        <div className="text-xs" style={{ color: "#f59e0b" }}>
          {marketError}
        </div>
      )}
      {notice && (
        <div className="text-xs break-words" style={{ color: notice.kind === "ok" ? "var(--text-secondary)" : "#f87171" }}>
          {notice.text}
        </div>
      )}

      {/* 插件列表 */}
      <div className="flex-1 min-h-0 overflow-auto space-y-2">
        {marketLoading && filtered.length === 0 && (
          <div className="text-sm py-8 text-center" style={{ color: "var(--text-muted)" }}>
            加载市场…
          </div>
        )}
        {marketLoaded && filtered.length === 0 && !marketLoading && (
          <div className="text-sm py-8 text-center" style={{ color: "var(--text-muted)" }}>
            没有匹配的插件
          </div>
        )}
        {filtered.map((it) => {
          // 同 id 不同作者仓库是不同插件（徽标/安装均按 repo 锚定）：key 与安装态判定都按 repo。
          const installed = Object.values(plugins).some((p) => {
            if (p.id !== it.id) return false;
            const folder = p.installDir.split(/[\\/]/).pop() ?? "";
            return folder === it.repo.split("/")[1];
          });
          // 同名 id 已有行（随应用分发或已安装）：安装将以本包实现替代那一行。
          const sameIdRow = Object.values(plugins).find((p) => p.id === it.id);
          return (
            <div
              key={it.repo}
              className="rounded border p-3"
              style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
            >
              <div className="flex items-center gap-2">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                      {it.name}
                    </span>
                    {it.badge && (
                      <span
                        className="text-[10px] px-1.5 py-0.5 rounded"
                        style={{
                          color: it.badge === "official" ? "var(--accent)" : "#f59e0b",
                          background: it.badge === "official" ? "rgba(212,175,55,0.12)" : "rgba(245,158,11,0.12)",
                        }}
                      >
                        {PLUGIN_BADGE_LABELS[it.badge]}
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] truncate flex items-center gap-1" style={{ color: "var(--text-muted)" }}>
                    <span className="truncate">
                      {it.type ? PLUGIN_TYPE_LABELS[it.type] : "插件"} · {it.repo} · {it.id}
                    </span>
                    <Star size={11} className="flex-shrink-0" />
                    <span className="flex-shrink-0">{it.stars}</span>
                  </div>
                </div>
                {installed ? (
                  <span className="text-[11px] px-2 py-1 rounded" style={{ color: "var(--text-secondary)" }}>
                    已安装
                  </span>
                ) : (
                  <button
                    onClick={() => setConfirming(it)}
                    disabled={installingRepo !== null}
                    className="flex items-center gap-1 px-2.5 py-1.5 rounded text-xs disabled:opacity-50"
                    style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
                  >
                    <Download size={13} />
                    {installingRepo === it.repo ? "安装中…" : sameIdRow ? "安装（替换同名）" : "安装"}
                  </button>
                )}
              </div>
              {sameIdRow && !installed && (
                <div className="mt-1 text-[11px] break-words" style={{ color: "var(--text-muted)" }}>
                  同名 id 行已存在（{PLUGIN_SOURCE_LABELS[sameIdRow.sourceKind]}，按索引自报 id 判定）。
                  若包内清单 id 与之一致，安装将以本包实现替代该行；新装一律停用（需到「已安装」tab 启用），
                  实际落位 id 以安装结果提示为准。
                  {sameIdRow.installDir === "" && scope === "vault"
                    ? "该行由随应用分发的实现占用：请选「本机」作用域安装。"
                    : ""}
                </div>
              )}
              {it.tagline && (
                <div className="mt-1 text-xs break-words" style={{ color: "var(--text-secondary)" }}>
                  {it.tagline}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {confirming && (
        <ConfirmDialog
          title={`安装「${confirming.name}」`}
          description="社区插件未经官方审查，可读写你的文件、运行程序、联网。仅从你信任的来源安装。"
          confirmText="继续安装"
          danger={false}
          onConfirm={() => {
            const entry = confirming;
            setConfirming(null);
            void doInstall(entry);
          }}
          onCancel={() => setConfirming(null)}
        />
      )}
    </div>
  );
}
