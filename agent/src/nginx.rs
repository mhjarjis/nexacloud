//! A tolerant, read-only nginx config parser.
//!
//! It tokenises nginx syntax (comments, quotes, `${var}`), builds a directive
//! tree, extracts every `server { }` block, and merges blocks that describe
//! the same site (e.g. the port-80 → https redirect block). It never follows
//! `include`s and never panics on malformed input.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Open,
    Close,
    Semi,
}

fn tokenize(src: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut word = String::new();
    let mut in_word = false;

    macro_rules! flush {
        () => {
            if in_word {
                out.push(Token::Word(std::mem::take(&mut word)));
                in_word = false;
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => {
                flush!();
                i += 1;
            }
            '#' if !in_word => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '{' => {
                if in_word && word.ends_with('$') {
                    // `${var}` inside a word.
                    while i < chars.len() && chars[i] != '}' {
                        word.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        word.push('}');
                        i += 1;
                    }
                } else {
                    flush!();
                    out.push(Token::Open);
                    i += 1;
                }
            }
            '}' => {
                flush!();
                out.push(Token::Close);
                i += 1;
            }
            ';' => {
                flush!();
                out.push(Token::Semi);
                i += 1;
            }
            '"' | '\'' if !in_word => {
                let quote = c;
                i += 1;
                let mut s = String::new();
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    s.push(chars[i]);
                    i += 1;
                }
                i += 1; // closing quote (or EOF)
                out.push(Token::Word(s));
            }
            '\\' if i + 1 < chars.len() => {
                word.push(chars[i + 1]);
                in_word = true;
                i += 2;
            }
            _ => {
                word.push(c);
                in_word = true;
                i += 1;
            }
        }
    }
    if in_word {
        out.push(Token::Word(word));
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct Directive {
    pub name: String,
    pub args: Vec<String>,
    pub block: Option<Vec<Directive>>,
}

/// Parse into a directive tree. Unbalanced braces are tolerated: stray `}`
/// are ignored and unclosed blocks are closed at EOF.
pub fn parse(src: &str) -> Vec<Directive> {
    let tokens = tokenize(src);
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < tokens.len() {
        out.extend(parse_block(&tokens, &mut pos, 0));
        // parse_block returns at a stray top-level `}`; skip it.
        if pos < tokens.len() {
            pos += 1;
        }
    }
    out
}

const MAX_DEPTH: usize = 64;

fn parse_block(tokens: &[Token], pos: &mut usize, depth: usize) -> Vec<Directive> {
    let mut out = Vec::new();
    let mut words: Vec<String> = Vec::new();
    while *pos < tokens.len() {
        match &tokens[*pos] {
            Token::Word(w) => {
                words.push(w.clone());
                *pos += 1;
            }
            Token::Semi => {
                *pos += 1;
                if !words.is_empty() {
                    let name = words.remove(0);
                    out.push(Directive {
                        name,
                        args: std::mem::take(&mut words),
                        block: None,
                    });
                }
            }
            Token::Open => {
                *pos += 1;
                let children = if depth < MAX_DEPTH {
                    parse_block(tokens, pos, depth + 1)
                } else {
                    Vec::new()
                };
                // consume the matching `}` if present
                if *pos < tokens.len() {
                    *pos += 1;
                }
                let name = if words.is_empty() {
                    String::new()
                } else {
                    words.remove(0)
                };
                out.push(Directive {
                    name,
                    args: std::mem::take(&mut words),
                    block: Some(children),
                });
            }
            Token::Close => {
                // Leave the `}` for the caller.
                break;
            }
        }
    }
    if !words.is_empty() {
        let name = words.remove(0);
        out.push(Directive {
            name,
            args: words,
            block: None,
        });
    }
    out
}

/// What we extract from one `server { }` block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerBlock {
    pub config_path: PathBuf,
    pub server_names: Vec<String>,
    pub root: Option<String>,
    pub fastcgi_pass: Option<String>,
    pub proxy_pass: Option<String>,
    pub ssl_certificate: Option<String>,
    pub listen_ssl: bool,
    pub has_return: bool,
}

impl ServerBlock {
    pub fn is_content(&self) -> bool {
        self.root.is_some() || self.proxy_pass.is_some() || self.fastcgi_pass.is_some()
    }
    pub fn ssl(&self) -> bool {
        self.ssl_certificate.is_some() || self.listen_ssl
    }
    pub fn id(&self) -> &str {
        self.server_names.first().map(String::as_str).unwrap_or("")
    }
}

fn find_servers<'a>(dirs: &'a [Directive], out: &mut Vec<&'a Directive>) {
    for d in dirs {
        if let Some(block) = &d.block {
            if d.name == "server" {
                out.push(d);
            } else {
                find_servers(block, out);
            }
        }
    }
}

