/**
 * 文本展示工具。
 */
import type { Node as FlowNode } from "@xyflow/react";
import type { ProviderConfig, SearchResultData, TableData, TextData, MediaData } from "@/types";

/** 模型显示名：昵称优先，缺省 = API 模型 ID（长 ID 可用昵称替代，值仍存 ID）。 */
function modelDisplayName(provider: ProviderConfig, modelId: string): string {
  return provider.models.find((m) => m.id === modelId)?.nickname ?? modelId;
}

/**
 * 按所属供应商作用域的模型显示名：该模型 ID 跨供应商同名（不同供应商混用）时带「供应商名 · 」前缀消歧，
 * 否则仅模型名（单供应商场景无噪音）。供模型触发器/默认生效模型显示用（区分同一 ID 的不同供应商实例）。
 */
export function modelDisplayLabel(
  providers: ProviderConfig[],
  provider: ProviderConfig,
  modelId: string,
): string {
  const name = modelDisplayName(provider, modelId) || modelId;
  const shared = providers.some(
    (p) => p.id !== provider.id && p.models.some((m) => m.id === modelId),
  );
  return shared ? `${provider.name} · ${name}` : name;
}

/** 截取内容前缀作为 @chip 显示名（约 12 字 + 省略号）。 */
export function prefix(text: string, len = 12): string {
  const t = (text ?? "").replace(/\s+/g, " ").trim();
  return t.length > len ? t.slice(0, len) + "…" : t;
}

/**
 * @ 提及显示名：文本节点取**笔记名称（title）**（缺失回退正文前缀），
 * 媒体节点取文件名，表格节点取标题。输入框内插入的可见文本 = `@${mentionTextOf(node)}`，发送时按此文本就地替换为引用内容。
 */
export function mentionTextOf(node: FlowNode): string {
  if (node.type === "text") {
    const d = node.data as unknown as TextData;
    return d.title || prefix(d.bodyMd ?? "") || "文本";
  }
  if (node.type === "search") {
    const d = node.data as unknown as SearchResultData;
    return prefix(d.query) || "搜索";
  }
  if (node.type === "table") {
    const d = node.data as unknown as TableData;
    return d.title || "表格";
  }
  if (node.type === "group") {
    const d = node.data as unknown as { label?: string };
    return prefix(d.label ?? "") || "分组";
  }
  if (node.type === "link") {
    const d = node.data as unknown as { url?: string };
    return prefix(d.url ?? "") || "链接";
  }
  const md = node.data as unknown as MediaData;
  return prefix(md.name ?? md.mime) || "文件";
}

/**
 * @标签 原位插入的纯文本变换（对话节点与 AI 对话面板输入框共用同一语义）：
 * 把 `[insertAt, end)` 区间（= `@` 到光标间的过滤词）替换为「前导分隔空格 + 标签 + 尾随空格」，
 * 返回新文本与「尾随空格之后」的光标位置。
 *
 * 插入位置只由传入的 `prev` 决定，调用方须在 `setInput(prev => ...)` 内按 `prev` 计算——
 * 用渲染期闭包的 `input` 会让同一 tick 内先后到达的两次插入互相覆盖（后写盖先写、@标签丢失）。
 * 前导空格仅在前文非空且不以空白结尾时补，保证胶囊前后为空白区（胶囊背景外扩不遮相邻字符）。
 */
export function insertMentionTag(
  prev: string,
  insertAt: number,
  end: number,
  mentionText: string,
): { text: string; caret: number } {
  const from = Math.min(Math.max(insertAt, 0), prev.length);
  const to = Math.min(Math.max(end, from), prev.length);
  const before = prev.slice(0, from);
  const sep = before && !/\s$/.test(before) ? " " : "";
  return {
    text: prev.slice(0, from) + sep + mentionText + " " + prev.slice(to),
    caret: from + sep.length + mentionText.length + 1,
  };
}

/** 输入框内 @提及 的命中片段（含精确位置）。 */
export interface MentionHit {
  start: number;
  end: number;
  mention: { nodeId: string; text: string };
}

/**
 * 扫描输入文本中的 @提及 命中（每条 mention 条目最多返回一处：首个不重叠的实例；
 * 同一节点被选择多次会产生多条条目，重复实例由多条条目分别命中）。
 * 同一位置只命中一次（重叠检查），命中按 start 升序。
 * 供组件渲染 @标签（splitMentions）与 store 发送时精确替换共用——
 * 保证「删除 / 替换按实例位置」而非 indexOf 首个出现（重复 @提及 时不错位）。
 */
export function scanMentionHits(
  input: string,
  mentions: { nodeId: string; text: string }[]
): MentionHit[] {
  const hits: MentionHit[] = [];
  for (const m of mentions) {
    if (!m.text) continue; // 空显示名不参与扫描（避免 {0,0} 命中在开头插入内容）
    let from = 0;
    while (from < input.length) {
      const idx = input.indexOf(m.text, from);
      if (idx < 0) break;
      const end = idx + m.text.length;
      if (!hits.some((h) => idx < h.end && end > h.start)) {
        hits.push({ start: idx, end, mention: m });
        break;
      }
      from = idx + 1;
    }
  }
  hits.sort((a, b) => a.start - b.start);
  return hits;
}

/** 输入文本按 @提及 切分后的段：普通文本段或标签段（文本匹配，不重叠，顺序排序）。 */
export interface MentionSeg {
  text: string;
  start: number;
  mention: { nodeId: string; text: string } | null;
}

/** 按 @提及 命中把输入文本切成「普通文本 / 标签」交替段（对话节点与 AI 对话面板输入框共用）。
 * 标签段吞相邻空格（插入路径恒补「前导分隔 + 尾随」空格）：胶囊金底覆盖空格 = 视觉整体，
 * 与 textarea 真实文本一致不破坏对齐；前导空格不吞前段已占的位置（防相邻胶囊争抢同一空格）。 */
export function splitMentions(
  input: string,
  mentions: { nodeId: string; text: string }[]
): MentionSeg[] {
  const segs: MentionSeg[] = [];
  let cursor = 0;
  for (const h of scanMentionHits(input, mentions)) {
    let start = h.start;
    let end = h.end;
    if (start > 0 && input[start - 1] === " " && start - 1 >= cursor) start -= 1;
    if (end < input.length && input[end] === " ") end += 1;
    if (start > cursor) segs.push({ text: input.slice(cursor, start), start: cursor, mention: null });
    segs.push({ text: input.slice(start, end), start, mention: h.mention });
    cursor = end;
  }
  if (cursor < input.length) segs.push({ text: input.slice(cursor), start: cursor, mention: null });
  return segs;
}
