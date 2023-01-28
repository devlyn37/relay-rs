use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dotenv::dotenv;
use ethers_core::types::{
    transaction::eip2718::TypedTransaction, Address, Eip1559TransactionRequest, U256,
};
use ethers_middleware::SignerMiddleware;
use ethers_providers::{Http, Middleware, Provider};
use ethers_signers::{LocalWallet, Signer};
use serde::Deserialize;
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
    let provider_url = std::env::var("PROVIDER_URL").unwrap();
    let signer = LocalWallet::from_str(&pk_hex_string).unwrap();
    let address: Address = payload.to.parse().unwrap();
    let provider =
        Provider::<Http>::try_from(&provider_url).expect("could not instantiate HTTP Provider");

    let txn = TypedTransaction::Eip1559(
        Eip1559TransactionRequest::new()
            .to(address)
            .value(U256::from_dec_str(&payload.value).unwrap()),
    );

    info!(
        "Wallet with address {}, is sending transaction {:?}",
        signer.address(),
        txn
    );

    let client = SignerMiddleware::new_with_provider_chain(provider.clone(), signer.clone())
        .await
        .unwrap();
    let pending_tx = client
        .send_transaction(txn, None)
        .await
        .expect("Something went wrong when sending transaction");

    info!("Transaction sent, waiting for it to be mined");

    let receipt = pending_tx
        .confirmations(2)
        .await
        .expect("Something went wrong while waiting for confirmations");

    info!(
        "Transaction mined with two confirmations, here's the receipt {:?}",
        receipt
    );

    (StatusCode::OK, Json(receipt))
}

#[derive(Deserialize, Debug)]
struct SubmitTransaction {
    to: String,
    value: String,
}
