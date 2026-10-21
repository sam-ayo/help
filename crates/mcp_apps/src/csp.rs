//! The Content-Security-Policy of a view.

use serde_json::Value;

/// The policy when the resource gives no domains.
const STRICT: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
                      img-src 'self' data:; media-src 'self' data:; connect-src 'none'";

/// The Content-Security-Policy for a view, from resource `_meta.ui.csp` (None → the strict default).
pub fn csp_header(csp: Option<&Value>) -> String {
    let domains = |key: &str| -> Vec<&str> {
        csp.and_then(|c| c.get(key))
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(Value::as_str).filter(|d| is_safe(d)).collect())
            .unwrap_or_default()
    };
    let resource = domains("resourceDomains");
    let connect = domains("connectDomains");
    let frame = domains("frameDomains");
    let base = domains("baseUriDomains");
    if resource.is_empty() && connect.is_empty() && frame.is_empty() && base.is_empty() {
        return STRICT.to_string();
    }

    // A directive with no sources gets `'none'`.
    let directive = |name: &str, sources: &[&str], extra: &[&str]| {
        let mut parts = vec![name];
        parts.extend(sources);
        parts.extend(extra);
        if parts.len() == 1 {
            parts.push("'none'");
        }
        parts.join(" ")
    };
    let connect_self: &[&str] = if connect.is_empty() { &[] } else { &["'self'"] };
    [
        directive("default-src", &[], &[]),
        directive("script-src", &["'self'", "'unsafe-inline'"], &resource),
        directive("style-src", &["'self'", "'unsafe-inline'"], &resource),
        directive("img-src", &["'self'", "data:"], &resource),
        directive("font-src", &["'self'"], &resource),
        directive("media-src", &["'self'", "data:"], &resource),
        directive("worker-src", &["'self'"], &resource),
        directive("connect-src", connect_self, &connect),
        directive("frame-src", &[], &frame),
        directive("base-uri", &[], &base),
        directive("object-src", &[], &[]),
    ]
    .join("; ")
}

/// A domain is safe when it cannot end the directive or add a new source.
fn is_safe(domain: &str) -> bool {
    !domain.is_empty() && !domain.chars().any(|c| matches!(c, ';' | '\'' | '"' | ',') || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_domains_give_the_strict_policy() {
        assert_eq!(csp_header(None), STRICT);
        assert_eq!(csp_header(Some(&json!({}))), STRICT);
        assert_eq!(csp_header(Some(&json!({"connectDomains": []}))), STRICT);
    }

    #[test]
    fn domains_go_to_their_directives() {
        let csp = json!({
            "resourceDomains": ["https://cdn.example.com"],
            "connectDomains": ["https://api.example.com", "wss://live.example.com"],
        });
        assert_eq!(
            csp_header(Some(&csp)),
            "default-src 'none'; \
             script-src 'self' 'unsafe-inline' https://cdn.example.com; \
             style-src 'self' 'unsafe-inline' https://cdn.example.com; \
             img-src 'self' data: https://cdn.example.com; \
             font-src 'self' https://cdn.example.com; \
             media-src 'self' data: https://cdn.example.com; \
             worker-src 'self' https://cdn.example.com; \
             connect-src 'self' https://api.example.com wss://live.example.com; \
             frame-src 'none'; base-uri 'none'; object-src 'none'"
        );
    }

    #[test]
    fn frame_and_base_domains_replace_none() {
        let csp = json!({"frameDomains": ["https://www.youtube.com"], "baseUriDomains": ["https://a.example"]});
        let header = csp_header(Some(&csp));
        assert!(header.contains("frame-src https://www.youtube.com;"), "{header}");
        assert!(header.contains("base-uri https://a.example;"), "{header}");
        assert!(header.contains("connect-src 'none';"), "{header}");
        assert!(header.contains("script-src 'self' 'unsafe-inline';"), "{header}");
    }

    #[test]
    fn unsafe_entries_are_dropped() {
        let csp = json!({"connectDomains": [
            "https://ok.example", "https://a.example; script-src *", "'unsafe-eval'",
            "\"x\"", "a b", "a\nb", "", 7,
        ]});
        let header = csp_header(Some(&csp));
        assert!(header.contains("connect-src 'self' https://ok.example;"), "{header}");
        assert!(!header.contains("unsafe-eval"), "{header}");
        assert!(!header.contains('*'), "{header}");
    }
}
