use anyhow::{Context, Error};
use axum::{
    extract::State, http::StatusCode, response::IntoResponse, response::Response, routing::post,
    Json, Router,
};
use dotenv::dotenv;
use ethers::{
    core::types::{serde_helpers::Numeric, Address, Eip1559TransactionRequest},
    middleware::{
        gas_escalator::{Frequency, GeometricGasPrice},
        nonce_manager::NonceManagerMiddleware,
        signer::SignerMiddleware,
    },
    prelude::gas_escalator::GasEscalatorMiddleware,
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::TransactionReceipt,
    utils::__serde_json::json,
};
use serde::Deserialize;
use std::{net::SocketAddr, str::FromStr, sync::Arc};
use tracing::info;

type ConfigedProvider = NonceManagerMiddleware<
    SignerMiddleware<GasEscalatorMiddleware<Provider<Http>, GeometricGasPrice>, LocalWallet>,
>;

struct AppState {
    provider: ConfigedProvider,
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt()
        .compact()
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .init();

    let pk_hex_string =
        std::env::var("PK").expect("Server not configured correctly, no private key");
    let provider_url =
        std::env::var("PROVIDER_URL").expect("Server not configured correctly, no provider url");
    let port = std::env::var("PORT").map_or(3000, |s| s.parse().expect("Port should be a number"));

    let signer = LocalWallet::from_str(&pk_hex_string)
        .expect("Server not configured correct, invalid private key");
    let address = signer.address();

    let provider = Provider::<Http>::try_from(provider_url)
        .expect("Server not configured correctly, invalid provider url");
    let escalator = GeometricGasPrice::new(1.125, 60u64, None::<u64>);
    let provider = GasEscalatorMiddleware::new(provider, escalator, Frequency::PerBlock);
    let provider = SignerMiddleware::new_with_provider_chain(provider, signer)
        .await
        .expect("Could not connect to provider");
    let provider = NonceManagerMiddleware::new(provider, address);

    let shared_state = Arc::new(AppState { provider });

    let app = Router::new()
        .route("/transaction", post(relay_transaction))
        .with_state(shared_state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Listening on port {}", port);
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn relay_transaction(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SubmitTransaction>,
) -> Result<Json<Option<TransactionReceipt>>, AppError> {
    let request = Eip1559TransactionRequest::new()
        .to(payload.to)
        .value(payload.value)
        .data(payload.data);
    let pending_tx = state
        .provider
        .send_transaction(request, None)
        .await
        .with_context(|| "Error submitting transaction")?;

    let hash = pending_tx.tx_hash();
    info!("Transaction sent, hash: {:?}.", hash);

    let receipt = pending_tx.confirmations(2).await.unwrap();

    info!("{:?} mined.", hash);

    Ok(Json(receipt))
}

#[derive(Deserialize)]
struct SubmitTransaction {
    to: Address,
    value: Numeric,
    #[serde(with = "hex::serde")]
    data: Vec<u8>,
}

#[derive(Debug)]
enum AppError {
    Standard(Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let error_message = format!("{:?}", self);
        let body = Json(json!({ "error": error_message }));
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

impl From<Error> for AppError {
    fn from(inner: Error) -> Self {
        AppError::Standard(inner)
    }
}
