use axum::{
    extract::State, http::StatusCode, response::IntoResponse, response::Response, routing::post,
    Json, Router,
};
use dotenv::dotenv;
use ethers::{
    core::types::{serde_helpers::Numeric, Address, Eip1559TransactionRequest},
    middleware::{nonce_manager::NonceManagerMiddleware, signer::SignerMiddleware},
    prelude::nonce_manager::NonceManagerError,
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::TransactionReceipt,
    utils::__serde_json::json,
};
use serde::Deserialize;
use std::{fmt, net::SocketAddr, str::FromStr, sync::Arc};
use tracing::{error, info, Level};
mod escalator1559;

type Base = escalator1559::Escalator1559Middleware<SignerMiddleware<Provider<Http>, LocalWallet>>;
type ConfigedProvider = NonceManagerMiddleware<Base>;
type ConfigedProviderError = NonceManagerError<Base>;

#[derive(Debug)]
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
        .with_max_level(Level::INFO)
        .init();
    // console_subscriber::init();

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
    let provider = SignerMiddleware::new_with_provider_chain(provider, signer)
        .await
        .expect("Could not connect to provider");
    let provider = escalator1559::Escalator1559Middleware::new(provider);
    let provider: ConfigedProvider = NonceManagerMiddleware::new(provider, address);

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

// #[tracing::instrument]
async fn relay_transaction(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RelayRequest>,
) -> Result<Json<Option<TransactionReceipt>>, AppError> {
    let (max_fee_estimate, priority_fee_estimate) =
        state.provider.estimate_eip1559_fees(None).await.unwrap();
    let base_fee_estimate = max_fee_estimate - priority_fee_estimate;
    let request = Eip1559TransactionRequest::new()
        .to(payload.to)
        .value(payload.value)
        .data(payload.data)
        .max_fee_per_gas(base_fee_estimate + 1)
        .max_priority_fee_per_gas(1); // TODO fix this is just for testing
    info!("Transaction: {:?}", request);
    let pending_tx = state.provider.send_transaction(request, None).await?;
    let hash = pending_tx.tx_hash();
    info!("Transaction sent, hash: {:?}.", hash);
    info!("Pending txn: {:?}", pending_tx);

    let receipt = pending_tx.confirmations(2).await.unwrap();
    tracing::info!("{:?} mined.", hash);

    let test = state
        .provider
        .get_transaction_receipt(hash)
        .await
        .unwrap()
        .unwrap();

    tracing::info!("receipt {:?}", test);

    Ok(Json(receipt))
}

#[derive(Deserialize)]
struct RelayRequest {
    to: Address,
    value: Numeric,
    #[serde(with = "hex::serde")]
    data: Vec<u8>,
}

impl fmt::Debug for RelayRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay Request")
            .field("to", &self.to)
            .field("data", &self.data) // TODO add value here
            .finish()
    }
}

#[derive(Debug)]
enum AppError {
    ProviderError(ConfigedProviderError),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        error!("{:?}", self);
        let body = Json(json!({"error": "connection issues, try again later!"}));
        (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
    }
}

impl From<ConfigedProviderError> for AppError {
    fn from(inner: ConfigedProviderError) -> Self {
        AppError::ProviderError(inner)
    }
}
