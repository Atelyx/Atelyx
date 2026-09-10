/**
 * 协作中转（collab-relay）WebSocket 客户端封装。
 *
 * 纯 I/O：连接/hello 入房/presence 上报/表格内容补丁上报/心跳保活/指数退避重连，
 * 状态与节流归 collabStore。
 * 协议见 `collab-relay/src/main.rs`（JSON，camelCase）：
 * - C→S `hello`：`{ type, vaultId, nickname, color, deviceName, version? }`（连接后首条必发；version = 本端应用版本号）
 * - C→S `presence`：`{ type, file?, selection?, view? }`
 * - C→S `table-patch`：`{ type, file, patch }`（表格增量补丁实时广播，LWW 按 id 应用）
 * - C→S `canvas-patch`：`{ type, file, patch }`（画布增量补丁实时广播，LWW 按 id 应用）
 * - C→S `ping`（保活）/ `bye`（离开）
 * - S→C `peers`：`{ type, peers: [...] }`（房间成员变化全量推送；成员含 version = 对方应用版本号）
 * - S→C `presence`：`{ type, peerId, presence }`（他人转发）
 * - S→C `table-patch`：`{ type, peerId, file, patch }`（他人补丁转发，不含自己）
 * - S→C `canvas-patch`：`{ type, peerId, file, patch }`（他人补丁转发，不含自己）
 */
import type {
  CanvasPatch,
  CollabHello,
  CollabPeer,
  CollabPresence,
  RelayTestResult,
  TablePatch,
} from "@/types";
import {
  registerCollabTransport,
  type CollabTransportFactory,
  type CollabTransportOptions,
} from "./transport";

/** 心跳间隔：relay 侧 30s 无消息超时踢出，25s 发 ping 保活。 */
const HEARTBEAT_MS = 25_000;
/** 脱线阈值：连续 3 个心跳周期（75s）无任何服务端帧，判为半开假死连接，强制断开重连。 */
const STALL_TIMEOUT_MS = HEARTBEAT_MS * 3;
/** 断线重连退避：1s 起，翻倍，封顶 15s。 */
const MAX_RETRY_MS = 15_000;
/** 连通性测试（检查连接）等待 hello-ack 的超时。 */
const TEST_TIMEOUT_MS = 5_000;

/**
 * 一次性连通性测试（设置页「检查连接」）：独立 WebSocket 连上后发探测 hello（随机房间 id），
 * 收到 hello-ack 即判定中转服务正常并立即断开——与常驻连接互不干扰（不复用连接、不触发重连）。
 * 失败原因：地址不可解析 / 网络不可达 / 中转拒绝（error 帧）/ 超时无应答 / 提前关闭。
 */
export function testRelayConnection(
  url: string,
  timeoutMs: number = TEST_TIMEOUT_MS,
): Promise<RelayTestResult> {
  return new Promise((resolve) => {
    let ws: WebSocket | null = null;
    let timer: number | null = null;
    let settled = false;
    const finish = (result: RelayTestResult) => {
      if (settled) return;
      settled = true;
      if (timer !== null) clearTimeout(timer);
      ws?.close();
      resolve(result);
    };
    try {
      ws = new WebSocket(url);
    } catch {
      finish({ ok: false, message: "地址无法连接（格式错误或协议不受支持）" });
      return;
    }
    timer = window.setTimeout(
      () => finish({ ok: false, message: `连接超时（${timeoutMs / 1000}s 内未收到中转响应）` }),
      timeoutMs,
    );
    ws.onopen = () => {
      // hello 首条必发（relay 才回 hello-ack）；随机房间 id 防误入真实仓库房间
      ws?.send(
        JSON.stringify({
          type: "hello",
          vaultId: `__probe__${Date.now()}`,
          nickname: "连接测试",
          color: "#888888",
          deviceName: "probe",
        }),
      );
    };
    ws.onmessage = (e) => {
      let msg: CollabServerMessage;
      try {
        msg = JSON.parse(e.data);
      } catch {
        return;
      }
      if (msg.type === "hello-ack") finish({ ok: true, message: "连接成功，中转服务正常" });
      else if (msg.type === "error") finish({ ok: false, message: `中转拒绝：${msg.message}` });
    };
    ws.onerror = () => finish({ ok: false, message: "无法连接（网络不可达或地址错误）" });
    ws.onclose = () => finish({ ok: false, message: "连接被对端关闭" });
  });
}

export type CollabServerMessage =
  | { type: "peers"; peers: CollabPeer[] }
  | { type: "hello-ack"; peerId: number }
  | { type: "presence"; peerId: number; presence: CollabPresence }
  | { type: "table-patch"; peerId: number; file: string; patch: TablePatch }
  | { type: "canvas-patch"; peerId: number; file: string; patch: CanvasPatch }
  | { type: "note-sync"; peerId: number; file: string; payload: string }
  | { type: "note-aware"; peerId: number; file: string; payload: string }
  | { type: "pong" }
  | { type: "error"; message: string };

