# Third-Party Notices

本项目包含或依赖以下第三方软件，许可与版权归属如下。完整传递依赖清单可用
`pnpm licenses list` 审计（本文件记录于 2026-10-03）。

## Vendored 源码

| 目录 | 许可 | 版权 | 说明 |
| --- | --- | --- | --- |
| `vendor/cordis/` | MIT | © 2021-present Shigma | Cordis 框架核心（上游 https://github.com/cordiverse/cordis） |
| `vendor/cosmokit/` | MIT | © 2021-present Shigma | Cordis 基础库（上游 https://github.com/shigma/cosmokit） |

## 运行时依赖（package.json `dependencies`）

| 包 | 许可 |
| --- | --- |
| dompurify | MPL-2.0 OR Apache-2.0 |
| @codemirror/commands / lang-markdown / language / language-data / state / view | MIT |
| @fontsource/ibm-plex-sans / ibm-plex-mono / fraunces | SIL OFL-1.1 |
| @lezer/highlight | MIT |
| @tauri-apps/api | Apache-2.0 OR MIT |
| @tauri-apps/plugin-clipboard-manager / dialog / process / shell / updater | MIT OR Apache-2.0 |
| @xyflow/react | MIT |
| esbuild-wasm | MIT |
| gray-matter | MIT |
| katex | MIT |
| lib0 | MIT |
| lucide-react | ISC |
| react / react-dom | MIT |
| y-codemirror.next / y-protocols / yjs | MIT |
| zustand | MIT |
| @standard-schema/spec | MIT |

### 随应用分发的字体

| 字体 | 许可 | 版权 |
| --- | --- | --- |
| IBM Plex Sans / IBM Plex Mono | SIL Open Font License 1.1 | © 2019 IBM Corp. |
| Fraunces | SIL Open Font License 1.1 | © 2020 The Fraunces Project Authors |

> 三者经 `@fontsource/*` 取 latin 子集 woff2，随构建产物一并分发（本地自托管，无外链请求）。
> OFL-1.1 允许自由使用、修改与再分发（含商用），要求保留版权与许可声明、衍生字体同样以 OFL
> 发布；许可全文见 `node_modules/@fontsource/*/LICENSE`。

## 开发期依赖（package.json `devDependencies`）

| 包 | 许可 |
| --- | --- |
| @tauri-apps/cli | Apache-2.0 OR MIT |
| @types/node / react / react-dom | MIT |
| @vitejs/plugin-react | MIT |
| autoprefixer / postcss / prettier / tailwindcss / globals | MIT |
| esbuild | MIT |
| eslint / eslint-plugin-react-hooks / eslint-plugin-react-refresh | MIT |
| typescript | Apache-2.0 |
| typescript-eslint | MIT |
| vite / vitest | MIT |

> `esbuild` 的原生二进制随应用再分发（`src-tauri/resources/esbuild/`，安装插件时用于把声明的 npm
> 依赖打成自包含产物）。上表许可与版权声明随该目录的 `LICENSE.md` 一并分发。


