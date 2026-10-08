/**
 * 随应用分发运行时解析测试（services/bundledRuntime）：按平台命中 / 候选目录回退 / 资源缺失与
 * 元数据损坏降级 / 未分发平台返回 null。依赖全部注入（不走真实 Tauri 面）。
 */
import { describe, expect, it } from "vitest";
import { resolveBundledRuntime, type BundledRuntimeDeps } from "./bundledRuntime";

/** 构造注入依赖：candidates = 资源目录候选（按序探测）；missing = 列目录会失败的候选目录。 */
function makeDeps(opts: {
  platform: string;
  candidates: string[];
  files?: Record<string, string>;
  present?: Record<string, Array<{ name: string; kind: "dir" | "file" }>>;
  missing?: string[];
}): BundledRuntimeDeps {
  const missing = new Set(opts.missing ?? []);
  return {
    async candidateDirs() {
      return opts.candidates;
    },
    async listDir(dir: string) {
      if (missing.has(dir)) throw new Error("目录不存在");
      return { entries: opts.present?.[dir] ?? [], total: 0, capped: false };
    },
    async readFile(path: string) {
      const content = opts.files?.[path];
      if (content === undefined) throw new Error("文件不存在");
      return content;
    },
    platform: () => opts.platform,
  };
}

const VERSION_JSON = JSON.stringify({ runtime: "node", version: "24.21.0" });

describe("resolveBundledRuntime", () => {
  it("Windows 命中：二进制在列 + version.json 有效 → 返回路径与版本（剥尾部斜杠拼接）", async () => {
    const dir = "C:/app/resources/";
    const info = await resolveBundledRuntime(
      makeDeps({
        platform: "windows-x64",
        candidates: [dir],
        present: { "C:/app/resources/runtime": [{ name: "node.exe", kind: "file" }] },
        files: { "C:/app/resources/runtime/version.json": VERSION_JSON },
      }),
    );
    expect(info).toEqual({ path: "C:/app/resources/runtime/node.exe", version: "24.21.0" });
  });

  it("首候选目录缺失 → 回退下一候选命中（dev 源目录回退路径）", async () => {
    const runtimeDirA = "C:/target/debug/runtime";
    const runtimeDirB = "E:/repo/src-tauri/resources/runtime";
    const info = await resolveBundledRuntime(
      makeDeps({
        platform: "linux-x64",
        candidates: ["C:/target/debug/", "E:/repo/src-tauri/resources"],
        present: { [runtimeDirB]: [{ name: "node", kind: "file" }] },
        files: { [`${runtimeDirB}/version.json`]: VERSION_JSON },
        missing: [runtimeDirA],
      }),
    );
    expect(info).toEqual({ path: `${runtimeDirB}/node`, version: "24.21.0" });
  });

  it("目录在而二进制缺失 → 跳过该候选，全部落空返回 null", async () => {
    const dir = "C:/app/resources";
    const info = await resolveBundledRuntime(
      makeDeps({
        platform: "windows-x64",
        candidates: [dir],
        present: { [`${dir}/runtime`]: [{ name: "LICENSE", kind: "file" }] },
        files: { [`${dir}/runtime/version.json`]: VERSION_JSON },
      }),
    );
    expect(info).toBeNull();
  });

  it("version.json 损坏或 runtime 名不符 → 不采用该候选，返回 null", async () => {
    const dir = "C:/app/resources";
    for (const content of ["{oops", JSON.stringify({ runtime: "deno", version: "1" }), "{}"]) {
      const info = await resolveBundledRuntime(
        makeDeps({
          platform: "windows-x64",
          candidates: [dir],
          present: { [`${dir}/runtime`]: [{ name: "node.exe", kind: "file" }] },
          files: { [`${dir}/runtime/version.json`]: content },
        }),
      );
      expect(info, content).toBeNull();
    }
  });

  it("未分发平台（安卓）直接返回 null，不做任何资源探测", async () => {
    let listed = false;
    const deps = makeDeps({ platform: "android", candidates: ["C:/app/resources"] });
    deps.listDir = async () => {
      listed = true;
      throw new Error("不应被调用");
    };
    const info = await resolveBundledRuntime(deps);
    expect(info).toBeNull();
    expect(listed).toBe(false);
  });
});
