//! tmd — terminal markdown previewer, powered by carbonyl.
//!
//! Renders a markdown file to HTML, serves it over a tiny local HTTP server
//! with live-reload via SSE, and launches `carbonyl` pointed at the URL so the
//! page shows up inside your terminal.

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Router,
};
use clap::Parser as ClapParser;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pulldown_cmark::{
    html as md_html, CodeBlockKind, CowStr, Event as MdEvent, HeadingLevel, Options,
    Parser as MdParser, Tag, TagEnd,
};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tower_http::services::ServeDir;

const PAGE_TMPL: &str = include_str!("../assets/page.html");
static MMDC_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(ClapParser)]
#[command(
    name = "tmd",
    version,
    about = "Terminal markdown previewer, powered by carbonyl"
)]
struct Cli {
    /// Markdown file to preview.
    file: PathBuf,
    /// Command used to open the rendered page.
    #[arg(long, default_value = "carbonyl")]
    browser: String,
    /// Command used to render Mermaid blocks via mermaid-cli.
    #[arg(long, default_value = "mmdc")]
    mmdc: String,
    /// Only serve; do not launch the browser.
    #[arg(long)]
    no_open: bool,
    /// Bind address.
    #[arg(long, default_value = "127.0.0.1")]
    addr: String,
}

#[derive(Clone)]
struct AppState {
    md_path: Arc<PathBuf>,
    doc_root: Arc<PathBuf>,
    mmdc: Arc<String>,
    tx: broadcast::Sender<()>,
}

struct TocEntry {
    level: u32,
    id: String,
    text: String,
}

#[derive(Clone, Copy)]
enum Theme {
    Dark,
    Light,
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    fn mermaid_cli_theme(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "default",
        }
    }
}

#[derive(Clone)]
struct MermaidSvgPair {
    dark: String,
    light: String,
}

fn ansi16_rgb(idx: u8) -> (u8, u8, u8) {
    match idx {
        0 => (0x00, 0x00, 0x00),
        1 => (0x80, 0x00, 0x00),
        2 => (0x00, 0x80, 0x00),
        3 => (0x80, 0x80, 0x00),
        4 => (0x00, 0x00, 0x80),
        5 => (0x80, 0x00, 0x80),
        6 => (0x00, 0x80, 0x80),
        7 => (0xc0, 0xc0, 0xc0),
        8 => (0x80, 0x80, 0x80),
        9 => (0xff, 0x00, 0x00),
        10 => (0x00, 0xff, 0x00),
        11 => (0xff, 0xff, 0x00),
        12 => (0x00, 0x00, 0xff),
        13 => (0xff, 0x00, 0xff),
        14 => (0x00, 0xff, 0xff),
        15 => (0xff, 0xff, 0xff),
        _ => (0x00, 0x00, 0x00),
    }
}

fn is_light_ansi_color(idx: u8) -> bool {
    let (r, g, b) = ansi16_rgb(idx);
    let luminance = 0.2126 * (r as f32) + 0.7152 * (g as f32) + 0.0722 * (b as f32);
    luminance >= 140.0
}

fn detect_terminal_theme() -> Theme {
    if let Ok(value) = std::env::var("TMD_THEME") {
        match value.trim().to_ascii_lowercase().as_str() {
            "light" => return Theme::Light,
            "dark" => return Theme::Dark,
            _ => {}
        }
    }

    if let Ok(value) = std::env::var("COLORFGBG") {
        if let Some(bg) = value.rsplit(';').next().and_then(|s| s.parse::<u8>().ok()) {
            return if is_light_ansi_color(bg) {
                Theme::Light
            } else {
                Theme::Dark
            };
        }
    }

    Theme::Dark
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "section".into()
    } else {
        out
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn mermaid_cache_key(source: &str, mmdc_cmd: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    mmdc_cmd.hash(&mut hasher);
    hasher.finish()
}

fn mermaid_output_path() -> PathBuf {
    let mut path = std::env::temp_dir();
    let seq = MMDC_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.push(format!("tmd-mermaid-{}-{seq}.svg", std::process::id()));
    path
}

fn run_mmdc(source: &str, mmdc_cmd: &str, theme: Theme) -> Result<String, String> {
    let output_path = mermaid_output_path();
    let mut child = Command::new(mmdc_cmd)
        .arg("--input")
        .arg("-")
        .arg("--output")
        .arg(&output_path)
        .arg("--theme")
        .arg(theme.mermaid_cli_theme())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start `{mmdc_cmd}`: {e}"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(source.as_bytes())
            .map_err(|e| format!("failed to write Mermaid source to `{mmdc_cmd}`: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait for `{mmdc_cmd}`: {e}"))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&output_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.trim();
        return Err(if message.is_empty() {
            format!("`{mmdc_cmd}` exited with status {}", output.status)
        } else {
            format!("`{mmdc_cmd}` failed: {message}")
        });
    }

    let svg = std::fs::read_to_string(&output_path).map_err(|e| {
        let _ = std::fs::remove_file(&output_path);
        format!("failed to read `{}`: {e}", output_path.display())
    })?;
    let _ = std::fs::remove_file(&output_path);
    Ok(svg)
}

