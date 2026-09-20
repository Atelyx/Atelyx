/**
 * 协作帧泵（WebSocket 连接内核）：WebSocket 生命周期 + 帧编解码 +
 * 心跳保活 + 半开检测 + 指数退避重连。泵不感知 hello 内容，由传输工厂组装后传入。
 *
 * 协议（JSON，camelCase，见 `collab-relay/src/ws.rs`）：
 * - C→S `hello`（首条必发）/ `presence` / `table-patch` / `canvas-patch` /
 *   `note-sync` / `note-aware` / `plugin-msg` / `ping` / `bye`
 * - S→C `peers`（成员全量）/ `hello-ack`（分配 peerId）/ `presence` / 各频道转发帧 /
 *   `resync`（接收队列被裁剪）/ `pong` / `error`
 */
import type { CanvasPatch, CollabHello, CollabPeer, CollabPresence, TablePatch } from "@/types";
import type { CollabTransportHandle, CollabTransportOptions } from "./transport";

/** 心跳间隔：服务端 30s 无消息超时踢出，25s 发 ping 保活。 */
const HEARTBEAT_MS = 25_000;
/** 脱线阈值：连续 3 个心跳周期（75s）无任何服务端帧，判为半开假死连接，强制断开重连。 */
const STALL_TIMEOUT_MS = HEARTBEAT_MS * 3;
/** 断线重连退避：1s 起，翻倍，封顶 15s。 */
const MAX_RETRY_MS = 15_000;

type CollabServerMessage =
  | { type: "peers"; peers: CollabPeer[] }
  | { type: "hello-ack"; peerId: number }
  | { type: "presence"; peerId: number; presence: CollabPresence }
  | { type: "table-patch"; peerId: number; file: string; patch: TablePatch }
  | { type: "canvas-patch"; peerId: number; file: string; patch: CanvasPatch }
  | { type: "note-sync"; peerId: number; file: string; payload: string }
  | { type: "note-aware"; peerId: number; file: string; payload: string }
  | { type: "plugin-msg"; peerId: number; channel: string; payload: unknown }
  | { type: "resync" }
  | { type: "pong" }
  | { type: "error"; message: string };

/** 帧泵的入站回调面（文件匹配与解码归调用方）。 */
interface CollabFramePumpCallbacks {
  /** 收到 hello-ack（分配的本连接 peerId）——客户端据此把自己过滤出 peers 列表。 */
  onHelloAck: (peerId: number) => void;
  onPeers: (peers: CollabPeer[]) => void;
  onPeerPresence: (peerId: number, presence: CollabPresence) => void;
  onTablePatch: (peerId: number, file: string, patch: TablePatch) => void;
  onCanvasPatch: (peerId: number, file: string, patch: CanvasPatch) => void;
  onNoteSync: (peerId: number, file: string, payload: string) => void;
  onNoteAware: (peerId: number, file: string, payload: string) => void;
  onPluginMsg: (peerId: number, channel: string, payload: unknown) => void;
  /** 接收队列被广播裁剪（消费过慢）：调用方需重新握手补齐。 */
  onResync: () => void;
  /** 收到服务端 error 帧（协议异常/鉴权拒绝等）——调用方决定日志或 UI 反馈。 */
  onServerError: (message: string) => void;
  onStatusChange: (connected: boolean) => void;
}

interface CollabFramePumpOptions extends CollabFramePumpCallbacks {
  url: string;
  /** 首帧 hello 负载（type 由泵补齐）：space 带 spaceId 与登录令牌。 */
  hello: CollabHello;
  /** 重连前刷新 hello（身份/令牌/配置变化后自动生效）；返回 null = 放弃重连并正常收尾。
   *  可选：不提供 = 重连沿用构造时的 hello。仅重连前调用，首连不用。 */
  refreshHello?: () => Promise<CollabHello | null>;
}

interface CollabFramePumpHandle {
  /** 上报本端 presence（调用方自行节流）。 */
  sendPresence(presence: CollabPresence): void;
  /** 广播表格增量补丁。返回是否已投递（未连接 = false）。 */
  sendTablePatch(file: string, patch: TablePatch): boolean;
  /** 广播画布增量补丁。返回是否已投递（未连接 = false）。 */
  sendCanvasPatch(file: string, patch: CanvasPatch): boolean;
  /** 广播笔记 Yjs 同步消息（base64，不透明转发）。返回是否已投递（未连接 = false）。 */
  sendNoteSync(file: string, payload: string): boolean;
  /** 广播笔记 awareness 更新（base64，不透明转发）。返回是否已投递（未连接 = false）。 */
  sendNoteAware(file: string, payload: string): boolean;
  /** 广播插件消息（payload 任意 JSON 不透明转发；targetPeerId 指定 = 定向单播）。
   *  返回是否已投递（未连接 = false）。 */
  sendPluginMsg(channel: string, payload: unknown, targetPeerId?: number): boolean;
  /** 主动离开房间（切仓库/关闭应用）。 */
  sendBye(): void;
  /** 断开连接且不再重连。 */
  disconnect(): void;
}

