# Vendored Cordis

vendored Cordis 核心源码。只 vendor 核心：本项目无 Node 运行时，loader 的 import 机制为 Node 专属，不 vendor。

## 清单

| 目录 | 来源 | 版本 | 上游仓库 | commit |
| --- | --- | --- | --- | --- |
| `cordis/` | 原版上游 | `4.0.0-rc.7` | https://github.com/cordiverse/cordis (`packages/core`) | `56b3d4f725681cf4556c1a8695a709cc3b6eed74` |
| `cosmokit/` | 原版上游 | `1.8.1` | https://github.com/shigma/cosmokit | `02e691c5aa7f37f6e0b1cee7ee8f4a21c2e34507` |

## 导入映射

- `tsconfig.json` paths：`@atelyx/cordis` → `./vendor/cordis/src/index.ts`、`@atelyx/cosmokit` → `./vendor/cosmokit/src/index.ts`
- `vite.config.ts` / `vitest.config.ts`：`resolve.alias` 同名映射
- 命名说明：原版无 scope（`cosmokit`）；本项目以 `@atelyx/*` 自持命名（见本地补丁 #1）

## 编译边界

- `vendor/tsconfig.json`：vendored 代码按放宽边界 typecheck（对齐上游代码风格：`noImplicitAny`/`noImplicitThis`/`strictFunctionTypes`/`noUncheckedIndexedAccess`/`exactOptionalPropertyTypes`/`noImplicitOverride`/`noUnusedLocals`/`noUnusedParameters` 关）；业务代码不受此边界影响
- 构建：`pnpm run build:vendor`（生成 `.d.ts` 到 `vendor/.tsbuild/`，供根项目引用 redirect）；并入 `pnpm run check`
- lint 排除 `vendor`

## 本地补丁清单

1. `cordis/src/*.ts`：`cosmokit` → `@atelyx/cosmokit`（8 处；机械替换，命名说明见上）。
2. `cordis/src/index.ts`：补 `export * from './reflect'`（原版索引未导出 reflect；审计机制需包装 `ReflectService.handler`）。该导出由 `src/services/cordis/vendorCore.test.ts` 守卫——删掉即测试红。其余无。

## 同步流程

1. 上游 checkout 记录 `git rev-parse HEAD`（`cordiverse/cordis` `packages/core` 与 `shigma/cosmokit`）
2. 复制 `src/` 覆盖本目录
3. 重打本地补丁清单（或上游已吸收则删除对应条目）
4. 更新清单表的版本号与 commit
5. `pnpm run build:vendor && pnpm run check`
