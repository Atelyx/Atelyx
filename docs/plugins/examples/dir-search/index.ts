/**
 * 双半示例：目录全文检索。客户端半只负责界面与渲染；递归遍历与匹配整体放宿主半
 * （宿主分发的 Node 跑内嵌脚本），进度经宿主半通知回传，超 5 秒由宿主半自动收尾。
 */

const HOST_SCRIPT_VERSION = 1;

const HOST_SCRIPT = `\
import { readdir, readFile, stat } from "node:fs/promises";
import { join, relative } from "node:path";

const PROTOCOL_VERSION = 1;
const MAX_DEPTH = 12;
const MAX_FILES = 20000;
const MAX_MATCHES = 200;
const DEADLINE_MS = 5000;
const MAX_LINE_CHARS = 200;
const TEXT_EXTS = new Set([
  "md", "txt", "markdown", "ts", "tsx", "js", "jsx", "mjs", "cjs", "json",
  "css", "scss", "html", "csv", "log", "yml", "yaml", "toml", "rs", "py", "go",
]);
const SKIP_DIRS = new Set([".git", "node_modules", ".atelyx"]);

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\\n");
}

function extOf(name) {
  const i = name.lastIndexOf(".");
  return i < 0 ? "" : name.slice(i + 1).toLowerCase();
}

async function walk(dir, root, query, acc) {
  if (acc.done) return;
  if (Date.now() > acc.deadline) {
    acc.timedOut = true;
    acc.done = true;
    return;
  }
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    if (acc.done) return;
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (acc.depth >= MAX_DEPTH || SKIP_DIRS.has(entry.name) || entry.name.startsWith(".")) continue;
      acc.depth += 1;
      await walk(full, root, query, acc);
      acc.depth -= 1;
    } else if (entry.isFile() && TEXT_EXTS.has(extOf(entry.name))) {
      acc.scanned += 1;
      if (acc.scanned % 500 === 0) {
        send({ jsonrpc: "2.0", method: "progress", params: { scanned: acc.scanned } });
      }
      let content;
      try {
        const info = await stat(full);
        if (info.size > 2 * 1024 * 1024) continue;
        content = await readFile(full, "utf8");
      } catch {
        continue;
      }
      const lines = content.split("\\n");
      for (let i = 0; i < lines.length; i++) {
        const at = lines[i].toLowerCase().indexOf(query);
        if (at < 0) continue;
        const text = lines[i].trim();
        acc.matches.push({
          file: relative(root, full).split("\\\\").join("/"),
          line: i + 1,
          text: text.length > MAX_LINE_CHARS ? text.slice(0, MAX_LINE_CHARS) + "…" : text,
        });
        if (acc.matches.length >= MAX_MATCHES) {
          acc.truncated = true;
          acc.done = true;
          return;
        }
      }
    }
    if (acc.scanned >= MAX_FILES) {
      acc.truncated = true;
      acc.done = true;
    }
  }
}

const methods = {
  search: async (params) => {
    const dir = params && typeof params.dir === "string" ? params.dir : "";
    const query = params && typeof params.query === "string" ? params.query.trim().toLowerCase() : "";
    if (!dir || !query) throw new Error("需要 dir 与 query 参数");
    const started = Date.now();
    const acc = { matches: [], scanned: 0, depth: 0, deadline: started + DEADLINE_MS, done: false, timedOut: false, truncated: false };
    await walk(dir, dir, query, acc);
    return {
      matches: acc.matches,
      scannedFiles: acc.scanned,
      truncated: acc.truncated,
      timedOut: acc.timedOut,
      durationMs: Date.now() - started,
    };
  },
};

function reply(message, result) {
  send({ jsonrpc: "2.0", id: message.id, result: result === undefined ? null : result });
}

import { createInterface } from "node:readline";
import { stdin } from "node:process";

createInterface({ input: stdin }).on("line", (line) => {
  if (!line.trim()) return;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    console.error("bad line:", line.slice(0, 80));
    return;
  }
  if (message.method === "initialize") {
    reply(message, {
      protocolVersion: PROTOCOL_VERSION,
      serverInfo: { name: "dir-search-host", version: "1.0.0" },
    });
    return;
  }
  if (message.id !== undefined && message.method !== undefined) {
    const method = methods[message.method];
    if (!method) {
      send({
        jsonrpc: "2.0",
        id: message.id,
        error: { code: -32601, message: "method not found: " + message.method },
      });
      return;
    }
    Promise.resolve()
      .then(() => method(message.params))
      .then((result) => reply(message, result))
      .catch((error) =>
        send({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32603, message: String((error && error.message) || error) },
        }),
      );
  }
});
`;

