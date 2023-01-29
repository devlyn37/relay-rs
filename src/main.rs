use anyhow::{anyhow, Context, Error};
use axum::{
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use dotenv::dotenv;
use ethers_core::types::{
    serde_helpers::Numeric, Address, Eip1559TransactionRequest, TransactionReceipt,
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
    let pk_hex_string =
        std::env::var("PK").expect("Server not configured correctly, no private key");
    let provider_url =
        std::env::var("PROVIDER_URL").expect("Server not configured correctly, no provider url");
    let signer = LocalWallet::from_str(&pk_hex_string)
        .expect("Server not configured correct, invalid private key");

    let provider = Provider::<Http>::try_from(&provider_url)
        .expect("Server not configured correctly, provider url or connection invalid");

    let request = Eip1559TransactionRequest::new()
        .to(payload.to)
        .value(payload.value)
        .data(payload.data);

    let client = SignerMiddleware::new_with_provider_chain(provider.clone(), signer.clone())
        .await
        .unwrap();

    let receipt = handle_transaction(request, client)
        .await
        .expect("Something went wrong when submitting transaction");

    (StatusCode::OK, Json(receipt))
}

async fn handle_transaction(
    txn_request: Eip1559TransactionRequest,
    client: SignerMiddleware<Provider<Http>, LocalWallet>,
) -> Result<TransactionReceipt, Error> {
    info!(
        "Wallet with address {}, is sending transaction {:?}",
        client.signer().address(),
        txn_request
    );

    let pending_tx = client
        .send_transaction(txn_request, None)
        .await
        .with_context(|| "Error submitting transaction")?;

    info!(
        "Transaction sent, hash: {}.\nWaiting for it to be mined...",
        pending_tx.tx_hash()
    );

    pending_tx
        .confirmations(2)
        .await?
        .ok_or(anyhow!("Transaction was not mined"))
}

#[derive(Deserialize)]
struct SubmitTransaction {
    to: Address,
    value: Numeric,
    #[serde(with = "hex::serde")]
    data: Vec<u8>,
}
