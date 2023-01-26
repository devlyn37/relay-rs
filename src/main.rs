use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .compact()
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .init();

    let app = Router::new()
        .route("/", get(root))
        .route("/transaction", post(submit_transaction));

    let port = 3000;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Listening on port {}", port);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn root() -> &'static str {
    "Hello, World!"
}

async fn submit_transaction(Json(payload): Json<SubmitTransaction>) -> impl IntoResponse {
    let user = Transaction {
        id: 1337,
        data: payload.data,
    };

    (StatusCode::OK, Json(user))
}

#[derive(Deserialize)]
struct SubmitTransaction {
    data: String,
}

// the output to our `create_user` handler
#[derive(Serialize)]
struct Transaction {
    id: u64,
    data: String,
}