fn render_mermaid_pair(
    source: &str,
    mmdc_cmd: &str,
    cache: &mut HashMap<u64, Result<MermaidSvgPair, String>>,
) -> Result<MermaidSvgPair, String> {
    let key = mermaid_cache_key(source, mmdc_cmd);
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }

    let rendered = (|| {
        Ok(MermaidSvgPair {
            dark: run_mmdc(source, mmdc_cmd, Theme::Dark)?,
            light: run_mmdc(source, mmdc_cmd, Theme::Light)?,
        })
    })();
    cache.insert(key, rendered.clone());
    rendered
}

fn mermaid_error_html(source: &str, error: &str) -> String {
    format!(
        concat!(
            r#"<div class="tmd-mermaid-error">"#,
            r#"<p><strong>Mermaid render failed via mmdc.</strong></p>"#,
            r#"<p>{}</p>"#,
            r#"<pre><code class="language-mermaid">{}</code></pre>"#,
            r#"</div>"#
        ),
        html_escape(error),
        html_escape(source)
    )
}

fn mermaid_html(pair: &MermaidSvgPair) -> String {
    format!(
        concat!(
            r#"<div class="tmd-mermaid">"#,
            r#"<div class="tmd-mermaid-variant tmd-mermaid-dark">{}</div>"#,
            r#"<div class="tmd-mermaid-variant tmd-mermaid-light">{}</div>"#,
            r#"</div>"#
        ),
        pair.dark, pair.light
    )
}

/// Walks the markdown event stream, injecting auto-generated anchor IDs on
/// every heading that does not already have one, and collecting a TOC.
fn process_events<'a>(parser: MdParser<'a>, mmdc_cmd: &str) -> (Vec<MdEvent<'a>>, Vec<TocEntry>) {
    let mut events: Vec<MdEvent<'a>> = Vec::new();
    let mut toc: Vec<TocEntry> = Vec::new();
    let mut heading_text: Option<String> = None;
    let mut heading_start_idx: Option<usize> = None;
    let mut mermaid_block: Option<String> = None;
    let mut mermaid_cache: HashMap<u64, Result<MermaidSvgPair, String>> = HashMap::new();
    let mut id_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for ev in parser {
        if let Some(buf) = &mut mermaid_block {
            match ev {
                MdEvent::Text(t) | MdEvent::Code(t) => buf.push_str(&t),
                MdEvent::SoftBreak | MdEvent::HardBreak => buf.push('\n'),
                MdEvent::End(TagEnd::CodeBlock) => {
                    let block = mermaid_block.take().unwrap_or_default();
                    let html = match render_mermaid_pair(&block, mmdc_cmd, &mut mermaid_cache) {
                        Ok(pair) => mermaid_html(&pair),
                        Err(err) => mermaid_error_html(&block, &err),
                    };
                    events.push(MdEvent::Html(CowStr::Boxed(html.into_boxed_str())));
                }
                _ => {}
            }
            continue;
        }

        match &ev {
            MdEvent::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))
                if lang.as_ref() == "mermaid" =>
            {
                mermaid_block = Some(String::new());
            }
            MdEvent::Start(Tag::Heading { .. }) => {
                heading_text = Some(String::new());
                heading_start_idx = Some(events.len());
                events.push(ev);
            }
            MdEvent::End(TagEnd::Heading(level)) => {
                let level_num = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                if let (Some(text), Some(idx)) = (heading_text.take(), heading_start_idx.take()) {
                    if let Some(MdEvent::Start(Tag::Heading { id, .. })) = events.get_mut(idx) {
                        let base_id = id
                            .as_ref()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| slugify(&text));
                        let count = id_counts.entry(base_id.clone()).or_insert(0);
                        let final_id = if *count == 0 {
                            base_id.clone()
                        } else {
                            format!("{base_id}-{count}")
                        };
                        *count += 1;
                        *id = Some(CowStr::Boxed(final_id.clone().into_boxed_str()));
                        toc.push(TocEntry {
                            level: level_num,
                            id: final_id,
                            text,
                        });
                    }
                }
                events.push(ev);
            }
            MdEvent::Text(t) => {
                if let Some(buf) = &mut heading_text {
                    buf.push_str(t);
                }
                events.push(ev);
            }
            MdEvent::Code(t) => {
                if let Some(buf) = &mut heading_text {
                    buf.push_str(t);
                }
                events.push(ev);
            }
            _ => events.push(ev),
        }
    }
    (events, toc)
}