/// First value of a directive anywhere in the block (depth-first), not
/// descending into nested `server` blocks.
fn find_first<'a>(dirs: &'a [Directive], name: &str) -> Option<&'a Directive> {
    for d in dirs {
        if d.name == name && d.block.is_none() {
            return Some(d);
        }
    }
    for d in dirs {
        if let Some(b) = &d.block {
            if d.name != "server" {
                if let Some(x) = find_first(b, name) {
                    return Some(x);
                }
            }
        }
    }
    None
}

fn first_arg(d: Option<&Directive>) -> Option<String> {
    d.and_then(|d| d.args.first().cloned())
        .filter(|s| !s.is_empty())
}

fn server_block(d: &Directive, path: &Path) -> Option<ServerBlock> {
    let body = d.block.as_ref()?;
    let mut names = Vec::new();
    for x in body.iter().filter(|x| x.name == "server_name" && x.block.is_none()) {
        for n in &x.args {
            let n = n.trim();
            if !n.is_empty() && n != "_" && !names.iter().any(|e| e == n) {
                names.push(n.to_string());
            }
        }
    }
    if names.is_empty() {
        return None;
    }
    // root: server level, else `location /`, else first anywhere.
    let root = first_arg(
        body.iter()
            .find(|x| x.name == "root" && x.block.is_none()),
    )
    .or_else(|| {
        body.iter()
            .filter(|x| x.name == "location" && x.args.last().map(String::as_str) == Some("/"))
            .find_map(|loc| first_arg(find_first(loc.block.as_deref().unwrap_or(&[]), "root")))
    })
    .or_else(|| first_arg(find_first(body, "root")));
    let listen_ssl = body
        .iter()
        .filter(|x| x.name == "listen")
        .any(|x| x.args.iter().skip(1).any(|a| a == "ssl" || a == "quic"));
    Some(ServerBlock {
        config_path: path.to_path_buf(),
        server_names: names,
        root,
        fastcgi_pass: first_arg(find_first(body, "fastcgi_pass")),
        proxy_pass: first_arg(find_first(body, "proxy_pass")),
        ssl_certificate: first_arg(find_first(body, "ssl_certificate")),
        listen_ssl,
        has_return: body.iter().any(|x| x.name == "return"),
    })
}

/// All server blocks (with a real server_name) in one config file's text.
pub fn server_blocks(src: &str, path: &Path) -> Vec<ServerBlock> {
    let tree = parse(src);
    let mut servers = Vec::new();
    find_servers(&tree, &mut servers);
    servers
        .into_iter()
        .filter_map(|d| server_block(d, path))
        .collect()
}

fn overlaps(g: &[ServerBlock], b: &ServerBlock) -> bool {
    g.iter()
        .any(|x| x.server_names.iter().any(|n| b.server_names.contains(n)))
}

