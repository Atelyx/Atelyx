/**
 * 启动/进仓加载屏：窗口创建即显示（init 完成前）与仓库加载期间（selectVault 全程）。
 * 深色全屏 + Logo + 循环扫光进度条（indeterminate）+ 加载步骤清单：
 * 步骤区固定可视窗（无滚动条），底部锚定——新条目从下轻入、旧条目被顶出可视区，
 * 顶部渐变阴影遮罩淡出；已完成项打勾（金色），最后一项 = 当前进行中（转圈 + 高亮）。
 * 步骤由 appStore 的加载会话上报（beginLoad/reportLoad/endLoad），清单为空时不渲染
 * （撕裂窗口/路由懒加载 fallback 复用本组件时保持纯 Logo + 扫光）。
 * 扫光动画（.sweep-track/.sweep-bar）与条目入场（load-step-in）定义在 styles/index.css。
 */
// 应用图标（与 src-tauri/icons/icon.svg 同源，加载屏 Logo 展示）
import { Check, Loader2 } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import appIcon from "@/assets/icon.svg";

export function LoadingScreen() {
  const loadSteps = useAppStore((s) => s.loadSteps);

  return (
    <div
      className="h-full flex flex-col items-center justify-center select-none"
      style={{ background: "#1e1e1e" }}
    >
      <img
        src={appIcon}
        alt="Atelyx"
        draggable={false}
        className="w-16 h-16 rounded-2xl shadow-lg ring-1 ring-white/10"
      />
      <div className="sweep-track mt-8">
        <div className="sweep-bar" />
      </div>

      {/* 步骤可视窗：固定高度 + overflow-hidden（无滚动条），justify-end 底部锚定——
          条目从下向上顶替，超出顶部者被遮罩渐变淡出；上下留白不顶格 */}
      {loadSteps.length > 0 && (
        <div className="relative mt-7 w-[460px] h-[156px] overflow-hidden">
          <div
            className="pointer-events-none absolute inset-x-0 top-0 h-10 z-10"
            style={{
              background:
                "linear-gradient(to bottom, #1e1e1e 0%, rgba(30,30,30,0) 100%)",
            }}
          />
          <ul className="h-full flex flex-col justify-end gap-1.5 px-1 py-1 text-[13px]">
            {loadSteps.map((step, i) => {
              const current = i === loadSteps.length - 1;
              return (
                <li
                  key={i}
                  className="flex items-center gap-2 min-w-0"
                  style={{ animation: "load-step-in 0.25s ease-out" }}
                >
                  {current ? (
                    <Loader2
                      size={12}
                      className="animate-spin flex-shrink-0"
                      style={{ color: "#d4af37" }}
                    />
                  ) : (
                    <Check
                      size={12}
                      className="flex-shrink-0"
                      style={{ color: "#d4af37" }}
                    />
                  )}
                  <span
                    className="truncate"
                    style={{
                      color: current
                        ? "rgba(255,255,255,0.9)"
                        : "rgba(255,255,255,0.45)",
                    }}
                  >
                    {step}
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </div>
  );
}