/** 建立连接。onStatusChange 初始调用一次 false（连接中），成功后 true，断线重连期间 false。 */
function connectFramePump(opts: CollabFramePumpOptions): CollabFramePumpHandle {
  let ws: WebSocket | null = null;
  let closed = false; // disconnect() 后不再重连
  let alive = false; // 曾连上（避免未连接阶段的重复 false 通知）
  let retryDelay = 1000;
  let retryTimer: number | null = null;
  let heartbeatTimer: number | null = null;
  let lastMessageAt = Date.now(); // 最近一次收到服务端帧的时间（含 pong/peers/presence 等）
  let currentHello = opts.hello; // 重连前可经 refreshHello 换新（令牌/身份/配置变化）

  function open(): void {
    try {
      ws = new WebSocket(opts.url);
    } catch {
      scheduleReconnect();
      return;
    }
    ws.onopen = () => {
      alive = true;
      retryDelay = 1000;
      // hello 必须先于任何其它帧发出：连接态回调同步触发各域重连钩子（可能立刻广播 note-sync），
      // 而服务端规定首条消息必须是 hello，否则整条连接被拒
      ws?.send(JSON.stringify({ type: "hello", ...currentHello }));
      opts.onStatusChange(true);
      heartbeatTimer = window.setInterval(() => {
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: "ping" }));
          // 半开连接检测：无任何服务端帧超阈值（服务端假死但 TCP 未断）→ 主动断开触发重连，
          // 否则 onStatusChange 永远停在已连接、presence 陈旧。ping 后服务端必回 pong，
          // 健康连接不会误判（广播 pong 亦刷新本端 lastMessageAt）。
          if (Date.now() - lastMessageAt > STALL_TIMEOUT_MS) {
            ws.close();
          }
        }
      }, HEARTBEAT_MS);
    };
    ws.onmessage = (e) => {
      lastMessageAt = Date.now();
      let msg: CollabServerMessage;
      try {
        msg = JSON.parse(e.data);
      } catch {
        return;
      }
      if (msg.type === "hello-ack") opts.onHelloAck(msg.peerId);
      else if (msg.type === "peers") opts.onPeers(msg.peers);
      else if (msg.type === "presence") opts.onPeerPresence(msg.peerId, msg.presence);
      else if (msg.type === "table-patch")
        opts.onTablePatch(msg.peerId, msg.file, msg.patch);
      else if (msg.type === "canvas-patch")
        opts.onCanvasPatch(msg.peerId, msg.file, msg.patch);
      else if (msg.type === "note-sync")
        opts.onNoteSync(msg.peerId, msg.file, msg.payload);
      else if (msg.type === "note-aware")
        opts.onNoteAware(msg.peerId, msg.file, msg.payload);
      else if (msg.type === "plugin-msg")
        opts.onPluginMsg(msg.peerId, msg.channel, msg.payload);
      else if (msg.type === "resync") opts.onResync();
      else if (msg.type === "pong") {
        // 保活回执：仅刷新 lastMessageAt（staleness 检测用），无其他副作用
      } else if (msg.type === "error") opts.onServerError(msg.message);
    };
    ws.onerror = () => ws?.close(); // 收尾统一走 onclose
    ws.onclose = () => {
      if (heartbeatTimer !== null) {
        clearInterval(heartbeatTimer);
        heartbeatTimer = null;
      }
      ws = null;
      if (alive) {
        alive = false;
        opts.onStatusChange(false);
      }
      scheduleReconnect();
    };
  }

  function scheduleReconnect(): void {
    if (closed) return;
    retryTimer = window.setTimeout(() => {
      retryTimer = null;
      void reopen();
    }, retryDelay);
    retryDelay = Math.min(retryDelay * 2, MAX_RETRY_MS);
  }

  /** 重连前刷新 hello：身份/令牌/配置可能已变；刷新返回 null = 放弃重连并正常收尾
   *  （连接已断、状态已通知，只差不再重试）。刷新调用自身异常（IPC 瞬时失败等）按原 hello
   *  降级重试，不因一次失败永久停摆——凭据真失效会收到服务端 error 帧，经 onServerError 可见。 */
  async function reopen(): Promise<void> {
    if (opts.refreshHello) {
      try {
        const fresh = await opts.refreshHello();
        if (closed) return; // 刷新挂起期间 disconnect() 抢先
        if (!fresh) {
          closed = true;
          return;
        }
        currentHello = fresh;
      } catch (e) {
        console.warn("协作重连刷新 hello 失败：", e instanceof Error ? e.message : String(e));
      }
    }
    open();
  }

  open();
  opts.onStatusChange(false);

  return {
    sendPresence: (presence) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "presence", ...presence }));
    },
    sendTablePatch: (file, patch) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(JSON.stringify({ type: "table-patch", file, patch }));
      return true;
    },
    sendCanvasPatch: (file, patch) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(JSON.stringify({ type: "canvas-patch", file, patch }));
      return true;
    },
    sendNoteSync: (file, payload) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(JSON.stringify({ type: "note-sync", file, payload }));
      return true;
    },
    sendNoteAware: (file, payload) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(JSON.stringify({ type: "note-aware", file, payload }));
      return true;
    },
    sendPluginMsg: (channel, payload, targetPeerId) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return false;
      ws.send(
        JSON.stringify(
          targetPeerId === undefined
            ? { type: "plugin-msg", channel, payload }
            : { type: "plugin-msg", channel, payload, targetPeerId },
        ),
      );
      return true;
    },
    sendBye: () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "bye" }));
      }
    },
    disconnect: () => {
      closed = true;
      if (retryTimer !== null) {
        clearTimeout(retryTimer);
        retryTimer = null;
      }
      if (heartbeatTimer !== null) {
        clearInterval(heartbeatTimer);
        heartbeatTimer = null;
      }
      const socket = ws;
      ws = null;
      alive = false;
      if (!socket) return;
      if (socket.readyState === WebSocket.CONNECTING) {
        // 连接尚未建立（如 effect 双跑的 cleanup 抢先）：此刻 close() 会触发浏览器告警
        // 「closed before the connection is established」。摘掉既有 handler 后改为「建立即关」——
        // 终态一致：不发 hello、不再重连、不再改连接状态。
        socket.onopen = () => socket.close();
        socket.onmessage = null;
        socket.onerror = null;
        socket.onclose = null;
        return;
      }
      socket.close();
    },
  };
}

