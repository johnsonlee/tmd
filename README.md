# tmd

Terminal markdown previewer, powered by [carbonyl](https://github.com/fathyb/carbonyl).

`tmd` renders a markdown file to styled HTML, serves it over a tiny local
HTTP server with live-reload, and launches `carbonyl` pointed at the URL —
so the page shows up **inside your terminal** as a real browser render,
complete with images, tables, links, and CSS.

## Install

```bash
brew tap johnsonlee/tap
brew install tmd
```

This installs `tmd` (~1 MB) and `carbonyl` (~150 MB) as a Homebrew
dependency. Single command, nothing else to configure.

## Usage

```bash
tmd README.md
```

Edit the file in another pane — the preview reloads automatically via
server-sent events.

### Flags

| Flag | Default | Description |
|---|---|---|
| `--browser <cmd>` | `carbonyl` | Command used to open the rendered page. Override to debug in Chrome: `--browser "open -a 'Google Chrome'"` |
| `--no-open` | `false` | Only start the server, do not launch a browser. Useful for connecting from another tool. |
| `--addr <host>` | `127.0.0.1` | Bind address for the HTTP server. |

## How it works

```
   +---------+      HTTP       +---------------+
   |   tmd   | <-------------> |   carbonyl    |
   |  (Rust) |   SSE reload    | (Chromium TTY)|
   +----+----+                 +---------------+
        | fsnotify
        v
   your file.md
```

- **Render**: `pulldown-cmark` converts markdown to HTML (GFM extensions:
  tables, footnotes, strikethrough, task lists, smart punctuation).
- **Serve**: `axum` serves the rendered page at `/`, static siblings
  (images, linked CSS) at `/*` from the markdown file's directory.
- **Reload**: `notify` watches the file; changes broadcast a tick on a
  `tokio::sync::broadcast` channel; the page listens via SSE and reloads.
- **Display**: `carbonyl` is a patched Chromium that renders pixels into
  terminal cells using Unicode quadrant characters and 24-bit color.

## Caveats

- **`carbonyl` upstream is frozen at v0.0.3 (2023-02-18).** There is no
  newer version. `tmd` is pinned to that release and inherits whatever
  bugs / compatibility quirks it has.
- **First install downloads ~66 MB** for the `carbonyl` bundle (mirrored
  at [johnsonlee/carbonyl](https://github.com/johnsonlee/carbonyl) for URL
  stability).
- **No server-side syntax highlighting yet.** Code blocks render with a
  neutral background; highlighting is a TODO.
- **Supported platforms** (bound by `carbonyl`'s releases):
  macOS arm64, macOS amd64, Linux arm64, Linux amd64.

## License

MIT
