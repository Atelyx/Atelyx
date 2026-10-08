/**
 * 随应用分发的脚本运行时解析（`ctx.process.bundledRuntime()` 的实现面）：落点 = 应用资源目录下
 * 的 `runtime/`，二进制与 version.json 由构建前脚本（scripts/sync-node-runtime.mjs）落位。
 *
 * 解析只读已有资源（列目录确认二进制存在 + 读 version.json 取版本），不新增 Tauri 命令；
 * 资源目录候选 = 运行时资源目录（打包版）→ 构建期注入的源码资源目录（dev 下 tauri 不把资源
 * 放到二进制旁）。全部候选不可用或平台不分发即返回 null，由插件按自己的降级口径处理。
 */
import { resourceDir } from "@tauri-apps/api/path";
import { externalListDir, externalReadFile } from "@/services/externalFs";
import { detectPlatform } from "@/utils/pluginHost";

/** dev 回退常量（vite define 注入源码资源目录绝对路径；打包版同样注入但该路径在用户机器不存在，
 *  探测失败自然跳过）。vitest 无 define，类型必须允许 undefined。 */
declare const __SOURCE_RESOURCE_DIR__: string | undefined;

/** 资源目录内的运行时子目录名（与同步脚本、bundle.resources 对齐）。 */
const RUNTIME_DIR = "runtime";

/** 平台 → 捆绑二进制名；未分发运行时的平台返回 null（安卓、未知平台等）。 */
function binaryNameFor(platform: string): string | null {
  if (platform === "windows-x64") return "node.exe";
  if (platform === "linux-x64") return "node";
  return null;
}

/** 拼资源路径：剥尾部分隔符后统一用 / 连接（Windows API 接受正斜杠，三端一致）。 */
function joinResource(dir: string, name: string): string {
  return `${dir.replace(/[\\/]+$/, "")}/${name}`;
}

/** 捆绑运行时信息：path 可直接作为 `ctx.process` 的 command。 */
export interface BundledRuntimeInfo {
  /** 可执行绝对路径。 */
  path: string;
  /** 运行时版本（version.json 记录，与二进制同批落位）。 */
  version: string;
}

/** 解析依赖（测试注入替身：假候选目录 / 假目录列表 / 假文件读取 / 假平台）。 */
export interface BundledRuntimeDeps {
  candidateDirs(): Promise<string[]>;
  listDir(dir: string): Promise<{ entries: Array<{ name: string; kind: "dir" | "file" }> }>;
  readFile(path: string): Promise<string>;
  platform(): string;
}

const defaultDeps: BundledRuntimeDeps = {
  async candidateDirs() {
    const dirs: string[] = [];
    try {
      dirs.push(await resourceDir());
    } catch {
      // 无资源目录的平台（该分支只在异常环境出现），留给下一个候选。
    }
    if (typeof __SOURCE_RESOURCE_DIR__ === "string" && __SOURCE_RESOURCE_DIR__ !== "") {
      dirs.push(__SOURCE_RESOURCE_DIR__);
    }
    return dirs;
  },
  listDir: (dir) => externalListDir(dir),
  readFile: (path) => externalReadFile(path),
  platform: () => detectPlatform(),
};

/** 解析随应用分发的脚本运行时；未分发（平台不支持或资源缺失）返回 null，不抛断。 */
export async function resolveBundledRuntime(
  deps: BundledRuntimeDeps = defaultDeps,
): Promise<BundledRuntimeInfo | null> {
  const binary = binaryNameFor(deps.platform());
  if (!binary) return null;
  for (const dir of await deps.candidateDirs()) {
    const runtimeDir = joinResource(dir, RUNTIME_DIR);
    try {
      const { entries } = await deps.listDir(runtimeDir);
      if (!entries.some((e) => e.kind === "file" && e.name === binary)) continue;
      const meta = JSON.parse(await deps.readFile(joinResource(runtimeDir, "version.json"))) as {
        runtime?: unknown;
        version?: unknown;
      };
      if (meta.runtime !== "node" || typeof meta.version !== "string" || meta.version === "") {
        continue;
      }
      return { path: joinResource(runtimeDir, binary), version: meta.version };
    } catch {
      // 该候选不可用（目录缺失 / 读取失败 / 元数据损坏）→ 换下一个；全部失败即视为未分发。
    }
  }
  return null;
}
