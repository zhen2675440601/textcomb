use axum::extract::Request;
use http::{HeaderMap, HeaderValue, Method, header};
use tower_http::cors::CorsLayer;
use url::{Origin, Url};

pub fn cors_layer(
    public_origin: Option<&Url>,
) -> Result<CorsLayer, http::header::InvalidHeaderValue> {
    let cors = CorsLayer::new()
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::HeaderName::from_static("idempotency-key"),
            header::HeaderName::from_static("x-request-id"),
        ]);
    // Same-origin browser requests need no CORS permission. Never reflect an
    // arbitrary Origin with credentials when the deployment uses auto mode.
    match public_origin {
        Some(origin) => Ok(cors
            .allow_origin(HeaderValue::from_str(
                &origin.origin().ascii_serialization(),
            )?)
            .allow_credentials(true)),
        None => Ok(cors),
    }
}

pub fn is_cross_site_mutation(request: &Request, public_origin: Option<&Url>) -> bool {
    if !matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return false;
    }
    mutation_origin_allowed(request, public_origin) != Some(true)
}

fn mutation_origin_allowed(request: &Request, public_origin: Option<&Url>) -> Option<bool> {
    let headers = request.headers();
    let fetch_site = single_header(headers, "sec-fetch-site")?;
    let origin = if let Some(raw) = single_header(headers, "origin")? {
        let parsed = http_url(raw)?;
        // Origin is a serialized origin, not a full URL, a list, or `null`.
        if parsed.origin().ascii_serialization() != raw {
            return Some(false);
        }
        Some(parsed.origin())
    } else {
        // Older browsers may provide only Referer. Keep requests without any
        // browser provenance usable by CLI clients, as before this change.
        match single_header(headers, "referer")? {
            Some(raw) => Some(http_url(raw)?.origin()),
            None => None,
        }
    };

    if let Some(expected) = public_origin {
        return Some(match origin {
            Some(origin) => origin == expected.origin(),
            None => !matches!(fetch_site, Some("cross-site" | "same-site")),
        });
    }

    // Browsers own Sec-* headers. Same-origin metadata survives TLS termination
    // and Host rewriting without trusting client-supplied forwarding headers.
    // `same-site` includes other subdomains and is deliberately not sufficient.
    match fetch_site {
        Some("same-origin") => return Some(true),
        Some("cross-site" | "same-site") => return Some(false),
        _ => {}
    }
    let Some(origin) = origin else {
        return Some(true);
    };
    // Plain HTTP deployments do not receive Fetch Metadata in many browsers.
    // Fall back to the actual request authority and transport scheme. Never use
    // X-Forwarded-Host/Proto or Forwarded without an explicit trust boundary.
    let host = single_header(headers, "host")?.or_else(|| {
        request
            .uri()
            .authority()
            .map(|authority| authority.as_str())
    })?;
    let scheme = request.uri().scheme_str().unwrap_or("http");
    let target = http_url(&format!("{scheme}://{host}"))?;
    if !target.username().is_empty()
        || target.password().is_some()
        || target.path() != "/"
        || target.query().is_some()
        || target.fragment().is_some()
    {
        return Some(false);
    }
    Some(origin == target.origin())
}

fn http_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    (matches!(url.origin(), Origin::Tuple(_, _, _)) && matches!(url.scheme(), "http" | "https"))
        .then_some(url)
}