/// Group blocks transitively by shared server_names.
fn group_by_overlap(blocks: Vec<ServerBlock>) -> Vec<Vec<ServerBlock>> {
    let mut groups: Vec<Vec<ServerBlock>> = Vec::new();
    for b in blocks {
        let overlapping: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| overlaps(g, &b))
            .map(|(i, _)| i)
            .collect();
        if overlapping.is_empty() {
            groups.push(vec![b]);
        } else {
            let mut merged = Vec::new();
            for &i in overlapping.iter().rev() {
                let mut g = groups.remove(i);
                g.append(&mut merged);
                merged = g;
            }
            merged.push(b);
            groups.insert(overlapping[0], merged);
        }
    }
    groups
}

/// Merge blocks that describe the same site. Content blocks (root / proxy /
/// fastcgi) sharing a server_name are one site (e.g. the same site on two
/// ports). Non-content blocks (typically the http→https redirect) are folded
/// into the content site they overlap; a redirect block covering several
/// sites never joins them together. The primary block of a site is the one
/// with content, preferring the TLS one.
pub fn merge_blocks(blocks: Vec<ServerBlock>) -> Vec<ServerBlock> {
    let (content, other): (Vec<ServerBlock>, Vec<ServerBlock>) =
        blocks.into_iter().partition(ServerBlock::is_content);
    let mut groups = group_by_overlap(content);
    let mut leftovers = Vec::new();
    for b in other {
        let hits: Vec<usize> = groups
            .iter()
            .enumerate()
            .filter(|(_, g)| g.iter().any(ServerBlock::is_content) && overlaps(g, &b))
            .map(|(i, _)| i)
            .collect();
        match hits.len() {
            0 => leftovers.push(b),
            // Exactly one site: its extra names (e.g. www.) belong to it.
            1 => groups[hits[0]].push(b),
            // Shared by several sites: already represented; contributes nothing.
            _ => {}
        }
    }
    groups.extend(group_by_overlap(leftovers));

    groups
        .into_iter()
        .map(|g| {
            let score = |b: &ServerBlock| (b.is_content() as u8) * 2 + b.ssl() as u8;
            let best = g
                .iter()
                .enumerate()
                .max_by(|(ia, a), (ib, b)| score(a).cmp(&score(b)).then(ib.cmp(ia)))
                .map(|(i, _)| i)
                .unwrap_or(0);
            let mut primary = g[best].clone();
            for (i, other) in g.iter().enumerate() {
                if i == best {
                    continue;
                }
                for n in &other.server_names {
                    if !primary.server_names.contains(n) {
                        primary.server_names.push(n.clone());
                    }
                }
                if primary.root.is_none() {
                    primary.root = other.root.clone();
                }
                if primary.fastcgi_pass.is_none() {
                    primary.fastcgi_pass = other.fastcgi_pass.clone();
                }
                if primary.proxy_pass.is_none() {
                    primary.proxy_pass = other.proxy_pass.clone();
                }
                if primary.ssl_certificate.is_none() {
                    primary.ssl_certificate = other.ssl_certificate.clone();
                }
                primary.listen_ssl |= other.listen_ssl;
            }
            primary
        })
        .collect()
}

/// `php8.3` in e.g. `unix:/var/run/php/php8.3-fpm.sock` → `8.3`.
pub fn php_version_from_socket(s: &str) -> Option<String> {
    let mut rest = s;
    while let Some(idx) = rest.find("php") {
        let after = &rest[idx + 3..];
        let digits: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let digits = digits.trim_end_matches('.');
        let parts: Vec<&str> = digits.split('.').collect();
        if parts.len() >= 2 && parts.iter().take(2).all(|p| !p.is_empty()) {
            return Some(format!("{}.{}", parts[0], parts[1]));
        }
        rest = after;
    }
    None
}

