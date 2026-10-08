/**
 * 协作连接宿主中继：主窗口（宿主）单点持有协作 WebSocket，撕裂窗口出站帧经事件线上行、入站帧靠宿主广播下行，服务端视角全应用一个连接一个身份。
 * 宿主面处理上行、转发入站、回环 plugin-msg、聚合 presence、记账 demand；客户端面为 proxyTransport 提供上行与下行订阅。
 */
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindowLabel } from "@/services/window";
import { bytesToBase64, base64ToBytes } from "@/utils/base64";
import type { CollabChannel } from "./transport";
import type { CollabPeer, CollabPresence } from "@/types";

/** 宿主窗口 label（主窗口；撕裂窗口经 emitTo 定向上行）。prune 的存活名单必须含它。 */
export const HOST_WINDOW_LABEL = "main";
const UP_EVENT = "collab-relay-up";
const DOWN_EVENT = "collab-relay-down";
const SNAPSHOT_EVENT = "collab-relay-snapshot";
/** 聚合 presence 的输出节流（各窗口上报侧已有同款节流，此处合并多窗口来源再压一拍）。 */
const PRESENCE_FLUSH_MS = 100;

// ---------- 载荷类型 ----------

/** 撕裂窗口 → 宿主的上行帧。 */
export type CollabRelayUp =
  /** 挂载：拉一次连接状态快照（boot 后 peers/myPeerId/connected 立即就位）。 */
  | { kind: "attach"; from: string }
  /** 收尾：清本窗口 demand 与 presence 条目（prune 为主清理路径，此为显式兜底）。 */
  | { kind: "detach"; from: string }
  /** 出站帧转发（域帧与插件消息同形；二进制载荷已在编码边界转 base64 包裹）。 */
  | {
      kind: "send";
      from: string;
      channel: CollabChannel;
      file: string;
      payload: unknown;
      targetPeerId?: number;
    }
  /** presence 上报（本窗口 provider 合并后的值，聚合在宿主侧完成）。 */
  | { kind: "presence"; from: string; presence: CollabPresence }
  /** 协作意愿声明变化（计数增减；窗口收尾由 detach/prune 清账）。 */
  | { kind: "demand"; from: string; delta: 1 | -1 };

/** 宿主 → 撕裂窗口的下行帧（服务端入站帧原样转发 + 宿主回环）。 */
export type CollabRelayDown =
  | { kind: "status"; connected: boolean }
  | { kind: "hello-ack"; peerId: number }
  | { kind: "peers"; peers: CollabPeer[] }
  | { kind: "peer-presence"; peerId: number; presence: CollabPresence }
  /** 频道消息（真实服务端帧或宿主回环的 plugin-msg；excludeFrom = 回环排除的来源窗口）。 */
  | {
      kind: "channel";
      peerId: number;
      channel: CollabChannel;
      file: string;
      payload: unknown;
      excludeFrom?: string;
    }
  | { kind: "meta-changed"; key: string }
  | { kind: "renamed"; oldPath: string; newPath: string }
  | { kind: "resync" };

/** attach 的定向响应：连接状态全量快照（乱序下行帧先到时以其覆盖收敛）。 */
export interface CollabRelaySnapshot {
  connected: boolean;
  peers: CollabPeer[];
  myPeerId: number | null;
}

/** 二进制载荷的事件线包裹（字节不裸过 JSON 事件线）。 */
interface BinaryEnvelope {
  enc: "b64";
  data: string;
}

function encodePayload(payload: unknown): unknown {
  if (payload instanceof Uint8Array) {
    return { enc: "b64", data: bytesToBase64(payload) } satisfies BinaryEnvelope;
  }
  return payload;
}

function decodePayload(payload: unknown): unknown {
  if (
    payload &&
    typeof payload === "object" &&
    (payload as BinaryEnvelope).enc === "b64" &&
    typeof (payload as BinaryEnvelope).data === "string"
  ) {
    return base64ToBytes((payload as BinaryEnvelope).data);
  }
  return payload;
}

