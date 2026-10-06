/**
 * 插件市场浏览：官方索引（CDN）搜索 / 类型筛选 / 安装。
 * 安装 = 按 repo 取源码（git clone，无 git 回退 GitHub 源码包）；安装前弹确认（社区插件未经官方审查、可访问本地数据）。
 * 只经 pluginStore 触达插件能力；全新插件默认停用、替换行沿用原启用状态，由「已安装」tab 确认启停。
 */
import { useEffect, useMemo, useState } from "react";
import {
  AppWindow,
  Boxes,
  Check,
  Download,
  LayoutGrid,
  LayoutPanelTop,
  Palette,
  Puzzle,
  RefreshCw,
  Server,
  Shield,
  SlidersHorizontal,
  Star,
  Table2,
  Terminal,
  Wrench,
  type LucideIcon,
} from "lucide-react";
import { Input } from "@/components/common/Input";
import { IconButton } from "@/components/common/Button";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { usePluginStore } from "@/stores/pluginStore";
import { PLUGIN_BADGE_LABELS, PLUGIN_SOURCE_LABELS, PLUGIN_TYPE_LABELS } from "@/constants/plugins";
import type { PluginIndexEntry, PluginType } from "@/types";

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

/** 插件类型 → 卡片图标（类型徽标之外的视觉区分）。 */
const TYPE_ICONS: Record<PluginType, LucideIcon> = {
  tool: Wrench,
  panel: LayoutPanelTop,
  app: AppWindow,
  node: Boxes,
  theme: Palette,
  setting: SlidersHorizontal,
  command: Terminal,
  background: Server,
  tableview: Table2,
};

