use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

const DEFAULT_OUTPUT: &str = "md-build.html";
const USAGE: &str = "\
Usage: md-build [-o FILE|--output FILE] [directory]

Build a directory of Markdown files into a self-contained HTML SPA.

Options:
  -o, --output FILE  output HTML file (default: md-build.html)
  -h, --help         show this help text
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
        Ok(summary) => {
            eprintln!(
                "Built {} from {} pages and {} assets",
                summary.output.display(),
                summary.pages,
                summary.assets
            );
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(config: Config) -> Result<BuildSummary, String> {
    let root = fs::canonicalize(&config.directory).map_err(|error| {
        format!(
            "failed to resolve directory {}: {error}",
            config.directory.display()
        )
    })?;

    if !root.is_dir() {
        return Err(format!("not a directory: {}", root.display()));
    }

    let output = absolute_path(&config.output)?;
    let site = build_site(&root, Some(&output))?;
    let document = site_to_document(&site);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create output directory {}: {error}",
                parent.display()
            )
        })?;
    }

    fs::write(&output, document)
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;

    Ok(BuildSummary {
        output,
        pages: site.pages.len(),
        assets: site.assets.len(),
    })
}

#[derive(Debug, Eq, PartialEq)]
struct BuildSummary {
    output: PathBuf,
    pages: usize,
    assets: usize,
}

#[derive(Debug, Eq, PartialEq)]
struct Config {
    directory: PathBuf,
    output: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
enum Args {
    Run(Config),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut directory = None;
    let mut output = PathBuf::from(DEFAULT_OUTPUT);
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Args::Help),
            "-o" | "--output" => {
                let Some(value) = args.next() else {
                    return Err(format!("{arg} requires a file path"));
                };
                output = PathBuf::from(value);
            }
            _ if arg.starts_with("--output=") => {
                output = PathBuf::from(arg.trim_start_matches("--output="));
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
            _ if directory.is_some() => return Err(format!("unexpected argument: {arg}")),
            _ => directory = Some(PathBuf::from(arg)),
        }
    }

    Ok(Args::Run(Config {
        directory: directory.unwrap_or_else(|| PathBuf::from(".")),
        output,
    }))
}

#[derive(Debug, Eq, PartialEq)]
struct Site {
    pages: BTreeMap<String, Page>,
    routes: BTreeMap<String, String>,
    assets: BTreeMap<String, Asset>,
}

#[derive(Debug, Eq, PartialEq)]
struct Page {
    title: String,
    source: String,
    html: String,
}

#[derive(Debug, Eq, PartialEq)]
struct Asset {
    content_type: &'static str,
    data_url: String,
}

