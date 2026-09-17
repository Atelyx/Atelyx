/**
 * 窗口控制按钮组（decorations: false 自定义标题栏用）。
 * 图标用 `--titlebar-icon`（夜间 #cccccc 浅灰）；最小化/最大化 hover 背景
 * 用 `--titlebar-hover`（夜间 #333333）；关闭 hover 背景 Windows 警告红
 * `--titlebar-close-hover`（夜间 #e81123）且 X 变白。高度由父容器决定。
 * `data-tauri-drag-region` 让空白区可拖拽窗口，按钮本身自动排除。
 * 窗口控制经 props 回调由页面注入（页面从 appStore 取，经 services 层转发）。
 */
export function TitleBarControls({
  onMinimize,
  onMaximize,
  onClose,
}: {
  onMinimize: () => void;
  /** 不传 = 隐藏最大化按钮（固定尺寸窗口最大化无意义）。 */
  onMaximize?: () => void;
  onClose: () => void;
}) {
  return (
    <div className="h-full flex items-stretch flex-shrink-0" data-tauri-drag-region>
      <button
        onClick={onMinimize}
        title="最小化"
        aria-label="最小化"
        className="w-[46px] h-full flex items-center justify-center transition-colors hover:bg-[var(--titlebar-hover)]"
        style={{ color: "var(--titlebar-icon)" }}
        data-tauri-drag-region="false"
      >
        <svg width="10" height="10" viewBox="0 0 10 10">
          <path d="M0 5h10" stroke="currentColor" />
        </svg>
      </button>
      {onMaximize && (
        <button
          onClick={onMaximize}
          title="最大化/还原"
          aria-label="最大化/还原"
          className="w-[46px] h-full flex items-center justify-center transition-colors hover:bg-[var(--titlebar-hover)]"
          style={{ color: "var(--titlebar-icon)" }}
          data-tauri-drag-region="false"
        >
          <svg width="10" height="10" viewBox="0 0 10 10">
            <rect x="1" y="1" width="8" height="8" fill="none" stroke="currentColor" />
          </svg>
        </button>
      )}
      <button
        onClick={onClose}
        title="关闭"
        aria-label="关闭"
        className="w-[46px] h-full flex items-center justify-center transition-colors hover:bg-[var(--titlebar-close-hover)] hover:text-white"
        style={{ color: "var(--titlebar-icon)" }}
        data-tauri-drag-region="false"
      >
        <svg width="10" height="10" viewBox="0 0 10 10">
          <path d="M1 1l8 8M9 1l-8 8" stroke="currentColor" />
        </svg>
      </button>
    </div>
  );
}
