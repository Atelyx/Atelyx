/**
 * 协作文档宿主 DocHost（内核，域无关）：数据访问抽象的实时面。
 *
 * 组合传输注册表（transport.ts，默认内建 relay）与域接线注册表（utils/collabHost）：
 * - 活动传输句柄由本模块持有（collabStore 是 store 门面，经 connectTransport/send* 管理）；
 * - 入站频道消息统一路由到 collabHost 通道注册表（各域 handler 自注册）；
 * - 出站经 send* 咽喉（断开时静默丢弃）；
 * - 文档注册表（docId→实例 + 引用计数）与模型注册制（registerDocModel）：文档生命周期
 *   由内核编排，具体模型（笔记 Yjs 等）运行时注册，本模块不 import 任何领域模型。
 */
import { dispatchCollabChannel } from "@/utils/collabHost";
import type { CollabPresence, RelayTestResult } from "@/types";
import {
  connectCollabTransport,
  testCollabTransport,
  type CollabChannel,
  type CollabTransportHandle,
  type CollabTransportOptions,
} from "./transport";
// 装配默认内建传输：relay.ts 模块加载时注册 collabRelayTransport（无环——relay 只依赖 transport）
import "@/services/collab/relay";

export type { CollabChannel } from "./transport";

/** 活动传输句柄（重连时替换；断开 = null）。 */
let activeTransport: CollabTransportHandle | null = null;

/** 建连请求（传输名 + 连接参数；onChannelMessage 由 DocHost 内部接通道注册表，调用方不必提供）。 */
export interface ConnectTransportRequest
  extends Omit<CollabTransportOptions, "onChannelMessage"> {
  name: string;
}

/** 建立连接（先 bye + 断开旧连接，再按名连新传输；未注册传输抛错）。
 *  建连失败时活动句柄置空（不残留已断开的旧句柄），由调用方记录降级。 */
export function connectTransport(req: ConnectTransportRequest): void {
  activeTransport?.sendBye();
  activeTransport?.disconnect();
  activeTransport = null;
  activeTransport = connectCollabTransport(req.name, {
    url: req.url,
    hello: req.hello,
    onHelloAck: req.onHelloAck,
    onPeers: req.onPeers,
    onPeerPresence: req.onPeerPresence,
    onChannelMessage: (peerId, channel, file, payload) =>
      dispatchCollabChannel(channel, peerId, file, payload),
    onServerError: req.onServerError,
    onStatusChange: req.onStatusChange,
  });
}

/** 断开并清空活动传输（不再重连）。 */
export function disconnectTransport(): void {
  activeTransport?.sendBye();
  activeTransport?.disconnect();
  activeTransport = null;
}

/** 出站咽喉：按频道透传（断开时静默丢弃）。 */
export function sendTransportMessage(channel: CollabChannel, file: string, payload: unknown): void {
  activeTransport?.sendMessage(channel, file, payload);
}

/** 出站 presence（断开时静默丢弃）。 */
export function sendTransportPresence(presence: CollabPresence): void {
  activeTransport?.sendPresence(presence);
}

/** 按名测试传输连通性（设置页「检查连接」用）。 */
export function testTransport(name: string, url: string): Promise<RelayTestResult> {
  return testCollabTransport(name, url);
}

// ===== 文档注册表 + 模型注册制 =====
// 文档 = 可实时同步的数据单元（docId = `${kind}:${file}`，首冒号拆分配 adapter）。
// 内核只做生命周期编排（绑定引用计数/创建/销毁/重连路由），不解读文档实例——
// 具体模型（笔记 Yjs/画布补丁/表格补丁）经 registerDocModel 在运行时注册，
// 本模块不 import 任何领域模型（模型侧自注册或经 store 接线注册，单向）。

/** 文档模型实例（内核不解读，模型私有形态）。 */
export type DocModelInstance = unknown;

export interface DocModelAdapter {
  kind: string;
  /** 以磁盘基线（或空）创建文档实例（含模型侧基线广播/监听装配）。 */
  createDoc(docId: string, baseline: unknown): DocModelInstance;
  /** 合入远端同步消息（payload 不透明；meta.remoteAuthor 供模型按操作人署名）。 */
  applyRemoteMessage(
    inst: DocModelInstance,
    peerId: number,
    payload: unknown,
    meta?: { remoteAuthor?: unknown },
  ): void;
  /** 重连后重新握手（重发全量状态索取）。 */
  resync(inst: DocModelInstance): void;
  /** 销毁实例（清观察者/定时器/awareness）。 */
  destroy(inst: DocModelInstance): void;
  /** 可选：远端 awareness 合入（光标/选中模型用）。 */
  applyRemoteAwareness?(inst: DocModelInstance, payload: unknown): void;
}

interface DocEntry {
  docId: string;
  kind: string;
  instance: DocModelInstance;
  refcount: number;
  adapter: DocModelAdapter;
}

const docModels = new Map<string, DocModelAdapter>();
const docEntries = new Map<string, DocEntry>();

/** 注册文档模型（幂等覆盖：同 kind 后注册者生效）。 */
export function registerDocModel(adapter: DocModelAdapter): void {
  docModels.set(adapter.kind, adapter);
}

/**
 * 绑定文档：有激活绑定复用现有实例（refcount++，跨面板共享）；无激活以基线重建
 * （先销毁残留实例，再 adapter.createDoc，refcount=1）。重建的基线广播归模型侧。
 */
export function bindDoc(docId: string, baseline?: unknown): DocModelInstance {
  const sep = docId.indexOf(":");
  const kind = sep < 0 ? docId : docId.slice(0, sep);
  const adapter = docModels.get(kind);
  if (!adapter) throw new Error(`协作文档模型未注册：${kind}`);
  const existing = docEntries.get(docId);
  if (existing && existing.refcount > 0) {
    existing.refcount += 1;
    return existing.instance;
  }
  if (existing) {
    existing.adapter.destroy(existing.instance);
    docEntries.delete(docId);
  }
  const instance = adapter.createDoc(docId, baseline);
  docEntries.set(docId, { docId, kind, instance, refcount: 1, adapter });
  return instance;
}

/** 解绑（refcount--；归零仍留注册表保留远端状态，下次绑定重置基线）。 */
export function unbindDoc(docId: string): void {
  const e = docEntries.get(docId);
  if (!e) return;
  e.refcount = Math.max(0, e.refcount - 1);
}

/** 重连后对全部激活文档重新握手。 */
export function resyncAllDocs(): void {
  for (const e of docEntries.values()) {
    if (e.refcount <= 0) continue;
    e.adapter.resync(e.instance);
  }
}

/** 全部销毁（应用退出/切仓库清空协作上下文）。 */
export function destroyAllDocs(): void {
  for (const e of docEntries.values()) {
    e.adapter.destroy(e.instance);
  }
  docEntries.clear();
}
