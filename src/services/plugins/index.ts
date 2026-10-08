/**
 * 插件平台 service：Rust `commands/plugin.rs` 的 invoke 封装（列表/安装/卸载/启停/更新/读入口/插件数据/默认组合播种）。
 * 前端组件只经 store 触达；插件运行时（Cordis 内核/挂载器）在 `services/cordis`，不在此层。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { PluginLayoutSpecNode, PluginPackageJson, PluginSourceKind, PluginType } from "@/types";

/** Rust `plugin_list` 返回行（原始清单由前端校验归一化）。 */
export interface PluginRow {
  id: string;
  name: string;
  version: string;
  type: PluginType;
  /** 安装目录（空 = 实现随应用编译，无磁盘目录）。 */
  installDir: string;
  /** 宿主产出的打包入口（相对安装目录）：有产物即用它，无产物用清单 main。 */
  entry?: string;
  sourceKind: PluginSourceKind;
  enabled: boolean;
  manifest: PluginPackageJson;
  /** 可回退到的上一版本（成功回退后为空）。 */
  previousVersion?: string;
  /** 同 id 行冲突说明（Rust 标记；双方行都携带且强制停用）。 */
  conflict?: string;
  /** 非致命警示（当前仅重装恢复保留数据未完成时给出；数据留在保留区）。 */
  warning?: string;
}

/** `plugin_list` 响应：行清单 + 插件状态健康度。 */
export interface PluginListResult {
  rows: PluginRow[];
  /** 插件状态文件不可读/损坏时的诊断；此时所有行以停用态返回（fail-closed，不落盘）。
   *  修复或删除该文件后下一次列表恢复正常。 */
  stateError?: string;
  /** 仍保留随仓库安装插件目录（`<root>/.atelyx/plugins` 非空）的仓库根：随仓库安装已不再支持，
   *  其中的插件不会加载，由调用方汇总提示；空/缺省 = 无。 */
  legacyVaultPluginRoots?: string[];
  /** 应用装配版本（Rust 进程内单调计数器快照）：行集合与装配输入的版本锚，
   *  与 global.json 读写的 `assemblyVersion` 同源；跨窗口据此比对装配快照新旧。
   *  Rust 侧恒返回；缺省（测试替身未填）视为 0 = 从未变更。 */
  assemblyVersion?: number;
}

/** 列出全部插件行（先按默认组合清单增量播种随应用分发的行，再列出磁盘包行）。 */
export function pluginList(defaults: PluginPackageJson[]): Promise<PluginListResult> {
  return invoke<PluginListResult>("plugin_list", { defaults });
}

/** 安装插件：来源为 GitHub `owner/repo`（市场）或完整 git 地址。启用口径：全新 id 落盘停用、
 *  由用户显式启用；同 id 替换视为实现更新，经安装确认后继承原行启停状态。 */
export function pluginInstall(repo: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_install", { repo });
}

/** 从本地目录安装插件（junction/符号链接实时引用，源目录改动即时生效；启用口径同 pluginInstall）。 */
export function pluginInstallLocal(path: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_install_local", { path });
}

/** 卸载插件（删安装目录/链接 + 清理状态记录；无落位目录的行只清记录）。
 *  keepData 为真时把插件目录的 data/（ctx.state / ctx.storage / ctx.fs.privateDir 落盘）
 *  搬到保留区，重装同 id 自动搬回；本地链接来源忽略（源目录数据不受卸载影响）。 */
export function pluginUninstall(id: string, keepData?: boolean): Promise<void> {
  return invoke("plugin_uninstall", { id, keepData: keepData ?? false });
}

/** 应用插件声明的默认布局（每插件一次性：Rust 侧判定标记与既有布局后追加，不激活）。 */
export function pluginApplyDefaultLayout(pluginId: string, name: string, tree: PluginLayoutSpecNode): Promise<void> {
  return invoke("plugin_apply_default_layout", { id: pluginId, name, tree });
}

/** 启用/停用插件（前端先确认权限再启用）。 */
export function pluginSetEnabled(id: string, enabled: boolean): Promise<void> {
  return invoke("plugin_set_enabled", { id, enabled });
}

/** 恢复默认装配（补播种缺失的默认组合行；管理 UI「恢复默认装配」入口，调用后重载插件列表）。 */
export function pluginSeedDefault(entries: PluginPackageJson[]): Promise<void> {
  return invoke("plugin_seed_default", { entries });
}

/** 更新插件（备份 → 安装 → 失败回滚）。 */
export function pluginUpdate(id: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_update", { id });
}

/** 回退到上一版本；成功后清空回退指针并保留插件数据。 */
export function pluginRollback(id: string, expectedPreviousVersion: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_rollback", { id, expectedPreviousVersion });
}

/** 重载本地插件：对源目录重跑依赖取件与打包（产物从干净目录重建），成功后广播 plugin-changed。 */
export function pluginRebuildLocal(id: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_rebuild_local", { id });
}

/** 读取当前应用装配版本（Rust 进程内单调计数器）：追平流程用它核对拉取前后输入是否仍新鲜。 */
export function getAssemblyVersion(): Promise<number> {
  return invoke<number>("plugin_assembly_version");
}

/** 订阅其他窗口完成的插件行变更（启停/安装/卸载/版本操作）：载荷带装配版本，
 *  各窗口据此比对自身装配快照新旧，落后者拉输入重算裁决后定向重挂。 */
export function onPluginChanged(handler: (payload: { version: number }) => void): Promise<UnlistenFn> {
  return listen<{ id: string; version: number }>("plugin-changed", (event) => handler(event.payload));
}

/** 读取插件入口源码（path 缺省 = 清单 main）。 */
export function pluginReadEntry(id: string, path?: string): Promise<string> {
  return invoke<string>("plugin_read_entry", { id, path });
}

/** 读取插件自持数据（单 JSON 对象）。 */
export function pluginReadState(id: string): Promise<unknown> {
  return invoke<unknown>("plugin_read_state", { id });
}

/** 写入插件自持数据（原子写）。 */
export function pluginWriteState(id: string, data: unknown): Promise<void> {
  return invoke("plugin_write_state", { id, data });
}

/** 读取插件键值存储（单 JSON 对象，独立于 state.json）。 */
export function pluginKvRead(id: string): Promise<Record<string, unknown>> {
  return invoke<Record<string, unknown>>("plugin_kv_read", { id });
}

/** 写插件键值存储的一个键（Rust 侧读改写串行，调用方无需整表往返）。 */
export function pluginKvSet(id: string, key: string, value: unknown): Promise<void> {
  return invoke("plugin_kv_set", { id, key, value });
}

/** 删插件键值存储的一个键（不存在 = no-op）。 */
export function pluginKvDelete(id: string, key: string): Promise<void> {
  return invoke("plugin_kv_delete", { id, key });
}

/** 整表覆盖插件键值存储（ctx.storage.clear 用）。 */
export function pluginKvWrite(id: string, data: Record<string, unknown>): Promise<void> {
  return invoke("plugin_kv_write", { id, data });
}
