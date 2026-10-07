/**
 * 转发传输工厂：撕裂窗口的协作传输后端——出站帧经事件线转发宿主发出、下行帧由宿主分发回来，本窗口不持 WebSocket。
 * 心跳/重连/补投在宿主连接内闭环：连接状态靠宿主下行帧同步，断连期间出站按契约返回 false。
 */
import { getCurrentWindowLabel } from "@/services/window";
import {
  onCollabDown,
  onCollabSnapshot,
  sendCollabUp,
} from "./collabRelay";
import { registerCollabTransport, type CollabTransportFactory, type CollabTransportOptions } from "./transport";

/** proxy 连接目标常量（目标恒定 → 重复 init 经 transportMatchesConnection 跳过拆建）。 */
export const PROXY_TARGET_URL = "proxy://collab-host";
export const PROXY_TARGET_HELLO = { nickname: "", color: "", deviceName: "" };

export const proxyCollabTransport: CollabTransportFactory = {
  name: "proxy",
  connect(opts: CollabTransportOptions) {
    let closed = false;
    let hostConnected = false;
    const label = getCurrentWindowLabel();
    // 下行帧喂回调面；snapshot 与 peers 为全量帧，乱序到达时后到者覆盖收敛
    const offDown = onCollabDown((frame) => {
      if (closed) return;
      switch (frame.kind) {
        case "status":
          hostConnected = frame.connected;
          opts.onStatusChange(frame.connected);
          return;
        case "hello-ack":
          opts.onHelloAck(frame.peerId);
          return;
        case "peers":
          opts.onPeers(frame.peers);
          return;
        case "peer-presence":
          opts.onPeerPresence(frame.peerId, frame.presence);
          return;
        case "channel":
          opts.onChannelMessage(frame.peerId, frame.channel, frame.file, frame.payload);
          return;
        case "meta-changed":
          opts.onMetaChanged(frame.key);
          return;
        case "renamed":
          opts.onRenamed(frame.oldPath, frame.newPath);
          return;
        case "resync":
          opts.onResync();
          return;
      }
    });
    // attach 拉基线：快照按 hello-ack → peers → status 序重放（基线晚于任何先前下行帧，收敛）
    const offSnapshot = onCollabSnapshot((snapshot) => {
      if (closed) return;
      hostConnected = snapshot.connected;
      if (snapshot.myPeerId !== null) opts.onHelloAck(snapshot.myPeerId);
      opts.onPeers(snapshot.peers);
      opts.onStatusChange(snapshot.connected);
    });
    // 未就绪先声明，快照到达后按宿主实况置位
    opts.onStatusChange(false);
    sendCollabUp({ kind: "attach", from: label });

    return {
      sendPresence: (presence) => {
        if (closed) return;
        sendCollabUp({ kind: "presence", from: label, presence });
      },
      sendMessage: (channel, file, payload, targetPeerId) => {
        if (closed || !hostConnected) return false;
        sendCollabUp({
          kind: "send",
          from: label,
          channel,
          file,
          payload,
          ...(targetPeerId !== undefined ? { targetPeerId } : {}),
        });
        return true;
      },
      pluginSeq: () => null,
      isClosed: () => closed,
      // 撕裂窗口不控制房间进出（bye 随宿主连接生命周期），空实现
      sendBye: () => {},
      disconnect: () => {
        if (closed) return;
        closed = true;
        hostConnected = false;
        offDown();
        offSnapshot();
        sendCollabUp({ kind: "detach", from: label });
      },
    };
  },
};

registerCollabTransport(proxyCollabTransport);
