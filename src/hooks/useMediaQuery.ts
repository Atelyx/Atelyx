/**
 * 响应式断点匹配（matchMedia）：窄屏走移动端布局（全屏 + 导航抽屉），宽屏走桌面布局。
 * 桌面窗口最小宽度 1440，故宽度即可区分两端，无需平台判断。
 */
import { useEffect, useState } from "react";

/** 当前是否匹配查询（无 matchMedia 的运行环境按不匹配处理）。 */
function queryMatches(query: string): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia(query).matches
    : false;
}

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => queryMatches(query));

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia(query);
    const onChange = (): void => setMatches(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);

  return matches;
}

/** 手机窄屏断点（与 Tailwind `sm` 对齐）。 */
export const NARROW_QUERY = "(max-width: 639px)";
