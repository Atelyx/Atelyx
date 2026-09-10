/**
 * 协作（presence）类型：与 collab-relay 的 JSON 协议对齐（camelCase 透传）。
 * 本端选中状态经节流广播给同仓库在线用户，远端按此渲染高亮。
 */
import type { TableSelection } from "@/types/table";

/**
 * 远端用户的选中状态（表格单元格 / 框选区域 / 整行整列整表 / 画布节点）。
 * 表格侧复用 `TableSelection`（含 range 拖拽框选，presence 透传），画布侧为 node 变体。
 */
export type CollabSelection = TableSelection | { kind: "node"; nodeId: string };

/** 画布对话节点独占编辑锁声明（presence 携带）。
 * 确定性锁主判定 = since 最小；同 since 按 peerId 递增取小（relay 全局递增分配，确定性）。 */
export interface CollabLockClaim {
  /** 对话节点 id。 */
  id: string;
  /** 获取时间戳（ms）。 */
  since: number;
}

/** 远端用户 presence：打开的文件 + 选中 + 编辑器视图（null = 未在看表格/笔记）。 */
export interface CollabPresence {
  file: string | null;
  selection: CollabSelection;
  /** 当前视图：note/canvas 为专属槽位；表格类一律 "table" 或插件表格视图 kind（远端按 note/canvas 判别，其余落 table 槽）。 */
  view: string | null;
  /** 该用户当前打开的全部文件（聚焦文件置顶；最多 3 个：当前画布/笔记/表格）。 */
  openFiles?: Array<{ file: string; view: "canvas" | "note" | "table" }>;
  /** 画布对话节点独占编辑锁（跨视图保活：用户看表格/笔记期间锁仍对端可见）。 */
  lockedNodes?: CollabLockClaim[];
  /** 画布正在 AI 生成的对话节点（生成中指示灯）。 */
  streamingNodeIds?: string[];
  /** 本端已打开编辑面的笔记（笔记面板或画布笔记节点）：对端据此显示「谁在看/改这篇」，
   *  与聚焦文件无关——画布节点上编辑笔记时聚焦文件仍是画布。 */
  editingNotes?: string[];
}

/** 房间（同仓库 vaultId）内一个在线用户。 */
export interface CollabPeer {
  peerId: number;
  nickname: string;
  color: string;
  deviceName: string;
  /** 该成员使用的应用版本号（hello 携带；旧客户端/旧中转缺省）。 */
  version?: string;
  presence: CollabPresence | null;
}

/** 连接时的身份声明（hello 消息，进入 vaultId 房间）。 */
export interface CollabHello {
  vaultId: string;
  nickname: string;
  color: string;
  deviceName: string;
  /** 本端应用版本号（协作房间展示各成员版本；旧客户端可缺省）。 */
  version?: string;
}

/** 连通性测试结果（设置页「检查连接」展示）。 */
export interface RelayTestResult {
  ok: boolean;
  message: string;
}