// ---------- 角色判定 ----------

/** 本窗口是否协作宿主（主窗口；非 Tauri 环境降级 = 宿主，行为与单窗口一致）。 */
export function isCollabHost(): boolean {
  try {
    return getCurrentWindowLabel() === HOST_WINDOW_LABEL;
  } catch {
    return true;
  }
}

// ---------- 宿主面 ----------

/** 宿主依赖（collabStore/panelStore 接线注入；模块不反向 import store）。 */
export interface CollabRelayHostDeps {
  /** 宿主出站咽喉（sendTransportMessage；返回是否已投递）。 */
  send(channel: CollabChannel, file: string, payload: unknown, targetPeerId?: number): boolean;
  /** 本连接 peerId（回环帧的发送方身份）。 */
  myPeerId(): number | null;
  /** 连接状态全量快照（attach 响应）。 */
  hostState(): CollabRelaySnapshot;
  /** demand 总数变化通知（panelStore 接线 syncCollabHost 重评估连接）。 */
  onDemandChanged(): void;
  /** 聚合 presence 输出（collabStore 接线 sendTransportPresence）。 */
  onPresenceOut(presence: CollabPresence): void;
}

let hostDeps: CollabRelayHostDeps | null = null;
let hostInstalled = false;
/** 各撕裂窗口协作意愿声明计数；宿主自身声明在 collabStore.pluginDemand，不进本表防双计。 */
const demandByWindow = new Map<string, number>();
/** 各窗口最近 presence（聚合与收缩基准；detach/prune 移除）。 */
const presenceByWindow = new Map<string, { presence: CollabPresence; at: number }>();
/** presence 条目序号（单调递增）：焦点取最新，同刻多次上报也能稳定区分先后。 */
let presenceSeq = 0;
/** 是否出现过 presence 条目：清空后须发一次空 presence 覆盖服务端旧值（防幽灵文件残留）。 */
let hadPresence = false;
let presenceFlushTimer: ReturnType<typeof setTimeout> | null = null;

/** 安装宿主中继（幂等一次：上行监听 + attach/detach 处理）。 */
export function installCollabRelayHost(deps: CollabRelayHostDeps): void {
  if (hostInstalled) return;
  hostInstalled = true;
  hostDeps = deps;
  void listen<CollabRelayUp>(UP_EVENT, (e) => {
    handleUp(e.payload);
  });
}

function handleUp(frame: CollabRelayUp): void {
  if (!hostDeps) return;
  switch (frame.kind) {
    case "attach":
      void emitTo(frame.from, SNAPSHOT_EVENT, hostDeps.hostState());
      return;
    case "detach":
      removeWindow(frame.from);
      return;
    case "send": {
      const payload = decodePayload(frame.payload);
      const ok = hostDeps.send(frame.channel, frame.file, payload, frame.targetPeerId);
      // 插件消息回环：服务端不回放发送者，同应用其他窗口的插件经此收到（来源窗口排除）
      const senderPeerId = hostDeps.myPeerId();
      if (ok && senderPeerId !== null && frame.channel === "plugin-msg") {
        void emit(DOWN_EVENT, {
          kind: "channel",
          peerId: senderPeerId,
          channel: frame.channel,
          file: frame.file,
          payload: encodePayload(payload),
          ...(frame.from ? { excludeFrom: frame.from } : {}),
        } satisfies CollabRelayDown);
      }
      return;
    }
    case "presence":
      presenceByWindow.set(frame.from, { presence: frame.presence, at: ++presenceSeq });
      hadPresence = true;
      schedulePresenceFlush();
      return;
    case "demand": {
      const next = Math.max(0, (demandByWindow.get(frame.from) ?? 0) + frame.delta);
      demandByWindow.set(frame.from, next);
      hostDeps.onDemandChanged();
      return;
    }
  }
}

function removeWindow(label: string): void {
  const hadDemand = (demandByWindow.get(label) ?? 0) > 0;
  demandByWindow.delete(label);
  const removed = presenceByWindow.delete(label);
  if (hadDemand) hostDeps?.onDemandChanged();
  if (removed) schedulePresenceFlush();
}

