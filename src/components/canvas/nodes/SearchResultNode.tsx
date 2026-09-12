/**
 * 搜索结果节点（AI 对话中自主联网搜索的产物节点）。
 *
 * - 标题 = 搜索词，内容 = 结果列表（每条：标题 + 摘要 + 链接）
 * - 节点数据嵌在 .atlx 的 node.data（结构化 JSON，不单独文件化）
 * - 每条结果可展开/折叠查看摘要；可勾选「仅将勾选条目注入上下文」（checked 下标，@引用/连边注入时按子集）
 * - 搜索失败：显示错误 + 重试（重试 = 重新执行搜索并更新 data，4.6 失败降级）
 * - 分层：走 canvasStore（updateNodeData），不直调 service（重试的搜索执行在 store 层调 service）
 */
import { AlertTriangle, ExternalLink, Search } from "lucide-react";
import { useState } from "react";
import type { NodeProps } from "@xyflow/react";
import type { SearchResultData, SearchResultItem } from "@/types";
import { useCanvasStore } from "@/stores/canvasStore";
import { useAppStore } from "@/stores/appStore";
import { isOpenableUrl } from "@/utils/markdown";
import { ConnectionFrame } from "./ConnectionFrame";
import { ResizeHandle } from "./ResizeHandle";

/** 单条结果行：点击标题展开/折叠摘要；checkbox 勾选（注入子集）。 */
function ResultRow({
  item,
  checked,
  onToggleCheck,
}: {
  item: SearchResultItem;
  checked: boolean;
  onToggleCheck: () => void;
}) {
  const [open, setOpen] = useState(false);
  const openUrl = useAppStore((s) => s.openUrl);
  // 搜索 provider 返回的 URL 属外部输入：仅可外部打开的协议才可点击
  const openable = isOpenableUrl(item.url);
  return (
    <li className="text-xs">
      <div className="flex items-start gap-1.5">
        <input
          type="checkbox"
          checked={checked}
          onChange={onToggleCheck}
          title="勾选 = 仅将勾选条目注入上下文"
          className="nodrag mt-0.5 flex-shrink-0"
        />
        <a
          href={openable ? item.url : undefined}
          onClick={(e) => {
            e.preventDefault();
            e.stopPropagation();
            if (openable) void openUrl(item.url);
          }}
          className="flex-1 min-w-0 truncate hover:opacity-80 inline-flex items-center gap-1"
          style={{ color: "var(--accent)", cursor: openable ? "pointer" : "default" }}
          title={item.url}
        >
          <span className="truncate">{item.title || item.url}</span>
          {openable && <ExternalLink size={10} className="flex-shrink-0" />}
        </a>
        <button
          onClick={() => setOpen((v) => !v)}
          className="flex-shrink-0 hover:opacity-80 text-[10px]"
          style={{ color: "var(--text-muted)" }}
          title={open ? "收起摘要" : "展开摘要"}
        >
          {open ? "收起" : "展开"}
        </button>
      </div>
      {open && item.snippet && (
        <p
          className="mt-1 pl-6 text-[11px] leading-relaxed"
          style={{ color: "var(--text-secondary)" }}
        >
          {item.snippet}
        </p>
      )}
    </li>
  );
}

export function SearchResultNode({ id, data, height, selected }: NodeProps) {
  const d = data as unknown as SearchResultData;
  const updateNodeData = useCanvasStore((s) => s.updateNodeData);
  const hasFixedHeight = height != null;

  const toggleCheck = (index: number) => {
    const checked = d.checked ?? [];
    updateNodeData(id, {
      checked: checked.includes(index)
        ? checked.filter((i) => i !== index)
        : [...checked, index],
    });
  };

  /** 重试：重新执行搜索并更新节点（失败降级；搜索执行在 store 层调 service）。 */
  const retry = () => {
    void useCanvasStore.getState().retrySearch(id, d.query);
  };

  return (
    <div
      className="rounded-lg shadow-lg border flex flex-col text-sm"
      style={{
        width: 280,
        height: height ?? undefined,
        minWidth: 220,
        minHeight: 120,
        background: "var(--bg-card)",
        borderColor: selected ? "var(--accent)" : "var(--border)",
        cursor: "default",
        position: "relative",
      }}
    >
      <ConnectionFrame topType="source" selected={selected} />
      <header
        className="px-3 py-2 border-b rounded-t-lg flex items-center gap-1.5"
        style={{
          borderColor: "var(--border)",
          background: "var(--bg-card)",
          cursor: "grab",
        }}
      >
        <Search
          size={13}
          className="flex-shrink-0"
          style={{ color: "var(--accent)" }}
        />
        <span
          className="font-medium text-xs truncate"
          style={{ color: "var(--text-primary)" }}
          title={d.query}
        >
          {d.query}
        </span>
        <span
          className="ml-auto text-[10px] flex-shrink-0"
          style={{ color: "var(--text-muted)" }}
        >
          {d.results.length} 条结果
        </span>
      </header>

      <div
        className={`nodrag nowheel overflow-auto px-3 py-2 ${hasFixedHeight ? "flex-1 min-h-0" : "max-h-[220px]"}`}
        style={{ userSelect: "text", WebkitUserSelect: "text", cursor: "text" }}
      >
        {d.error ? (
          <div className="flex flex-col gap-2">
            <p
              className="text-xs flex items-center gap-1"
              style={{ color: "#f87171" }}
            >
              <AlertTriangle size={13} className="flex-shrink-0" />
              {d.error}
            </p>
            <button
              onClick={() => void retry()}
              className="nodrag text-xs px-2 py-0.5 rounded border self-start hover:opacity-80"
              style={{
                borderColor: "var(--border)",
                color: "var(--text-secondary)",
              }}
            >
              重试
            </button>
          </div>
        ) : d.results.length === 0 ? (
          <p className="text-xs" style={{ color: "var(--text-muted)" }}>
            无搜索结果
          </p>
        ) : (
          <ul className="space-y-2">
            {d.results.map((item, i) => (
              <ResultRow
                key={`${item.url}-${i}`}
                item={item}
                checked={(d.checked ?? []).includes(i)}
                onToggleCheck={() => toggleCheck(i)}
              />
            ))}
          </ul>
        )}
      </div>

      {/* 统一缩放柄（ResizeHandle：与其余可缩放节点同样式，改外观只动共享件） */}
      <ResizeHandle />
    </div>
  );
}
