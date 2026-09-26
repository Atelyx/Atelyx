<div align="center" style="padding:6px 0 10px">

<a href="../README.md" style="display:inline-block;padding:4px 16px;border:1px solid #d8d8dc;border-radius:999px;color:#6b6b70;font-size:13px;line-height:1.6;text-decoration:none">简体中文 →</a>

</div>

<div align="center" style="background:#17171a;border:1px solid #2a2a2e;border-radius:16px;padding:48px 24px 40px;margin:0 0 32px">

<img src="../src-tauri/icons/icon.png" alt="Atelyx" width="92">

<h1 style="color:#E5E0D5;font-weight:700;letter-spacing:3px;margin:16px 0 10px">ATELYX</h1>

<p style="color:#D4AF37;font-size:17px;font-weight:600;margin:0 0 14px">Put conversations, notes, tables, and files into one extensible workbench — AI assistance, multi-user collaboration</p>

<p style="color:#8b8b8b;max-width:660px;margin:0 auto 30px;font-size:15px;line-height:1.8">
Atelyx is a human-first, extensible desktop workbench: conversations, notes, tables, and files in one workbench; run your own server to enable real-time multi-user collaboration. The application itself is a thin kernel — conversations, notes, tables, canvas, search, and calendar are implemented as bundled plugins that ship with the app; they can be disabled or replaced by third-party plugins. Atelyx's aim is to explore new paradigms of collaboration and work in the AI era.
</p>

<span style="background:#D4AF37;color:#1C1C1E;border-radius:999px;padding:4px 18px;font-size:13px;font-weight:700;margin:0 4px">Windows</span>
<span style="border:1px solid #5a5a5e;color:#9a9a9e;border-radius:999px;padding:3px 17px;font-size:13px;font-weight:600;margin:0 4px">Linux · Wayland</span>
<span style="border:1px solid #5a5a5e;color:#9a9a9e;border-radius:999px;padding:3px 17px;font-size:13px;font-weight:600;margin:0 4px">Apache-2.0</span>

</div>

## Design Philosophy

Day-to-day work often means switching between several tools: writing in one app, tables in another, coordination in a chat app — every switch breaks your flow. Atelyx puts the common scenarios into one workbench:

- **One workbench for common work** — conversations, notes, tables, files, and search open side by side in a single workbench; tabs dock, panels tear off into independent windows, and layouts combine freely. The built-in features are themselves plugins: they can be disabled or replaced, and the workbench form is not predetermined.
- **AI assistance everywhere** — AI is embedded in conversations, notes, tables, files, and search, ready as you work without switching to a separate AI tool. You lead the work; AI assists.
- **Reusable assets** — search results, distilled paragraphs, and pasted materials settle into reusable assets that plug into any conversation.
- **Collaboration spaces** — open a collaborative space on your own server: peers see each other in real time, co-editing a note, co-editing a canvas, co-viewing a table. The server holds the single source of truth, deployable on a LAN or over the public internet.
- **Files are the vault** — personal vaults have no database: canvases, notes, and attachments are plain local files, backup-able and Git-syncable; personal vaults and collaboration spaces sit side by side in the file panel.

## Features at a Glance

The kernel stays thin — everything bundled with the app is a plugin:

- **AI chat** — connect any OpenAI-compatible model with custom providers and keys; conversations branch into canvas nodes, with edges expressing data flow.
- **Notes** — Markdown editing with tables, math formulas, wiki links, tags, footnotes, and more; real-time collaborative editing.
- **Tables** — structured data tables with image support and real-time collaboration.
- **Canvas** — a spatial canvas where conversation branches, notes, and materials become nodes, and edges express references and outputs.
- **Files** — the vault file tree: personal vaults and collaboration spaces side by side, switched in place from the file panel.
- **Search** — web search and full-text vault search; results settle into reusable assets for any conversation.
- **Endlessly extensible** — built-in features are just the starting point: install third-party plugins from the in-app marketplace. Plugins share the same mechanism as built-ins — no privileges — and can be disabled or replace the defaults at any time. The workbench's ceiling is set by its plugins.

## Installation

Download the installer for your platform from [GitHub Releases](https://github.com/Atelyx/Atelyx/releases):

| Platform | Package |
| --- | --- |
| Windows 10/11 (x64) | `.exe` installer (in-app auto-update supported) |
| Linux (native Wayland, X11 compatible) | build from source |

Prerequisites: Node.js 20.19+, pnpm 10, Rust (stable), Tauri 2 system dependencies (see [Tauri docs](https://v2.tauri.app/start/prerequisites/)). To build from source, see "Development" below.

## Development

```bash
pnpm install         # install frontend dependencies
pnpm run tauri:dev   # start dev (Vite + Tauri window)
pnpm run tauri:build # build installers
pnpm run check       # full gate: typecheck + ESLint + frontend tests + cargo test
```

## Self-Hosting the Collaboration Server

Collaboration spaces keep their source of truth on the server (`collab-relay/`, a single Rust process) — one Linux box or NAS is enough:

```bash
# Option 1: Docker Compose (data lands in ./data; backup = copy that directory)
cd collab-relay && docker compose up -d

# Option 2: Linux + systemd (builds and registers the service automatically; data dir /var/lib/atelyx)
sudo bash collab-relay/install.sh
# Custom port / data dir: sudo bash collab-relay/install.sh 13000 /mnt/nas/atelyx-data
```

- The server listens on port `11224` by default; set `TLS_CERT` + `TLS_KEY` to enable HTTPS/WSS. More options: comments in `collab-relay/docker-compose.yml` and the header of `install.sh`.
- Once deployed, open `http://<server>:11224` in a browser for the admin console: register an account (the first registered account becomes the admin), create spaces, and invite members. Clients connect by entering the server address in the app.

## Plugin Development

Atelyx consists of a kernel and plugins: the kernel handles windows, layout, and plugin loading; capabilities are provided as plugins. The bundled plugins that ship with the app use the same mounting mechanism as third-party plugins — no privileges, and they can be disabled or replaced. The plugin kernel is built on the Cordis base (reversible effects + typed events), sharing its origin with [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness); the two differ in positioning — deepseek-harness is an Agent runtime, while Atelyx is a desktop workbench for humans.

A plugin is a Git repository: declare its entry and capabilities in the `atelyx` block of `package.json`, push it to GitHub, and tag it with the `atelyx-plugin` topic — the plugin marketplace picks it up automatically. See the [plugin development guide](docs/plugins/README.md).

## Contributing

- Report bugs / request features: [Issues](https://github.com/Atelyx/Atelyx/issues)
- Before submitting a pull request, please read [CONTRIBUTING.md](CONTRIBUTING.md) (Chinese)

## License

[Apache-2.0](../LICENSE)