/** 宿主把真实连接的入站帧全量广播给撕裂窗口（error 帧除外——宿主唯一弹点，防重复提示）。
 *  collabStore 回调面接线；未安装（撕裂窗口/单窗口）= no-op。 */
export function forwardCollabInbound(frame: CollabRelayDown): void {
  if (!hostInstalled) return;
  if (frame.kind === "channel") {
    void emit(DOWN_EVENT, { ...frame, payload: encodePayload(frame.payload) });
    return;
  }
  void emit(DOWN_EVENT, frame);
}

/** 宿主自身发出的 plugin-msg 回环（服务端不回放发送者；撕裂窗口插件订阅经此可达）。
 *  域帧不回环（内容视图全局唯一渲染，其他窗口无消费方）；同应用窗口不在彼此 peers 里，
 *  故回环恒为广播——定向单播对同应用窗口不生效（见 sendPluginMessage 的语义损失说明）。 */
export function loopbackPluginMsg(channel: string, payload: unknown): void {
  if (!hostDeps) return;
  const senderPeerId = hostDeps.myPeerId();
  if (senderPeerId === null) return; // 未连接无发送方身份
  void emit(DOWN_EVENT, {
    kind: "channel",
    peerId: senderPeerId,
    channel: "plugin-msg" satisfies CollabChannel,
    file: channel,
    payload: encodePayload(payload),
  } satisfies CollabRelayDown);
}

/** 全应用协作意愿声明总数（撕裂窗口上行记账；宿主自身声明在 collabStore.pluginDemand，不进本表防双计）。
 *  宿主连接的建立/拆除以 pluginDemand + 本值评估（panelStore.syncCollabHost）。 */
export function remoteCollabDemandTotal(): number {
  let total = 0;
  for (const n of demandByWindow.values()) total += n;
  return total;
}

/** 按存活窗口清理 demand/presence 条目（宿主布局镜像变化时调用；撕裂窗口被杀的兜底收账）。
 *  alive 必须含宿主 label，否则宿主自身 presence 条目被误清。 */
export function pruneCollabRelayWindows(alive: string[]): void {
  if (!hostDeps) return;
  const keep = new Set(alive);
  let demandChanged = false;
  let presenceChanged = false;
  for (const label of [...demandByWindow.keys()]) {
    if (!keep.has(label)) {
      demandByWindow.delete(label);
      demandChanged = true;
    }
  }
  for (const label of [...presenceByWindow.keys()]) {
    if (!keep.has(label)) {
      presenceByWindow.delete(label);
      presenceChanged = true;
    }
  }
  if (demandChanged) hostDeps.onDemandChanged();
  if (presenceChanged) schedulePresenceFlush();
}

/** 本窗口协作意愿声明变化：撕裂窗口上行宿主记账；宿主自身不记账（collabStore.pluginDemand
 *  已承载且订阅线触发重评估，记账会与全应用总数双计）。 */
export function reportCollabDemand(delta: 1 | -1): void {
  if (!hostInstalled || isCollabHost()) return;
  void emitTo(HOST_WINDOW_LABEL, UP_EVENT, {
    kind: "demand",
    from: currentLabel(),
    delta,
  } satisfies CollabRelayUp);
}

/** 宿主自身 presence 进聚合器（与服务端单 peer 语义对齐：聚合值 = 全窗口并集，
 *  宿主直发会与聚合输出互相覆盖）。撕裂窗口经 proxy 上行，不走此出口。 */
export function reportCollabPresence(presence: CollabPresence): void {
  if (!hostInstalled) return;
  handleUp({ kind: "presence", from: HOST_WINDOW_LABEL, presence });
}

/** 宿主收尾清自身聚合条目（dispose）：残留条目会把陈旧焦点并进撕裂窗口的后续聚合输出。 */
export function clearCollabHostPresence(): void {
  if (!hostInstalled) return;
  removeWindow(HOST_WINDOW_LABEL);
}

