use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use merman::render::HeadlessRenderer;
use pulldown_cmark::{html, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

const DEFAULT_BIND: &str = "127.0.0.1:8000";
const USAGE: &str = "\
Usage: md-serve [--bind ADDR] [directory]

Serve a directory of Markdown files over HTTP.

Options:
  --bind ADDR  address to listen on (default: 127.0.0.1:8000)
  -h, --help   show this help text
";

fn main() -> ExitCode {
    let config = match parse_args(std::env::args().skip(1)) {
        Ok(Args::Run(config)) => config,
        Ok(Args::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match run(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(config: Config) -> Result<(), String> {
    let root = fs::canonicalize(&config.directory).map_err(|error| {
        format!(
            "failed to resolve directory {}: {error}",
            config.directory.display()
        )
    })?;

    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let listener = TcpListener::bind(config.bind)
        .map_err(|error| format!("failed to bind {}: {error}", config.bind))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to read listener address: {error}"))?;

    eprintln!("Serving {} at http://{local_addr}/", root.display());

    for connection in listener.incoming() {
        match connection {
            Ok(stream) => {
                if let Err(error) = handle_connection(stream, &root) {
                    eprintln!("request failed: {error}");
                }
            }
            Err(error) => eprintln!("connection failed: {error}"),
        }
    }

    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct Config {
    bind: SocketAddr,
    directory: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
enum Args {
    Run(Config),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut bind = DEFAULT_BIND
        .parse::<SocketAddr>()
        .expect("default bind address should parse");
    let mut directory = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Args::Help),
            "--bind" => {
                let Some(value) = args.next() else {
                    return Err("--bind requires an address".to_string());
                };
                bind = value
                    .parse()
                    .map_err(|error| format!("invalid bind address {value}: {error}"))?;
            }
            _ if arg.starts_with("--bind=") => {
                let value = arg.trim_start_matches("--bind=");
                bind = value
                    .parse()
                    .map_err(|error| format!("invalid bind address {value}: {error}"))?;
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
            _ if directory.is_some() => return Err(format!("unexpected argument: {arg}")),
            _ => directory = Some(PathBuf::from(arg)),
        }
    }

    Ok(Args::Run(Config {
        bind,
        directory: directory.unwrap_or_else(|| PathBuf::from(".")),
    }))
}

fn handle_connection(mut stream: TcpStream, root: &Path) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("failed to set read timeout: {error}"))?;

    let request = read_request(&mut stream)?;
    let response = match request {
        Request::Get { path } => response_for_path(root, &path, false),
        Request::Head { path } => response_for_path(root, &path, true),
        Request::UnsupportedMethod => Response::text(
            Status::MethodNotAllowed,
            "method not allowed\n",
            Some(("Allow", "GET, HEAD")),
        ),
        Request::Bad(message) => Response::text(Status::BadRequest, &format!("{message}\n"), None),
    };

    response.write_to(&mut stream)
}

#[derive(Debug, Eq, PartialEq)]
enum Request {
    Get { path: String },
    Head { path: String },
    UnsupportedMethod,
    Bad(String),
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();

    reader
        .read_line(&mut request_line)
        .map_err(|error| format!("failed to read request line: {error}"))?;

    if request_line.len() > 8192 {
        return Ok(Request::Bad("request line is too long".to_string()));
    }

    let request_line = request_line.trim_end_matches(['\r', '\n']);
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return Ok(Request::Bad("missing method".to_string()));
    };
    let Some(target) = parts.next() else {
        return Ok(Request::Bad("missing request target".to_string()));
    };
    let Some(version) = parts.next() else {
        return Ok(Request::Bad("missing HTTP version".to_string()));
    };

    if parts.next().is_some() {
        return Ok(Request::Bad("malformed request line".to_string()));
    }

    if !version.starts_with("HTTP/") {
        return Ok(Request::Bad("unsupported request version".to_string()));
    }

    let mut header_line = String::new();
    loop {
        header_line.clear();
        reader
            .read_line(&mut header_line)
            .map_err(|error| format!("failed to read headers: {error}"))?;

        if header_line.is_empty() || header_line == "\r\n" || header_line == "\n" {
            break;
        }

        if header_line.len() > 8192 {
            return Ok(Request::Bad("header line is too long".to_string()));
        }
    }

    let path = strip_query_and_fragment(target).to_string();

    match method {
        "GET" => Ok(Request::Get { path }),
        "HEAD" => Ok(Request::Head { path }),
        _ => Ok(Request::UnsupportedMethod),
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ResolvedTarget {
    Markdown(PathBuf),
    Static(PathBuf),
    Redirect(String),
}

fn response_for_path(root: &Path, raw_path: &str, head_only: bool) -> Response {
    match resolve_target(root, raw_path) {
        Ok(ResolvedTarget::Markdown(path)) => match fs::read_to_string(&path) {
            Ok(markdown) => {
                let title = page_title(root, &path);
                let html = markdown_to_document(&title, &markdown);
                Response::html(Status::Ok, html, head_only)
            }
            Err(error) => Response::text(
                Status::InternalServerError,
                &format!("failed to read Markdown file: {error}\n"),
                None,
            ),
        },
        Ok(ResolvedTarget::Static(path)) => match fs::read(&path) {
            Ok(body) => Response::bytes(Status::Ok, content_type(&path), body, head_only),
            Err(error) => Response::text(
                Status::InternalServerError,
                &format!("failed to read file: {error}\n"),
                None,
            ),
        },
        Ok(ResolvedTarget::Redirect(location)) => Response::redirect(location),
        Err(RouteError::BadRequest(message)) => {
            Response::text_with_head(Status::BadRequest, &message, None, head_only)
        }
        Err(RouteError::Forbidden) => {
            Response::text_with_head(Status::Forbidden, "forbidden\n", None, head_only)
        }
        Err(RouteError::NotFound) => {
            Response::text_with_head(Status::NotFound, "not found\n", None, head_only)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum RouteError {
    BadRequest(String),
    Forbidden,
    NotFound,
}

fn resolve_target(root: &Path, raw_path: &str) -> Result<ResolvedTarget, RouteError> {
    let raw_route_path = strip_query_and_fragment(raw_path);
    let route_path = decode_route_path(raw_route_path)?;
    let requested_path = route_path_to_pathbuf(&route_path)?;
    let candidate = root.join(&requested_path);

    if candidate.is_dir() {
        let directory = checked_directory(root, &candidate)?;

        if !route_path.ends_with('/') {
            return Ok(ResolvedTarget::Redirect(format!("{raw_route_path}/")));
        }

        return match find_readme(&directory) {
            Some(readme) => checked_file_target(root, &readme),
            None => Err(RouteError::NotFound),
        };
    }

    if candidate.is_file() {
        return checked_file_target(root, &candidate);
    }

    if candidate.extension().is_none() {
        for extension in ["md", "markdown", "mdown"] {
            let markdown_candidate = candidate.with_extension(extension);
            if markdown_candidate.is_file() {
                return checked_file_target(root, &markdown_candidate);
            }
        }
    }

    Err(RouteError::NotFound)
}

fn decode_route_path(raw_path: &str) -> Result<String, RouteError> {
    if !raw_path.starts_with('/') {
        return Err(RouteError::BadRequest(
            "request path must start with /".to_string(),
        ));
    }

    let bytes = percent_decode(raw_path.as_bytes())?;
    String::from_utf8(bytes).map_err(|_| {
        RouteError::BadRequest("request path must be valid UTF-8 after decoding".to_string())
    })
}

fn strip_query_and_fragment(target: &str) -> &str {
    let query_index = target.find('?').unwrap_or(target.len());
    let fragment_index = target.find('#').unwrap_or(target.len());
    &target[..query_index.min(fragment_index)]
}

fn percent_decode(input: &[u8]) -> Result<Vec<u8>, RouteError> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;

    while index < input.len() {
        match input[index] {
            b'%' => {
                let Some(high) = input.get(index + 1).copied().and_then(hex_value) else {
                    return Err(RouteError::BadRequest(
                        "request path contains invalid percent encoding".to_string(),
                    ));
                };
                let Some(low) = input.get(index + 2).copied().and_then(hex_value) else {
                    return Err(RouteError::BadRequest(
                        "request path contains invalid percent encoding".to_string(),
                    ));
                };
                output.push((high << 4) | low);
                index += 3;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }

    Ok(output)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn route_path_to_pathbuf(route_path: &str) -> Result<PathBuf, RouteError> {
    let mut path = PathBuf::new();

    for segment in route_path.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }

        if segment == ".." || segment.contains('\\') || segment.chars().any(char::is_control) {
            return Err(RouteError::Forbidden);
        }

        let segment_path = Path::new(segment);
        if segment_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(RouteError::Forbidden);
        }

        path.push(segment);
    }

    Ok(path)
}

fn checked_directory(root: &Path, path: &Path) -> Result<PathBuf, RouteError> {
    let canonical = path.canonicalize().map_err(|_| RouteError::NotFound)?;

    if !canonical.starts_with(root) {
        return Err(RouteError::Forbidden);
    }

    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(RouteError::NotFound)
    }
}

fn checked_file_target(root: &Path, path: &Path) -> Result<ResolvedTarget, RouteError> {
    let canonical = path.canonicalize().map_err(|_| RouteError::NotFound)?;

    if !canonical.starts_with(root) {
        return Err(RouteError::Forbidden);
    }

    if is_markdown_path(&canonical) {
        Ok(ResolvedTarget::Markdown(canonical))
    } else {
        Ok(ResolvedTarget::Static(canonical))
    }
}

fn find_readme(directory: &Path) -> Option<PathBuf> {
    for name in ["README.md", "README.markdown", "README.mdown"] {
        let path = directory.join(name);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut readmes = fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_readme_path(path))
        .collect::<Vec<_>>();

    readmes.sort_by(|left, right| {
        readme_rank(left)
            .cmp(&readme_rank(right))
            .then_with(|| left.file_name().cmp(&right.file_name()))
    });

    readmes.into_iter().next()
}

fn is_readme_path(path: &Path) -> bool {
    path.file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem.eq_ignore_ascii_case("readme"))
        && is_markdown_path(path)
}

fn readme_rank(path: &Path) -> usize {
    match path.extension().and_then(OsStr::to_str) {
        Some(extension) if extension.eq_ignore_ascii_case("md") => 0,
        Some(extension) if extension.eq_ignore_ascii_case("markdown") => 1,
        Some(extension) if extension.eq_ignore_ascii_case("mdown") => 2,
        _ => 3,
    }
}

fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md")
                || extension.eq_ignore_ascii_case("markdown")
                || extension.eq_ignore_ascii_case("mdown")
        })
}

fn page_title(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn markdown_to_document(title: &str, markdown: &str) -> String {
    let body = markdown_to_html(markdown);

    format!(
        "\
<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>{}</title>
<style>
:root {{ color-scheme: light dark; }}
body {{
  margin: 0 auto;
  max-width: 78ch;
  padding: 2rem;
  font-family: system-ui, -apple-system, BlinkMacSystemFont, \"Segoe UI\", sans-serif;
  line-height: 1.55;
}}
pre {{
  overflow-x: auto;
  padding: 1rem;
}}
code, pre {{
  font-family: ui-monospace, SFMono-Regular, Consolas, \"Liberation Mono\", monospace;
}}
img, svg {{
  max-width: 100%;
}}
svg {{
  height: auto;
}}
table {{
  border-collapse: collapse;
}}
td, th {{
  border: 1px solid color-mix(in srgb, CanvasText 25%, transparent);
  padding: 0.35rem 0.55rem;
}}
.mermaid-diagram {{
  margin: 1.5rem 0;
  overflow-x: auto;
}}
.mermaid-diagram-error {{
  border-left: 0.25rem solid color-mix(in srgb, CanvasText 50%, transparent);
  padding-left: 1rem;
}}
.mermaid-diagram-error pre {{
  margin-bottom: 0;
}}
</style>
</head>
<body>
{}
</body>
</html>
",
        escape_html(title),
        body
    )
}

fn markdown_to_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(markdown, options);
    let renderer = HeadlessRenderer::new();
    let events = render_mermaid_blocks(parser, &renderer);
    let mut body = String::new();
    html::push_html(&mut body, events.into_iter());

    body
}

#[derive(Debug)]
struct MermaidBlock {
    index: usize,
    source: String,
}

fn render_mermaid_blocks<'a>(
    events: impl IntoIterator<Item = Event<'a>>,
    renderer: &HeadlessRenderer,
) -> Vec<Event<'static>> {
    let mut transformed = Vec::new();
    let mut mermaid_block: Option<MermaidBlock> = None;
    let mut next_diagram_index = 1;

    for event in events {
        if mermaid_block.is_some() {
            match event {
                Event::End(TagEnd::CodeBlock) => {
                    let block = mermaid_block
                        .take()
                        .expect("checked mermaid block is present");
                    let html = render_mermaid_figure(renderer, &block.source, block.index);
                    transformed.push(Event::Html(CowStr::from(html)));
                }
                Event::Text(text) => {
                    mermaid_block
                        .as_mut()
                        .expect("checked mermaid block is present")
                        .source
                        .push_str(&text);
                }
                Event::SoftBreak | Event::HardBreak => {
                    mermaid_block
                        .as_mut()
                        .expect("checked mermaid block is present")
                        .source
                        .push('\n');
                }
                _ => {}
            }

            continue;
        }

        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if is_mermaid_code_fence(&info) =>
            {
                mermaid_block = Some(MermaidBlock {
                    index: next_diagram_index,
                    source: String::new(),
                });
                next_diagram_index += 1;
            }
            event => transformed.push(event.into_static()),
        }
    }

    transformed
}

