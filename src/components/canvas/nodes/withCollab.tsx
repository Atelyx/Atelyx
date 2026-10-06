/**
 * 画布节点协作装饰 HOC：包一层节点组件，叠加远端选中描边（多用户同心叠加）与「生成中」脉冲点。
 * 非侵入：overlay 走 absolute + pointer-events-none，不改动内层组件；协作 state 由 store 订阅穿透 memo。
 */
import { memo, type ComponentType } from "react";
import { useNodeCollab } from "@/hooks/useNodeCollab";

export function withCollab<P extends { id?: string }>(
  Wrapped: ComponentType<P>,
): ComponentType<P> {
  return memo(function CollabNodeWrapper(props: P) {
    const nodeId = props.id ?? "";
    const { selectingPeers, streamingPeers } = useNodeCollab(nodeId);
    const streamingPeer = streamingPeers[0];
    return (
      <div className="relative w-full h-full">
        <Wrapped {...props} />
        {selectingPeers.map((p, i) => (
          <div
            key={p.peerId}
            className="absolute rounded pointer-events-none"
            style={{
              inset: i * 3,
              border: `2px solid ${p.color}`,
            }}
          />
        ))}
        {streamingPeer && (
          <div
            className="absolute top-1 right-1 w-2 h-2 rounded-full pointer-events-none animate-pulse"
            style={{ background: streamingPeer.color }}
            title={`${streamingPeer.nickname} 正在生成`}
          />
        )}
      </div>
    );
  });
}