fn build_site(root: &Path, output: Option<&Path>) -> Result<Site, String> {
    let files = collect_files(root, output)?;
    let mut markdown_files = Vec::new();
    let mut asset_files = Vec::new();

    for file in files {
        let relative = file
            .strip_prefix(root)
            .map_err(|error| format!("failed to relativize {}: {error}", file.display()))?
            .to_path_buf();

        if is_markdown_path(&relative) {
            markdown_files.push(relative);
        } else {
            asset_files.push(relative);
        }
    }

    if markdown_files.is_empty() {
        return Err(format!("no Markdown files found in {}", root.display()));
    }

    markdown_files.sort_by(|left, right| {
        markdown_collision_key(left)
            .cmp(&markdown_collision_key(right))
            .then_with(|| markdown_rank(left).cmp(&markdown_rank(right)))
            .then_with(|| left.cmp(right))
    });
    asset_files.sort();

    let mut page_routes = BTreeMap::new();
    let mut primary_routes = BTreeSet::new();
    let mut routes = BTreeMap::new();

    for relative in &markdown_files {
        let canonical_route = canonical_page_route(relative)?;
        let route = if primary_routes.insert(canonical_route.clone()) {
            canonical_route
        } else {
            path_to_route(relative)?
        };

        page_routes.insert(relative.clone(), route.clone());

        for alias in page_route_aliases(relative, &route)? {
            routes.entry(alias).or_insert_with(|| route.clone());
        }
    }

    let mut assets_by_file = BTreeMap::new();
    let mut assets = BTreeMap::new();

    for relative in &asset_files {
        let route = path_to_route(relative)?;
        let path = root.join(relative);
        let bytes = fs::read(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let content_type = content_type(relative);
        let asset = Asset {
            content_type,
            data_url: data_url(content_type, &bytes),
        };

        assets_by_file.insert(relative.clone(), asset.data_url.clone());
        assets.insert(route, asset);
    }

    let resolver = LinkResolver {
        page_routes: &page_routes,
        asset_urls: &assets_by_file,
    };
    let mut pages = BTreeMap::new();

    for relative in &markdown_files {
        let path = root.join(relative);
        let markdown = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read Markdown file {}: {error}", path.display()))?;
        let route = page_routes
            .get(relative)
            .expect("page route should exist for each Markdown file")
            .clone();
        let title = page_title(relative);
        let html = markdown_to_html(&markdown, relative, &route, &resolver);

        pages.entry(route).or_insert(Page {
            title,
            source: path_to_display_string(relative)?,
            html,
        });
    }

    Ok(Site {
        pages,
        routes,
        assets,
    })
}

fn collect_files(root: &Path, output: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files_from(
        root,
        output.map(normalize_absolute_path).as_ref(),
        &mut files,
    )?;
    files.sort();
    Ok(files)
}

fn collect_files_from(
    directory: &Path,
    output: Option<&PathBuf>,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to read directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "failed to read directory entry in {}: {error}",
                directory.display()
            )
        })?;

    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to read file type for {}: {error}", path.display()))?;

        if output.is_some_and(|output| normalize_absolute_path(&path) == *output) {
            continue;
        }

        if file_type.is_dir() {
            collect_files_from(&path, output, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to read current directory: {error}"))?
            .join(path)
    };

    Ok(normalize_absolute_path(&path))
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }

    normalized
}

fn canonical_page_route(path: &Path) -> Result<String, String> {
    if is_readme_path(path) {
        return directory_route(path.parent().unwrap_or_else(|| Path::new("")));
    }

    path_to_route(&path_without_extension(path))
}

fn page_route_aliases(path: &Path, route: &str) -> Result<Vec<String>, String> {
    let mut aliases = vec![route.to_string(), path_to_route(path)?];

    if let Some(extensionless) = markdown_extensionless_route(path)? {
        aliases.push(extensionless);
    }

    if is_readme_path(path) {
        let directory = path.parent().unwrap_or_else(|| Path::new(""));
        let directory_route = directory_route(directory)?;

        aliases.push(directory_route.clone());

        if directory_route != "/" {
            aliases.push(directory_route.trim_end_matches('/').to_string());
        }
    }

    aliases.sort();
    aliases.dedup();
    Ok(aliases)
}

fn markdown_extensionless_route(path: &Path) -> Result<Option<String>, String> {
    if path.extension().is_some() {
        Ok(Some(path_to_route(&path_without_extension(path))?))
    } else {
        Ok(None)
    }
}

fn path_without_extension(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    path.set_extension("");
    path
}

fn markdown_collision_key(path: &Path) -> PathBuf {
    if is_readme_path(path) {
        return path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("__readme__");
    }

    path_without_extension(path)
}

fn markdown_rank(path: &Path) -> usize {
    match path.extension().and_then(OsStr::to_str) {
        Some(extension) if extension.eq_ignore_ascii_case("md") => 0,
        Some(extension) if extension.eq_ignore_ascii_case("markdown") => 1,
        Some(extension) if extension.eq_ignore_ascii_case("mdown") => 2,
        _ => 3,
    }
}

fn directory_route(path: &Path) -> Result<String, String> {
    let mut route = path_to_route(path)?;

    if route != "/" && !route.ends_with('/') {
        route.push('/');
    }

    Ok(route)
}