fn is_mermaid_code_fence(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"))
}

fn render_mermaid_figure(renderer: &HeadlessRenderer, source: &str, index: usize) -> String {
    let diagram_id = format!("mermaid-diagram-{index}");

    match renderer.render_svg_resvg_safe_sync_with_diagram_id(source, &diagram_id) {
        Ok(Some(svg)) => format!("<figure class=\"mermaid-diagram\">{svg}</figure>\n"),
        Ok(None) => render_mermaid_error(source, "no Mermaid diagram detected"),
        Err(error) => render_mermaid_error(source, &error.to_string()),
    }
}

fn render_mermaid_error(source: &str, message: &str) -> String {
    format!(
        "\
<figure class=\"mermaid-diagram mermaid-diagram-error\">
<p>Mermaid rendering failed: {}</p>
<pre><code>{}</code></pre>
</figure>
",
        escape_html(message),
        escape_html(source)
    )
}

fn escape_html(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());

    for character in input.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }

    escaped
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("css") => "text/css; charset=utf-8",
        Some("gif") => "image/gif",
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("jpeg") | Some("jpg") => "image/jpeg",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, Clone, Copy)]
enum Status {
    Ok,
    BadRequest,
    Forbidden,
    NotFound,
    MethodNotAllowed,
    InternalServerError,
    TemporaryRedirect,
}

