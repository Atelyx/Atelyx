/**
 * 通用确认弹窗（fixed 遮罩 + 居中卡片，用于破坏性操作确认，也可作风险提示确认）。
 * Esc / 点击遮罩取消；danger=true（默认）确认按钮红色（破坏性语义），
 * danger=false 用强调色（如安装第三方代码的知情确认）。
 */
import { useEffect, useRef } from "react";
import { AlertTriangle } from "lucide-react";

export function ConfirmDialog({
  title,
  description,
  confirmText = "确认",
  cancelText = "取消",
  danger = true,
  onConfirm,
  onCancel,
}: {
  title: string;
  description?: string;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;

  // Esc 取消
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancelRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onClick={onCancel}
    >
      <div
        className="w-80 rounded-lg border shadow-xl p-4"
        style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start gap-2 mb-2">
          <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" style={{ color: "#f59e0b" }} />
          <h3 className="text-sm font-medium leading-5" style={{ color: "var(--text-primary)" }}>
            {title}
          </h3>
        </div>
        {description && (
          <p className="text-xs mb-3 whitespace-pre-wrap break-words" style={{ color: "var(--text-muted)" }}>
            {description}
          </p>
        )}
        <div className="flex justify-end gap-2 mt-4">
          <button
            onClick={onCancel}
            className="px-3 py-1.5 text-xs rounded hover:bg-[var(--hover)]"
            style={{ color: "var(--text-primary)" }}
          >
            {cancelText}
          </button>
          <button
            onClick={onConfirm}
            className={`px-3 py-1.5 text-xs rounded ${danger ? "bg-red-600 hover:bg-red-500 text-white" : ""}`}
            style={danger ? undefined : { background: "var(--accent)", color: "var(--accent-fg)" }}
          >
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
