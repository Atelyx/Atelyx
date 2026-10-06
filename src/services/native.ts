/**
 * 原始 Rust 命令调用 service（插件 `ctx.native.invoke` 面）：未封装成 ctx 服务的命令经此触达。
 */
import { invoke } from "@tauri-apps/api/core";

/** 调用任意已注册 Rust 命令（command 不存在由宿主侧拒绝；args 缺省空对象）。
 * 全量放行与完全信任模型自洽；调用形状经审计脱敏记录（命令名 + 参数个数，参数原文不进审计，见 services/cordis/audit）。 */
export function nativeInvoke(command: string, args?: Record<string, unknown>): Promise<unknown> {
  return invoke(command, args ?? {});
}