fn path_to_route(path: &Path) -> Result<String, String> {
    let mut route = String::from("/");
    let mut first = true;

    for component in path.components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        let segment = segment
            .to_str()
            .ok_or_else(|| format!("path contains invalid UTF-8: {}", path.display()))?;

        if first {
            first = false;
        } else {
            route.push('/');
        }

        route.push_str(segment);
    }

    Ok(route)
}

fn path_to_display_string(path: &Path) -> Result<String, String> {
    let mut display = String::new();
    let mut first = true;

    for component in path.components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        let segment = segment
            .to_str()
            .ok_or_else(|| format!("path contains invalid UTF-8: {}", path.display()))?;

        if first {
            first = false;
        } else {
            display.push('/');
        }

        display.push_str(segment);
    }

    Ok(display)
}

struct LinkResolver<'a> {
    page_routes: &'a BTreeMap<PathBuf, String>,
    asset_urls: &'a BTreeMap<PathBuf, String>,
}

impl LinkResolver<'_> {
    fn rewrite_link(&self, source: &Path, source_route: &str, destination: &str) -> Option<String> {
        if is_external_reference(destination) {
            return None;
        }

        let (path, suffix) = split_reference(destination);

        if path.is_empty() {
            return suffix
                .starts_with('#')
                .then(|| format!("#{source_route}{suffix}"));
        }

        let target = resolve_reference_path(source, path)?;

        if let Some(route) = self.page_route_for_reference(&target) {
            return Some(format!("#{route}{suffix}"));
        }

        self.asset_urls
            .get(&target)
            .map(|data_url| format!("{data_url}{suffix}"))
    }

    fn rewrite_image(&self, source: &Path, destination: &str) -> Option<String> {
        if is_external_reference(destination) {
            return None;
        }

        let (path, suffix) = split_reference(destination);
        let target = resolve_reference_path(source, path)?;

        self.asset_urls
            .get(&target)
            .map(|data_url| format!("{data_url}{suffix}"))
    }

    fn page_route_for_reference(&self, target: &Path) -> Option<&str> {
        if let Some(route) = self.page_routes.get(target) {
            return Some(route);
        }

        if target.extension().is_none() {
            for extension in ["md", "markdown", "mdown"] {
                let markdown_target = target.with_extension(extension);

                if let Some(route) = self.page_routes.get(&markdown_target) {
                    return Some(route);
                }
            }

            for readme in ["README.md", "README.markdown", "README.mdown"] {
                let readme_target = target.join(readme);

                if let Some(route) = self.page_routes.get(&readme_target) {
                    return Some(route);
                }
            }
        }

        None
    }
}

fn split_reference(destination: &str) -> (&str, &str) {
    let query_index = destination.find('?').unwrap_or(destination.len());
    let fragment_index = destination.find('#').unwrap_or(destination.len());
    let split_index = query_index.min(fragment_index);

    (&destination[..split_index], &destination[split_index..])
}

fn is_external_reference(destination: &str) -> bool {
    if destination.starts_with("//") {
        return true;
    }

    let path = split_reference(destination).0;
    has_url_scheme(path)
}

fn has_url_scheme(path: &str) -> bool {
    let mut chars = path.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    if !first.is_ascii_alphabetic() {
        return false;
    }

    for character in chars {
        match character {
            ':' => return true,
            'a'..='z' | 'A'..='Z' | '0'..='9' | '+' | '-' | '.' => {}
            _ => return false,
        }
    }

    false
}

fn resolve_reference_path(source: &Path, reference: &str) -> Option<PathBuf> {
    let decoded = percent_decode_to_string(reference).ok()?;
    let reference = Path::new(decoded.trim_start_matches('/'));
    let mut path = if reference.is_absolute() || decoded.starts_with('/') {
        PathBuf::new()
    } else {
        source
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf()
    };

    path.push(reference);
    normalize_relative_path(&path)
}

fn normalize_relative_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => normalized.push(segment),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(normalized)
}

fn percent_decode_to_string(input: &str) -> Result<String, String> {
    let bytes = percent_decode(input.as_bytes())?;

    String::from_utf8(bytes)
        .map_err(|_| "reference path must be valid UTF-8 after decoding".to_string())
}

