/**
 * 协作空间登录态 service。
 *
 * 令牌与用户身份经 OS keychain 的通用应用秘密存储（见 `services/keychain`），不落明文文件：
 * - 令牌条目名 `space-token-<sha256(serverUrl)>`（哈希避免 serverUrl 含特殊字符作 keychain 条目名）。
 * - 用户身份条目名 `space-user-<sha256(serverUrl)>`，值为 `{userId,username,displayName}` JSON。
 * 登录过的服务器清单经 `services/global` 的 `updateGlobalConfig`（字段 `spaceServers`）维护（增删去重）。
 *
 * 只编排 keychain 与 HTTP 客户端，不做状态（运行时状态在 `stores/spaceAuthStore`）。
 */

import { createSpaceClient, type SpaceApiError, type SpaceClient, type DeviceInfo } from "./client";
import { getAppSecret, setAppSecret, deleteAppSecret } from "@/services/keychain";
import { getHostname, readGlobalConfig, updateGlobalConfig } from "@/services/global";

/** 登录态的用户身份（含所属服务器，便于 store 直接持有）。 */
export interface SpaceUserInfo {
  serverUrl: string;
  userId: string;
  username: string;
  displayName: string;
}

/** 令牌内存缓存（keychain 为真源，缓存避免每次请求回读 keychain）。 */
const tokenCache = new Map<string, string>();