// Outer None means a malformed or duplicate header; inner None means absent.
fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<Option<&'a str>> {
    let mut values = headers.get_all(name).iter();
    let value = values.next().map(|value| value.to_str()).transpose().ok()?;
    values.next().is_none().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn request(method: Method, host: &str, origin: &str, fetch_site: Option<&str>) -> Request {
        let mut builder = Request::builder()
            .method(method)
            .uri("/api/v1/auth/login")
            .header(header::HOST, host)
            .header(header::ORIGIN, origin);
        if let Some(site) = fetch_site {
            builder = builder.header("sec-fetch-site", site);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn origin_auto_allows_http_ips_ports_domains_and_ipv6_without_fetch_metadata() {
        for (host, origin) in [
            ("192.0.2.10:18080", "http://192.0.2.10:18080"),
            ("textcomb.example:3000", "http://textcomb.example:3000"),
            ("localhost:5173", "http://localhost:5173"),
            ("[2001:db8::1]:3000", "http://[2001:db8::1]:3000"),
            ("textcomb.example:80", "http://textcomb.example"),
        ] {
            for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
                assert!(
                    !is_cross_site_mutation(&request(method, host, origin, None), None),
                    "{host}"
                );
            }
        }
    }

    #[test]
    fn origin_auto_accepts_browser_same_origin_behind_tls_and_host_rewriting() {
        let request = request(
            Method::POST,
            "api:8080",
            "https://textcomb.example",
            Some("same-origin"),
        );
        assert!(!is_cross_site_mutation(&request, None));
    }

    #[test]
    fn origin_auto_rejects_other_sites_subdomains_schemes_and_ports() {
        for (origin, metadata) in [
            ("https://evil.example", Some("cross-site")),
            ("https://other.textcomb.example", Some("same-site")),
            ("http://evil.example", None),
            ("https://textcomb.example", None),
            ("http://textcomb.example:8081", None),
            ("http://textcomb.example.evil.example", None),
            ("http://textcomb.example", Some("cross-site")),
        ] {
            let request = request(Method::POST, "textcomb.example", origin, metadata);
            assert!(
                is_cross_site_mutation(&request, None),
                "{origin}/{metadata:?}"
            );
        }
    }

    #[test]
    fn origin_auto_does_not_trust_spoofed_forwarding_headers() {
        let mut request = request(
            Method::POST,
            "textcomb.example",
            "https://evil.example",
            None,
        );
        for (name, value) in [
            ("x-forwarded-host", "evil.example"),
            ("x-forwarded-proto", "https"),
            ("forwarded", "host=evil.example;proto=https"),
        ] {
            request
                .headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
        assert!(is_cross_site_mutation(&request, None));
    }

    #[test]
    fn origin_auto_rejects_invalid_origins_even_with_same_origin_metadata() {
        for origin in [
            "null",
            "",
            "https://textcomb.example/login",
            "https://textcomb.example?x=1",
            "https://user@textcomb.example",
            "https://textcomb.example https://evil.example",
            "file:///example",
            "https://textcomb.example/",
        ] {
            let request = request(
                Method::POST,
                "textcomb.example",
                origin,
                Some("same-origin"),
            );
            assert!(is_cross_site_mutation(&request, None), "{origin}");
        }
    }

    #[test]
    fn origin_auto_rejects_duplicate_security_headers_and_invalid_host() {
        for name in [
            header::ORIGIN,
            header::HeaderName::from_static("sec-fetch-site"),
        ] {
            let mut request = request(
                Method::POST,
                "textcomb.example",
                "http://textcomb.example",
                Some("same-origin"),
            );
            request
                .headers_mut()
                .append(name, HeaderValue::from_static("invalid"));
            assert!(is_cross_site_mutation(&request, None));
        }
        for host in [
            "textcomb.example@evil.example",
            "textcomb.example/path",
            "",
            "textcomb.example,evil.example",
        ] {
            let request = request(Method::POST, host, "http://textcomb.example", None);
            assert!(is_cross_site_mutation(&request, None), "{host}");
        }
        let mut request = request(
            Method::POST,
            "textcomb.example",
            "http://textcomb.example",
            None,
        );
        request
            .headers_mut()
            .append(header::HOST, HeaderValue::from_static("textcomb.example"));
        assert!(is_cross_site_mutation(&request, None));
    }

    #[test]
    fn origin_explicit_configuration_pins_the_origin_even_for_same_origin_metadata() {
        let configured = Url::parse("https://frontend.example/").unwrap();
        let allowed = request(
            Method::POST,
            "api:8080",
            "https://frontend.example",
            Some("cross-site"),
        );
        assert!(!is_cross_site_mutation(&allowed, Some(&configured)));
        let denied = request(
            Method::POST,
            "api:8080",
            "https://other.example",
            Some("same-origin"),
        );
        assert!(is_cross_site_mutation(&denied, Some(&configured)));
    }

    #[test]
    fn origin_missing_headers_preserve_cli_access_but_referer_is_checked() {
        let mut request = request(
            Method::POST,
            "textcomb.example",
            "http://textcomb.example",
            None,
        );
        request.headers_mut().remove(header::ORIGIN);
        assert!(!is_cross_site_mutation(&request, None));
        for (referer, denied) in [
            ("http://textcomb.example/login", false),
            ("http://evil.example/login", true),
            ("null", true),
        ] {
            request
                .headers_mut()
                .insert(header::REFERER, HeaderValue::from_static(referer));
            assert_eq!(is_cross_site_mutation(&request, None), denied);
        }
        request.headers_mut().remove(header::REFERER);
        request
            .headers_mut()
            .insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        assert!(is_cross_site_mutation(&request, None));
    }

    #[test]
    fn origin_safe_requests_remain_accessible() {
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            let request = request(method, "textcomb.example", "null", Some("cross-site"));
            assert!(!is_cross_site_mutation(&request, None));
        }
    }

    #[tokio::test]
    async fn origin_cors_only_grants_credentialed_access_to_an_explicit_origin() {
        use axum::{Router, routing::post};
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::{TcpListener, TcpStream},
        };

        for (configured, source, granted) in [
            (None, "https://evil.example", false),
            (
                Some("https://frontend.example"),
                "https://frontend.example",
                true,
            ),
            (
                Some("https://frontend.example"),
                "https://evil.example",
                false,
            ),
        ] {
            let configured = configured.map(|raw| Url::parse(raw).unwrap());
            let app = Router::new()
                .route("/login", post(|| async {}))
                .layer(cors_layer(configured.as_ref()).unwrap());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream.write_all(format!(
                "OPTIONS /login HTTP/1.1\r\nHost: {address}\r\nOrigin: {source}\r\nAccess-Control-Request-Method: POST\r\nConnection: close\r\n\r\n"
            ).as_bytes()).await.unwrap();
            let mut response = String::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                stream.read_to_string(&mut response),
            )
            .await
            .unwrap()
            .unwrap();
            server.abort();
            let response = response.to_ascii_lowercase();
            assert!(response.starts_with("http/1.1 200"), "{response}");
            assert_eq!(
                response.contains(&format!("access-control-allow-origin: {source}\r\n")),
                granted,
                "{response}"
            );
            if granted {
                assert!(response.contains("access-control-allow-credentials: true\r\n"));
            }
        }
    }
}