impl Status {
    fn code_and_reason(self) -> &'static str {
        match self {
            Self::Ok => "200 OK",
            Self::BadRequest => "400 Bad Request",
            Self::Forbidden => "403 Forbidden",
            Self::NotFound => "404 Not Found",
            Self::MethodNotAllowed => "405 Method Not Allowed",
            Self::InternalServerError => "500 Internal Server Error",
            Self::TemporaryRedirect => "307 Temporary Redirect",
        }
    }
}

struct Response {
    status: Status,
    content_type: &'static str,
    body: Vec<u8>,
    head_only: bool,
    extra_header: Option<(&'static str, String)>,
}

impl Response {
    fn html(status: Status, body: String, head_only: bool) -> Self {
        Self::bytes(
            status,
            "text/html; charset=utf-8",
            body.into_bytes(),
            head_only,
        )
    }

    fn text(status: Status, body: &str, extra_header: Option<(&'static str, &str)>) -> Self {
        Self::text_with_head(status, body, extra_header, false)
    }

    fn text_with_head(
        status: Status,
        body: &str,
        extra_header: Option<(&'static str, &str)>,
        head_only: bool,
    ) -> Self {
        Self {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.as_bytes().to_vec(),
            head_only,
            extra_header: extra_header.map(|(name, value)| (name, value.to_string())),
        }
    }

    fn bytes(status: Status, content_type: &'static str, body: Vec<u8>, head_only: bool) -> Self {
        Self {
            status,
            content_type,
            body,
            head_only,
            extra_header: None,
        }
    }

    fn redirect(location: String) -> Self {
        Self {
            status: Status::TemporaryRedirect,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            head_only: false,
            extra_header: Some(("Location", location)),
        }
    }

    fn write_to(self, stream: &mut TcpStream) -> Result<(), String> {
        let mut headers = String::new();
        write!(
            headers,
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.status.code_and_reason(),
            self.content_type,
            self.body.len()
        )
        .unwrap();

        if let Some((name, value)) = self.extra_header {
            write!(headers, "{name}: {value}\r\n").unwrap();
        }

        headers.push_str("\r\n");

        stream
            .write_all(headers.as_bytes())
            .and_then(|()| {
                if self.head_only {
                    Ok(())
                } else {
                    stream.write_all(&self.body)
                }
            })
            .map_err(|error| format!("failed to write response: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn defaults_to_current_directory_and_default_bind_address() {
        assert_eq!(
            parse([]),
            Ok(Args::Run(Config {
                bind: DEFAULT_BIND.parse().unwrap(),
                directory: PathBuf::from("."),
            }))
        );
    }

    #[test]
    fn accepts_bind_address_and_directory() {
        assert_eq!(
            parse(["--bind", "127.0.0.1:0", "docs"]),
            Ok(Args::Run(Config {
                bind: "127.0.0.1:0".parse().unwrap(),
                directory: PathBuf::from("docs"),
            }))
        );
    }

    #[test]
    fn accepts_equals_bind_syntax() {
        assert_eq!(
            parse(["--bind=127.0.0.1:3000"]),
            Ok(Args::Run(Config {
                bind: "127.0.0.1:3000".parse().unwrap(),
                directory: PathBuf::from("."),
            }))
        );
    }

    #[test]
    fn rejects_unknown_option() {
        assert!(parse(["--port", "3000"]).is_err());
    }

    #[test]
    fn resolves_root_to_readme() {
        let fixture = Fixture::new("readme-root");
        fixture.write("README.md", "# Home\n");

        assert_eq!(
            resolve_target(&fixture.root, "/"),
            Ok(ResolvedTarget::Markdown(fixture.root.join("README.md")))
        );
    }

    #[test]
    fn resolves_readme_variants() {
        let fixture = Fixture::new("readme-variant");
        fixture.write("readme.markdown", "# Home\n");

        assert_eq!(
            resolve_target(&fixture.root, "/"),
            Ok(ResolvedTarget::Markdown(
                fixture.root.join("readme.markdown")
            ))
        );
    }

    #[test]
    fn redirects_directory_without_trailing_slash() {
        let fixture = Fixture::new("directory-redirect");
        fixture.write("guide/README.md", "# Guide\n");

        assert_eq!(
            resolve_target(&fixture.root, "/guide"),
            Ok(ResolvedTarget::Redirect("/guide/".to_string()))
        );
    }

    #[test]
    fn resolves_directory_with_trailing_slash_to_readme() {
        let fixture = Fixture::new("directory-readme");
        fixture.write("guide/README.md", "# Guide\n");

        assert_eq!(
            resolve_target(&fixture.root, "/guide/"),
            Ok(ResolvedTarget::Markdown(
                fixture.root.join("guide/README.md")
            ))
        );
    }

    #[test]
    fn resolves_extensionless_markdown_path() {
        let fixture = Fixture::new("extensionless");
        fixture.write("guide/install.md", "# Install\n");

        assert_eq!(
            resolve_target(&fixture.root, "/guide/install"),
            Ok(ResolvedTarget::Markdown(
                fixture.root.join("guide/install.md")
            ))
        );
    }

    #[test]
    fn serves_non_markdown_files_as_static_assets() {
        let fixture = Fixture::new("static");
        fixture.write("logo.svg", "<svg></svg>\n");

        assert_eq!(
            resolve_target(&fixture.root, "/logo.svg"),
            Ok(ResolvedTarget::Static(fixture.root.join("logo.svg")))
        );
    }

    #[test]
    fn rejects_path_traversal() {
        let fixture = Fixture::new("path-traversal");

        assert_eq!(
            resolve_target(&fixture.root, "/../Cargo.toml"),
            Err(RouteError::Forbidden)
        );
        assert_eq!(
            resolve_target(&fixture.root, "/%2e%2e/Cargo.toml"),
            Err(RouteError::Forbidden)
        );
    }

    #[test]
    fn rejects_bad_percent_encoding() {
        assert_eq!(
            decode_route_path("/bad%xx"),
            Err(RouteError::BadRequest(
                "request path contains invalid percent encoding".to_string()
            ))
        );
    }

    #[test]
    fn renders_markdown_as_html_document() {
        let html = markdown_to_document("README.md", "# Hello\n\n- item\n");

        assert!(html.contains("<title>README.md</title>"));
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<li>item</li>"));
    }

    #[test]
    fn renders_mermaid_fences_as_svg_figures() {
        let html = markdown_to_html(
            "\
```mermaid
flowchart TD
    A --> B
```
",
        );

        assert!(html.contains("<figure class=\"mermaid-diagram\">"));
        assert!(html.contains("<svg"));
        assert!(!html.contains("language-mermaid"));
    }

    #[test]
    fn preserves_non_mermaid_fences_as_code_blocks() {
        let html = markdown_to_html(
            "\
```rust
fn main() {}
```
",
        );

        assert!(html.contains("<pre><code class=\"language-rust\">"));
        assert!(html.contains("fn main() {}"));
    }

    #[test]
    fn detects_mermaid_fence_language_case_insensitively() {
        assert!(is_mermaid_code_fence("mermaid"));
        assert!(is_mermaid_code_fence("Mermaid title"));
        assert!(!is_mermaid_code_fence("rust"));
        assert!(!is_mermaid_code_fence(""));
    }

    fn parse<const N: usize>(args: [&str; N]) -> Result<Args, String> {
        parse_args(args.into_iter().map(String::from))
    }

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "rust-tools-md-serve-{name}-{}-{nanos}",
                std::process::id()
            ));

            fs::create_dir_all(&root).unwrap();
            let root = root.canonicalize().unwrap();

            Self { root }
        }

        fn write(&self, path: &str, contents: &str) {
            let path = self.root.join(path);

            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }

            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }
}
