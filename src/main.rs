use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dotenv::dotenv;
use ethers_signers::{LocalWallet, Signer};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, str::FromStr};
use tracing::info;

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt()
        .compact()
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .init();

    let app = Router::new()
        .route("/", get(root))
        .route("/transaction", post(submit_transaction));

    let port = std::env::var("PORT").map_or(3000, |s| s.parse().expect("Port should be a number"));
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
    let pk_hex_string = std::env::var("PK").unwrap();
    let wallet = LocalWallet::from_str(&pk_hex_string).unwrap();

    info!("Wallet with address {}", wallet.address());

    let txn = Transaction {
        data: payload.data,
        id: 1,
        to: payload.to,
        value: payload.value,
    };

    (StatusCode::OK, Json(txn))
}

#[derive(Deserialize, Debug)]
struct SubmitTransaction {
    data: String,
    to: String,
    value: String,
}

// the output to our `create_user` handler
#[derive(Serialize, Debug)]
struct Transaction {
    id: u64,
    data: String,
    to: String,
    value: String,
}
