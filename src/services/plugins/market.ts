/**
 * 插件市场数据源：官方索引（CDN 静态 JSON）消费。
 *
 * - `index.json`：插件清单（id/name/repo/stars/type…）
 * - `blocklist.json`：封禁表 `{ id: 原因 }`（命中 = 不可安装/启用）
 * - `endorsed.json`：精选表 `{ owner/repo: 理由 }`（授予精选徽标）
 *
 * 消费策略：内存 + localStorage 缓存（6h 过期），离线/失败时回落缓存快照并带时间戳提示；
 * 徽标 = 作者账号锚点（GitHub `owner/repo`，账号归属 GitHub 背书不可伪造）：
 * official = repo owner 命中官方名单；精选 = 严格按 `owner/repo` 命中精选表——
 * 不同作者巧合同 id 视为不同插件，自报 id 不能继承任一徽标（id + 作者账号双重校验）。
 * 安装/更新取源码（git clone，无 git 回退 GitHub 源码包；见 commands/plugin.rs）。
 */
import type { PluginBadge, PluginIndex, PluginIndexEntry } from "@/types";
import {
  OFFICIAL_PLUGIN_ORGS,
  PLUGIN_BLOCKLIST_URL,
  PLUGIN_ENDORSED_URL,
  PLUGIN_INDEX_CACHE_MS,
  PLUGIN_INDEX_URL,
} from "@/constants/plugins";

/** 市场快照（内存/缓存形态）。 */
export interface MarketSnapshot {
  items: PluginIndexEntry[];
  generatedAt: string;
  /** 拉取时间（ms，过期判定用）。 */
  fetchedAt: number;
}

const INDEX_CACHE_KEY = "atelyx:plugin-market:v1";

/** 缓存是否过期（无缓存 = 过期）。 */
export function isMarketStale(fetchedAt: number): boolean {
  return Date.now() - fetchedAt > PLUGIN_INDEX_CACHE_MS;
}

/** 官方账号判定（repo owner 命中官方名单 → official 徽标）。 */
export function isOfficialRepo(repo: string): boolean {
  const owner = repo.split("/")[0];
  return (OFFICIAL_PLUGIN_ORGS as readonly string[]).includes(owner);
}

/** 徽标：official = repo owner 命中官方名单；精选 = 严格按 `owner/repo` 命中精选表。
 *  不按自报 id 授予（id 可被伪造，仓库归属 GitHub 背书）——id + 作者账号双重校验，
 *  同 id 不同作者仓库不继承徽标。 */
export function badgeFor(
  entry: Pick<PluginIndexEntry, "repo" | "id">,
  endorsed: ReadonlySet<string>,
): PluginBadge | undefined {
  if (isOfficialRepo(entry.repo)) return "official";
  if (endorsed.has(entry.repo)) return "endorsed";
  return undefined;
}

async function fetchJson<T>(url: string, timeoutMs = 15000): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    // cache: "no-store"：jsdelivr 对 @main 下发 7 天 max-age，WebView2 HTTP 缓存会命中旧索引——
    // 市场/封禁/精选必须每次走网络，否则新插件最长 7 天、下架条目最长 7 天不可见。
    const resp = await fetch(url, { signal: controller.signal, cache: "no-store" });
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return (await resp.json()) as T;
  } finally {
    clearTimeout(timer);
  }
}

function readCache<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
}

function writeCache<T>(key: string, data: T): void {
  try {
    localStorage.setItem(key, JSON.stringify(data));
  } catch {
    // 存储不可用（隐私模式/配额）静默跳过，仅影响离线降级
  }
}

/** 读取市场缓存快照（离线降级 + 启动秒开用）。 */
export function readMarketCache(): MarketSnapshot | null {
  return readCache<MarketSnapshot>(INDEX_CACHE_KEY);
}

/** 拉取市场索引（index + blocklist + endorsed 合并；失败抛错，由调用方回落缓存）。 */
export async function fetchMarketIndex(): Promise<MarketSnapshot> {
  const [index, blocklist, endorsed] = await Promise.all([
    fetchJson<PluginIndex>(PLUGIN_INDEX_URL),
    fetchJson<Record<string, string>>(PLUGIN_BLOCKLIST_URL).catch(() => ({}) as Record<string, string>),
    fetchJson<Record<string, string>>(PLUGIN_ENDORSED_URL).catch(() => ({}) as Record<string, string>),
  ]);
  const endorsedSet = new Set(Object.keys(endorsed ?? {}));
  const items = (index?.items ?? []).map((it) => ({
    ...it,
    badge: badgeFor(it, endorsedSet),
    blockedReason: (blocklist ?? {})[it.id],
  }));
  const snapshot: MarketSnapshot = {
    items,
    generatedAt: index?.generatedAt ?? "",
    fetchedAt: Date.now(),
  };
  writeCache(INDEX_CACHE_KEY, snapshot);
  return snapshot;
}