// ---------- presence 聚合 ----------

/** 合并全窗口 presence：焦点字段取最后活跃窗口，打开文件按活跃新到旧并集去重，
 *  锁/生成中节点/编辑中笔记取并集（小集合，上限个位数——见 CollabPresence 类型）。 */
function mergedPresence(): CollabPresence | null {
  if (presenceByWindow.size === 0) return null;
  const entries = [...presenceByWindow.entries()].sort((a, b) => b[1].at - a[1].at);
  const focus = entries[0][1].presence;
  const merged: CollabPresence = {
    file: focus.file,
    selection: focus.selection,
    view: focus.view,
  };
  const openFiles: NonNullable<CollabPresence["openFiles"]> = [];
  const lockedNodes: NonNullable<CollabPresence["lockedNodes"]> = [];
  const streaming = new Set<string>();
  const editing = new Set<string>();
  const lockedIds = new Set<string>();
  for (const [, { presence }] of entries) {
    for (const f of presence.openFiles ?? []) {
      if (!openFiles.some((o) => o.file === f.file)) openFiles.push(f);
    }
    for (const l of presence.lockedNodes ?? []) {
      if (!lockedIds.has(l.id)) {
        lockedIds.add(l.id);
        lockedNodes.push(l);
      }
    }
    for (const id of presence.streamingNodeIds ?? []) streaming.add(id);
    for (const f of presence.editingNotes ?? []) editing.add(f);
  }
  if (openFiles.length) merged.openFiles = openFiles;
  if (lockedNodes.length) merged.lockedNodes = lockedNodes;
  if (streaming.size) merged.streamingNodeIds = [...streaming];
  if (editing.size) merged.editingNotes = [...editing];
  return merged;
}

function schedulePresenceFlush(): void {
  if (!hostDeps) return;
  if (presenceFlushTimer !== null) return;
  presenceFlushTimer = setTimeout(() => {
    presenceFlushTimer = null;
    const merged = mergedPresence();
    if (merged) {
      hostDeps?.onPresenceOut(merged);
    } else if (hadPresence) {
      // 条目清空（全窗口 detach/prune）：发一次空 presence 覆盖服务端旧值，防幽灵文件残留
      hostDeps?.onPresenceOut({ file: null, selection: null, view: null });
    }
  }, PRESENCE_FLUSH_MS);
}

// ---------- 撕裂窗口客户端面（proxyTransport 消费） ----------

function currentLabel(): string {
  try {
    return getCurrentWindowLabel();
  } catch {
    return "";
  }
}

/** 上行出口（撕裂窗口 proxy 出站帧）。 */
export function sendCollabUp(frame: CollabRelayUp): void {
  if (frame.kind === "send") {
    void emitTo(HOST_WINDOW_LABEL, UP_EVENT, { ...frame, payload: encodePayload(frame.payload) });
    return;
  }
  void emitTo(HOST_WINDOW_LABEL, UP_EVENT, frame);
}

/** 下行订阅（当前活动 proxy 的回调面；返回撤销函数）。
 *  来源排除按注册时捕获的窗口 label 判定（订阅者身份在窗口生命周期内恒定）。 */
export function onCollabDown(handler: (frame: CollabRelayDown) => void): () => void {
  const myLabel = currentLabel();
  const un = listen<CollabRelayDown>(DOWN_EVENT, (e) => {
    const frame = e.payload;
    if (frame.kind === "channel" && frame.excludeFrom && frame.excludeFrom === myLabel) return;
    handler(frame.kind === "channel" ? { ...frame, payload: decodePayload(frame.payload) } : frame);
  });
  return () => {
    void un.then((f) => f());
  };
}

/** attach 快照的定向响应订阅（返回撤销函数）。 */
export function onCollabSnapshot(handler: (snapshot: CollabRelaySnapshot) => void): () => void {
  const un = listen<CollabRelaySnapshot>(SNAPSHOT_EVENT, (e) => handler(e.payload));
  return () => {
    void un.then((f) => f());
  };
}
