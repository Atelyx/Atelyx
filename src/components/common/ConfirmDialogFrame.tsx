/**
 * 确认类弹窗的通用外框：fixed 遮罩 + 居中卡片 + 警示标题行（图标 + 标题）；正文与操作区由调用方提供。
 * 遮罩点击与 Esc / 返回键均触发 onCancel；卡片规格（窄卡、四边等距内边距）由本组件统一提供，
 * 避免同类弹窗的尺寸与关闭行为各自漂移。
 */
import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import { AlertTriangle } from "lucide-react";
import { useBackHandler } from "@/hooks/useBackHandler";

export function ConfirmDialogFrame({
  title,
  onCancel,
  children,
}: {
  title: string;
  onCancel: () => void;
  children: ReactNode;
}) {
  const onCancelRef = useRef(onCancel);
  onCancelRef.current = onCancel;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancelRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useBackHandler(true, () => {
    onCancelRef.current();
    return true;
  });

  return (
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center"
      style={{ background: "var(--scrim)" }}
      onClick={onCancel}
    >
      <div
        className="w-80 max-w-[calc(100vw-2rem)] rounded-[var(--radius-lg)] border shadow-[var(--shadow-pop)] p-4"
        style={{ background: "var(--bg-overlay)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start gap-2 mb-2">
          <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" style={{ color: "var(--warning)" }} />
          <h3 className="text-sm font-medium leading-5" style={{ color: "var(--text-primary)" }}>
            {title}
          </h3>
        </div>
        {children}
      </div>
    </div>
  );
}