fn render_toc(entries: &[TocEntry]) -> String {
    // Skip h1 (normally the document title) — show h2+ in the sidebar.
    let items: Vec<&TocEntry> = entries.iter().filter(|e| e.level >= 2).collect();
    if items.is_empty() {
        return r#"<p class="tmd-toc-empty">No sections.</p>"#.into();
    }
    let min_level = items.iter().map(|e| e.level).min().unwrap_or(2);
    let mut out = String::from(r#"<ul class="tmd-toc-list">"#);
    for e in items {
        let depth = e.level - min_level;
        out.push_str(&format!(
            r##"<li class="tmd-toc-l{}" style="padding-left:{}px"><a href="#{}">{}</a></li>"##,
            e.level,
            depth * 14,
            html_escape(&e.id),
            html_escape(&e.text)
        ));
    }
    out.push_str("</ul>");
    out
}

fn render_page(md_path: &std::path::Path, mmdc_cmd: &str) -> std::io::Result<String> {
    let src = std::fs::read_to_string(md_path)?;
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_SMART_PUNCTUATION);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);
    let parser = MdParser::new_ext(&src, opts);
    let (events, toc) = process_events(parser, mmdc_cmd);
    let mut body = String::new();
    md_html::push_html(&mut body, events.into_iter());
    let toc_html = render_toc(&toc);
    let title = md_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("tmd");
    let theme = detect_terminal_theme();
    Ok(PAGE_TMPL
        .replace("{{TITLE}}", title)
        .replace("{{INITIAL_THEME}}", theme.as_str())
        .replace("{{TOC}}", &toc_html)
        .replace("{{BODY}}", &body))
}

async fn index(State(st): State<AppState>) -> Response {
    match tokio::task::spawn_blocking({
        let p = st.md_path.clone();
        let mmdc = st.mmdc.clone();
        move || render_page(&p, mmdc.as_str())
    })
    .await
    {
        Ok(Ok(html)) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            html,
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn md_intercept(
    State(st): State<AppState>,
    request: Request,
    next: middleware::Next,
) -> Response {
    if request.uri().path().ends_with(".md") {
        let rel = request.uri().path().trim_start_matches('/');
        let md_file = st.doc_root.join(rel);
        if let Ok(canonical) = md_file.canonicalize() {
            if canonical.starts_with(&*st.doc_root) {
                let mmdc = st.mmdc.clone();
                return match tokio::task::spawn_blocking(move || {
                    render_page(&canonical, mmdc.as_str())
                })
                .await
                {
                    Ok(Ok(html)) => (
                        [
                            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                            (header::CACHE_CONTROL, "no-store"),
                        ],
                        html,
                    )
                        .into_response(),
                    Ok(Err(e)) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
                    }
                    Err(e) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
                    }
                };
            }
        }
    }
    next.run(request).await
}

async fn events(
    State(st): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = BroadcastStream::new(st.tx.subscribe())
        .filter_map(|r| r.ok())
        .map(|_| Ok(Event::default().data("reload")));
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

fn spawn_watcher(md_path: PathBuf, tx: broadcast::Sender<()>) {
    std::thread::spawn(move || {
        let parent = match md_path.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };
        let (wtx, wrx) = std::sync::mpsc::channel();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(wtx) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("tmd: watcher init failed: {e}");
                return;
            }
        };
        if let Err(e) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
            eprintln!("tmd: watch failed: {e}");
            return;
        }
        let mut last = Instant::now() - Duration::from_secs(1);
        for res in wrx {
            let ev = match res {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                continue;
            }
            if !ev.paths.iter().any(|p| p == &md_path) {
                continue;
            }
            if last.elapsed() < Duration::from_millis(80) {
                continue;
            }
            last = Instant::now();
            let _ = tx.send(());
        }
    });
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let md_path = cli.file.canonicalize()?;
    let doc_root = md_path
        .parent()
        .ok_or("md file has no parent dir")?
        .to_path_buf();

    let (tx, _) = broadcast::channel::<()>(16);
    let state = AppState {
        md_path: Arc::new(md_path.clone()),
        doc_root: Arc::new(doc_root.clone()),
        mmdc: Arc::new(cli.mmdc.clone()),
        tx: tx.clone(),
    };

    let listener = tokio::net::TcpListener::bind(format!("{}:0", cli.addr)).await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://{}:{}/", cli.addr, port);

    let app = Router::new()
        .route("/", get(index))
        .route("/__tmd/events", get(events))
        .fallback_service(ServeDir::new(&doc_root))
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, md_intercept));

    eprintln!(
        "tmd: serving {} at {}",
        md_path.file_name().unwrap().to_string_lossy(),
        url
    );

    spawn_watcher(md_path.clone(), tx);

    let child = if !cli.no_open {
        match tokio::process::Command::new(&cli.browser)
            .arg(&url)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!(
                    "tmd: failed to launch {:?}: {} (server still at {})",
                    cli.browser, e, url
                );
                eprintln!("tmd: ensure `carbonyl` is installed (brew install johnsonlee/tap/carbonyl)");
                None
            }
        }
    } else {
        None
    };

    let serve = axum::serve(listener, app);

    tokio::select! {
        r = serve => { r?; }
        _ = tokio::signal::ctrl_c() => {}
        _ = wait_child(child) => {}
    }
    Ok(())
}

async fn wait_child(child: Option<tokio::process::Child>) {
    if let Some(mut c) = child {
        let _ = c.wait().await;
    } else {
        std::future::pending::<()>().await
    }
}