/** 卡片胶囊徽标统一尺寸。 */
const TAG_CLASS = "inline-flex items-center gap-1 h-5 px-2 rounded-full text-micro font-medium whitespace-nowrap";

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
      const result = await install(repo);
      const idNote = result.id === entry.id ? "" : `（包内 id 为 ${result.id}，与索引 id ${entry.id} 不同）`;
      const enableNote = result.replaced
        ? "已替代同名行，沿用其原启用状态"
        : "默认未启用，到「已安装」tab 启用";
      // 保留数据恢复等非致命警示优先于成功文案，不能被成功提示吞掉
      setNotice(
        result.warning
          ? { kind: "error", text: result.warning }
          : { kind: "ok", text: `已安装 ${repo}${idNote}：${enableNote}` },
      );
    } catch (e) {
      setNotice({ kind: "error", text: `安装失败：${e instanceof Error ? e.message : String(e)}` });
    } finally {
      setInstallingRepo(null);
    }
  };

  return (
    <div className="flex flex-col gap-3 min-h-0">
      {/* 搜索 / 刷新 */}
      <div className="flex gap-2">
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索插件（名称 / id / 描述 / 仓库）"
          className="flex-1 min-w-0"
        />
        <IconButton
          icon={<RefreshCw size={13} />}
          label="刷新市场索引"
          onClick={() => void loadMarket(true)}
          variant="secondary"
          size="sm"
          className="border"
          style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
        />
      </div>

      {/* 类型筛选胶囊：类型多且面板可窄，单行横向滚动（折行会把工具条撑高） */}
      <div className="flex items-center gap-1.5 overflow-x-auto pb-0.5">
        {TYPE_FILTERS.map((f) => {
          const active = f.value === typeFilter;
          const Icon = f.value === "all" ? LayoutGrid : TYPE_ICONS[f.value];
          return (
            <button
              key={f.value}
              onClick={() => setTypeFilter(f.value)}
              aria-pressed={active}
              className="inline-flex items-center gap-1.5 h-[26px] px-2.5 rounded-full border text-xs flex-shrink-0 transition-colors"
              style={
                active
                  ? {
                      color: "var(--accent)",
                      background: "var(--accent-soft)",
                      borderColor: "color-mix(in srgb, var(--accent) 34%, transparent)",
                    }
                  : {
                      color: "var(--text-secondary)",
                      background: "var(--bg-primary)",
                      borderColor: "var(--border)",
                    }
              }
            >
              <Icon size={12} className="flex-shrink-0" />
              {f.label}
            </button>
          );
        })}
      </div>

      {marketError && (
        <div className="text-xs" style={{ color: "var(--warning)" }}>
          {marketError}
        </div>
      )}
      {notice && (
        <div className="text-xs break-words" style={{ color: notice.kind === "ok" ? "var(--text-secondary)" : "var(--danger)" }}>
          {notice.text}
        </div>
      )}

      {/* 插件卡片网格 */}
      <div className="flex-1 min-h-0 overflow-auto">
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
        <div
          className="grid gap-3 pb-2"
          style={{ gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))" }}
        >
          {filtered.map((it) => {
            // 同 id 不同作者仓库是不同插件（徽标/安装均按 repo 锚定）：key 与安装态判定都按 repo。
            const installed = Object.values(plugins).some((p) => {
              if (p.id !== it.id) return false;
              const folder = p.installDir.split(/[\\/]/).pop() ?? "";
              return folder === it.repo.split("/")[1];
            });
            // 同名 id 已有行（随应用分发或已安装）：安装将以本包实现替代那一行。
            const sameIdRow = Object.values(plugins).find((p) => p.id === it.id);
            const Icon = (it.type && TYPE_ICONS[it.type]) || Puzzle;
            return (
              <div
                key={it.repo}
                className="flex flex-col gap-3 rounded-[var(--radius-md)] border border-[var(--border)] p-3 transition-colors bg-[var(--bg-tertiary)] hover:border-[var(--border-strong)]"
              >
                {/* 图标 + 名称/作者 + 徽标 */}
                <div className="flex items-start gap-3">
                  <div
                    className="w-9 h-9 shrink-0 grid place-items-center rounded-[var(--radius-sm)]"
                    style={{ background: "var(--bg-overlay)", border: "1px solid var(--border)", color: "var(--accent)" }}
                  >
                    <Icon size={18} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                      {it.name}
                    </div>
                    <div
                      className="text-micro font-mono truncate mt-0.5"
                      style={{ color: "var(--text-muted)" }}
                      title={`${it.repo} · ${it.id}`}
                    >
                      {it.repo}
                    </div>
                  </div>
                  {it.badge && (
                    <span
                      className={TAG_CLASS}
                      style={{
                        color: it.badge === "official" ? "var(--accent)" : "var(--info)",
                        background:
                          it.badge === "official"
                            ? "var(--accent-soft)"
                            : "color-mix(in srgb, var(--info) 12%, transparent)",
                        border: `1px solid ${
                          it.badge === "official"
                            ? "color-mix(in srgb, var(--accent) 32%, transparent)"
                            : "color-mix(in srgb, var(--info) 30%, transparent)"
                        }`,
                      }
              }
                    >
                      {it.badge === "official" ? <Shield size={11} /> : <Star size={11} />}
                      {PLUGIN_BADGE_LABELS[it.badge]}
                    </span>
                  )}
                </div>

                {/* 简介 */}
                <div className="text-xs leading-[18px] min-h-[36px]" style={{ color: "var(--text-secondary)" }}>
                  {it.tagline || it.description || "暂无简介"}
                </div>

                {sameIdRow && !installed && (
                  <div className="text-micro break-words" style={{ color: "var(--text-muted)" }}>
                    同名 id 行已存在（{PLUGIN_SOURCE_LABELS[sameIdRow.sourceKind]}，按索引自报 id 判定）。
                    若包内清单 id 与之一致，安装将以本包实现替代该行并沿用其原启用状态；全新插件默认停用
                    （需到「已安装」tab 启用），实际落位 id 以安装结果提示为准。
                    {sameIdRow.installDir === "" ? "该行由随应用分发的实现占用：安装将整体替代该行。" : ""}
                  </div>
                )}

                {/* 类型 / 星标 + 安装动作 */}
                <div
                  className="mt-auto flex items-center gap-2 pt-3 border-t"
                  style={{ borderColor: "var(--border-subtle)" }}
                >
                  <span
                    className={TAG_CLASS}
                    style={{ color: "var(--text-secondary)", background: "var(--bg-overlay)", border: "1px solid var(--border)" }}
                  >
                    {it.type ? PLUGIN_TYPE_LABELS[it.type] : "插件"}
                  </span>
                  <span
                    className={`${TAG_CLASS} font-mono`}
                    style={{ color: "var(--text-muted)", background: "var(--bg-overlay)", border: "1px solid var(--border)" }}
                    title="星标数"
                  >
                    <Star size={11} />
                    {it.stars}
                  </span>
                  <div className="ml-auto">
                    {installed ? (
                      <span
                        className={TAG_CLASS}
                        style={{
                          color: "var(--success)",
                          background: "color-mix(in srgb, var(--success) 10%, transparent)",
                          border: "1px solid color-mix(in srgb, var(--success) 30%, transparent)",
                        }}
                      >
                        <Check size={11} />
                        已安装
                      </span>
                    ) : (
                      <button
                        onClick={() => setConfirming(it)}
                        disabled={installingRepo !== null}
                        className="inline-flex items-center gap-1 h-6 px-2.5 rounded-[var(--radius-sm)] text-xs disabled:opacity-50"
                        style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
                      >
                        <Download size={13} />
                        {installingRepo === it.repo ? "安装中…" : sameIdRow ? "安装（替换同名）" : "安装"}
                      </button>
                    )}
                  </div>
                </div>
              </div>
            );
          })}
        </div>
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