/** SHA-256 小写十六进制（WebCrypto，无第三方依赖）。 */
async function sha256Hex(input: string): Promise<string> {
  const data = new TextEncoder().encode(input);
  const digest = await crypto.subtle.digest("SHA-256", data);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function tokenEntryName(serverUrl: string): Promise<string> {
  return sha256Hex(serverUrl).then((h) => `space-token-${h}`);
}

function userEntryName(serverUrl: string): Promise<string> {
  return sha256Hex(serverUrl).then((h) => `space-user-${h}`);
}

function makeClient(serverUrl: string): SpaceClient {
  return createSpaceClient(serverUrl, async () => tokenCache.get(serverUrl) ?? "");
}

async function saveSession(serverUrl: string, token: string, user: Omit<SpaceUserInfo, "serverUrl">): Promise<void> {
  await setAppSecret(await tokenEntryName(serverUrl), token);
  await setAppSecret(await userEntryName(serverUrl), JSON.stringify(user));
}

async function clearSession(serverUrl: string): Promise<void> {
  await deleteAppSecret(await tokenEntryName(serverUrl));
  await deleteAppSecret(await userEntryName(serverUrl));
}

/** 探测服务器可达：能拿到任何 HTTP 响应（含 401/5xx）即视为可达；仅网络/超时（status 0）抛错。 */
async function assertReachable(serverUrl: string): Promise<void> {
  try {
    await makeClient(serverUrl).spaces.list();
  } catch (err) {
    // 401/403/5xx 说明服务器已响应；只有未达服务端（网络/超时）才视为不可达
    if (err instanceof Error && (err as SpaceApiError).name === "SpaceApiError") {
      const e = err as SpaceApiError;
      if (e.status === 0) throw e;
    }
  }
}

async function addServerToList(serverUrl: string): Promise<void> {
  const { config } = await readGlobalConfig();
  const servers = config.spaceServers ?? [];
  if (!servers.includes(serverUrl)) {
    await updateGlobalConfig({ spaceServers: [...servers, serverUrl] });
  }
}

async function removeServerFromList(serverUrl: string): Promise<void> {
  const { config } = await readGlobalConfig();
  const servers = config.spaceServers ?? [];
  if (servers.includes(serverUrl)) {
    await updateGlobalConfig({ spaceServers: servers.filter((s) => s !== serverUrl) });
  }
}

/**
 * 登录或注册的共享实现：先探测可达（不可达直接抛网络错误，不发送凭据），再调认证端点；
 * 成功后令牌与用户身份入库 keychain，并把服务器加入 spaceServers 清单。
 */
async function authenticate(
  serverUrl: string,
  username: string,
  password: string,
  deviceName: string | undefined,
  displayName: string | undefined,
  endpoint: "login" | "register",
): Promise<SpaceUserInfo> {
  await assertReachable(serverUrl);
  const device = deviceName?.trim() || (await getHostname());
  const body = { username, password, deviceName: device, displayName: displayName || undefined };
  const resp =
    endpoint === "login"
      ? await makeClient(serverUrl).auth.login(body)
      : await makeClient(serverUrl).auth.register(body);
  tokenCache.set(serverUrl, resp.token);
  const user = { userId: resp.userId, username: resp.username, displayName: resp.displayName };
  await saveSession(serverUrl, resp.token, user);
  await addServerToList(serverUrl);
  return { serverUrl, ...user };
}

/** 登录某协作服务器。 */
export function login(
  serverUrl: string,
  username: string,
  password: string,
  deviceName?: string,
  displayName?: string,
): Promise<SpaceUserInfo> {
  return authenticate(serverUrl, username, password, deviceName, displayName, "login");
}

/** 注册并登录（形状与 login 一致）。 */
export function register(
  serverUrl: string,
  username: string,
  password: string,
  deviceName?: string,
  displayName?: string,
): Promise<SpaceUserInfo> {
  return authenticate(serverUrl, username, password, deviceName, displayName, "register");
}

/**
 * 登出某协作服务器：调服务端 logout（401 视作已登出），本地令牌与用户身份清掉，
 * 并从 spaceServers 清单移除。服务端注销失败时本地仍清令牌（避免残留失效会话），
 * 非 401 错误重新抛出由调用方可见。
 */
export async function logout(serverUrl: string): Promise<void> {
  try {
    await makeClient(serverUrl).auth.logout();
  } catch (err) {
    if (err instanceof Error && (err as SpaceApiError).name === "SpaceApiError") {
      const e = err as SpaceApiError;
      if (e.status === 401) {
        // 会话已失效，等同登出成功
      } else {
        throw e;
      }
    } else {
      throw err;
    }
  } finally {
    tokenCache.delete(serverUrl);
    await clearSession(serverUrl).catch(() => undefined);
    await removeServerFromList(serverUrl).catch(() => undefined);
  }
}

/**
 * 恢复会话：读本地令牌 → 调轻量鉴权端点（listDevices）验证会话有效。
 * 有效返回用户身份；会话失效（401/403）清本地并返回 null；网络/其他错误抛出（无法判定）。
 */
export async function restoreSession(serverUrl: string): Promise<SpaceUserInfo | null> {
  const tokenName = await tokenEntryName(serverUrl);
  const token = await getAppSecret(tokenName);
  if (!token) return null;
  const userRaw = await getAppSecret(await userEntryName(serverUrl));
  if (!userRaw) {
    // 孤儿令牌：令牌在但用户身份条目缺失（半写入/被手工清理），会话永远无法恢复——
    // 对齐 401 分支清理本地残留（令牌 + 身份 + 服务器清单），不留每次启动空恢复的孤儿
    await clearSession(serverUrl).catch(() => undefined);
    await removeServerFromList(serverUrl).catch(() => undefined);
    return null;
  }
  let user: Omit<SpaceUserInfo, "serverUrl">;
  try {
    user = JSON.parse(userRaw) as Omit<SpaceUserInfo, "serverUrl">;
  } catch {
    return null;
  }
  tokenCache.set(serverUrl, token);
  try {
    await makeClient(serverUrl).auth.listDevices();
    return { serverUrl, ...user };
  } catch (err) {
    if (err instanceof Error && (err as SpaceApiError).name === "SpaceApiError") {
      const e = err as SpaceApiError;
      if (e.status === 401 || e.status === 403) {
        tokenCache.delete(serverUrl);
        await clearSession(serverUrl).catch(() => undefined);
        // 会话失效：从 spaceServers 清单剔除并落盘（与 login/logout 一致，由本 service 维护清单）
        await removeServerFromList(serverUrl).catch(() => undefined);
        return null;
      }
    }
    throw err;
  }
}
export async function listDevices(serverUrl: string): Promise<DeviceInfo[]> {
  return makeClient(serverUrl).auth.listDevices();
}

/** 透传：吊销某设备会话。 */
export async function revokeDevice(serverUrl: string, sessionId: string): Promise<void> {
  return makeClient(serverUrl).auth.revokeDevice(sessionId);
}

/**
 * 取当前令牌（供空间内容后端随请求附加 Authorization）。
 * 会话经 keychain 缓存于内存，未登录返回空串（客户端据此不携带令牌）。
 */
export async function getToken(serverUrl: string): Promise<string> {
  return tokenCache.get(serverUrl) ?? "";
}
