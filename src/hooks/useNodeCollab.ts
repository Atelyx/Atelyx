/**
 * 单节点的协作实时状态（画布）：远端选中高亮 / 独占编辑锁主 / 生成中，供节点 HOC 与
 * ConversationNode 只读态共用。
 *
 * - 选中高亮：同一画布（presence.file + view=canvas）且 selection.kind==="node" 命中。
 * - 锁主：所有声明（本端 `lockedConversations` + 对端 presence.lockedNodes，仅按 nodeId 匹配，
 *   不按 file/view 过滤——锁跨视图保活）经 `resolveLockState` 确定性判定（since 最小、同 since
 *   按 peerId 取小）。本端非锁主 → 只读；发送前须校验 `iOwnLock`。
 * - 生成中：对端 presence.streamingNodeIds 命中（仅按 nodeId，同锁）。
 *
 * 订阅粒度：presence 每次更新（他端动鼠标）都会让下列 selector 各求值一次，`useShallow` 只负责
 * 「派生结果按值相等则不重渲染」，不省求值——因此 selector 内只做单次遍历的 `filter`/`find`。
 * 返回的每个字段必须是「引用稳定或标量」：selector 返回含新建数组的包装对象时 `useShallow` 恒判
 * 不等（无限重渲染），故数组字段各自订阅并用 `useShallow` 比较。锁主结果是标量对，同样以
 * `useShallow` 比较；锁主对象再由独立订阅解引用（`lockedByPeer` 引用取自 `peers`）。
 */
import { useShallow } from "zustand/react/shallow";
import { useCollabStore } from "@/stores/collabStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useAppStore } from "@/stores/appStore";
import { resolveLockState } from "@/utils/canvasCollab";
import type { CollabPeer } from "@/types";

export interface NodeCollabState {
  /** 远端选中本节点的用户（用户色高亮叠加）。 */
  selectingPeers: CollabPeer[];
  /** 正在本节点 AI 生成的远端用户（生成中指示灯）。 */
  streamingPeers: CollabPeer[];
  /** 本节点的确定性锁主（非本端）= 该用户在独占编辑；null = 无他人持锁。 */
  lockedByPeer: CollabPeer | null;
  /** 本端是否为该节点的确定性锁主（发送/编辑前校验）。 */
  iOwnLock: boolean;
}

export function useNodeCollab(nodeId: string): NodeCollabState {
  const canvasFile = useAppStore((s) => s.currentCanvasFile);
  const myPeerId = useCollabStore((s) => s.myPeerId);
  const myLocks = useCanvasStore((s) => s.lockedConversations);

  // 数组字段：useShallow 逐元素比较（peer 对象引用未变即不重渲染）
  const selectingPeers = useCollabStore(
    useShallow((s) =>
      s.peers.filter(
        (p) =>
          p.presence?.file === canvasFile &&
          p.presence?.view === "canvas" &&
          p.presence?.selection?.kind === "node" &&
          p.presence.selection.nodeId === nodeId,
      ),
    ),
  );
  // 锁/流式按 nodeId 匹配（不依赖 presence.file/view——锁跨视图保活，用户看表格/笔记期间仍持锁）
  const streamingPeers = useCollabStore(
    useShallow((s) => s.peers.filter((p) => p.presence?.streamingNodeIds?.includes(nodeId))),
  );

  // 锁主：两个标量/引用字段（CollabPeer 引用取自 peers，引用不变即不重渲染）
  const { owner, lockedByMe } = useCollabStore(
    useShallow((s) => resolveLockState(nodeId, myLocks[nodeId], myPeerId, s.peers)),
  );
  const lockedByPeer = useCollabStore((s) =>
    owner !== null && !lockedByMe ? (s.peers.find((p) => p.peerId === owner) ?? null) : null,
  );

  return { selectingPeers, streamingPeers, lockedByPeer, iOwnLock: lockedByMe };
}
