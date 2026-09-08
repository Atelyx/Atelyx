/**
 * 插件平台 service：Rust `commands/plugin.rs` 的 invoke 封装 + 运行时（bridge）re-export。
 * 插件列表/安装/卸载/启停/更新/读入口/插件数据都经这里，前端组件只经 store 触达。
 */
import { invoke } from "@tauri-apps/api/core";
import type { PluginManifest, PluginScope, PluginSourceKind, PluginType } from "@/types";

/** Rust `plugin_list` 返回行（原始清单由前端校验归一化）。 */
export interface PluginRow {
  id: string;
  name: string;
  version: string;
  type: PluginType;
  scope: PluginScope;
  installDir: string;
  sourceKind: PluginSourceKind;
  enabled: boolean;
  manifest: PluginManifest;
}

/** 列出全部已装插件（app 级 + 当前仓库 vault 级）。 */
export function pluginList(): Promise<PluginRow[]> {
  return invoke<PluginRow[]>("plugin_list");
}

/** 安装插件：来源为 GitHub `owner/repo`（市场）或完整 git 地址；安装后默认未启用。 */
export function pluginInstall(repo: string, scope: PluginScope): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_install", { repo, scope });
}

/** 从本地目录安装插件（junction/符号链接实时引用，源目录改动即时生效）。 */
export function pluginInstallLocal(path: string, scope: PluginScope): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_install_local", { path, scope });
}

/** 卸载插件（删除插件目录 + 清理状态记录）。 */
export function pluginUninstall(id: string, scope: PluginScope): Promise<void> {
  return invoke("plugin_uninstall", { id, scope });
}

/** 启用/停用插件（前端先确认权限再启用）。 */
export function pluginSetEnabled(id: string, enabled: boolean): Promise<void> {
  return invoke("plugin_set_enabled", { id, enabled });
}

/** 恢复内置插件（补播种缺失的内置条目；管理 UI「恢复内置插件」入口，调用后重载插件列表）。 */
export function pluginSeedBuiltin(): Promise<void> {
  return invoke("plugin_seed_builtin");
}

/** 更新插件（备份 → 安装 → 失败回滚）。 */
export function pluginUpdate(id: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_update", { id });
}

/** 读取插件入口 JS（path 缺省 = 清单 main；主线程 UI 入口传 mainUi）。 */
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

export {
  attachPlugin,
  callPluginContributionFn,
  contributedCommands,
  contributedPluginTools,
  emitPluginEvent,
  hostCapabilityLabel,
  hostCapabilityNames,
  hostCapabilitySensitive,
  listPluginContributions,
  loadPlugin,
  onRuntimeChange,
  pluginCapabilitiesByOwner,
  pluginCapabilityOwner,
  registerHostCapability,
  registerHostCapabilityMeta,
  runContributedCommand,
  runtimeSnapshot,
  setAppPageOpener,
  unloadPlugin,
} from "./bridge";
export type {
  HostCapabilityHandler,
  HostCapabilityMeta,
  PluginCommandContribution,
  PluginContributionEntry,
  PluginRuntimeEntry,
  PluginStreamSink,
} from "./bridge";
export {
  exposePluginFacade,
  getPluginAppPages,
  getPluginCommands,
  getPluginNode,
  getPluginNodes,
  getPluginSetting,
  getPluginSettings,
  getPluginTableView,
  getPluginTableViews,
  getViewContribution,
  listUiContributions,
  loadUiPlugin,
  onPluginUiChange,
  pluginViewKinds,
  pluginViewLabel,
  registerBuiltinView,
  setBuiltinPluginIds,
  setPluginTableAccess,
  setPluginVaultAccess,
  unregisterPluginUi,
} from "./ui";
export type {
  PluginAppPageRegistration,
  PluginCommandRegistration,
  PluginMainThreadFacade,
  PluginNodeRegistration,
  PluginSettingRegistration,
  PluginTableAccess,
  PluginTableViewRegistration,
  PluginUiContribution,
  ViewContribution,
} from "./ui";
export { createPluginWorker, buildProxySource } from "./worker";
export { transpileTs } from "./transpile";
export { startPluginProcess } from "./process";
