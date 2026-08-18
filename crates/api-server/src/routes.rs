use axum::routing::{get, post};
use axum::Router;

pub fn router() -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/workshops", post(create_workshop).get(list_workshops))
        .route("/v1/workshops/:name", get(get_workshop).delete(delete_workshop))
}

async fn create_workshop() -> &'static str {
    // TODO: valider le JWT, construire le CR Workshop, le creer via kube::Api
    "not implemented"
}

async fn list_workshops() -> &'static str {
    // TODO: lister les Workshop appartenant au sujet JWT courant
    "not implemented"
}

async fn get_workshop() -> &'static str {
    "not implemented"
}

async fn delete_workshop() -> &'static str {
    "not implemented"
}
