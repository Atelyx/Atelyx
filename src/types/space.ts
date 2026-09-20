/**
 * 协作空间服务端 JSON 镜像类型（与 `services/space/client` 的契约一一对应）。
 * 类型落位 types/ 供组件层引用（组件不得 import services 的分层守卫）。
 */

/** 服务端设备会话（auth/devices 端点）。 */
export interface DeviceInfo {
  id: string;
  deviceName: string;
  createdAt: number;
  lastSeenAt: number;
  current: boolean;
}

/** 服务端邀请码（spaces/invites 端点）。 */
export interface InviteInfo {
  code: string;
  role: string;
  expiresAt?: number | null;
  maxUses?: number | null;
}
