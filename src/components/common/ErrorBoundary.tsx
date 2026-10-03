/**
 * 全局错误边界：渲染错误不白屏，显示错误面板（错误信息 + 重新加载 + 关闭窗口）。
 *
 * 主窗口与撕裂窗口共用：无边界时 React 18 渲染崩溃会卸载整棵根树 → 空白白屏且
 * 无任何可操作 UI（自定义标题栏也未渲染，窗口无法关闭）——边界保证崩溃后可读、可关。
 */
import { Component, type ErrorInfo, type ReactNode } from "react";
import { Button } from "@/components/common/Button";
import { useAppStore } from "@/stores/appStore";

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("界面渲染错误", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return <ErrorFallback error={this.state.error} />;
    }
    return this.props.children;
  }
}

/** 错误面板：错误信息 + 重新加载 + 关闭窗口（撕裂窗口崩溃后仍可关闭）。 */
function ErrorFallback({ error }: { error: Error }) {
  const closeWindow = useAppStore((s) => s.closeWindow);
  return (
    <div
      className="h-full w-full flex items-center justify-center select-none"
      style={{ background: "var(--bg-primary)" }}
    >
      <div className="max-w-md w-full text-center px-6">
        <h2 className="text-lg font-semibold" style={{ color: "var(--text-primary)" }}>
          界面渲染出错
        </h2>
        <pre
          className="mt-3 text-xs text-left whitespace-pre-wrap break-all overflow-auto"
          style={{
            color: "var(--danger)",
            background: "var(--bg-secondary)",
            border: "1px solid var(--border)",
            borderRadius: "var(--radius-sm)",
            padding: 8,
            maxHeight: 200,
          }}
        >
          {String(error?.message ?? error)}
        </pre>
        <div className="mt-4 flex items-center justify-center gap-2">
          <Button variant="primary" size="md" onClick={() => window.location.reload()}>
            重新加载
          </Button>
          <Button variant="secondary" size="md" onClick={() => void closeWindow()}>
            关闭窗口
          </Button>
        </div>
      </div>
    </div>
  );
}
