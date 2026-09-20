/**
 * 协作空间登录态 store（运行时内存态 + 编排 service）。
 *
 * 编排 `services/space/auth` 与 `services/global`：所有 I/O（keychain 令牌、global.json 清单、
 * 网络请求）都在 service 层；本 store 只持有内存态并提供动作。不 import 组件、不渲染。
 *
 * `servers` = 已验证有效的会话（来自 spaceServers 清单 + 启动 restore）；`spaceServers`
 * 清单本身持久化在 global.json（由 auth service 维护），本 store 不直接写 global.json。
 *
 * restore 与并发登录的竞态：restore 逐服务器网络校验耗时长，期间用户可能完成 login/register。
 * 进行中的 restore 重入复用同一次恢复；完成时结果与当前 state 按服务器地址合并——
 * state 已有的条目（并发登录的新会话）优先保留，restore 结果只补足缺失的地址，不做整体替换。
 */

import { create } from "zustand";
import {
  login as authLogin,
  register as authRegister,
  logout as authLogout,
  restoreSession,
  listDevices as authListDevices,
  revokeDevice as authRevokeDevice,
} from "@/services/space/auth";
import { readGlobalConfig } from "@/services/global";
import type { DeviceInfo } from "@/services/space/client";

export interface SpaceServerEntry {
  serverUrl: string;
  userId: string;
  username: string;
  displayName: string;
}

interface SpaceAuthState {
  /** 已验证有效的会话（内存态）。 */
  servers: SpaceServerEntry[];
  /** 启动 restore 是否已完成（无论成功失败），避免重复 restore。 */
  restored: boolean;
  /** 是否有登录/注册/恢复在进行中。 */
  busy: boolean;

  /** 启动时调用：遍历 spaceServers 逐个恢复会话，失效的从清单剔除并落盘。 */
  restore: () => Promise<void>;
  login: (serverUrl: string, username: string, password: string, deviceName?: string, displayName?: string) => Promise<SpaceServerEntry>;
  register: (serverUrl: string, username: string, password: string, deviceName?: string, displayName?: string) => Promise<SpaceServerEntry>;
  logout: (serverUrl: string) => Promise<void>;
  /** 某服务器的设备会话列表（设置区设备管理用）。 */
  listDevices: (serverUrl: string) => Promise<DeviceInfo[]>;
  /** 吊销某设备会话（踢下线）。 */
  revokeDevice: (serverUrl: string, sessionId: string) => Promise<void>;
  /** 取某服务器的已验证会话（未登录/失效返回 undefined）。 */
  getServer: (serverUrl: string) => SpaceServerEntry | undefined;
}

/** 进行中的 restore（重入复用：并发调用共享同一次恢复，旧快照不重复拉取/覆盖）。 */
let restoreInFlight: Promise<void> | null = null;

export const useSpaceAuthStore = create<SpaceAuthState>((set, get) => ({
  servers: [],
  restored: false,
  busy: false,

  restore: async () => {
    if (get().restored) return;
    if (restoreInFlight) return restoreInFlight;
    restoreInFlight = (async () => {
      set({ busy: true });
      const { config } = await readGlobalConfig();
      const known = config.spaceServers ?? [];
      const live: SpaceServerEntry[] = [];
      // 逐服务器恢复；失效（401/403）从清单剔除，网络/其他错误保留清单但本机不可用时不上线
      for (const serverUrl of known) {
        try {
          const user = await restoreSession(serverUrl);
          if (user) live.push(user);
          // user === null：会话失效，restoreSession 已清本地并从清单剔除
        } catch (err) {
          // 网络/超时/5xx：无法判定会话有效性，保留清单（下次启动重试），仅不上线
          console.warn(`[spaceAuth] 恢复会话失败（保留清单）：${serverUrl}`, err);
        }
      }
      // 按地址合并而非整体替换：恢复期间并发 login/register 写入 state 的新会话条目
      // 优先保留（restore 读到的可能是更旧的校验结果），restore 结果只补足 state 缺失的地址
      set((s) => {
        const byUrl = new Map(live.map((e) => [e.serverUrl, e]));
        for (const entry of s.servers) byUrl.set(entry.serverUrl, entry);
        return { servers: [...byUrl.values()], restored: true, busy: false };
      });
    })();
    try {
      await restoreInFlight;
    } finally {
      restoreInFlight = null;
    }
  },

  login: async (serverUrl, username, password, deviceName, displayName) => {
    set({ busy: true });
    try {
      const user = await authLogin(serverUrl, username, password, deviceName, displayName);
      set((s) => ({
        servers: [...s.servers.filter((e) => e.serverUrl !== serverUrl), user],
        busy: false,
      }));
      return user;
    } catch (err) {
      set({ busy: false });
      throw err;
    }
  },

  register: async (serverUrl, username, password, deviceName, displayName) => {
    set({ busy: true });
    try {
      const user = await authRegister(serverUrl, username, password, deviceName, displayName);
      set((s) => ({
        servers: [...s.servers.filter((e) => e.serverUrl !== serverUrl), user],
        busy: false,
      }));
      return user;
    } catch (err) {
      set({ busy: false });
      throw err;
    }
  },

  logout: async (serverUrl) => {
    await authLogout(serverUrl);
    set((s) => ({ servers: s.servers.filter((e) => e.serverUrl !== serverUrl) }));
  },

  listDevices: (serverUrl) => authListDevices(serverUrl),

  revokeDevice: (serverUrl, sessionId) => authRevokeDevice(serverUrl, sessionId),

  getServer: (serverUrl) => get().servers.find((e) => e.serverUrl === serverUrl),
}));
