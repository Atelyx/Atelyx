/**
 * vendored Cordis 核心行为锚点（`vendor/cordis` 同步/升级前先跑这条）。
 *
 * 验证核心在 vitest（node 环境、经 Vite 转换管线）可启动：typed events 声明合并、
 * 服务提供/读取/撤销、on/once、waterfall 环绕、插件注册随 fiber 卸载可逆撤销。
 * 与 kernel/loader/slotsApi 等测试的分工：那些走宿主封装，本文件只钉 vendor 自身语义。
 * 另对账 vendor/README.md 的声明：导入映射补丁的全集与上游 commit 记录存在
 * （局限：测试无法离线校验上游 commit 真伪，只校验「声明存在且补丁无越界」）。
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { Context, ReflectService } from "@atelyx/cordis";
import { describe, expect, it } from "vitest";

/** typed events 声明合并：扩展点即类型系统（本文件演示合并语法）。 */
declare module "@atelyx/cordis" {
  interface Context {
    vendorNote: { append: (text: string) => void };
  }
  interface Events {
    "vendor/emit": (msg: string) => void;
    "vendor/wf": (value: string, next: (value?: string) => string) => string;
  }
}

describe("vendored Cordis 核心行为", () => {
  it("reflect 导出就绪（审计包装 ReflectService.handler 的依赖）", () => {
    expect(typeof ReflectService.handler.get).toBe("function");
  });

  it("根 Context 启动且基础服务就绪", () => {
    const ctx = new Context();
    expect(ctx.events).toBeDefined();
    expect(ctx.registry).toBeDefined();
    expect(ctx.reflect).toBeDefined();
  });

  it("服务提供/读取 + 撤销", () => {
    const ctx = new Context();
    const dispose = ctx.provide("vendorNote", { append: () => {} });
    expect(ctx.vendorNote).toBeDefined();
    dispose();
    expect(ctx.get("vendorNote")).toBeUndefined();
  });

  it("typed event：emit/on/once", () => {
    const ctx = new Context();
    const seen: string[] = [];
    ctx.on("vendor/emit", (msg) => {
      seen.push(`on:${msg}`);
    });
    ctx.once("vendor/emit", (msg) => {
      seen.push(`once:${msg}`);
    });
    ctx.emit("vendor/emit", "a");
    ctx.emit("vendor/emit", "b");
    expect(seen).toEqual(["on:a", "once:a", "on:b"]);
  });

  it("waterfall 环绕：next 续链", () => {
    const ctx = new Context();
    ctx.on("vendor/wf", (value, next) => `[${next(value)}]`);
    const result = ctx.waterfall("vendor/wf", "v", (value) => `${value}!`);
    expect(result).toBe("[v!]");
  });

  it("插件挂载 + 注册随 fiber 卸载可逆撤销", async () => {
    const ctx = new Context();
    const seen: string[] = [];
    const fiber = ctx.plugin((c: Context) => {
      c.provide("vendorNote", { append: (text) => { seen.push(text); } });
      c.on("vendor/emit", (msg) => { seen.push(msg); });
    });
    await fiber.await();
    ctx.vendorNote.append("x");
    ctx.emit("vendor/emit", "y");
    expect(seen).toEqual(["x", "y"]);
    await fiber.dispose();
    expect(ctx.get("vendorNote")).toBeUndefined();
    ctx.emit("vendor/emit", "z");
    expect(seen).toEqual(["x", "y"]);
  });
});

describe("vendor 来源对账（vendor/README.md 声明 ↔ 实际文件全集）", () => {
  const vendorRoot = join(process.cwd(), "vendor");
  const readme = readFileSync(join(vendorRoot, "README.md"), "utf8");

  function tsFilesUnder(dir: string): string[] {
    const out: string[] = [];
    for (const entry of readdirSync(dir)) {
      const full = join(dir, entry);
      if (statSync(full).isDirectory()) out.push(...tsFilesUnder(full));
      else if (/\.tsx?$/.test(entry)) out.push(full);
    }
    return out;
  }

  it("补丁 #1 全集：cordis 源码无 cosmokit 直引，@atelyx/cosmokit 引用文件数与 README 声明一致", () => {
    const files = tsFilesUnder(join(vendorRoot, "cordis", "src"));
    const directImports = files.filter((f) => /from\s+["']cosmokit["']/.test(readFileSync(f, "utf8")));
    expect(directImports, "cosmokit 必须经 @atelyx 别名引用（README 补丁 #1）").toEqual([]);

    const mapped = files.filter((f) => /@atelyx\/cosmokit/.test(readFileSync(f, "utf8")));
    const declared = readme.match(/`cosmokit` → `@atelyx\/cosmokit`（(\d+) 处/u)?.[1];
    expect(declared, "README 补丁 #1 须声明处数").toBeDefined();
    expect(mapped.length, "映射文件数 = README 声明（改了请同步 README）").toBe(Number(declared));
  });

  it("补丁 #2 存在：index.ts 导出 ./reflect（审计包装 ReflectService 的依赖）", () => {
    const index = readFileSync(join(vendorRoot, "cordis", "src", "index.ts"), "utf8");
    expect(index).toContain("export * from './reflect'");
  });

  it("README 清单表声明了两个上游 commit（同步流程的核对依据）", () => {
    const commits = [...readme.matchAll(/`([0-9a-f]{40})`/gu)].map((m) => m[1]);
    expect(commits.length, "cordis 与 cosmokit 各声明一条 commit").toBeGreaterThanOrEqual(2);
  });
});
