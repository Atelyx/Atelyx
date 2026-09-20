/**
 * 协作空间传输工厂（collab-relay 的 `/ws/space`）：帧收发共用帧泵（framePump.ts），
 * 入口 hello 携带 spaceId（房间 = space:<spaceId>）与空间登录令牌（服务端据此
 * 鉴权成员资格，失败发 error 帧后断开，经 onServerError 可见）。
 * 令牌仅随 hello 帧发送，本模块不打任何日志（不落令牌）。
 */
import { registerCollabTransport, type CollabTransportFactory } from "./transport";
import { connectChannelPump } from "./framePump";

/**
 * 空间服务器地址（http/https）→ 空间实时频道 WebSocket 地址：
 * 协议替换（http→ws、https→wss）+ 去尾斜杠 + 拼 `/ws/space`。
 * 已是 ws/wss 的输入保持协议；空串返回空串；无法解析的输入原样返回（建连失败经 onStatusChange 可见）。
 */
export function spaceWsUrl(serverUrl: string): string {
  const input = serverUrl.trim();
  if (!input) return "";
  try {
    const u = new URL(input);
    const wsProto =
      u.protocol === "http:" || u.protocol === "ws:"
        ? "ws:"
        : u.protocol === "https:" || u.protocol === "wss:"
          ? "wss:"
          : null;
    if (!wsProto) return input;
    return `${wsProto}//${u.host}${u.pathname.replace(/\/+$/, "")}/ws/space`;
  } catch {
    return input;
  }
}

/** 空间传输工厂：hello 由调用方（collabStore 经 resolveCollabTarget）带 spaceId/token。 */
export const spaceCollabTransport: CollabTransportFactory = {
  name: "space",
  connect: connectChannelPump,
};

registerCollabTransport(spaceCollabTransport);
