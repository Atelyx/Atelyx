/**
 * 协作帧泵（WebSocket 连接内核）：WebSocket 生命周期 + 帧编解码 +
 * 心跳保活 + 半开检测 + 指数退避重连。泵不感知 hello 内容，由传输工厂组装后传入。
 *
 * 协议（JSON，camelCase，见 `collab-relay/src/ws.rs`）：
 * - C→S `hello`（首条必发）/ `presence` / `table-patch` / `canvas-patch` /
 *   `note-sync` / `note-aware` / `plugin-msg` / `plugin-replay` / `ping` / `bye`
 * - S→C `peers`（成员全量）/ `hello-ack`（分配 peerId + 房间插件序号头）/ `presence` / 各频道转发帧 /
 *   `meta-changed`（团队 meta 落地广播，只带键名）/ `resync`（接收队列被裁剪）/ `pong` / `error`
 *
 * 插件消息：JSON 载荷走 `plugin-msg` 文本帧（带房间级序号 seq）；二进制载荷（Uint8Array）走
 * WebSocket 二进制帧直传（不经 base64，编解码见 encodePluginBinaryFrame/decodePluginBinaryFrame）。
 * 可靠有序（服务端缓存补投）：连接参数 pluginLastSeq = 上次收到的房间级插件帧序号（跨连接延续、
 * 换房重置，由 DocHost 维护）；hello-ack 后非 null 即发 `plugin-replay` 请求补投缺帧；入站按 seq
 * 检测缺口（补投缓存已逐出）上报 onResync——序号对账全部在泵内闭环，域回调面不感知 seq。
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
  | { type: "hello-ack"; peerId: number; pluginSeq?: number }
  | { type: "presence"; peerId: number; presence: CollabPresence }
  | { type: "table-patch"; peerId: number; file: string; patch: TablePatch }
  | { type: "canvas-patch"; peerId: number; file: string; patch: CanvasPatch }
  | { type: "note-sync"; peerId: number; file: string; payload: string }
  | { type: "note-aware"; peerId: number; file: string; payload: string }
  | { type: "plugin-msg"; peerId: number; channel: string; payload: unknown; seq?: number }
  | { type: "meta-changed"; key: string }
  | { type: "renamed"; payload: { oldPath: string; newPath: string } }
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
  /** 插件消息入站（JSON 载荷与二进制帧统一回调；二进制载荷为 Uint8Array，发送方原样透传）。 */
  onPluginMsg: (peerId: number, channel: string, payload: unknown) => void;
  /** 收到团队 meta 落地广播帧（服务端单向，无 peerId）：只带键名，值由消费方回读磁盘真源。 */
  onMetaChanged: (key: string) => void;
  /** 收到改名/移动落地广播帧（服务端单向，无 peerId，含发起者回放）：打开中的文件据此切到新路径。 */
  onRenamed: (oldPath: string, newPath: string) => void;
  /** 传输侧提示本端接收队列过慢（帧被裁剪）：调用方需重新握手补齐。 */
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
  /** 上次收到的房间级插件帧序号（可靠补投的对账基准；换房 = null）。 */
  pluginLastSeq?: number | null;
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
  /** 广播插件消息（payload 任意 JSON 或二进制 Uint8Array 不透明转发；targetPeerId 指定 = 定向单播）。
   *  返回是否已投递（未连接 = false）。 */
  sendPluginMsg(channel: string, payload: unknown, targetPeerId?: number): boolean;
  /** 本连接已收到的最大插件帧序号（可靠补投对账基准；未收到过 = null）。 */
  pluginSeq(): number | null;
  /** 连接是否已永久收尾（disconnect 调用或重连放弃）。 */
  isClosed(): boolean;
  /** 主动离开房间（切仓库/关闭应用）。 */
  sendBye(): void;
  /** 断开连接且不再重连。 */
  disconnect(): void;
}

// ===== 插件消息二进制帧编解码（WebSocket 二进制帧直传，不经 base64） =====
// 布局（小端，与 collab-relay parse_plugin_binary 逐字节同构）：
// [0]=kind(1) [1]=flags(bit0=有 targetPeerId) [2..3]=channel 字节长
// [channel utf8] [8B seq] [8B targetPeerId（flags.bit0 时）] [8B senderPeerId] [payload 原样字节]。
// seq/senderPeerId 由服务端分配回填（C→S 恒 0），载荷原样透传不解析。

