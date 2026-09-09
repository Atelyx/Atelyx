/**
 * 插件包清单校验与归一化。
 * 原始输入 = 插件根目录的 `package.json`（npm 标准字段 + 嵌套 `atelyx` 块）；归一化为
 * `PluginManifest`（展平）。校验目标是「坏清单不让 App 内部功能出问题」，而不是拒绝一切：
 * 未知字段、未知附加分类一律跳过（前向兼容），只对结构性问题（缺字段、类型错误、未知主分类）报错。
 */
import {
  type PluginManifest,
  type PluginScope,
  type PluginType,
  type ThemeDefinition,
} from "@/types";

export type ManifestValidateResult =
  | { ok: true; manifest: PluginManifest }
  | { ok: false; errors: string[] };

/** 已知插件类型（判定的唯一依据；新增类型在 types/plugin.ts 的联合里加）。 */
const KNOWN_PLUGIN_TYPES: readonly string[] = [
  "tool",
  "setting",
  "panel",
  "app",
  "node",
  "theme",
  "command",
  "background",
  "tableview",
];

const PLUGIN_ID_PATTERN = /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$/;

/** id 是否合法：反向域名式（至少两段、小写字母/数字/中划线、段不以中划线收尾）。 */
export function pluginIdValid(id: string): boolean {
  return id.length <= 128 && PLUGIN_ID_PATTERN.test(id);
}

/** 是否为已知插件类型（未知主分类拒绝；未知附加分类在旧 App 上安全跳过）。 */
export function isKnownPluginType(type: string): boolean {
  return KNOWN_PLUGIN_TYPES.includes(type);
}

/** 全部分类（含主分类，去重；缺省 = [type]）。 */
export function pluginTypeList(manifest: Pick<PluginManifest, "type" | "types">): PluginType[] {
  return [...new Set([manifest.type, ...(manifest.types ?? [])])];
}

/** 语义化版本比较（容忍 1/2/3 段与缺段，非数字段按 0 处理）。 */
export function compareVersions(a: string, b: string): number {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? 0;
    const y = pb[i] ?? 0;
    if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}

function parseVersion(value: string): number[] {
  return value.split(".").map((seg) => {
    const n = Number.parseInt(seg, 10);
    return Number.isNaN(n) ? 0 : n;
  });
}

/** 插件是否兼容当前宿主（版本范围 + 平台）。 */
export function pluginCompatibleWithHost(
  manifest: Pick<PluginManifest, "atelyxVersionMin" | "atelyxVersionMax" | "platforms">,
  hostVersion: string,
  platform: string,
): { ok: true } | { ok: false; reason: string } {
  if (manifest.atelyxVersionMin && compareVersions(hostVersion, manifest.atelyxVersionMin) < 0) {
    return { ok: false, reason: `需要 Atelyx ${manifest.atelyxVersionMin} 及以上版本` };
  }
  if (manifest.atelyxVersionMax && compareVersions(hostVersion, manifest.atelyxVersionMax) >= 0) {
    return { ok: false, reason: `仅支持 Atelyx ${manifest.atelyxVersionMax} 以下版本` };
  }
  if (manifest.platforms && manifest.platforms.length > 0 && !manifest.platforms.includes(platform)) {
    return { ok: false, reason: `不支持当前平台（${platform}）` };
  }
  return { ok: true };
}

/** 校验并归一化插件包清单（package.json）；结构错误返回原因列表。 */
export function validatePluginManifest(raw: unknown): ManifestValidateResult {
  if (typeof raw !== "object" || raw === null) return { ok: false, errors: ["清单必须是对象"] };
  const data = raw as Record<string, unknown>;

  const errors: string[] = [];

  const id = data.name;
  if (typeof id !== "string" || !pluginIdValid(id)) errors.push("name 必须是合法的反向域名标识");

  const version = data.version;
  if (typeof version !== "string" || version.trim().length === 0) errors.push("version 不能为空");

  const main = data.main;
  if (main !== undefined && (typeof main !== "string" || main.trim().length === 0)) {
    errors.push("main 必须是非空字符串");
  }

  // atelyx 块：插件元数据（显示名/类型/作用域/披露/主题等）。
  const atelyx = data.atelyx;
  if (typeof atelyx !== "object" || atelyx === null || Array.isArray(atelyx)) {
    errors.push("缺少 atelyx 块");
    return { ok: false, errors };
  }
  const ax = atelyx as Record<string, unknown>;

  const type = ax.type;
  if (typeof type !== "string" || !isKnownPluginType(type)) errors.push(`未知插件类型：${String(type)}`);

  if (errors.length > 0) return { ok: false, errors };

  const types = normalizeTypes(type as string, ax.types, errors);
  // main 仅在纯 theme 插件（无任何代码承载类型）时可省略——theme 是声明式皮肤，无入口。
  const themeOnly = types.every((t) => t === "theme");
  if (!themeOnly && main === undefined) errors.push("main 不能为空");

  const declares = normalizeStringList(ax.declares, "declares", errors);
  const permissions = normalizePermissions(ax.permissions, errors);
  const platforms = normalizeStringList(ax.platforms, "platforms", errors);
  const themes = normalizeThemes(ax.themes, errors);
  const themeOptions = normalizeThemeOptions(ax.themeOptions, errors);
  if (errors.length > 0) return { ok: false, errors };

  const manifest: PluginManifest = {
    id: id as string,
    name: normalizeName(ax.name, id as string),
    version: version as string,
    type: type as PluginType,
    ...(typeof main === "string" && main.trim().length > 0 ? { main } : {}),
    scope: normalizeScope(ax.scope),
    ...(types.length > 0 ? { types } : {}),
    ...(declares.length > 0 ? { declares } : {}),
    ...(Object.keys(permissions).length > 0 ? { permissions } : {}),
    ...(platforms.length > 0 ? { platforms } : {}),
    ...(themes ? { themes } : {}),
    ...(themeOptions ? { themeOptions } : {}),
    ...(typeof ax.atelyxVersionMin === "string" ? { atelyxVersionMin: ax.atelyxVersionMin } : {}),
    ...(typeof ax.atelyxVersionMax === "string" ? { atelyxVersionMax: ax.atelyxVersionMax } : {}),
    ...(typeof ax.hostApiVersion === "number" ? { hostApiVersion: ax.hostApiVersion } : {}),
    ...(typeof ax.tagline === "string" ? { tagline: ax.tagline } : {}),
    ...(typeof ax.description === "string"
      ? { description: ax.description }
      : typeof data.description === "string"
        ? { description: data.description }
        : {}),
    ...(typeof ax.author === "string"
      ? { author: ax.author }
      : typeof data.author === "string"
        ? { author: data.author }
        : {}),
    ...(typeof ax.license === "string"
      ? { license: ax.license }
      : typeof data.license === "string"
        ? { license: data.license }
        : {}),
    ...(Array.isArray(ax.tags) && ax.tags.every((t) => typeof t === "string")
      ? { tags: ax.tags as string[] }
      : Array.isArray(data.keywords) && data.keywords.every((t) => typeof t === "string")
        ? { tags: data.keywords as string[] }
        : {}),
  };
  return { ok: true, manifest };
}

