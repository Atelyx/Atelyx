/**
 * 确认类弹窗外框（DialogFrame 的警示特化）：窄卡 + 警示标题行（图标 + 标题）；
 * 正文与操作区由调用方提供，尺寸与关闭行为由 DialogFrame 统一。
 */
import type { ReactNode } from "react";
import { AlertTriangle } from "lucide-react";
import { DialogFrame } from "@/components/common/DialogFrame";
import { Z_LAYERS } from "@/constants/zLayers";

export function ConfirmDialogFrame({
  title,
  onCancel,
  children,
}: {
  title: string;
  onCancel: () => void;
  children: ReactNode;
}) {
  return (
    <DialogFrame
      onClose={onCancel}
      z={Z_LAYERS.dialog}
      panelClassName="w-80 max-w-[calc(100vw-2rem)] p-4"
      ariaLabel={title}
    >
      <div className="flex items-start gap-2 mb-2">
        <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" style={{ color: "var(--warning)" }} />
        <h3 className="text-sm font-medium leading-5" style={{ color: "var(--text-primary)" }}>
          {title}
        </h3>
      </div>
      {children}
    </DialogFrame>
  );
}
