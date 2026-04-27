# tmd

Terminal markdown previewer, powered by [carbonyl](https://github.com/fathyb/carbonyl).

`tmd` renders a markdown file to styled HTML, serves it over a tiny local
HTTP server with live-reload, and launches `carbonyl` pointed at the URL —
so the page shows up **inside your terminal** as a real browser render,
complete with images, tables, links, and CSS.

<img alt="tmd" src="https://github.com/user-attachments/assets/469bb462-025e-4451-a576-56b33cf944e7" />


## Install

```bash
brew tap johnsonlee/tap
brew install tmd
```

This installs `tmd` (~1 MB) and `carbonyl` (~150 MB) as a Homebrew
dependency. Single command, nothing else to configure.

If you want Mermaid diagrams rendered with the same parser as `mmdc`, install
`@mermaid-js/mermaid-cli` separately so the `mmdc` executable is on `PATH`.

## Usage

```bash
tmd README.md
```

Edit the file in another pane — the preview reloads automatically via
server-sent events.

`tmd` also infers the initial page theme from your terminal when possible
via `COLORFGBG`. If detection is wrong, override it with `TMD_THEME=dark`
or `TMD_THEME=light`.

### Flags

| Flag | Default | Description |
|---|---|---|
| `--browser <cmd>` | `carbonyl` | Command used to open the rendered page. Override to debug in Chrome: `--browser "open -a 'Google Chrome'"` |
| `--mmdc <cmd>` | `mmdc` | Command used to render Mermaid fenced blocks into inline SVG. |
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
  tables, footnotes, strikethrough, task lists, smart punctuation). Mermaid
  fenced blocks are rendered server-side via `mmdc` into inline SVG.
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
- **Mermaid rendering now depends on `mmdc`.** If `mmdc` is missing or fails,
  `tmd` shows the error and the original Mermaid source block in the page.
- **No server-side syntax highlighting yet.** Code blocks render with a
  neutral background; highlighting is a TODO.
- **Supported platforms** (bound by `carbonyl`'s releases):
  macOS arm64, macOS amd64, Linux arm64, Linux amd64.

## License

MIT