/** 显示名归一化：atelyx.name 缺省 = id。 */
function normalizeName(raw: unknown, fallback: string): string {
  return typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : fallback;
}

/** 全部分类归一化：附加分类只保留已知类型，未知的跳过（前向兼容）。 */
function normalizeTypes(type: string, rawTypes: unknown, errors: string[]): PluginType[] {
  if (rawTypes === undefined) return [type as PluginType];
  if (!Array.isArray(rawTypes)) {
    errors.push("types 必须是数组");
    return [];
  }
  const known: PluginType[] = [];
  for (const item of rawTypes) {
    if (typeof item === "string" && isKnownPluginType(item)) known.push(item as PluginType);
  }
  return [...new Set([type as PluginType, ...known])];
}

/** permissions 归一化：必须是服务名 → 非空字符串的表。 */
function normalizePermissions(raw: unknown, errors: string[]): Record<string, string> {
  if (raw === undefined) return {};
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    errors.push("permissions 必须是对象");
    return {};
  }
  const result: Record<string, string> = {};
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value === "string" && value.trim().length > 0) result[key] = value;
  }
  return result;
}

function normalizeStringList(raw: unknown, field: string, errors: string[]): string[] {
  if (raw === undefined) return [];
  if (!Array.isArray(raw) || !raw.every((item) => typeof item === "string" && item.length > 0)) {
    errors.push(`${field} 必须是非空字符串数组`);
    return [];
  }
  return raw as string[];
}

/** themes 归一化：非空数组（每项 id/name/colorScheme/variables），id 插件内唯一；未知字段跳过。 */
function normalizeThemes(raw: unknown, errors: string[]): ThemeDefinition[] | undefined {
  if (raw === undefined) return undefined;
  if (!Array.isArray(raw)) {
    errors.push("themes 必须是数组");
    return undefined;
  }
  const items: ThemeDefinition[] = [];
  const seen = new Set<string>();
  for (const rawItem of raw) {
    if (typeof rawItem !== "object" || rawItem === null) {
      errors.push("themes 项必须是对象");
      continue;
    }
    const item = rawItem as Record<string, unknown>;
    const id = item.id;
    const name = item.name;
    const colorScheme = item.colorScheme;
    if (typeof id !== "string" || id.trim().length === 0) {
      errors.push("themes 项 id 必须是非空字符串");
      continue;
    }
    if (typeof name !== "string" || name.trim().length === 0) {
      errors.push(`themes[${id}].name 必须是非空字符串`);
      continue;
    }
    if (colorScheme !== "light" && colorScheme !== "dark") {
      errors.push(`themes[${id}].colorScheme 仅支持 light/dark`);
      continue;
    }
    const variables = normalizeVarTable(item.variables);
    if (variables === undefined) {
      errors.push(`themes[${id}].variables 必须是字符串值对象`);
      continue;
    }
    if (seen.has(id)) {
      errors.push(`themes 内 id 重复：${id}`);
      continue;
    }
    seen.add(id);
    items.push({ id, name, colorScheme, variables });
  }
  if (items.length === 0) {
    errors.push("themes 至少需要一个主题条目");
    return undefined;
  }
  return items;
}

/** themeOptions 归一化：仅接受 { accent?: boolean }；未知键跳过。 */
function normalizeThemeOptions(raw: unknown, errors: string[]): { accent?: boolean } | undefined {
  if (raw === undefined) return undefined;
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    errors.push("themeOptions 必须是对象");
    return undefined;
  }
  const obj = raw as Record<string, unknown>;
  if (obj.accent !== undefined && typeof obj.accent !== "boolean") {
    errors.push("themeOptions.accent 必须是布尔值");
    return undefined;
  }
  return obj.accent === true ? { accent: true } : undefined;
}

function normalizeVarTable(raw: unknown): Record<string, string> | undefined {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return undefined;
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof value === "string") out[key] = value;
  }
  return out;
}

function normalizeScope(rawScope: unknown): PluginScope {
  return rawScope === "vault" ? "vault" : "app";
}