fn percent_decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;

    while index < input.len() {
        match input[index] {
            b'%' => {
                let Some(high) = input.get(index + 1).copied().and_then(hex_value) else {
                    return Err("reference path contains invalid percent encoding".to_string());
                };
                let Some(low) = input.get(index + 2).copied().and_then(hex_value) else {
                    return Err("reference path contains invalid percent encoding".to_string());
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

fn markdown_to_html(
    markdown: &str,
    source: &Path,
    source_route: &str,
    resolver: &LinkResolver<'_>,
) -> String {
    let parser = Parser::new_ext(markdown, markdown_options()).map(|event| match event {
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let dest_url = resolver
                .rewrite_link(source, source_route, &dest_url)
                .map(CowStr::from)
                .unwrap_or(dest_url);

            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            })
        }
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            let dest_url = resolver
                .rewrite_image(source, &dest_url)
                .map(CowStr::from)
                .unwrap_or(dest_url);

            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            })
        }
        _ => event,
    });
    let mut body = String::new();

    html::push_html(&mut body, parser);
    body
}

fn markdown_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options
}

fn page_title(path: &Path) -> String {
    if is_readme_path(path) {
        let parent = path.parent().unwrap_or_else(|| Path::new(""));

        if parent.as_os_str().is_empty() {
            return "Home".to_string();
        }

        return path_to_display_string(parent).unwrap_or_else(|_| parent.display().to_string());
    }

    path.file_stem()
        .and_then(OsStr::to_str)
        .map_or_else(|| path.display().to_string(), ToString::to_string)
}

fn is_readme_path(path: &Path) -> bool {
    path.file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem.eq_ignore_ascii_case("readme"))
        && is_markdown_path(path)
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

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("css") => "text/css;charset=utf-8",
        Some("gif") => "image/gif",
        Some("html") | Some("htm") => "text/html;charset=utf-8",
        Some("jpeg") | Some("jpg") => "image/jpeg",
        Some("js") => "text/javascript;charset=utf-8",
        Some("json") => "application/json;charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain;charset=utf-8",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    }
}

fn data_url(content_type: &str, bytes: &[u8]) -> String {
    format!("data:{content_type};base64,{}", base64_encode(bytes))
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);

        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);

        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }

        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0b0011_1111) as usize] as char);
        } else {
            output.push('=');
        }
    }

    output
}

fn site_to_document(site: &Site) -> String {
    let mut document = String::new();

    write!(
        document,
        "\
<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Markdown Build</title>
<style>
{}
</style>
</head>
<body>
<div class=\"shell\">
  <aside class=\"sidebar\">
    <div class=\"brand\">Docs</div>
    <nav id=\"page-list\" aria-label=\"Pages\"></nav>
  </aside>
  <main class=\"content\" id=\"content\" tabindex=\"-1\"></main>
</div>
<script>
",
        APP_CSS
    )
    .unwrap();

    write_site_data(&mut document, site);
    document.push_str(APP_JS);
    document.push_str(
        "\
</script>
</body>
</html>
",
    );

    document
}

fn write_site_data(output: &mut String, site: &Site) {
    output.push_str("const MD_BUILD = {\n  pages: {\n");

    for (route, page) in &site.pages {
        write!(
            output,
            "    {}: {{ title: {}, source: {}, html: {} }},\n",
            js_string(route),
            js_string(&page.title),
            js_string(&page.source),
            js_string(&page.html)
        )
        .unwrap();
    }

    output.push_str("  },\n  routes: {\n");

    for (alias, route) in &site.routes {
        writeln!(output, "    {}: {},", js_string(alias), js_string(route)).unwrap();
    }

    output.push_str("  },\n  assets: {\n");

    for (route, asset) in &site.assets {
        write!(
            output,
            "    {}: {{ contentType: {}, dataUrl: {} }},\n",
            js_string(route),
            js_string(asset.content_type),
            js_string(&asset.data_url)
        )
        .unwrap();
    }

    output.push_str("  }\n};\n");
}