const PLUGIN_BINARY_KIND = 1;
const PLUGIN_BINARY_HAS_TARGET = 0b1;
/** 帧头：kind(1) + flags(1) + channel 字节长(2)。 */
const PLUGIN_BINARY_HEADER = 4;
/** channel 之后的固定尾段：seq(8) + senderPeerId(8)。 */
const PLUGIN_BINARY_TAIL = 8 + 8;

const frameTextEncoder = new TextEncoder();
const frameTextDecoder = new TextDecoder();

/** 解码后的二进制插件帧（payload 是入站 buffer 的视图，消费方同步使用或自行拷贝）。 */
export interface PluginBinaryFrame {
  channel: string;
  payload: Uint8Array;
  seq: number;
  targetPeerId?: number;
  senderPeerId: number;
}

/** 编码二进制插件帧（客户端发送：seq/senderPeerId 置 0，由服务端回填）。 */
export function encodePluginBinaryFrame(
  channel: string,
  payload: Uint8Array,
  targetPeerId?: number,
): Uint8Array {
  const name = frameTextEncoder.encode(channel);
  if (name.length > 0xffff) throw new Error("插件频道名过长");
  const hasTarget = targetPeerId !== undefined;
  const frame = new Uint8Array(
    PLUGIN_BINARY_HEADER + name.length + PLUGIN_BINARY_TAIL + (hasTarget ? 8 : 0) + payload.length,
  );
  const view = new DataView(frame.buffer);
  frame[0] = PLUGIN_BINARY_KIND;
  frame[1] = hasTarget ? PLUGIN_BINARY_HAS_TARGET : 0;
  view.setUint16(2, name.length, true);
  frame.set(name, PLUGIN_BINARY_HEADER);
  let offset = PLUGIN_BINARY_HEADER + name.length;
  view.setBigUint64(offset, 0n, true); // seq：服务端分配
  offset += 8;
  if (hasTarget) {
    view.setBigUint64(offset, BigInt(targetPeerId), true);
    offset += 8;
  }
  view.setBigUint64(offset, 0n, true); // senderPeerId：服务端回填
  offset += 8;
  frame.set(payload, offset);
  return frame;
}

