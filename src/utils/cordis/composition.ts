/**
 * 组合层纯函数：默认组合层（随应用分发的行）+ 已装插件行 → 列表行与装配顺序。
 *
 * 组合 = 「App 由哪些插件组成」：默认组合层定义随应用分发（存在与顺序由宿主定义），
 * 用户安装的插件追加在后；启停 = 行 enabled（插件状态持久化，运行时唯一真相）。
 * 「谁替换谁」由插件自己在 apply 里经 ctx.slots 的 priority / inject 声明（作者侧决定），
 * 组合层不提供行级覆盖——行序只在「同 priority 槽平局」与「inject 依赖序」两个技术边角生效。
 * 纯函数无副作用，可直测；默认组合层定义见 components/plugins/cordis/builtins.tsx。
 */
import type { InstalledPlugin, PluginSourceKind } from "@/types";

/** 默认组合层行（随应用分发；数组顺序 = 装配顺序）。 */
export interface CompositionDefault {
  id: string;
  name: string;
  tagline?: string;
}

/** 已装插件行（来自插件列表）。 */
export interface CompositionPackage {
  id: string;
  name: string;
  version: string;
  tagline?: string;
  sourceKind: PluginSourceKind;
  enabled: boolean;
}

/** 列表行（管理页渲染单位：默认组合成员 ∪ 已装插件）。 */
export interface CompositionRow extends CompositionPackage {
  /** 是否已安装（false 仅默认组合成员可能出现：已卸载，恢复默认装配即装回）。 */
  installed: boolean;
}

/** 插件行 → 组合包行（组合层输入）。 */
export function compositionPackages(plugins: Record<string, InstalledPlugin>): CompositionPackage[] {
  return Object.values(plugins).map((p) => ({
    id: p.id,
    name: p.manifest.name,
    version: p.manifest.version,
    tagline: p.manifest.tagline,
    sourceKind: p.sourceKind,
    enabled: p.enabled,
  }));
}

/** 列表行推导：默认组合层按定义顺序在前，其余已装插件按 id 追加在后（已卸载的默认成员成灰行）。 */
export function composePlugins(
  defaults: CompositionDefault[],
  packages: CompositionPackage[],
): CompositionRow[] {
  const byId = new Map(packages.map((p) => [p.id, p]));
  const defaultsById = new Map(defaults.map((d) => [d.id, d]));
  const rows: CompositionRow[] = defaults.map((d) => {
    const pkg = byId.get(d.id);
    return {
      id: d.id,
      name: pkg?.name ?? d.name,
      version: pkg?.version ?? "",
      tagline: pkg?.tagline ?? d.tagline,
      sourceKind: pkg?.sourceKind ?? "builtin",
      enabled: pkg?.enabled ?? false,
      installed: pkg !== undefined,
    };
  });
  const appended = packages.filter((p) => !defaultsById.has(p.id)).sort((a, b) => (a.id < b.id ? -1 : 1));
  for (const pkg of appended) rows.push({ ...pkg, installed: true });
  return rows;
}

/** 装配顺序：已安装且启用的行 id（默认组合成员在前，其提供者先就绪）。 */
export function mountOrder(rows: CompositionRow[]): string[] {
  return rows.filter((row) => row.installed && row.enabled).map((row) => row.id);
}