function ensureHostScript(ctx: any): Promise<string> {
  return Promise.resolve(ctx).then(async (c: any) => {
    const dir = (await c.fs.privateDir()).replace(/[\\/]+$/, "");
    const path = `${dir}/dir-search-host-v${HOST_SCRIPT_VERSION}.mjs`;
    try {
      if ((await c.fs.readFile(path)) === HOST_SCRIPT) return path;
    } catch {
      // 首次或被清理：照常写入
    }
    await c.fs.writeFile(path, HOST_SCRIPT);
    return path;
  });
}

async function search(ctx: any, dir: string, query: string, onProgress: (scanned: number) => void): Promise<any> {
  const rt = await ctx.process.bundledRuntime();
  if (!rt) throw new Error("本平台未分发捆绑运行时，检索不可用");
  const script = await ensureHostScript(ctx);
  const channel = await ctx.rpc.connect({ command: rt.path, args: [script] });
  const off = channel.on("progress", (p: any) => onProgress(Number(p?.scanned ?? 0)));
  try {
    return await channel.call("search", { dir, query }, { timeoutMs: 30_000 });
  } finally {
    off();
    await channel.close();
  }
}

export default async function apply(ctx: any): Promise<void> {
  function Panel(): any {
    const [query, setQuery] = React.useState("");
    const [status, setStatus] = React.useState("输入关键词并选择目录");
    const [progress, setProgress] = React.useState<number | null>(null);
    const [result, setResult] = React.useState<any>(null);
    const [busy, setBusy] = React.useState(false);

    const run = async () => {
      const q = query.trim();
      if (busy || !q) return;
      const dir = await ctx.dialog.pickDirectory();
      if (!dir) return;
      setBusy(true);
      setResult(null);
      setProgress(0);
      setStatus(`检索 ${dir}`);
      try {
        const r = await search(ctx, dir, q, (n) => setProgress(n));
        setResult(r);
        setStatus("完成");
      } catch (e) {
        setStatus(`失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
        setProgress(null);
      }
    };

    const kids: any[] = [
      React.createElement(
        "div",
        { key: "head", style: { fontWeight: 600 } },
        "目录检索（宿主半执行）",
      ),
      React.createElement(
        "div",
        { key: "row", style: { display: "flex", gap: 8 } },
        React.createElement("input", {
          key: "input",
          value: query,
          placeholder: "关键词（大小写不敏感）",
          onChange: (e: any) => setQuery(e.target.value),
          style: { flex: 1 },
        }),
        React.createElement(
          "button",
          { key: "go", disabled: busy || !query.trim(), onClick: () => void run() },
          busy ? "检索中…" : "选择目录并检索",
        ),
      ),
      React.createElement(
        "div",
        { key: "status", style: { opacity: 0.7 } },
        progress !== null && busy ? `已扫描 ${progress} 个文件…` : status,
      ),
    ];
    if (result) {
      kids.push(
        React.createElement(
          "div",
          { key: "sum" },
          `命中 ${result.matches.length}${result.truncated ? "（已达上限，结果截断）" : ""} / 扫描 ${result.scannedFiles} 个文件 / ${result.durationMs}ms`,
        ),
        ...result.matches.map((m: any, i: number) =>
          React.createElement(
            "div",
            { key: i, style: { display: "flex", gap: 8 } },
            React.createElement(
              "span",
              { style: { opacity: 0.6, whiteSpace: "nowrap" } },
              `${m.file}:${m.line}`,
            ),
            React.createElement("span", null, m.text),
          ),
        ),
      );
    }
    return React.createElement(
      "div",
      { style: { padding: 12, display: "flex", flexDirection: "column", gap: 8, fontSize: 13 } },
      ...kids,
    );
  }

  ctx.slots.registerView({ kind: "com.atelyx.example.dir-search", label: "目录检索（双半示例）", component: Panel });
}
