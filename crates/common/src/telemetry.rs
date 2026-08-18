//! Initialisation OpenTelemetry commune a tous les binaires d'Atelier.
//!
//! Convention : chaque `main.rs` appelle `atelier_common::telemetry::init("nom-du-binaire")`
//! avant toute autre chose, et garde le `TelemetryGuard` renvoye en vie
//! jusqu'a la fin de `main` (son `Drop` flush/ferme l'exporteur OTLP).
//!
//! Sans `OTEL_EXPORTER_OTLP_ENDPOINT` dans l'environnement (cas des tests
//! d'integration et du dev local sans collecteur), on retombe sur un simple
//! logging `tracing_subscriber::fmt`, sans exporter de traces.

use opentelemetry::global;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[must_use = "conserver ce guard jusqu'a la fin de main pour flush les traces"]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            if let Err(err) = provider.shutdown() {
                eprintln!("erreur a l'arret du tracer provider OpenTelemetry: {err}");
            }
        }
    }
}

/// Plusieurs dependances (kube, kanidm_client, ...) compilent chacune leur
/// propre choix par defaut de provider crypto rustls (`aws-lc-rs`, `ring`),
/// ce qui rend le choix automatique ambigu au premier usage TLS et fait
/// paniquer rustls. On tranche explicitement une fois pour tout le
/// processus. Idempotent : safe a appeler plusieurs fois (ex: depuis les
/// tests d'integration, qui n'appellent pas `init()`).
pub fn ensure_crypto_provider() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn init(service_name: &str) -> TelemetryGuard {
    ensure_crypto_provider();
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer();

    let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt_layer)
            .init();
        return TelemetryGuard { provider: None };
    };

    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .expect("construction de l'exporteur OTLP");

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_service_name(service_name.to_string())
                .build(),
        )
        .build();

    global::set_tracer_provider(provider.clone());
    let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, service_name.to_string());
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    TelemetryGuard {
        provider: Some(provider),
    }
}
