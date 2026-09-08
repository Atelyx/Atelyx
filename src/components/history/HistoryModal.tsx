/**
 * 通用历史面板（模态浮层）：画布/笔记/表格共用——版本列表（时间/作者/行为/人话摘要）+
 * 按 kind 预览（笔记 = 全文；画布/表格 = 相对上一版本的变更 diff）+ 回滚。
 * 数据经各 store（noteHistoryLoad / canvasHistoryLoad / tableHistoryLoad 等）读写，
 * 组件不直连 service。回滚成功经 onRollback(content) 回传调用方（笔记编辑器刷新正文；
 * 画布/表格由 store 自行重载内存态）。
 */
import { useEffect, useMemo, useState } from "react";
import { History, RotateCcw, ChevronDown, ChevronRight, X } from "lucide-react";
import {
  useVaultStore,
  type HistoryVersion,
} from "@/stores/vaultStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useTableStore } from "@/stores/tableStore";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { diffTableVersions } from "@/utils/table";
import { diffCanvasVersions } from "@/utils/canvasCollab";
import type { CanvasFile, TableFile } from "@/types";

type HistoryKind = "note" | "canvas" | "table";

interface Props {
  kind: HistoryKind;
  file: string;
  open: boolean;
  onClose: () => void;
  /** 回滚成功：回传回滚后全文/快照（调用方按需刷新；画布/表格 store 已自行重载）。 */
  onRollback?: (content: string) => void;
}

/** 版本行为 → 中文文案（历史面板共用单一来源）；未知/旧值回落显示原始 action。 */
export const ACTION_LABEL: Record<string, string> = {
  edit: "编辑",
  restore: "回滚",
};

const KIND_LABEL: Record<HistoryKind, string> = {
  note: "笔记",
  canvas: "画布",
  table: "表格",
};

