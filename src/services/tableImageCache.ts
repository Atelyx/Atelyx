/**
 * 表格图片显示缓存：单元格存的仓库相对路径 → dataURL（底层走 `read_attachment_data_url`，与画布媒体节点同源）。
 */
import { readAttachmentDataUrl } from "@/services/vault";

/** 缓存条目上限：~256 张（大表全量图片超限时自动淘汰最久未用，重渲染再读）。 */
const MAX_ENTRIES = 256;

interface CacheEntry {
  url: string;
  at: number;
}

/** 键 = 相对路径（含 tableId 目录段，跨表天然不撞）。 */
const cache = new Map<string, CacheEntry>();
/** 进行中的读取：同一路径并发请求共用同一 promise（首读突发时去重）。 */
const inflight = new Map<string, Promise<string>>();

/** 解析表格图片条目为 dataURL；进行中复用、成功入缓存、失败丢弃（下次重试），命中返回缓存内原字符串引用（零拷贝）。
 * 遗留内嵌 dataURL 条目原样透传（不读盘不入缓存——大字符串缓存无收益）。 */
export function resolveTableImageUrl(file: string): Promise<string> {
  if (file.startsWith("data:")) return Promise.resolve(file);
  const hit = cache.get(file);
  if (hit) {
    hit.at = Date.now();
    return Promise.resolve(hit.url);
  }
  const pending = inflight.get(file);
  if (pending) return pending;
  const p = readAttachmentDataUrl(file)
    .then((url) => {
      inflight.delete(file);
      cache.set(file, { url, at: Date.now() });
      if (cache.size > MAX_ENTRIES) evictLeastRecent();
      return url;
    })
    .catch((e) => {
      inflight.delete(file);
      throw e;
    });
  inflight.set(file, p);
  return p;
}

/** 清空缓存（load/clear 时调用：换表后旧图不入内存，防长会话内存累积）。进行中读取一并清掉，避免未完成读取 `.then` 写回复活陈旧条目。 */
export function clearTableImageCache(): void {
  cache.clear();
  inflight.clear();
}

function evictLeastRecent(): void {
  let oldestKey: string | null = null;
  let oldestAt = Infinity;
  for (const [k, v] of cache) {
    if (v.at < oldestAt) {
      oldestAt = v.at;
      oldestKey = k;
    }
  }
  if (oldestKey !== null) cache.delete(oldestKey);
}