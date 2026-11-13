//! The camouflage website REALITY points to ("self steal"): a real HTTPS site on
//! the user's own domain, served on a loopback port of the same machine.
//!
//! A connection without a client key is relayed by mihomo to this site, so an
//! active probe sees an ordinary web server with a valid certificate. The same
//! path carries Let's Encrypt's TLS-ALPN-01 validation, so the certificate is
//! obtained through port 443 without anything else listening.
//! Until a certificate exists (or for names that cannot get one) the site
//! answers with a self-signed certificate so the node already works.
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::Full;
use hyper::{body::Incoming, header, Request, Response, StatusCode};
use hyper_util::{rt::TokioExecutor, rt::TokioIo, server::conn::auto};
use rustls_acme::{
    caches::DirCache,
    rustls::{
        self,
        crypto::ring::sign::any_supported_type,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        server::{ClientHello, ResolvesServerCert},
        sign::CertifiedKey,
    },
    AcmeConfig, EventOk, ResolvesServerCertAcme,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

/// What the site needs; a change restarts it.
#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub domain: String,
    pub port: u16,
    /// Ask Let's Encrypt for a certificate (public domain names only).
    pub acme: bool,
    pub contact: Option<String>,
    pub cache: PathBuf,
    /// Also answer plain HTTP on port 80 with a redirect to HTTPS, like most sites.
    pub redirect_http: bool,
}

/// Certificate state shown on the page.
#[derive(Clone, Debug, PartialEq)]
pub enum Certificate {
    Starting,
    /// Self-signed; the text says why.
    SelfSigned(String),
    Issued,
    Failed(String),
}

/// Real certificate (and ACME challenges) first, a self-signed one otherwise.
#[derive(Debug)]
struct Resolver {
    acme: Option<Arc<ResolvesServerCertAcme>>,
    fallback: Arc<CertifiedKey>,
}
impl ResolvesServerCert for Resolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let Some(acme) = &self.acme else {
            return Some(self.fallback.clone());
        };
        if rustls_acme::is_tls_alpn_challenge(&hello) {
            return acme.resolve(hello);
        }
        acme.resolve(hello).or_else(|| Some(self.fallback.clone()))
    }
}

fn self_signed(domain: &str) -> Result<Arc<CertifiedKey>, String> {
    let generated =
        rcgen::generate_simple_self_signed(vec![domain.to_owned()]).map_err(|e| e.to_string())?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        generated.signing_key.serialize_der(),
    ));
    let key = any_supported_type(&key).map_err(|e| e.to_string())?;
    Ok(Arc::new(CertifiedKey::new(
        vec![CertificateDer::from(generated.cert.der().to_vec())],
        key,
    )))
}

/// Serve until cancelled. `report` receives certificate changes.
pub async fn run(
    spec: Spec,
    report: impl Fn(Certificate) + Send + Sync + 'static,
) -> Result<(), String> {
    report(Certificate::Starting);
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], spec.port)))
        .await
        .map_err(|e| {
            format!(
                "The camouflage site cannot listen on 127.0.0.1:{}: {e}",
                spec.port
            )
        })?;
    let fallback = self_signed(&spec.domain)?;
    let report = Arc::new(report);
    let acme = if spec.acme {
        let _ = std::fs::create_dir_all(&spec.cache);
        let mut config = AcmeConfig::new([spec.domain.clone()])
            .cache(DirCache::new(spec.cache.clone()))
            .directory_lets_encrypt(true);
        if let Some(c) = &spec.contact {
            config = config.contact_push(format!("mailto:{c}"));
        }
        let mut state = config.state();
        let resolver = state.resolver();
        report(Certificate::SelfSigned(
            "Getting a certificate from Let's Encrypt…".into(),
        ));
        let report = report.clone();
        tokio::spawn(async move {
            while let Some(event) = state.next().await {
                match event {
                    Ok(EventOk::DeployedCachedCert | EventOk::DeployedNewCert) => {
                        report(Certificate::Issued)
                    }
                    Ok(_) => {}
                    Err(e) => report(Certificate::Failed(e.to_string())),
                }
            }
        });
        Some(resolver)
    } else {
        report(Certificate::SelfSigned(
            "This name cannot get a public certificate.".into(),
        ));
        None
    };
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
    .map_err(|e| e.to_string())?
    .with_no_client_auth()
    .with_cert_resolver(Arc::new(Resolver { acme, fallback }));
    tls.alpn_protocols = vec![
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
        rustls_acme::acme::ACME_TLS_ALPN_NAME.to_vec(),
    ];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    if spec.redirect_http {
        let domain = spec.domain.clone();
        tokio::spawn(async move {
            // Optional: most hosts need root or CAP_NET_BIND_SERVICE for port 80.
            if let Ok(plain) = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 80))).await {
                serve(plain, None, domain).await;
            }
        });
    }
    serve(listener, Some(acceptor), spec.domain).await;
    Ok(())
}