/** 解码二进制插件帧（kind 不符/长度不足等畸形帧返回 null，调用方忽略）。 */
export function decodePluginBinaryFrame(data: ArrayBuffer): PluginBinaryFrame | null {
  if (data.byteLength < PLUGIN_BINARY_HEADER) return null;
  const bytes = new Uint8Array(data);
  const view = new DataView(data);
  if (bytes[0] !== PLUGIN_BINARY_KIND) return null;
  const nameLen = view.getUint16(2, true);
  if (PLUGIN_BINARY_HEADER + nameLen + 8 > data.byteLength) return null;
  const channel = frameTextDecoder.decode(
    bytes.subarray(PLUGIN_BINARY_HEADER, PLUGIN_BINARY_HEADER + nameLen),
  );
  let offset = PLUGIN_BINARY_HEADER + nameLen;
  const seq = Number(view.getBigUint64(offset, true));
  offset += 8;
  let targetPeerId: number | undefined;
  if (bytes[1] & PLUGIN_BINARY_HAS_TARGET) {
    if (offset + 8 > data.byteLength) return null;
    targetPeerId = Number(view.getBigUint64(offset, true));
    offset += 8;
  }
  if (offset + 8 > data.byteLength) return null;
  const senderPeerId = Number(view.getBigUint64(offset, true));
  offset += 8;
  return { channel, payload: bytes.subarray(offset), seq, targetPeerId, senderPeerId };
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
  // 房间级插件帧序号 = 最后按序投递的 seq（重连补投的 after 基准）：初值来自连接参数，
  // 跨本连接的自动重连延续
  let pluginSeq: number | null = opts.pluginLastSeq ?? null;
  // 补投窗口的乱序治理：plugin-replay 请求在服务端与直播转发竞争入队，直播帧可能先于补投帧
  // 到达。发出补投请求后进入缓冲模式——帧按 seq 暂存不投递，补投帧（expected 连续段）到达即
  // 按序冲刷；expected 越过 hello-ack 序号头（缓存内补投帧全部消费完）回归直通，冲刷超时
  // （缓存已逐出缺口）上报 onResync 后按 seq 升序尽力投递剩余帧。
  let replayWindow: {
    buffer: Map<number, { peerId: number; channel: string; payload: unknown }>;
    expected: number;
    /** hello-ack 时的房间序号头：expected 越过即补投段全部消费完。 */
    head: number;
    timer: number;
  } | null = null;
  const REPLAY_FLUSH_TIMEOUT_MS = 5_000;

  /** 投递一帧并推进按序基线。 */
  function deliverPluginMsg(peerId: number, channel: string, payload: unknown, seq: number): void {
    pluginSeq = seq;
    opts.onPluginMsg(peerId, channel, payload);
  }

  /** 退出缓冲模式（取消超时兜底计时）。 */
  function exitReplayWindow(): void {
    if (!replayWindow) return;
    window.clearTimeout(replayWindow.timer);
    replayWindow = null;
  }

  /** 从缓冲按序冲刷连续段（expected 起推进）；越过头且缓冲清空 = 补投段消费完，回归直通模式。 */
  function flushReplayWindow(): void {
    if (!replayWindow) return;
    while (replayWindow.buffer.has(replayWindow.expected)) {
      const frame = replayWindow.buffer.get(replayWindow.expected)!;
      replayWindow.buffer.delete(replayWindow.expected);
      deliverPluginMsg(frame.peerId, frame.channel, frame.payload, replayWindow.expected);
      replayWindow.expected += 1;
    }
    if (replayWindow.buffer.size === 0 && replayWindow.expected > replayWindow.head) {
      exitReplayWindow();
    }
  }

  /** 缓冲超时：补投未覆盖全部缺口（缓存已逐出等），上报 onResync（缺口不静默）后按 seq 升序尽力投递。 */
  function flushReplayWindowWithResync(): void {
    if (!replayWindow) return;
    const frames = [...replayWindow.buffer.entries()].sort((a, b) => a[0] - b[0]);
    exitReplayWindow();
    opts.onResync();
    for (const [seq, frame] of frames) {
      deliverPluginMsg(frame.peerId, frame.channel, frame.payload, seq);
    }
  }

  /** 入站插件帧的对账与投递：
   *  - 缓冲模式（补投在途）：无序号单播帧尽力而为直投；其余按 seq 入缓冲等按序冲刷，
   *    seq < expected 的重复帧丢弃（不误报缺口）；
   *  - 直通模式：seq 缺口（> 基线+1，补投缓存已逐出）上报 onResync；seq ≤ 0 或缺省
   *    （单播帧无序号——JSON 缺省 / 二进制哨兵 0 / 旧服务端）跳过对账原样投递。 */
  function trackPluginSeq(seq: number | undefined, peerId: number, channel: string, payload: unknown): void {
    if (replayWindow) {
      if (typeof seq !== "number" || seq <= 0) {
        opts.onPluginMsg(peerId, channel, payload);
        return;
      }
      if (seq < replayWindow.expected) return;
      replayWindow.buffer.set(seq, { peerId, channel, payload });
      flushReplayWindow();
      return;
    }
    if (typeof seq !== "number" || seq <= 0) {
      opts.onPluginMsg(peerId, channel, payload);
      return;
    }
    // 重复帧（补投超时按序投递后迟到的补投帧等）丢弃，不重复送达
    if (pluginSeq !== null && seq <= pluginSeq) return;
    if (pluginSeq !== null && seq > pluginSeq + 1) opts.onResync();
    deliverPluginMsg(peerId, channel, payload, seq);
  }

  function open(): void {
    try {
      ws = new WebSocket(opts.url);
    } catch {
      scheduleReconnect();
      return;
    }
    ws.binaryType = "arraybuffer"; // 二进制插件帧以 ArrayBuffer 入站（JSON 帧仍是 string）
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
      // 二进制帧 = 插件消息二进制载荷（服务端只对 plugin-msg 使用二进制直传）
      if (typeof e.data !== "string") {
        if (!(e.data instanceof ArrayBuffer)) return;
        const frame = decodePluginBinaryFrame(e.data);
        if (!frame) return;
        trackPluginSeq(frame.seq, frame.senderPeerId, frame.channel, frame.payload);
        return;
      }
      let msg: CollabServerMessage;
      try {
        msg = JSON.parse(e.data);
      } catch {
        return;
      }
      if (msg.type === "hello-ack") {
        exitReplayWindow();
        opts.onHelloAck(msg.peerId);
        if (typeof msg.pluginSeq !== "number") {
          // 旧服务端无序号空间：补投请求照发（被忽略），帧无 seq 走直通，不进缓冲模式
          if (pluginSeq !== null) {
            ws?.send(JSON.stringify({ type: "plugin-replay", after: pluginSeq }));
          }
        } else {
          // 序号头小于本地基线 = 房间序号空间已重置（房间清空重建）：基线归零，
          // 离线期间新房间已广播的帧经 after=0 补投取回
          if (pluginSeq !== null && msg.pluginSeq < pluginSeq) {
            pluginSeq = 0;
          }
          if (pluginSeq !== null && pluginSeq < msg.pluginSeq) {
            // 有缺帧可补才请求补投并进缓冲；基线已到头（静音房间重连）无事可补，保持直通
            ws?.send(JSON.stringify({ type: "plugin-replay", after: pluginSeq }));
            replayWindow = {
              buffer: new Map(),
              expected: pluginSeq + 1,
              head: msg.pluginSeq,
              timer: window.setTimeout(flushReplayWindowWithResync, REPLAY_FLUSH_TIMEOUT_MS),
            };
          }
        }
      } else if (msg.type === "peers") opts.onPeers(msg.peers);
      else if (msg.type === "presence") opts.onPeerPresence(msg.peerId, msg.presence);
      else if (msg.type === "table-patch")
        opts.onTablePatch(msg.peerId, msg.file, msg.patch);
      else if (msg.type === "canvas-patch")
        opts.onCanvasPatch(msg.peerId, msg.file, msg.patch);
      else if (msg.type === "note-sync")
        opts.onNoteSync(msg.peerId, msg.file, msg.payload);
      else if (msg.type === "note-aware")
        opts.onNoteAware(msg.peerId, msg.file, msg.payload);
      else if (msg.type === "plugin-msg") {
        trackPluginSeq(msg.seq, msg.peerId, msg.channel, msg.payload);
      } else if (msg.type === "meta-changed") opts.onMetaChanged(msg.key);
      else if (msg.type === "renamed") opts.onRenamed(msg.payload.oldPath, msg.payload.newPath);
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
      // 断线即丢弃补投缓冲：残留帧属于旧连接，不得在重连等待期经超时兜底投递
      exitReplayWindow();
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
      if (payload instanceof Uint8Array) {
        // 二进制载荷走 WebSocket 二进制帧直传（不经 base64；seq/senderPeerId 由服务端回填）
        ws.send(encodePluginBinaryFrame(channel, payload, targetPeerId));
        return true;
      }
      ws.send(
        JSON.stringify(
          targetPeerId === undefined
            ? { type: "plugin-msg", channel, payload }
            : { type: "plugin-msg", channel, payload, targetPeerId },
        ),
      );
      return true;
    },
    pluginSeq: () => pluginSeq,
    isClosed: () => closed,
    sendBye: () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "bye" }));
      }
    },
    disconnect: () => {
      closed = true;
      // 断开即丢弃补投缓冲（残留帧属于旧连接，不得投递给新连接的订阅者）
      exitReplayWindow();
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
    pluginLastSeq: opts.pluginLastSeq,
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
    onMetaChanged: opts.onMetaChanged,
    onRenamed: opts.onRenamed,
    onResync: opts.onResync,
    onServerError: opts.onServerError,
    onStatusChange: opts.onStatusChange,
  });
  return {
    sendPresence: (presence) => pump.sendPresence(presence),
    pluginSeq: () => pump.pluginSeq(),
    isClosed: () => pump.isClosed(),
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
