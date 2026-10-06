/**
 * 外部白板格式（.canvas JSON）映射纯函数。
 *
 * 只读查看与「转换为画布」共用同一映射规则：
 * - `file` 节点：.md → 文本节点（file 引用 + bodyMd 实时读）；其他文件 → 媒体节点（图片读 thumb、文本类读 body）
 * - `text` 节点 → 画布内文本节点（bodyMd 内嵌）
 * - `group` / `link` → 分组 / 链接节点（label/color/url 透传）
 * - 边 → 无向边（`directed: false`，handle 映射为 `{side}-source/target` 对齐连接边框）
 * - 未知类型 / 端点缺失的边丢弃
 *
 * 文件内容读取通过 io 参数注入（本模块不依赖 service 层，保持纯函数可测）。
 */
import type { Node } from "@xyflow/react";
import type {
  CanvasEdge,
  GroupFileData,
  LinkFileData,
  MediaData,
  TextFileData,
  WhiteboardEdge,
  WhiteboardFile,
  WhiteboardNode,
} from "@/types";
import { prefix } from "@/utils/text";
import { baseName, noteTitleFromFile } from "@/utils/filename";
import { imageMimeFromExt, isImageFileName } from "@/utils/image";

/** 映射时读取文件内容的注入接口（services/vault 提供实现）。 */
export interface WhiteboardIo {
  readText: (file: string) => Promise<string>;
  readDataUrl: (file: string) => Promise<string>;
}

/** 按图片扩展名推 mime（仅图片；其他返回 text/plain 兜底）。 */
export function inferImageMime(name: string): string {
  const ext = name.match(/\.([a-z0-9]+)$/i)?.[1] ?? "";
  return imageMimeFromExt(ext) ?? "text/plain";
}

/** 解析 .canvas JSON（格式损坏抛错，由调用方降级提示）。 */
export function parseWhiteboard(raw: string): WhiteboardFile {
  const data = JSON.parse(raw) as Partial<WhiteboardFile>;
  return {
    nodes: Array.isArray(data.nodes) ? data.nodes : [],
    edges: Array.isArray(data.edges) ? data.edges : [],
  };
}

/**
 * 节点映射：.canvas 节点 → 运行时节点（React Flow 格式）。
 * file 节点按扩展名读内容（读失败尽力降级：正文留空/parseFailed，不阻塞整体加载）。
 */
export async function mapWhiteboardNodes(
  nodes: WhiteboardNode[],
  io: WhiteboardIo,
): Promise<Node[]> {
  const out: Node[] = [];
  for (const n of nodes) {
    const base = {
      id: n.id,
      position: { x: n.x, y: n.y },
      width: n.width,
      height: n.height,
    };
    if (n.type === "file" && n.file) {
      if (/\.md$/i.test(n.file)) {
        const data: TextFileData = {
          title: noteTitleFromFile(n.file),
          file: n.file,
        };
        let bodyMd = "";
        try {
          bodyMd = await io.readText(n.file);
        } catch {
          // 文件缺失：正文留空，文本节点保持可读（不阻塞白板展示）
        }
        out.push({
          ...base,
          type: "text",
          data: { ...data, bodyMd } as unknown as Node["data"],
        });
      } else {
        const data: MediaData = {
          file: n.file,
          mime: inferImageMime(n.file),
          kind: isImageFileName(n.file) ? "image" : "file",
          name: baseName(n.file),
        };
        if (data.kind === "image") {
          try {
            data.thumb = await io.readDataUrl(n.file);
          } catch {
            // 图片读取失败：无缩略图，仅展示占位
          }
        } else {
          try {
            data.body = await io.readText(n.file);
          } catch {
            data.parseFailed = true;
          }
        }
        out.push({
          ...base,
          type: "media",
          data: data as unknown as Node["data"],
        });
      }
    } else if (n.type === "text") {
      out.push({
        ...base,
        type: "text",
        data: {
          title: prefix(n.text ?? "", 16) || "文本",
          bodyMd: n.text ?? "",
        } as unknown as Node["data"],
      });
    } else if (n.type === "group") {
      const data: GroupFileData = { label: n.label ?? "分组", color: n.color };
      out.push({
        ...base,
        type: "group",
        zIndex: -1,
        data: data as unknown as Node["data"],
      });
    } else if (n.type === "link") {
      const data: LinkFileData = { url: n.url ?? "" };
      out.push({
        ...base,
        type: "link",
        data: data as unknown as Node["data"],
      });
    }
  }
  return out;
}

/**
 * 边映射：无向边（directed: false）+ 锚点映射。
 * 端点被跳过（未知类型/无 id）或自环的边丢弃，防 React Flow 报缺失节点错误。
 */
export function mapWhiteboardEdges(
  edges: WhiteboardEdge[],
  nodeIds: Set<string>,
): CanvasEdge[] {
  const out: CanvasEdge[] = [];
  for (const e of edges) {
    if (e.fromNode === e.toNode) continue;
    if (!nodeIds.has(e.fromNode) || !nodeIds.has(e.toNode)) continue;
    out.push({
      id: e.id,
      source: e.fromNode,
      target: e.toNode,
      sourceHandle: e.fromSide ? `${e.fromSide}-source` : undefined,
      targetHandle: e.toSide ? `${e.toSide}-target` : undefined,
      directed: false,
    });
  }
  return out;
}
