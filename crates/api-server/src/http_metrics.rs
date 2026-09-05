//! Metriques HTTP minimales (spec `docs/specs/12-observabilite.md` §4.3) :
//! compteur de requetes + histogramme de latence, par route/methode/statut.
//!
//! Volontairement minimal — pas de metrique metier par endpoint a concevoir
//! une par une, juste de quoi prioriser ou creuser ensuite (voir `crate::
//! routes::TraceLayer`, meme logique pour les traces).

use axum::extract::MatchedPath;
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::metrics::{Counter, Histogram};
use opentelemetry::KeyValue;
use std::sync::OnceLock;
use std::time::Instant;

struct Metrics {
    requests: Counter<u64>,
    duration: Histogram<f64>,
}

/// Cree les instruments une seule fois par process : `opentelemetry::global::
/// meter` est bon marche a appeler plusieurs fois, mais les instruments
/// eux-memes doivent rester les MEMES d'un appel a l'autre pour que
/// l'agregation (compteur cumulatif, buckets de l'histogramme) ait un sens.
fn metrics() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let meter = opentelemetry::global::meter("atelier-api-server");
        Metrics {
            requests: meter
                .u64_counter("http.server.request_count")
                .with_description("Nombre de requetes HTTP servies")
                .build(),
            duration: meter
                .f64_histogram("http.server.duration")
                .with_description("Latence de traitement d'une requete HTTP")
                .with_unit("ms")
                .build(),
        }
    })
}

/// Middleware `axum::middleware::from_fn` : a poser via `.route_layer()`
/// (pas `.layer()`) pour que `MatchedPath` soit deja resolu — sans quoi
/// seul le chemin BRUT serait disponible (cardinalite non bornee pour les
/// routes du type `/v1/workshops/{name}/...`).
pub async fn record_http_metrics(
    matched_path: Option<MatchedPath>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let method = req.method().to_string();
    let route = matched_path
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    let attrs = [
        KeyValue::new("http.method", method),
        KeyValue::new("http.route", route),
        // `http.response.status_code`, pas l'ancien `http.status_code`
        // (deprecie) : c'est le nom attendu par le panneau "Error Rate" du
        // dashboard "RED Metrics" deja auto-provisionne par
        // `grafana/otel-lgtm` (verifie empiriquement — les panneaux
        // Request Rate/Duration fonctionnaient deja avec nos metriques,
        // seul celui-ci filtrait sur un nom d'attribut que nous ne
        // produisions pas).
        KeyValue::new(
            "http.response.status_code",
            response.status().as_u16() as i64,
        ),
    ];
    let m = metrics();
    m.requests.add(1, &attrs);
    m.duration.record(elapsed_ms, &attrs);
    response
}
