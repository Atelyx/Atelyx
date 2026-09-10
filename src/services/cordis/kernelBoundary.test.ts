/**
 * 内核路径导入守卫（静态扫描源码）。
 *
 * 断言内核启动/组合路径不依赖领域 store——「内核可脱离领域代码独立启动」的可回归证据。
 * 运行时无法观测模块图，静态读源码断言 import 说明符是唯一可靠手段。
 * 扫描口径：`from "..."` / `import "..."` / `import("...")` 三种 ESM 形式（本仓库 import 均走其中之一）。
 * 例外：宿主接线模块 stores/pluginStore.ts 不在内核路径清单内（它把领域 store 的数据源注入内核
 * ctx 服务），其领域依赖以显式清单登记——新增依赖会让本测试失败，须在此登记理由。
 */
import { readdir, readFile, stat } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const srcRoot = resolve(process.cwd(), "src");

/** 领域 store（领域功能运行时状态；内核路径不得 import）。
 *  内核自带的 store（appStore/panelStore/uiStateStore/settingsStore/collabStore/notificationStore 等）不在名单内。
 *  局限：经中间模块间接 re-export、非 ESM 的 require 不在扫描范围。 */
const DOMAIN_STORES = [
  "canvasStore",
  "tableStore",
  "chatPanelStore",
  "noteStore",
  "noteCollabStore",
  "noteUndoStore",
  "calendarStore",
  "repoHistoryStore",
] as const;

/** 内核启动/组合路径：进程启动、页面装配、服务层、内核注册表 + 内核 store。 */
const KERNEL_PATH = [
  "App.tsx",
  "pages",
  "services",
  "utils/kernelLifecycle.ts",
  "utils/vaultEvents.ts",
  "utils/collabHost.ts",
  "stores/appStore.ts",
  "stores/panelStore.ts",
  "stores/uiStateStore.ts",
  "stores/vaultStore.ts",
];

/** 宿主接线模块（显式例外）：领域 store 仅用于把数据源注入内核 ctx 服务。 */
const HOST_WIRING_EXCEPTION = "stores/pluginStore.ts";
/** 该例外当前登记的领域依赖（新增即须在此登记并说明理由）。
 *  canvasStore/tableStore/repoHistoryStore/noteStore = ctx.canvas/ctx.table/ctx.history（含笔记历史回滚）数据源。 */
const HOST_WIRING_DOMAIN_DEPS = ["canvasStore", "tableStore", "repoHistoryStore", "noteStore"];

describe("内核路径导入守卫", () => {
  it("内核路径不 import 领域 store", async () => {
    const files = (await Promise.all(KERNEL_PATH.map((entry) => sourcesUnder(resolve(srcRoot, entry))))).flat();
    expect(files.length).toBeGreaterThan(0);
    const offenders: string[] = [];
    for (const file of files) {
      for (const store of await importedDomainStores(file)) {
        offenders.push(`${relative(srcRoot, file)} → ${store}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("服务层不 import 任何状态层模块（分层：服务不依赖 store）", async () => {
    const files = await sourcesUnder(resolve(srcRoot, "services"));
    const offenders: string[] = [];
    for (const file of files) {
      for (const specifier of await importSpecifiers(file)) {
        if (specifier.startsWith("@/stores/") || specifier.startsWith("../stores/")) {
          offenders.push(`${relative(srcRoot, file)} → ${specifier}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });

  it("宿主接线模块的领域依赖限于已登记清单", async () => {
    const deps = await importedDomainStores(resolve(srcRoot, HOST_WIRING_EXCEPTION));
    expect([...deps].sort()).toEqual([...HOST_WIRING_DOMAIN_DEPS].sort());
  });
});

/** 单文件或目录下的全部非测试源码。 */
async function sourcesUnder(path: string): Promise<string[]> {
  const info = await stat(path);
  if (!info.isDirectory()) return isSource(path) ? [path] : [];
  const entries = await readdir(path, { withFileTypes: true });
  const nested = await Promise.all(
    entries.map((entry) => sourcesUnder(resolve(path, entry.name))),
  );
  return nested.flat();
}

function isSource(file: string): boolean {
  return /\.tsx?$/.test(file) && !/\.test\.tsx?$/.test(file);
}

/** 该文件 import 的领域 store 名（去重）。barrel（`@/stores`）命中即视为引入全部领域 store。 */
async function importedDomainStores(file: string): Promise<string[]> {
  const found = new Set<string>();
  for (const specifier of await importSpecifiers(file)) {
    const target = resolveSpecifier(file, specifier);
    if (!target) continue;
    if (target === resolve(srcRoot, "stores") || target === resolve(srcRoot, "stores", "index")) {
      for (const store of DOMAIN_STORES) found.add(store);
      continue;
    }
    for (const store of DOMAIN_STORES) {
      if (target === resolve(srcRoot, "stores", store)) found.add(store);
    }
  }
  return [...found];
}

/** 源码中的 ESM import 说明符。 */
async function importSpecifiers(file: string): Promise<string[]> {
  const source = await readFile(file, "utf8");
  const out: string[] = [];
  for (const pattern of [
    /\bfrom\s+["']([^"']+)["']/gu,
    /\bimport\s+["']([^"']+)["']/gu,
    /\bimport\s*\(\s*["']([^"']+)["']/gu,
  ]) {
    for (const match of source.matchAll(pattern)) out.push(match[1]!);
  }
  return out;
}

/** 说明符 → 去扩展名的绝对路径（非本仓库路径返回 null）。 */
function resolveSpecifier(fromFile: string, specifier: string): string | null {
  if (specifier.startsWith("@/")) return stripExt(resolve(srcRoot, specifier.slice(2)));
  if (specifier.startsWith("./") || specifier.startsWith("../")) {
    return stripExt(resolve(dirname(fromFile), specifier));
  }
  return null;
}

function stripExt(path: string): string {
  return path.replace(/\.tsx?$/, "");
}
