/**
 * 通用确认弹窗（破坏性操作确认，也可作风险提示确认）：正文与按钮见下，弹窗外框与关闭行为
 * （Esc / 遮罩点击 / 返回键）由 ConfirmDialogFrame 统一提供。
 * danger=true（默认）确认按钮红色（破坏性语义），danger=false 用强调色（如安装第三方代码的知情确认）。
 */
import type { ReactNode } from "react";
import { Button } from "@/components/common/Button";
import { ConfirmDialogFrame } from "@/components/common/ConfirmDialogFrame";

export function ConfirmDialog({
  title,
  description,
  children,
  confirmText = "确认",
  cancelText = "取消",
  danger = true,
  onConfirm,
  onCancel,
}: {
  title: string;
  description?: string;
  children?: ReactNode;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <ConfirmDialogFrame title={title} onCancel={onCancel}>
      {description && (
        <p className="text-xs mb-3 whitespace-pre-wrap break-words" style={{ color: "var(--text-muted)" }}>
          {description}
        </p>
      )}
      {children && <div className="mb-3">{children}</div>}
      <div className="flex justify-end gap-2 mt-4">
        <Button variant="ghost" size="sm" onClick={onCancel}>
          {cancelText}
        </Button>
        <Button variant={danger ? "dangerSolid" : "primary"} size="sm" onClick={onConfirm}>
          {confirmText}
        </Button>
      </div>
    </ConfirmDialogFrame>
  );
}
