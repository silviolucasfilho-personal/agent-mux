//! The browser client, compiled into the binary.
//!
//! Nothing is fetched at build or run time and nothing is read from disk:
//! a request path is matched against this fixed table, never joined to a
//! filesystem path. Provenance of the vendored files is in
//! `web/vendor/VERSIONS`.

pub struct Asset {
    /// Path under `/assets/`; `index.html` and `unauthorized.html` are
    /// served from the routes rather than that prefix.
    pub path: &'static str,
    pub content_type: &'static str,
    pub body: &'static [u8],
}

const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";

pub const ASSETS: &[Asset] = &[
    Asset {
        path: "index.html",
        content_type: HTML,
        body: include_bytes!("../../web/index.html"),
    },
    Asset {
        path: "unauthorized.html",
        content_type: HTML,
        body: include_bytes!("../../web/unauthorized.html"),
    },
    Asset {
        path: "app.js",
        content_type: JS,
        body: include_bytes!("../../web/app.js"),
    },
    Asset {
        path: "app.css",
        content_type: CSS,
        body: include_bytes!("../../web/app.css"),
    },
    Asset {
        path: "vendor/xterm.js",
        content_type: JS,
        body: include_bytes!("../../web/vendor/xterm.js"),
    },
    Asset {
        path: "vendor/xterm.css",
        content_type: CSS,
        body: include_bytes!("../../web/vendor/xterm.css"),
    },
    Asset {
        path: "vendor/addon-unicode11.js",
        content_type: JS,
        body: include_bytes!("../../web/vendor/addon-unicode11.js"),
    },
];

pub fn lookup(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

pub fn index() -> &'static Asset {
    lookup("index.html").expect("index.html is in ASSETS")
}

