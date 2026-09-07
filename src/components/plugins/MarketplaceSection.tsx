/**
 * 插件市场浏览：官方索引（CDN）搜索/筛选/安装。
 *
 * - 搜索：名称/id/描述/仓库全文匹配；类型筛选（全部/各类型）；徽标展示（官方/精选）
 * - 封禁条目灰显不可安装（官方下架）
 * - 安装 = 按 repo 取源码（git clone，无 git 回退 GitHub 源码包），默认未启用，
 *   由「已安装」tab 确认启用；scope 由本区顶部的安装作用域选择决定
 * - 顶部下拉（类型筛选/安装作用域）用统一 DropdownSelect 组件（自绘弹层，非原生 select）
 * 分层：只经 pluginStore 触达插件能力。
 */
import { useEffect, useMemo, useState } from "react";
import { Download, RefreshCw } from "lucide-react";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { usePluginStore } from "@/stores/pluginStore";
import { PLUGIN_BADGE_LABELS, PLUGIN_TYPE_LABELS } from "@/constants/plugins";
import type { PluginScope, PluginType } from "@/types";

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

  const doInstall = async (repo: string): Promise<void> => {
    if (installingRepo) return;
    setInstallingRepo(repo);
    setNotice(null);
    try {
      await install(repo, scope);
      setNotice({ kind: "ok", text: `已安装 ${repo}（默认未启用，到「已安装」tab 启用）` });
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
          const installed = !!plugins[it.id];
          const blocked = !!it.blockedReason;
          return (
            <div
              key={it.id}
              className="rounded border p-3"
              style={{ borderColor: "var(--border)", background: "var(--bg-primary)", opacity: blocked ? 0.55 : 1 }}
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
                    {blocked && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ color: "#f87171", background: "rgba(248,113,113,0.1)" }}>
                        已被官方下架
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] truncate" style={{ color: "var(--text-muted)" }}>
                    {it.type ? PLUGIN_TYPE_LABELS[it.type] : "插件"} · {it.repo} · {it.id} · ⭐{it.stars}
                  </div>
                </div>
                {blocked ? (
                  <span className="text-[11px] px-2 py-1 rounded" style={{ color: "var(--text-muted)" }}>
                    不可安装
                  </span>
                ) : installed ? (
                  <span className="text-[11px] px-2 py-1 rounded" style={{ color: "var(--text-secondary)" }}>
                    已安装
                  </span>
                ) : (
                  <button
                    onClick={() => void doInstall(it.repo)}
                    disabled={installingRepo !== null}
                    className="flex items-center gap-1 px-2.5 py-1.5 rounded text-xs disabled:opacity-50"
                    style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
                  >
                    <Download size={13} />
                    {installingRepo === it.repo ? "安装中…" : "安装"}
                  </button>
                )}
              </div>
              {it.tagline && (
                <div className="mt-1 text-xs break-words" style={{ color: "var(--text-secondary)" }}>
                  {it.tagline}
                </div>
              )}
              {blocked && it.blockedReason && (
                <div className="mt-1 text-[11px] break-words" style={{ color: "#f87171" }}>
                  下架原因：{it.blockedReason}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