fn js_string(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len() + 2);

    escaped.push('"');

    for character in input.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            character if character.is_control() => {
                write!(escaped, "\\u{:04x}", character as u32).unwrap();
            }
            character => escaped.push(character),
        }
    }

    escaped.push('"');
    escaped
}

const APP_CSS: &str = r#":root {
  color-scheme: light dark;
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  line-height: 1.55;
}

* {
  box-sizing: border-box;
}

body {
  margin: 0;
  background: Canvas;
  color: CanvasText;
}

.shell {
  display: grid;
  grid-template-columns: minmax(13rem, 18rem) minmax(0, 1fr);
  min-height: 100vh;
}

.sidebar {
  border-right: 1px solid color-mix(in srgb, CanvasText 14%, transparent);
  padding: 1rem;
  position: sticky;
  top: 0;
  align-self: start;
  height: 100vh;
  overflow-y: auto;
}

.brand {
  font-size: 0.78rem;
  font-weight: 700;
  letter-spacing: 0;
  margin: 0 0 1rem;
  text-transform: uppercase;
}

.page-link {
  border-radius: 6px;
  color: inherit;
  display: block;
  overflow-wrap: anywhere;
  padding: 0.45rem 0.55rem;
  text-decoration: none;
}

.page-link:hover,
.page-link[aria-current="page"] {
  background: color-mix(in srgb, CanvasText 10%, transparent);
}

.content {
  margin: 0 auto;
  max-width: 84ch;
  min-width: 0;
  padding: 2.25rem;
  width: 100%;
}

.content:focus {
  outline: none;
}

.content img {
  max-width: 100%;
}

.content pre {
  background: color-mix(in srgb, CanvasText 8%, transparent);
  border-radius: 6px;
  overflow-x: auto;
  padding: 1rem;
}

.content code,
.content pre {
  font-family: ui-monospace, SFMono-Regular, Consolas, "Liberation Mono", monospace;
}

.content table {
  border-collapse: collapse;
  display: block;
  overflow-x: auto;
}

.content td,
.content th {
  border: 1px solid color-mix(in srgb, CanvasText 22%, transparent);
  padding: 0.35rem 0.55rem;
}

.asset-preview img {
  display: block;
  margin-block: 1rem;
}

@media (max-width: 760px) {
  .shell {
    display: block;
  }

  .sidebar {
    border-bottom: 1px solid color-mix(in srgb, CanvasText 14%, transparent);
    border-right: 0;
    height: auto;
    position: static;
  }

  #page-list {
    display: flex;
    gap: 0.35rem;
    overflow-x: auto;
    padding-bottom: 0.2rem;
  }

  .page-link {
    flex: 0 0 auto;
    white-space: nowrap;
  }

  .content {
    padding: 1.25rem;
  }
}
"#;

const APP_JS: &str = r##"
const pageList = document.getElementById("page-list");
const content = document.getElementById("content");

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (character) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    "\"": "&quot;",
    "'": "&#39;",
  }[character]));
}

function parseHash() {
  let hash = window.location.hash.slice(1) || "/";
  let fragment = "";
  const fragmentIndex = hash.indexOf("#");

  if (fragmentIndex >= 0) {
    fragment = hash.slice(fragmentIndex + 1);
    hash = hash.slice(0, fragmentIndex);
  }

  const queryIndex = hash.indexOf("?");

  if (queryIndex >= 0) {
    hash = hash.slice(0, queryIndex);
  }

  try {
    hash = decodeURI(hash);
  } catch (_) {
    hash = "/";
  }

  if (!hash.startsWith("/")) {
    hash = "/" + hash;
  }

  hash = hash.replace(/\/{2,}/g, "/");

  return { path: hash || "/", fragment };
}

function resolveRoute(path) {
  if (MD_BUILD.routes[path]) {
    return MD_BUILD.routes[path];
  }

  if (path.length > 1 && MD_BUILD.routes[path + "/"]) {
    return MD_BUILD.routes[path + "/"];
  }

  return null;
}

