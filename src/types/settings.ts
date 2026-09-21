/**
 * 仓库设置的目标身份：仓库级配置（`VaultConfig` / Agent / 提示词标记）的读写对象。
 *
 * 与激活态解耦——设置弹窗可由文件面板的仓库行/空间行打开，被编辑的仓库不一定是当前激活仓库；
 * 激活仓库同样用本类型表达（由 appStore 的 `vaultRoot` / `vaultIdentity` 派生），
 * 于是「目标是激活仓库」退化成一次身份键比较。
 *
 * - `name` = 展示名（弹窗标题与提示里用，取最近列表里的名字，不参与身份判定）；
 * - `role` = 协作空间内本账号角色（`""` = 未知，写权限最终由服务端裁决）。
 */
export type VaultSettingsTarget =
  | { kind: "local"; root: string; name: string }
  | { kind: "space"; serverUrl: string; spaceId: string; name: string; role: string };
