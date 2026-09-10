/**
 * 笔记正文编辑能力消费 hook：编辑面（笔记面板/画布文本节点）经此打开会话并订阅其状态。
 * 只依赖契约与注册表（`utils/noteSurfaceHost`），实现缺席（笔记插件停用）时返回 null，编辑面降级只读。
 */

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { NoteBodySession, NoteBodySessionView, NoteSurfaceProvider } from "@/types/noteSurface";
import { getNoteSurface, onNoteSurfaceChange } from "@/utils/noteSurfaceHost";

/** 编辑面持有的会话句柄：session 提交变更，view 订阅状态。 */
export interface NoteBodySessionHandle {
  session: NoteBodySession | null;
  view: NoteBodySessionView | null;
}

/** 当前注册的提供者；null = 正文编辑能力不可用。 */
export function useNoteSurface(): NoteSurfaceProvider | null {
  return useSyncExternalStore(onNoteSurfaceChange, getNoteSurface, getNoteSurface);
}

/**
 * 打开并订阅某笔记的正文编辑会话。
 * baselineContent 供编辑面首帧有内容可渲染（随后以磁盘为准），只在打开时取值，不因输入变化重开会话。
 */
export function useNoteBodySession(
  file: string | null,
  baselineContent?: string,
): NoteBodySessionHandle {
  const provider = useNoteSurface();
  const [session, setSession] = useState<NoteBodySession | null>(null);
  const baselineRef = useRef(baselineContent);
  baselineRef.current = baselineContent;

  useEffect(() => {
    if (!provider || !file) return;
    const opened = provider.open(file, baselineRef.current);
    setSession(opened);
    return () => {
      setSession(null);
      provider.close(file);
    };
  }, [provider, file]);

  const subscribe = useCallback(
    (onChange: () => void) => session?.subscribe(onChange) ?? (() => {}),
    [session],
  );
  const getSnapshot = useCallback(() => session?.getState() ?? null, [session]);
  const view = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return { session, view };
}

/** 该笔记是否有未决冲突（会话关闭后仍为真）；供非常开会话的编辑面（如画布节点）显示提示。 */
export function useNoteConflicted(file: string | null): boolean {
  const getSnapshot = useCallback(
    () => (file ? getNoteSurface()?.isConflicted(file) ?? false : false),
    [file],
  );
  return useSyncExternalStore(onNoteSurfaceChange, getSnapshot, getSnapshot);
}

/** 命令式取已打开的会话（不改变引用计数）；用于按焦点归属的快捷键路由。 */
export function getOpenNoteSession(file: string): NoteBodySession | null {
  return getNoteSurface()?.get(file) ?? null;
}