function renderNav(activeRoute) {
  pageList.textContent = "";

  for (const [route, page] of Object.entries(MD_BUILD.pages)) {
    const link = document.createElement("a");
    link.className = "page-link";
    link.href = "#" + route;
    link.textContent = page.title || route;

    if (route === activeRoute) {
      link.setAttribute("aria-current", "page");
    }

    pageList.appendChild(link);
  }
}

function renderPage(route, fragment) {
  const page = MD_BUILD.pages[route];

  document.title = page.title;
  content.innerHTML = page.html;
  renderNav(route);
  content.focus({ preventScroll: true });

  if (fragment) {
    const target = document.getElementById(fragment) || document.querySelector(`[name="${CSS.escape(fragment)}"]`);

    if (target) {
      target.scrollIntoView();
      return;
    }
  }

  window.scrollTo(0, 0);
}

function renderAsset(path) {
  const asset = MD_BUILD.assets[path];
  const name = path.split("/").filter(Boolean).pop() || "download";
  const preview = asset.contentType.startsWith("image/")
    ? `<img src="${asset.dataUrl}" alt="">`
    : "";

  document.title = name;
  content.innerHTML = `
    <section class="asset-preview">
      <h1>${escapeHtml(name)}</h1>
      <p><a href="${asset.dataUrl}" download="${escapeHtml(name)}">Open embedded file</a></p>
      ${preview}
    </section>
  `;
  renderNav("");
  content.focus({ preventScroll: true });
}

function renderNotFound(path) {
  document.title = "Not found";
  content.innerHTML = `<h1>Not found</h1><p>No embedded page or asset matches <code>${escapeHtml(path)}</code>.</p>`;
  renderNav("");
  content.focus({ preventScroll: true });
}

function renderCurrentRoute() {
  const { path, fragment } = parseHash();
  const route = resolveRoute(path);

  if (route) {
    renderPage(route, fragment);
  } else if (MD_BUILD.assets[path]) {
    renderAsset(path);
  } else {
    renderNotFound(path);
  }
}

document.addEventListener("click", (event) => {
  const link = event.target.closest("a[href]");

  if (!link || event.defaultPrevented || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
    return;
  }

  const href = link.getAttribute("href");

  if (!href || href.startsWith("#/") || href.startsWith("data:")) {
    return;
  }

  if (href.startsWith("#")) {
    event.preventDefault();
    const { path } = parseHash();
    window.location.hash = `${path}${href}`;
  }
});