pub fn unauthorized() -> &'static Asset {
    lookup("unauthorized.html").expect("unauthorized.html is in ASSETS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    fn web_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("web")
    }

    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if entry.path().is_dir() {
                walk(&entry.path(), &rel, out);
            } else {
                out.push(rel);
            }
        }
    }

    /// Files under `web/` that are documentation, not served content.
    fn is_not_served(rel: &str) -> bool {
        rel == "vendor/VERSIONS" || rel == "vendor/LICENSE-xterm.txt"
    }

    #[test]
    fn every_file_under_web_is_served_and_every_entry_exists() {
        let mut files = Vec::new();
        walk(&web_dir(), "", &mut files);
        assert!(!files.is_empty(), "web/ is empty");

        let table: HashSet<&str> = ASSETS.iter().map(|a| a.path).collect();
        for rel in files.iter().filter(|r| !is_not_served(r)) {
            assert!(
                table.contains(rel.as_str()),
                "web/{rel} is not in ASSETS; add it or list it in is_not_served"
            );
        }
        let on_disk: HashSet<&str> = files.iter().map(String::as_str).collect();
        for a in ASSETS {
            assert!(
                on_disk.contains(a.path),
                "ASSETS lists a missing {}",
                a.path
            );
            assert!(!a.body.is_empty(), "{} is empty", a.path);
        }
    }

    #[test]
    fn the_page_references_only_assets_we_serve() {
        let html = std::str::from_utf8(index().body).unwrap();
        let mut found = 0;
        for (attr, _) in [("src=\"", 0), ("href=\"", 0)] {
            let mut rest = html;
            while let Some(i) = rest.find(attr) {
                rest = &rest[i + attr.len()..];
                let Some(end) = rest.find('"') else { break };
                let url = &rest[..end];
                if let Some(name) = url.strip_prefix("/assets/") {
                    assert!(
                        lookup(name).is_some(),
                        "index.html references /assets/{name}"
                    );
                    found += 1;
                }
                rest = &rest[end..];
            }
        }
        assert!(found >= 4, "expected the page to reference its assets");
    }

    #[test]
    fn no_asset_reaches_out_to_a_cdn_or_inlines_a_script() {
        for name in ["index.html", "unauthorized.html", "app.js", "app.css"] {
            let text = std::str::from_utf8(lookup(name).unwrap().body).unwrap();
            for needle in ["http://", "https://", "//cdn.", "//unpkg"] {
                assert!(
                    !text.contains(needle),
                    "{name} contains {needle}: the page must be self-contained"
                );
            }
        }
        // A style attribute is blocked by the policy even with
        // style-src 'unsafe-inline' unless 'unsafe-hashes' is added, which
        // it is not -- so pages put their styling in app.css.
        for name in ["index.html", "unauthorized.html"] {
            let text = std::str::from_utf8(lookup(name).unwrap().body).unwrap();
            assert!(
                !text.contains("style=\""),
                "{name} uses a style attribute; the CSP blocks it"
            );
        }
        // Inline scripts would need a CSP exception; every script has a src.
        let html = std::str::from_utf8(index().body).unwrap();
        for piece in html.split("<script").skip(1) {
            let tag = piece.split('>').next().unwrap_or("");
            assert!(tag.contains("src="), "inline <script{tag}> in index.html");
        }
    }

    #[test]
    fn content_types_match_the_extensions_and_text_is_utf8() {
        for a in ASSETS {
            let expected = match a.path.rsplit('.').next().unwrap() {
                "html" => HTML,
                "js" => JS,
                "css" => CSS,
                other => panic!("no content type rule for .{other}"),
            };
            assert_eq!(a.content_type, expected, "{}", a.path);
            std::str::from_utf8(a.body).unwrap_or_else(|_| panic!("{} is not utf-8", a.path));
        }
    }

    #[test]
    fn the_vendored_bundles_are_the_ones_we_think() {
        let xterm = std::str::from_utf8(lookup("vendor/xterm.js").unwrap().body).unwrap();
        assert!(
            xterm.contains("e.Terminal="),
            "xterm.js has no Terminal export"
        );
        let unicode =
            std::str::from_utf8(lookup("vendor/addon-unicode11.js").unwrap().body).unwrap();
        assert!(unicode.contains("Unicode11Addon"), "addon is not unicode11");
        let versions = std::fs::read_to_string(web_dir().join("vendor/VERSIONS")).unwrap();
        for name in ["xterm.js", "xterm.css", "addon-unicode11.js"] {
            assert!(versions.contains(name), "VERSIONS does not record {name}");
        }
        assert!(
            web_dir().join("vendor/LICENSE-xterm.txt").exists(),
            "the MIT notice must ship with the bundle"
        );
    }

    /// The client and the server have to agree on every `t` value, and
    /// nothing but this test checks a JS string against a Rust constant.
    #[test]
    fn the_client_only_speaks_frames_the_server_knows() {
        use crate::remote::protocol::{CLIENT_FRAMES, SERVER_FRAMES};
        let js = std::str::from_utf8(lookup("app.js").unwrap().body).unwrap();

        // What app.js sends: `{ t: 'name'` / `{ t: "name"`.
        let mut sent = HashSet::new();
        let mut rest = js;
        while let Some(i) = rest.find("t: '") {
            rest = &rest[i + 4..];
            if let Some(end) = rest.find('\'') {
                sent.insert(&rest[..end]);
            }
        }
        assert!(!sent.is_empty(), "found no outgoing frames in app.js");
        for name in &sent {
            assert!(
                CLIENT_FRAMES.contains(name),
                "app.js sends unknown frame {name:?}"
            );
        }

        // What app.js handles: the `case 'name':` arms of the one switch
        // over `msg.t`. Scoped to that switch so the unrelated switch over
        // RuntimeState does not look like a frame table.
        let dispatch = js
            .split_once("switch (msg.t) {")
            .expect("app.js has no frame dispatch")
            .1;
        let dispatch = dispatch
            .split_once("\n    }")
            .expect("unterminated switch")
            .0;
        let mut handled = HashSet::new();
        let mut rest = dispatch;
        while let Some(i) = rest.find("case '") {
            rest = &rest[i + 6..];
            if let Some(end) = rest.find('\'') {
                handled.insert(&rest[..end]);
            }
        }
        for name in &handled {
            assert!(
                SERVER_FRAMES.contains(name),
                "app.js handles unknown frame {name:?}"
            );
        }
        for name in SERVER_FRAMES {
            assert!(
                handled.contains(name),
                "app.js ignores server frame {name:?}"
            );
        }
    }

    /// `hidden` is how the page shows and hides the sheet, the menu, the
    /// empty state and the second key row. A `display:` rule on any of
    /// their classes silently outranks the browser's own
    /// `[hidden] { display: none }`, which pinned the launch sheet open
    /// over the terminal -- visible only in a browser, which no test here
    /// has. This asserts the override that makes the attribute win.
    #[test]
    fn the_hidden_attribute_beats_every_display_rule() {
        let html = std::str::from_utf8(index().body).unwrap();
        let css = std::str::from_utf8(lookup("app.css").unwrap().body).unwrap();

        // Classes carried by elements that rely on `hidden`.
        let mut classes: Vec<&str> = Vec::new();
        for tag in html.split('<').filter(|t| t.contains(" hidden")) {
            let tag = tag.split('>').next().unwrap_or("");
            if let Some(rest) = tag.split_once("class=\"")
                && let Some(list) = rest.1.split('"').next()
            {
                classes.extend(list.split_whitespace());
            }
        }
        assert!(
            !classes.is_empty(),
            "expected the page to hide elements with the hidden attribute"
        );

        let at_risk: Vec<&&str> = classes
            .iter()
            .filter(|c| {
                css.split(&format!(".{c}")).skip(1).any(|block| {
                    block
                        .split('}')
                        .next()
                        .is_some_and(|b| b.contains("display:"))
                })
            })
            .collect();

        if !at_risk.is_empty() {
            assert!(
                css.contains("[hidden]") && css.contains("display: none !important"),
                "{at_risk:?} set `display:`, which beats the browser's \
                 [hidden] rule; app.css needs [hidden] {{ display: none !important }}"
            );
        }
    }
}
