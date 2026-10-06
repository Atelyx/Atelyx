/**
 * Frontmatter 解析/序列化工具（笔记属性面板数据层）。解析用 `gray-matter`（内部 js-yaml v3）：
 * 兼容 `tags: [a, b]` 与块级两种数组写法；stringify 必须显式 `lineWidth`（防默认 80 列折行长值）
 * 与 `noRefs`（防 `&ref` 引用标记）；空对象序列化不输出 `---` 块（删光属性即回到无 frontmatter 态）。
 */
import matter from "gray-matter";
import { BADGE_PROPERTY_KEYS } from "@/constants/notes";

// gray-matter 依赖 Node 全局 Buffer（lib/utils.js 的 toBuffer 调 Buffer.from，每次解析都执行），
// WebView2 浏览器无此全局 → 提供最小 shim：本项目输入恒为字符串，原样返回即可（kind-of 判断 buffer 类型不依赖全局）。
if (typeof (globalThis as { Buffer?: unknown }).Buffer === "undefined") {
  (globalThis as { Buffer?: unknown }).Buffer = {
    from: (input: string) => input,
  };
}

/** 解析结果：data 用宽类型保留未知类型值（数字/布尔/嵌套等，只读展示不破坏类型）。 */
export interface ParsedFrontmatter {
  data: Record<string, unknown>;
  /** frontmatter 之后的正文（不含 `---` 块，编辑时原样拼回）。 */
  body: string;
  /** 文件开头 frontmatter 块原文（含闭合换行，格式原样保留）；无 frontmatter 时为空串。
   * 正文编辑合并回完整 content 时用 `fmPrefix + body`，避免 stringify 重排 frontmatter 格式。 */
  fmPrefix: string;
  /** YAML 格式错误时 false（面板显示小红条，不阻塞正文编辑）。 */
  ok: boolean;
}

/** 徽章类 key（内置标签属性）：渲染为 `#` 前缀徽章；其余数组渲染为 Clock 列表。
 *  名单唯一来源 = constants/notes.ts 的 BADGE_PROPERTY_KEYS（避免两处维护分歧）。 */
const BADGE_KEYS = new Set(BADGE_PROPERTY_KEYS);

export function isBadgeKey(key: string): boolean {
  return BADGE_KEYS.has(key);
}

// ===== 属性值类型（键名前图标 + 类型切换菜单共用）=====
//
// YAML 值即类型（文本即真相，无独立 schema）：类型由值推断，切换类型 = 把值转换为目标 YAML 类型落盘。

/** 属性值类型：文本 / 标签（徽章数组）/ 日期 / 数字 / 布尔 / 列表（Clock 数组）。 */
export type PropertyValueType = "text" | "tag" | "date" | "number" | "boolean" | "list";

/** 按键名 + 值推断当前类型：数组按徽章键区分标签/列表（非徽章键数组 = 列表）。 */
export function inferPropertyType(key: string, value: unknown): PropertyValueType {
  if (Array.isArray(value)) return isBadgeKey(key) ? "tag" : "list";
  if (value instanceof Date) return "date";
  if (typeof value === "number") return "number";
  if (typeof value === "boolean") return "boolean";
  return "text";
}

/** Date 显示/转文本用：YYYY-MM-DD。gray-matter/js-yaml 把 `2024-01-15` 解析为 UTC 零点 Date，
 *  用 UTC getter 取日历日（本地 getter 在西时区会偏移前一天）。 */
export function formatDate(d: Date): string {
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, "0");
  const day = String(d.getUTCDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** 把值转换为目标类型；不可转换返回 null（调用方保留原值，防破坏 YAML 数据）。 */
export function convertPropertyValue(
  value: unknown,
  type: PropertyValueType,
): unknown | null {
  switch (type) {
    case "text":
      if (value === null || value === undefined) return "";
      if (value instanceof Date) return formatDate(value);
      if (Array.isArray(value)) return value.map(String).join("，");
      return String(value);
    case "tag":
    case "list": {
      if (value === null || value === undefined) return [];
      const arr = Array.isArray(value) ? value : [value];
      return arr
        .map((v) => (v instanceof Date ? formatDate(v) : String(v)))
        .filter((s) => s.trim() !== "");
    }
    case "date": {
      if (value instanceof Date) return value;
      if (typeof value === "number") {
        const d = new Date(value);
        return Number.isNaN(d.getTime()) ? null : d;
      }
      if (typeof value === "string" && value.trim() !== "") {
        const d = new Date(value.trim());
        return Number.isNaN(d.getTime()) ? null : d;
      }
      return null;
    }
    case "number": {
      if (typeof value === "number") return value;
      if (typeof value === "boolean") return value ? 1 : 0;
      if (typeof value === "string" && /^-?\d+(\.\d+)?$/.test(value.trim())) {
        return parseFloat(value);
      }
      return null;
    }
    case "boolean": {
      if (typeof value === "boolean") return value;
      if (typeof value === "number") return value !== 0;
      if (typeof value === "string") {
        const s = value.trim().toLowerCase();
        if (s === "true" || s === "yes" || s === "1") return true;
        if (s === "false" || s === "no" || s === "0") return false;
      }
      return null;
    }
  }
}

/** gray-matter 的类型声明未覆盖 js-yaml dump 的透传参数，此处补充（官方文档说明 options 会传给 js-yaml）。 */
interface StringifyOptions extends matter.GrayMatterOption<string, never> {
  lineWidth?: number;
  noRefs?: boolean;
}

const YAML_OPTIONS: StringifyOptions = { lineWidth: 1000, noRefs: true };

export function parseFrontmatter(content: string): ParsedFrontmatter {
  let data: Record<string, unknown> = {};
  let body = content;
  let fmPrefix = "";
  let ok = true;
  try {
    const file = matter(content);
    data = file.data as Record<string, unknown>;
    // gray-matter 会剥掉 frontmatter 闭合后紧跟的一个换行（body 前导空行丢失，源码 index.js 行为），
    // 自己按文件开头 `---` 块定位：fmPrefix 保留块原文（格式/CRLF 原样），body 字节级保留正文，
    // 保证「仅编辑正文/属性不改变另一侧内容」；内容组可选以支持空 frontmatter（`---\n---`，模板插入形态）
    const fm = content.match(/^---[^\n]*\r?\n([\s\S]*?\r?\n)?---[^\n]*(?:\r?\n)?/);
    if (fm) {
      fmPrefix = fm[0];
      body = content.slice(fm[0].length);
    }
  } catch (e) {
    ok = false;
    // 诊断日志：仅记录错误信息（不含笔记内容，防隐私落日志）；格式错误已有 UI 红条提示
    console.error("[frontmatter] parse error:", (e as Error).message);
  }
  return { data, body, fmPrefix, ok };
}

export function stringifyFrontmatter(
  data: Record<string, unknown>,
  body: string
): string {
  let out = matter.stringify({ content: body }, data, YAML_OPTIONS);
  // gray-matter 在 body 结尾无换行时会补一个（lib/stringify.js newline()），
  // 还原为原 body 形态：仅编辑属性不应改变正文内容
  if (!body.endsWith("\n")) out = out.replace(/\n$/, "");
  // 外部工具产生的 CRLF 文件（Windows）：frontmatter 块跟随正文换行风格，避免文件内混用
  if (body.includes("\r\n")) {
    const fm = out.match(/^---[^\n]*\r?\n([\s\S]*?\r?\n)?---(?:\r?\n|$)/);
    if (fm) out = fm[0].replace(/(?<!\r)\n/g, "\r\n") + out.slice(fm[0].length);
  }
  return out;
}