window.addEventListener("hashchange", renderCurrentRoute);
renderCurrentRoute();
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn defaults_to_current_directory_and_default_output() {
        assert_eq!(
            parse([]),
            Ok(Args::Run(Config {
                directory: PathBuf::from("."),
                output: PathBuf::from(DEFAULT_OUTPUT),
            }))
        );
    }

    #[test]
    fn accepts_output_and_directory() {
        assert_eq!(
            parse(["--output", "site.html", "docs"]),
            Ok(Args::Run(Config {
                directory: PathBuf::from("docs"),
                output: PathBuf::from("site.html"),
            }))
        );
    }

    #[test]
    fn accepts_short_output_option() {
        assert_eq!(
            parse(["-o", "site.html"]),
            Ok(Args::Run(Config {
                directory: PathBuf::from("."),
                output: PathBuf::from("site.html"),
            }))
        );
    }

    #[test]
    fn accepts_equals_output_syntax() {
        assert_eq!(
            parse(["--output=public/index.html"]),
            Ok(Args::Run(Config {
                directory: PathBuf::from("."),
                output: PathBuf::from("public/index.html"),
            }))
        );
    }

    #[test]
    fn rejects_unknown_option() {
        assert!(parse(["--bind", "127.0.0.1:0"]).is_err());
    }

    #[test]
    fn builds_readme_as_root_route() {
        let fixture = Fixture::new("readme-root");
        fixture.write("README.md", "# Home\n");

        let site = build_site(&fixture.root, None).unwrap();

        assert!(site.pages.contains_key("/"));
        assert_eq!(site.routes.get("/"), Some(&"/".to_string()));
        assert_eq!(site.routes.get("/README.md"), Some(&"/".to_string()));
    }

    #[test]
    fn builds_nested_readme_and_extensionless_page_routes() {
        let fixture = Fixture::new("nested-routes");
        fixture.write("README.md", "# Home\n");
        fixture.write("guide/README.md", "# Guide\n");
        fixture.write("guide/install.md", "# Install\n");

        let site = build_site(&fixture.root, None).unwrap();

        assert!(site.pages.contains_key("/guide/"));
        assert!(site.pages.contains_key("/guide/install"));
        assert_eq!(site.routes.get("/guide"), Some(&"/guide/".to_string()));
        assert_eq!(
            site.routes.get("/guide/install.md"),
            Some(&"/guide/install".to_string())
        );
    }

    #[test]
    fn keeps_exact_routes_for_markdown_files_with_colliding_canonical_routes() {
        let fixture = Fixture::new("colliding-routes");
        fixture.write("README.md", "# Primary\n");
        fixture.write("README.markdown", "# Alternate\n");

        let site = build_site(&fixture.root, None).unwrap();

        assert!(site.pages.contains_key("/"));
        assert!(site.pages.contains_key("/README.markdown"));
        assert_eq!(site.routes.get("/"), Some(&"/".to_string()));
        assert_eq!(
            site.routes.get("/README.markdown"),
            Some(&"/README.markdown".to_string())
        );
    }

    #[test]
    fn embeds_static_assets_as_data_urls() {
        let fixture = Fixture::new("static-assets");
        fixture.write("README.md", "![Logo](logo.svg)\n");
        fixture.write("logo.svg", "<svg></svg>\n");

        let site = build_site(&fixture.root, None).unwrap();
        let page = site.pages.get("/").unwrap();

        assert_eq!(
            site.assets.get("/logo.svg").map(|asset| asset.content_type),
            Some("image/svg+xml")
        );
        assert!(page.html.contains("src=\"data:image/svg+xml;base64,"));
    }

    #[test]
    fn rewrites_markdown_links_to_hash_routes() {
        let fixture = Fixture::new("link-routes");
        fixture.write("README.md", "[Install](guide/install.md)\n");
        fixture.write("guide/install.md", "# Install\n");

        let site = build_site(&fixture.root, None).unwrap();
        let page = site.pages.get("/").unwrap();

        assert!(page.html.contains("href=\"#/guide/install\""));
    }

    #[test]
    fn rewrites_anchor_links_to_current_route() {
        let fixture = Fixture::new("anchor-routes");
        fixture.write("README.md", "[Section](#section)\n");

        let site = build_site(&fixture.root, None).unwrap();
        let page = site.pages.get("/").unwrap();

        assert!(page.html.contains("href=\"#/#section\""));
    }

    #[test]
    fn skips_output_file_when_building_from_same_directory() {
        let fixture = Fixture::new("skip-output");
        let output = fixture.root.join("md-build.html");
        fixture.write("README.md", "# Home\n");
        fixture.write("md-build.html", "previous build");

        let site = build_site(&fixture.root, Some(&output)).unwrap();

        assert!(!site.assets.contains_key("/md-build.html"));
    }

    #[test]
    fn rejects_bad_percent_encoding_in_references() {
        assert_eq!(
            percent_decode_to_string("/bad%xx"),
            Err("reference path contains invalid percent encoding".to_string())
        );
    }

    #[test]
    fn renders_single_page_document_with_inline_style_and_script() {
        let fixture = Fixture::new("document");
        fixture.write("README.md", "# Hello\n\n- item\n");

        let site = build_site(&fixture.root, None).unwrap();
        let document = site_to_document(&site);

        assert!(document.contains("<style>"));
        assert!(document.contains("<script>"));
        assert!(document.contains("const MD_BUILD = {"));
        assert!(document.contains("\\u003ch1\\u003eHello\\u003c/h1\\u003e"));
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
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
                "rust-tools-md-build-{name}-{}-{nanos}",
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
