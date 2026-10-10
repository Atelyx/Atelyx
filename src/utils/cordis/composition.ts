/**
 * 组合层纯函数：默认组合层（随应用分发的行）+ 已装插件行 → 列表行、接管裁决与装配顺序。
 * 组合 = 「App 由哪些插件组成」：默认组合层定义随应用分发（定义见
 * components/plugins/cordis/builtins.tsx），用户安装的插件追加在后；启停 = 行 enabled（运行时唯一真相）。
 * 行 `impl` 决定装配时跑谁的代码（缺省 = 行自身实现）；「接管」= 把某行 impl 指到别处，行 id
 * 不变——它承担装配位置、插件自持数据归属、审计与启停真源。裁决两层后者赢：插件清单声明 → 用户层。
 */
import type { InstalledPlugin, PluginSourceKind } from "@/types";
import { COMPOSITION_IMPL_DEFAULT } from "@/constants/plugins";
import {
  type CompositionBinding,
  type CompositionDeclarer,
  type CompositionMount,
  type CompositionPatchDeclaration,
  type CompositionResolution,
  type CompositionUnmatchedDeclaration,
  type CompositionUserPatches,
} from "@/types";

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

/** 已展开归属的接管声明（调用方只收已装且已启用的插件，停用即声明失效）。 */
export interface ActiveCompositionDeclaration extends CompositionPatchDeclaration {
  pluginId: string;
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

/** 用户层 patch 表归一化：磁盘脏值（手改/旧格式）只保留非空字符串项。 */
export function sanitizeCompositionPatches(raw: unknown): CompositionUserPatches {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return {};
  const out: CompositionUserPatches = {};
  for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
    if (key && typeof value === "string" && value) out[key] = value;
  }
  return out;
}

/**
 * 入口图顺序：按 kind 所属组合行的位置排（行内保持入参顺序，`rowIndexOf` 返回同一值即同键）。
 * 入口图不依赖槽注册的到达时间——插件异步注册或某行被接管后，视图仍落在它所属行的位置上。
 */
export function orderViewKindsByRow(
  kinds: string[],
  rowIndexOf: (kind: string) => number,
): string[] {
  return kinds
    .map((kind, index) => ({ kind, index, row: rowIndexOf(kind) }))
    .sort((a, b) => a.row - b.row || a.index - b.index)
    .map((item) => item.kind);
}

/** 声明方排序：priority 降序，同 priority 按插件 id 升序（与槽位「同优先级后注册者胜」无关——
 *  声明是静态清单，无注册先后，故取确定性的 id 序）。 */
function compareDeclarers(a: CompositionDeclarer, b: CompositionDeclarer): number {
  if (a.priority !== b.priority) return b.priority - a.priority;
  return a.pluginId < b.pluginId ? -1 : a.pluginId > b.pluginId ? 1 : 0;
}

/**
 * 组合裁决：逐行定「谁装配」，并给出装配计划。
 *
 * - 用户层条目恒胜（即「钉住」）；`"default"` = 用户显式要求回本行默认实现，此时插件声明不参与。
 * - 无用户层条目时取声明方 priority 最高者。
 * - 声明的实现不可用（未安装 / 已停用 / 无入口）时回退本行默认实现，并带可读原因（不静默）。
 * - 被其他装配行引用为实现的插件行不独立装配（只作为别处的实现出现）；相互引用的行都保留
 *   自身装配，避免成环时两行一起消失。
 */
export function resolveComposition(opts: {
  rows: CompositionRow[];
  declarations: ActiveCompositionDeclaration[];
  userPatches: CompositionUserPatches;
  /** 实现是否可挂载：命中随应用编译的实现，或插件已装 + 已启用 + 有入口。 */
  implUsable(implId: string): boolean;
}): CompositionResolution {
  const { rows, declarations, userPatches, implUsable } = opts;
  const rowsById = new Map(rows.map((r) => [r.id, r]));

  const declarersByTarget = new Map<string, CompositionDeclarer[]>();
  const unmatched: CompositionUnmatchedDeclaration[] = [];
  for (const decl of declarations) {
    if (!rowsById.has(decl.target)) {
      unmatched.push({ pluginId: decl.pluginId, target: decl.target });
      continue;
    }
    const list = declarersByTarget.get(decl.target) ?? [];
    list.push({ pluginId: decl.pluginId, priority: decl.priority ?? 0 });
    declarersByTarget.set(decl.target, list);
  }
  for (const list of declarersByTarget.values()) list.sort(compareDeclarers);

  const bindings: Record<string, CompositionBinding> = {};
  for (const row of rows) {
    const declarers = declarersByTarget.get(row.id) ?? [];
    // hasOwn：行 id 是任意字符串，`userPatches[id]` 会命中原型链（如 "constructor"）
    const userImpl = Object.hasOwn(userPatches, row.id) ? userPatches[row.id] : null;
    let implId = row.id;
    let source: CompositionBinding["source"] = "default";
    if (userImpl !== null) {
      source = "user";
      implId = userImpl === COMPOSITION_IMPL_DEFAULT ? row.id : userImpl;
    } else if (declarers.length > 0) {
      source = "plugin";
      implId = declarers[0].pluginId;
    }
    let problem: string | undefined;
    if (implId !== row.id && !implUsable(implId)) {
      problem = `实现「${implId}」不可用（未安装、已停用或缺入口），已回退本行默认实现`;
      implId = row.id;
    }
    bindings[row.id] = {
      rowId: row.id,
      selfAvailable: implUsable(row.id),
      declarers,
      userImpl,
      implId,
      source,
      ...(problem ? { problem } : {}),
    };
  }

  const candidateIds = mountOrder(rows);
  const candidates = new Set(candidateIds);
  const referenced = new Set<string>();
  for (const id of candidateIds) {
    const implId = bindings[id].implId;
    if (implId !== id) referenced.add(implId);
  }
  const suppressed = new Set(
    // 成环参与者（既引用他人、又被引用）保留自身装配：否则 A↔B 互相引用会让两行都消失
    [...referenced].filter((id) => !(candidates.has(id) && bindings[id].implId !== id)),
  );
  const mounts: CompositionMount[] = candidateIds
    .filter((id) => !suppressed.has(id))
    .map((id) => ({ rowId: id, implId: bindings[id].implId }));

  return { mounts, bindings, unmatched };
}