export interface CollabRelayHandle {
  /** 上报本端 presence（调用方自行节流）。 */
  sendPresence(presence: CollabPresence): void;
  /** 广播表格增量补丁（relay 转发给房间内其他成员；未连接时静默丢弃）。 */
  sendTablePatch(file: string, patch: TablePatch): void;
  /** 广播画布增量补丁（relay 转发给房间内其他成员；未连接时静默丢弃）。 */
  sendCanvasPatch(file: string, patch: CanvasPatch): void;
  /** 广播笔记 Yjs 同步消息（base64，relay 不透明转发；未连接时静默丢弃）。 */
  sendNoteSync(file: string, payload: string): void;
  /** 广播笔记 awareness 更新（base64，relay 不透明转发；未连接时静默丢弃）。 */
  sendNoteAware(file: string, payload: string): void;
  /** 主动离开房间（切仓库/关闭应用）。 */
  sendBye(): void;
  /** 断开连接且不再重连。 */
  disconnect(): void;
}

export interface CollabRelayOptions {
  url: string;
  hello: CollabHello;
  /** 收到 hello-ack（分配的本连接 peerId）——客户端据此把自己过滤出 peers 列表。 */
  onHelloAck: (peerId: number) => void;
  onPeers: (peers: CollabPeer[]) => void;
  onPeerPresence: (peerId: number, presence: CollabPresence) => void;
  /** 收到他人表格补丁（文件匹配由调用方判定——只应用当前打开的表格）。 */
  onTablePatch: (peerId: number, file: string, patch: TablePatch) => void;
  /** 收到他人画布补丁（文件匹配由调用方判定——只应用当前打开的画布）。 */
  onCanvasPatch: (peerId: number, file: string, patch: CanvasPatch) => void;
  /** 收到他人笔记 Yjs 同步消息（base64 → 调用方解码合入；文件匹配由调用方判定）。 */
  onNoteSync: (peerId: number, file: string, payload: string) => void;
  /** 收到他人笔记 awareness 更新（base64 → 调用方解码应用）。 */
  onNoteAware: (peerId: number, file: string, payload: string) => void;
  /** 收到服务端 error 帧（协议异常/房间拒绝等）——调用方决定日志或 UI 反馈。 */
  onServerError: (message: string) => void;
  onStatusChange: (connected: boolean) => void;
}

/** 连接 collab-relay。onStatusChange 初始调用一次 false（连接中），成功后 true，断线重连期间 false。 */
export function connectCollabRelay(opts: CollabRelayOptions): CollabRelayHandle {
  let ws: WebSocket | null = null;
  let closed = false; // disconnect() 后不再重连
  let alive = false; // 曾连上（避免未连接阶段的重复 false 通知）
  let retryDelay = 1000;
  let retryTimer: number | null = null;
  let heartbeatTimer: number | null = null;
  let lastMessageAt = Date.now(); // 最近一次收到服务端帧的时间（含 pong/peers/presence 等）

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
      opts.onStatusChange(true);
      ws?.send(JSON.stringify({ type: "hello", ...opts.hello }));
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
      open();
    }, retryDelay);
    retryDelay = Math.min(retryDelay * 2, MAX_RETRY_MS);
  }

  open();
  opts.onStatusChange(false);

  return {
    sendPresence: (presence) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "presence", ...presence }));
    },
    sendTablePatch: (file, patch) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "table-patch", file, patch }));
    },
    sendCanvasPatch: (file, patch) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "canvas-patch", file, patch }));
    },
    sendNoteSync: (file, payload) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "note-sync", file, payload }));
    },
    sendNoteAware: (file, payload) => {
      if (!ws || ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "note-aware", file, payload }));
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

/** 默认内建传输：relay 工厂（把频道收发映射到现有四个 send* / on* 回调；协议不变）。
 *  模块加载即注册进传输注册表——地址可配置，换后端 = 注册新 factory，域零改动。 */
export const collabRelayTransport: CollabTransportFactory = {
  name: "relay",
  connect: (opts: CollabTransportOptions) => {
    const handle = connectCollabRelay({
      url: opts.url,
      hello: opts.hello,
      onHelloAck: opts.onHelloAck,
      onPeers: opts.onPeers,
      onPeerPresence: opts.onPeerPresence,
      onTablePatch: (peerId, file, patch) =>
        opts.onChannelMessage(peerId, "table-patch", file, patch),
      onCanvasPatch: (peerId, file, patch) =>
        opts.onChannelMessage(peerId, "canvas-patch", file, patch),
      onNoteSync: (peerId, file, payload) =>
        opts.onChannelMessage(peerId, "note-sync", file, payload),
      onNoteAware: (peerId, file, payload) =>
        opts.onChannelMessage(peerId, "note-aware", file, payload),
      onServerError: opts.onServerError,
      onStatusChange: opts.onStatusChange,
    });
    return {
      sendPresence: (presence) => handle.sendPresence(presence),
      sendMessage: (channel, file, payload) => {
        if (channel === "note-sync") handle.sendNoteSync(file, payload as string);
        else if (channel === "note-aware") handle.sendNoteAware(file, payload as string);
        else if (channel === "canvas-patch") handle.sendCanvasPatch(file, payload as CanvasPatch);
        else if (channel === "table-patch") handle.sendTablePatch(file, payload as TablePatch);
      },
      sendBye: () => handle.sendBye(),
      disconnect: () => handle.disconnect(),
    };
  },
  testConnection: testRelayConnection,
};

registerCollabTransport(collabRelayTransport);