function fmtTime(ts: number): string {
  const d = new Date(ts);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 版本作者展示：单作者保留「name · device」（与历史原展示一致）；多协作者并发存档点展示全部参与者名（按名去重）。 */
function formatAuthors(
  author: HistoryVersion["author"],
  coAuthors?: HistoryVersion["coAuthors"],
): string {
  if (!coAuthors || coAuthors.length === 0) {
    return author.device && author.device !== author.name
      ? `${author.name} · ${author.device}`
      : author.name;
  }
  const uniq = [...new Set([author.name, ...coAuthors.map((c) => c.name)])];
  return uniq.join("、");
}

/** 画布快照解析：`标题 · N 节点 · M 连线 · K 对话消息`；解析失败返回 null。 */
function canvasStats(content: string): string | null {
  try {
    const f = JSON.parse(content) as CanvasFile;
    if (!f || !Array.isArray(f.nodes) || !Array.isArray(f.edges)) return null;
    let messages = 0;
    for (const n of f.nodes) {
      const msgs = (n as { data?: { messages?: unknown[] } }).data?.messages;
      if (Array.isArray(msgs)) messages += msgs.length;
    }
    return `${f.title} · ${f.nodes.length} 节点 · ${f.edges.length} 连线 · ${messages} 对话消息`;
  } catch {
    return null;
  }
}

/** 表格快照解析：`标题 · N 字段 · M 行`；解析失败返回 null。 */
function tableStats(content: string): string | null {
  try {
    const f = JSON.parse(content) as TableFile;
    if (!f || !Array.isArray(f.fields) || !Array.isArray(f.rows)) return null;
    return `${f.title} · ${f.fields.length} 字段 · ${f.rows.length} 行`;
  } catch {
    return null;
  }
}

export function HistoryModal({ kind, file, open, onClose, onRollback }: Props) {
  const [versions, setVersions] = useState<HistoryVersion[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [confirmSeq, setConfirmSeq] = useState<number | null>(null);
  /** 展开预览的 seq。 */
  const [previewSeq, setPreviewSeq] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoaded(false);
    setError(null);
    const load =
      kind === "note"
        ? () => useVaultStore.getState().noteHistoryLoad(file)
        : kind === "canvas"
          ? () => useCanvasStore.getState().canvasHistoryLoad(file)
          : () => useTableStore.getState().tableHistoryLoad(file);
    void load().then((vs) => {
      if (!cancelled) {
        setVersions(vs);
        setLoaded(true);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [open, file, kind]);

  async function doRollback(seq: number) {
    setConfirmSeq(null);
    let content: string | null = null;
    try {
      if (kind === "note") content = await useVaultStore.getState().noteHistoryRollback(file, seq);
      else if (kind === "canvas") content = await useCanvasStore.getState().canvasHistoryRollback(file, seq);
      else content = await useTableStore.getState().tableHistoryRollback(file, seq);
    } catch {
      content = null;
    }
    if (content == null) {
      setError("回滚失败：目标版本内容不可用");
      return;
    }
    setError(null);
    onRollback?.(content);
    onClose();
  }

  if (!open) return null;

  const name = file.split("/").pop() ?? file;

  return (
    <div
      className="fixed inset-0 z-[90] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.4)" }}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="w-[560px] max-h-[80vh] flex flex-col rounded-lg shadow-2xl"
        style={{ background: "var(--bg-primary)", border: "1px solid var(--border)" }}
      >
        <div
          className="flex items-center gap-2 px-3 py-2 flex-shrink-0 select-none"
          style={{ borderBottom: "1px solid var(--border)" }}
        >
          <History size={15} style={{ color: "var(--accent)" }} />
          <span className="text-sm" style={{ color: "var(--text-primary)" }}>
            历史记录 · {KIND_LABEL[kind]} · {name}
          </span>
          <button
            className="ml-auto p-1 rounded hover:opacity-80"
            style={{ color: "var(--text-muted)" }}
            onClick={onClose}
          >
            <X size={15} />
          </button>
        </div>

        <div className="flex-1 overflow-auto p-2">
          {error && (
            <div className="px-2 py-1.5 mb-2 text-xs rounded" style={{ color: "#f87171", background: "rgba(248,113,113,0.1)" }}>
              {error}
            </div>
          )}
          {loaded && versions.length === 0 && (
            <div className="py-8 text-center text-xs" style={{ color: "var(--text-muted)" }}>
              暂无历史记录
            </div>
          )}
          {!loaded && (
            <div className="py-8 text-center text-xs" style={{ color: "var(--text-muted)" }}>
              加载中…
            </div>
          )}
          {/* 最新在前；每个版本带其上一版本内容（diff 基准，首版为空串） */}
          {versions
            .map((v, i) => ({ v, prev: i > 0 ? versions[i - 1].content : "" }))
            .reverse()
            .map(({ v, prev }) => (
            <div
              key={v.seq}
              className="mb-1.5 rounded border px-2 py-1.5 text-xs"
              style={{ borderColor: "var(--border)", background: "var(--input-bg)" }}
            >
              <div className="flex items-center gap-2">
                <button
                  className="p-0.5 rounded hover:opacity-80"
                  style={{ color: "var(--text-muted)" }}
                  onClick={() => setPreviewSeq(previewSeq === v.seq ? null : v.seq)}
                >
                  {previewSeq === v.seq ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                </button>
                <span className="font-medium" style={{ color: "var(--text-primary)" }}>
                  {ACTION_LABEL[v.action] ?? v.action}
                </span>
                <span style={{ color: "var(--text-muted)" }}>
                  {formatAuthors(v.author, v.coAuthors)}
                </span>
                <span className="ml-auto" style={{ color: "var(--text-muted)" }}>
                  {fmtTime(v.ts)}
                </span>
                <span style={{ color: "var(--text-secondary)" }}>
                  {v.summary ?? `${v.content.length} 字`}
                </span>
                <button
                  className="flex items-center gap-1 px-1.5 py-0.5 rounded hover:opacity-80"
                  style={{ color: "var(--accent)" }}
                  onClick={() => setConfirmSeq(v.seq)}
                >
                  <RotateCcw size={12} /> 回滚
                </button>
              </div>
              {previewSeq === v.seq && (
                <VersionPreview kind={kind} content={v.content} prevContent={prev} note={v.note} />
              )}
            </div>
          ))}
        </div>
      </div>

      {confirmSeq !== null && (
        <ConfirmDialog
          title={`回滚${KIND_LABEL[kind]}`}
          description={`确定回滚到 ${fmtTime(versions.find((v) => v.seq === confirmSeq)?.ts ?? 0)} 的版本？\n当前未保存的编辑将被覆盖，此操作会记录一条「回滚」历史。`}
          onConfirm={() => void doRollback(confirmSeq)}
          onCancel={() => setConfirmSeq(null)}
        />
      )}
    </div>
  );
}

// ===== 版本详情预览（画布/表格 = 相对上一版本的变更 diff）=====

/** 展开详情：笔记 = 全文；画布/表格 = 统计 + diff。 */
function VersionPreview({
  kind,
  content,
  prevContent,
  note,
}: {
  kind: HistoryKind;
  content: string;
  prevContent: string;
  note?: string;
}) {
  return (
    <div
      className="mt-1.5 px-2 py-1.5 rounded max-h-48 overflow-auto"
      style={{ background: "var(--input-bg)", color: "var(--text-primary)", fontSize: 11 }}
    >
      {note && <div className="mb-1" style={{ color: "var(--text-muted)" }}>{note}</div>}
      {kind === "note" ? (
        <pre className="whitespace-pre-wrap break-words">{content}</pre>
      ) : (
        <>
          <div className="mb-1" style={{ color: "var(--accent)" }}>
            {kind === "canvas" ? canvasStats(content) : tableStats(content)}
          </div>
          {kind === "canvas" ? (
            <CanvasDiffView prev={prevContent} next={content} />
          ) : (
            <TableDiffView prev={prevContent} next={content} />
          )}
        </>
      )}
    </div>
  );
}

/** 列表截断上限（超出折叠为「还有 N 项」）。 */
const MAX_DIFF_CELLS = 8;
const MAX_DIFF_ROWS = 5;

/** 表格版本 diff 渲染：字段增删/改名 → 行增删 → 单元格修改 → 顺序。 */
function TableDiffView({ prev, next }: { prev: string; next: string }) {
  const diff = useMemo(() => diffTableVersions(prev, next), [prev, next]);
  const cells = diff.cellChanges;
  const hasChanges =
    diff.addedFields.length > 0 ||
    diff.removedFields.length > 0 ||
    diff.renamedFields.length > 0 ||
    diff.addedRows.length > 0 ||
    diff.removedRows.length > 0 ||
    cells.length > 0 ||
    diff.fieldOrderChanged ||
    diff.rowOrderChanged;
  if (!prev) {
    return <div style={{ color: "var(--text-muted)" }}>初始版本快照</div>;
  }
  if (!hasChanges) {
    return <div style={{ color: "var(--text-muted)" }}>无内容变化</div>;
  }
  return (
    <div className="flex flex-col gap-0.5 leading-relaxed break-words">
      {diff.addedFields.map((f, i) => (
        <div key={i}>+ 字段「{f.name}」</div>
      ))}
      {diff.removedFields.map((f, i) => (
        <div key={i}>− 字段「{f.name}」</div>
      ))}
      {diff.renamedFields.map((f, i) => (
        <div key={i}>
          字段改名「{f.from}」→「{f.to}」
        </div>
      ))}
      {diff.addedRows.slice(0, MAX_DIFF_ROWS).map((label, i) => (
        <div key={i}>+ 行「{label}」</div>
      ))}
      {diff.addedRows.length > MAX_DIFF_ROWS && (
        <div style={{ color: "var(--text-muted)" }}>+ 还有 {diff.addedRows.length - MAX_DIFF_ROWS} 行</div>
      )}
      {diff.removedRows.slice(0, MAX_DIFF_ROWS).map((label, i) => (
        <div key={i}>− 行「{label}」</div>
      ))}
      {diff.removedRows.length > MAX_DIFF_ROWS && (
        <div style={{ color: "var(--text-muted)" }}>− 还有 {diff.removedRows.length - MAX_DIFF_ROWS} 行</div>
      )}
      {cells.slice(0, MAX_DIFF_CELLS).map((c, i) => (
        <div key={i}>
          {c.fieldName} · 第 {c.rowIndex} 行:{" "}
          <span style={{ color: "var(--text-muted)" }}>{c.from}</span>
          {" → "}
          <span style={{ color: "var(--accent)" }}>{c.to}</span>
        </div>
      ))}
      {cells.length > MAX_DIFF_CELLS && (
        <div style={{ color: "var(--text-muted)" }}>还有 {cells.length - MAX_DIFF_CELLS} 处单元格修改</div>
      )}
      {diff.fieldOrderChanged && <div>调整列顺序</div>}
      {diff.rowOrderChanged && <div>调整行顺序</div>}
    </div>
  );
}

/** 画布版本 diff 渲染：节点增删/修改 → 连线增删 → 对话消息增减。 */
function CanvasDiffView({ prev, next }: { prev: string; next: string }) {
  const diff = useMemo(() => diffCanvasVersions(prev, next), [prev, next]);
  const hasChanges =
    diff.addedNodes.length > 0 ||
    diff.removedNodes.length > 0 ||
    diff.modifiedNodes.length > 0 ||
    diff.addedEdges > 0 ||
    diff.removedEdges > 0 ||
    diff.msgDelta !== 0;
  if (!prev) {
    return <div style={{ color: "var(--text-muted)" }}>初始版本快照</div>;
  }
  if (!hasChanges) {
    return <div style={{ color: "var(--text-muted)" }}>无内容变化</div>;
  }
  const nodeLine = (prefix: string, labels: string[]) => {
    const shown = labels.slice(0, MAX_DIFF_ROWS).map((l) => `「${l}」`).join("、");
    const more = labels.length > MAX_DIFF_ROWS ? ` 等 ${labels.length} 项` : "";
    return `${prefix} ${shown}${more}`;
  };
  return (
    <div className="flex flex-col gap-0.5 leading-relaxed break-words">
      {diff.addedNodes.length > 0 && <div>{nodeLine("+ 节点", diff.addedNodes)}</div>}
      {diff.removedNodes.length > 0 && <div>{nodeLine("− 节点", diff.removedNodes)}</div>}
      {diff.modifiedNodes.length > 0 && <div>{nodeLine("修改节点", diff.modifiedNodes)}</div>}
      {diff.addedEdges > 0 && <div>+ {diff.addedEdges} 连线</div>}
      {diff.removedEdges > 0 && <div>− {diff.removedEdges} 连线</div>}
      {diff.msgDelta > 0 && <div>对话消息 +{diff.msgDelta}</div>}
      {diff.msgDelta < 0 && <div>对话消息 {diff.msgDelta}</div>}
    </div>
  );
}
