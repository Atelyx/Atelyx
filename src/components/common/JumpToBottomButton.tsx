/**
 * 「新消息」回底按钮：滚动区上翻停止跟随后浮现，点击回底恢复跟随。
 * 对话节点（画布，需 nodrag 防拖拽）与 AI 对话面板共用；追加类经 className 透传。
 */
import { ArrowDown } from "lucide-react";

export function JumpToBottomButton({
  onClick,
  className,
}: {
  onClick: () => void;
  className?: string;
}) {
  return (
    // 保留原生 button（不归基元）：悬浮胶囊按钮控件高约 30px，落在 md(28) 与 lg(32) 两档之间，
    // 且是浮层控件（胶囊形 + 投影 + bg-overlay 底 + 边框），形态与基元的方形控件档位不同
    <button
      onClick={onClick}
      className={`absolute bottom-2 left-1/2 -translate-x-1/2 flex items-center gap-1 text-xs rounded-full px-2.5 py-1 shadow-[var(--shadow-pop)] hover:opacity-80 ${className ?? ""}`}
      style={{
        background: "var(--bg-overlay)",
        border: "1px solid var(--border)",
        color: "var(--text-secondary)",
      }}
    >
      <ArrowDown size={12} /> 新消息
    </button>
  );
}
