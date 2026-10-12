# Vendored winit (patched copy)

Base: winit 0.30.13 (crates.io), unmodified except the changes listed below.

## Local changes

All in `src/platform_impl/windows/`:

- `event_loop.rs`: on `WM_DPICHANGED`, the custom window repositioning (cursor-ratio
  bias, monitor nudge loop) now only runs on Windows 10 (build < 22000). On Windows 11+
  the OS-suggested rectangle is applied directly. On Windows 11 the custom repositioning
  caused windows to jump / oscillate / resize when dragged onto a monitor with a
  different DPI scale factor.
  Same change as upstream rust-windowing/winit#4341 (merged upstream, unreleased in any
  0.30.x).
- `dark_mode.rs`: `WIN10_BUILD_VERSION` is `pub(crate)` (used by `event_loop.rs`).
- Non-essential upstream directories (`examples`, `tests`, `docs`, `Cargo.lock`) were
  dropped.

Remove this directory and the `[patch.crates-io]` entry in the workspace root
`Cargo.toml` once the iced dependency stack pulls a winit release that includes
upstream #4341.