/// `laravel` if `<root>/../artisan` exists; `php` if fastcgi_pass present;
/// `proxy` if proxy_pass; `static` if root and no php; else `unknown`.
pub fn site_kind(has_root: bool, artisan_exists: bool, has_fastcgi: bool, has_proxy: bool) -> &'static str {
    if has_root && artisan_exists {
        "laravel"
    } else if has_fastcgi {
        "php"
    } else if has_proxy {
        "proxy"
    } else if has_root {
        "static"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LARAVEL: &str = r#"
# Managed by hand
server {
    listen 80;
    listen [::]:80;
    server_name shop.example.com www.shop.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name shop.example.com www.shop.example.com;
    root /var/www/shop/public;   # laravel public dir

    ssl_certificate /etc/letsencrypt/live/shop.example.com/fullchain.pem; # managed by Certbot
    ssl_certificate_key /etc/letsencrypt/live/shop.example.com/privkey.pem;
    include /etc/letsencrypt/options-ssl-nginx.conf;

    index index.php;
    charset utf-8;

    location / {
        try_files $uri $uri/ /index.php?$query_string;
    }

    location ~ \.php$ {
        fastcgi_pass unix:/var/run/php/php8.3-fpm.sock;
        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
        include fastcgi_params;
    }

    location ~ /\.(?!well-known).* {
        deny all;
    }
}
"#;

    const PLAIN_PHP: &str = r#"
server {
	listen 80;
	server_name blog.example.org;
	root "/var/www/blog";
	index index.php index.html;
	location ~ \.php$ {
		include snippets/fastcgi-php.conf;
		fastcgi_pass unix:/run/php/php8.1-fpm.sock;
	}
}
"#;

    const PROXY: &str = r#"
upstream app_backend {
    server 127.0.0.1:3000;
}
server {
    listen 443 ssl;
    server_name app.example.net;
    ssl_certificate /etc/ssl/app.pem;
    location / {
        proxy_pass http://app_backend;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
"#;

    const WEIRD: &str = r#"
# server { server_name commented.example.com; }
map $http_upgrade $connection_upgrade { default upgrade; '' close; }
server {
    listen 80 default_server;
    server_name _;
    return 444;
}
server { listen 80; server_name "quoted.example.com" ; root /srv/q; # trailing comment { }
  location /x { add_header X-Test "a;b{c}"; }
  set $foo "${host}bar";
}
server {
    server_name broken.example.com;
    location / {
        root /srv/broken;
"#;

    fn p() -> PathBuf {
        PathBuf::from("/etc/nginx/sites-enabled/test")
    }

    #[test]
    fn laravel_with_redirect_block_is_one_site() {
        let blocks = server_blocks(LARAVEL, &p());
        assert_eq!(blocks.len(), 2);
        let sites = merge_blocks(blocks);
        assert_eq!(sites.len(), 1);
        let s = &sites[0];
        assert_eq!(s.id(), "shop.example.com");
        assert_eq!(s.server_names, vec!["shop.example.com", "www.shop.example.com"]);
        assert_eq!(s.root.as_deref(), Some("/var/www/shop/public"));
        assert_eq!(s.fastcgi_pass.as_deref(), Some("unix:/var/run/php/php8.3-fpm.sock"));
        assert_eq!(
            s.ssl_certificate.as_deref(),
            Some("/etc/letsencrypt/live/shop.example.com/fullchain.pem")
        );
        assert!(s.ssl());
        assert!(s.proxy_pass.is_none());
        assert_eq!(php_version_from_socket(s.fastcgi_pass.as_ref().unwrap()).as_deref(), Some("8.3"));
    }

    #[test]
    fn plain_php() {
        let sites = merge_blocks(server_blocks(PLAIN_PHP, &p()));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].root.as_deref(), Some("/var/www/blog"));
        assert!(!sites[0].ssl());
        assert_eq!(
            php_version_from_socket(sites[0].fastcgi_pass.as_ref().unwrap()).as_deref(),
            Some("8.1")
        );
    }

    #[test]
    fn proxy_site_and_upstream_is_not_a_server_block() {
        let sites = merge_blocks(server_blocks(PROXY, &p()));
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].id(), "app.example.net");
        assert_eq!(sites[0].proxy_pass.as_deref(), Some("http://app_backend"));
        assert!(sites[0].root.is_none());
        assert!(sites[0].ssl());
        assert_eq!(site_kind(false, false, false, true), "proxy");
    }

    #[test]
    fn weird_config_is_tolerated() {
        let sites = merge_blocks(server_blocks(WEIRD, &p()));
        let ids: Vec<&str> = sites.iter().map(|s| s.id()).collect();
        assert_eq!(ids, vec!["quoted.example.com", "broken.example.com"]);
        assert_eq!(sites[0].root.as_deref(), Some("/srv/q"));
        assert_eq!(sites[1].root.as_deref(), Some("/srv/broken"));
    }

    #[test]
    fn garbage_never_panics() {
        for src in ["}}}{{{", "server {", "server", ";;;", "\"unterminated", "a ${b", "\\", "server { server_name a; } }"] {
            let _ = merge_blocks(server_blocks(src, &p()));
        }
    }

    #[test]
    fn multiple_sites_in_one_file_and_cross_file_merge() {
        let mut blocks = server_blocks(PLAIN_PHP, &p());
        blocks.extend(server_blocks(PROXY, Path::new("/etc/nginx/conf.d/app.conf")));
        // A separate redirect for the proxy site in another file.
        blocks.extend(server_blocks(
            "server { listen 80; server_name app.example.net; return 301 https://$host$request_uri; }",
            Path::new("/etc/nginx/sites-enabled/redirects"),
        ));
        let sites = merge_blocks(blocks);
        assert_eq!(sites.len(), 2);
        let app = sites.iter().find(|s| s.id() == "app.example.net").unwrap();
        assert_eq!(app.config_path, PathBuf::from("/etc/nginx/conf.d/app.conf"));
    }

    #[test]
    fn shared_redirect_block_does_not_merge_sites() {
        let src = "
server { listen 80; server_name a.com b.com; return 301 https://$host$request_uri; }
server { listen 443 ssl; server_name a.com; root /srv/a; ssl_certificate /a.pem; }
server { listen 443 ssl; server_name b.com; root /srv/b; ssl_certificate /b.pem; }
server { listen 80; server_name www.a.com a.com; return 301 https://a.com$request_uri; }
server { listen 80; server_name parked.com; return 404; }
";
        let sites = merge_blocks(server_blocks(src, &p()));
        let ids: Vec<&str> = sites.iter().map(|s| s.id()).collect();
        assert_eq!(ids, vec!["a.com", "b.com", "parked.com"]);
        assert_eq!(sites[0].root.as_deref(), Some("/srv/a"));
        assert_eq!(sites[0].ssl_certificate.as_deref(), Some("/a.pem"));
        assert_eq!(sites[0].server_names, vec!["a.com", "www.a.com"]);
        assert_eq!(sites[1].root.as_deref(), Some("/srv/b"));
        assert_eq!(sites[1].ssl_certificate.as_deref(), Some("/b.pem"));
        assert_eq!(sites[1].server_names, vec!["b.com"]);
    }

    #[test]
    fn kinds() {
        assert_eq!(site_kind(true, true, true, false), "laravel");
        assert_eq!(site_kind(true, false, true, false), "php");
        assert_eq!(site_kind(false, false, false, true), "proxy");
        assert_eq!(site_kind(true, false, false, false), "static");
        assert_eq!(site_kind(false, false, false, false), "unknown");
    }

    #[test]
    fn php_socket_versions() {
        assert_eq!(php_version_from_socket("unix:/run/php/php7.4-fpm.sock").as_deref(), Some("7.4"));
        assert_eq!(php_version_from_socket("unix:/run/php/php-fpm.sock"), None);
        assert_eq!(php_version_from_socket("127.0.0.1:9000"), None);
        assert_eq!(php_version_from_socket("unix:/var/run/php8.2-fpm-shop.sock").as_deref(), Some("8.2"));
    }
}
