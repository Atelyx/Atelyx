/**
 * API key 存储 service（按仓库隔离）。
 *
 * 对应 Rust `commands/keychain.rs`，通过 OS keychain 存取 AI provider 的 API key。
 * 安全边界：key 默认仅存 keychain，不落仓库文件或 global.json；
 * 仅当仓库开启 `syncKeys`（「API key 随仓库保存」，多设备同步）时，key 由 settingsStore
 * 明文写入 `.atelyx/config.json` 随仓库同步，本 service 不再参与。
 * keychain 条目 = `provider-<sha256(root)>-<providerId>`（仓库身份 = root 绝对路径，
 * Rust 侧取哈希作条目名，复制的仓库与原件互不共条目）。
 *
 * 边界捕获：keychain 不可用（如 Linux 无 secret service）时抛错，前端 toast 提示，
 * 不降级为明文文件。provider 的 key 留空，AI 调用会失败但应用不崩溃。
 */
import { invoke } from "@tauri-apps/api/core";

/** 保存仓库内 provider 的 API key 到 keychain（空串覆盖旧值）。 */
export async function setApiKey(vaultRoot: string, providerId: string, key: string): Promise<void> {
  await invoke("set_api_key", { vaultRoot, providerId, key });
}

/**
 * 读取仓库内 provider 的 API key。
 * 未设置 key 返回空串（keychain 无条目），keychain 故障时 reject。
 */
export async function getApiKey(vaultRoot: string, providerId: string): Promise<string> {
  return invoke<string>("get_api_key", { vaultRoot, providerId });
}

/** 删除仓库内 provider 的 keychain 条目（幂等，条目不存在不报错）。 */
export async function deleteApiKey(vaultRoot: string, providerId: string): Promise<void> {
  await invoke("delete_api_key", { vaultRoot, providerId });
}
