use axum::{
    extract::State, http::StatusCode, response::IntoResponse, response::Response, routing::post,
    Json, Router,
};
use dotenv::dotenv;
use ethers::{
    core::types::{serde_helpers::Numeric, Address, Eip1559TransactionRequest},
    middleware::{nonce_manager::NonceManagerMiddleware, signer::SignerMiddleware},
    providers::{Http, Provider},
    signers::{LocalWallet, Signer},
};
use serde::Deserialize;
use std::{fmt, net::SocketAddr, str::FromStr, sync::Arc};
use tracing::{info, Level};
mod transaction_monitor;
pub use transaction_monitor::TransactionMonitor;

type ConfigedProvider = NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>;
type ConfigedMonitor = TransactionMonitor<ConfigedProvider>;

#[derive(Debug)]
struct AppState {
    monitor: ConfigedMonitor,
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
    let provider = NonceManagerMiddleware::new(provider, address);
    let monitor: ConfigedMonitor = TransactionMonitor::new(provider, 2);

    let shared_state = Arc::new(AppState { monitor });

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
) -> Result<Json<String>, AppError> {
    let request = Eip1559TransactionRequest::new()
        .to(payload.to)
        .value(payload.value)
        .data(payload.data);
    info!("Transaction: {:?}", request);
    let id = state
        .monitor
        .send_monitored_transaction(request, None)
        .await?;

    Ok(Json(id.to_string()))
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

struct AppError(anyhow::Error);
// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