/** 传输工厂适配：把传输层的频道面映射到帧泵的收发面——
 *  入站按帧类型分发到 onChannelMessage，出站 sendMessage 按频道选择对应 send*。 */
export function connectChannelPump(opts: CollabTransportOptions): CollabTransportHandle {
  const pump = connectFramePump({
    url: opts.url,
    hello: opts.hello,
    refreshHello: opts.refreshHello,
    onHelloAck: opts.onHelloAck,
    onPeers: opts.onPeers,
    onPeerPresence: opts.onPeerPresence,
    onTablePatch: (peerId, file, patch) => opts.onChannelMessage(peerId, "table-patch", file, patch),
    onCanvasPatch: (peerId, file, patch) =>
      opts.onChannelMessage(peerId, "canvas-patch", file, patch),
    onNoteSync: (peerId, file, payload) => opts.onChannelMessage(peerId, "note-sync", file, payload),
    onNoteAware: (peerId, file, payload) =>
      opts.onChannelMessage(peerId, "note-aware", file, payload),
    onPluginMsg: (peerId, channel, payload) =>
      opts.onChannelMessage(peerId, "plugin-msg", channel, payload),
    onResync: opts.onResync,
    onServerError: opts.onServerError,
    onStatusChange: opts.onStatusChange,
  });
  return {
    sendPresence: (presence) => pump.sendPresence(presence),
    sendMessage: (channel, file, payload, targetPeerId) => {
      if (channel === "note-sync") return pump.sendNoteSync(file, payload as string);
      if (channel === "note-aware") return pump.sendNoteAware(file, payload as string);
      if (channel === "canvas-patch") return pump.sendCanvasPatch(file, payload as CanvasPatch);
      if (channel === "table-patch") return pump.sendTablePatch(file, payload as TablePatch);
      // plugin-msg 的 file 槽承载插件频道名（与笔记 file 路由键角色一致）
      return pump.sendPluginMsg(file, payload, targetPeerId);
    },
    sendBye: () => pump.sendBye(),
    disconnect: () => pump.disconnect(),
  };
}