async fn serve(listener: TcpListener, tls: Option<tokio_rustls::TlsAcceptor>, domain: String) {
    let domain: Arc<str> = domain.into();
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let (tls, domain) = (tls.clone(), domain.clone());
        tokio::spawn(async move {
            let secure = tls.is_some();
            let service = hyper::service::service_fn(move |req| {
                let domain = domain.clone();
                async move { Ok::<_, std::convert::Infallible>(respond(&req, &domain, secure)) }
            });
            let builder = auto::Builder::new(TokioExecutor::new());
            match tls {
                Some(tls) => {
                    // ACME validation connections end after the handshake; that is fine.
                    if let Ok(stream) = tls.accept(tcp).await {
                        let _ = builder
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                    }
                }
                None => {
                    let _ = builder.serve_connection(TokioIo::new(tcp), service).await;
                }
            }
        });
    }
}

fn respond(req: &Request<Incoming>, domain: &str, secure: bool) -> Response<Full<Bytes>> {
    let page = |status: StatusCode, kind: &str, body: String| {
        Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, kind)
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(Full::new(Bytes::from(body)))
            .unwrap_or_default()
    };
    if !secure {
        let target = format!(
            "https://{domain}{}",
            req.uri().path_and_query().map_or("/", |p| p.as_str())
        );
        return Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header(header::LOCATION, target)
            .body(Full::new(Bytes::new()))
            .unwrap_or_default();
    }
    match req.uri().path() {
        "/" | "/index.html" => page(StatusCode::OK, "text/html; charset=utf-8", home(domain)),
        "/robots.txt" => page(
            StatusCode::OK,
            "text/plain; charset=utf-8",
            "User-agent: *\nAllow: /\n".into(),
        ),
        _ => page(
            StatusCode::NOT_FOUND,
            "text/html; charset=utf-8",
            not_found(),
        ),
    }
}

fn home(domain: &str) -> String {
    let name = domain.split('.').next().unwrap_or(domain);
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{name}</title>
<style>body{{margin:0;font:16px/1.6 Georgia,serif;color:#222;background:#fafaf7}}main{{max-width:38rem;margin:4rem auto;padding:0 1.25rem}}h1{{font-weight:normal;font-size:1.8rem}}a{{color:#225}}footer{{margin-top:3rem;color:#888;font-size:.85rem}}</style>
</head><body><main>
<h1>{name}</h1>
<p>Notes on photography, small trips and the occasional recipe. Most of the old posts are being moved here from another host, so for now the archive is short.</p>
<p>New entries appear every few weeks.</p>
<footer>&copy; {domain}</footer>
</main></body></html>
"#
    )
}

fn not_found() -> String {
    "<!doctype html><html><head><title>404 Not Found</title></head><body><h1>Not Found</h1><p>The requested URL was not found on this server.</p></body></html>\n".into()
}
