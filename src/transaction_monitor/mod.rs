use ethers::{
    prelude::{k256::ecdsa::SigningKey, NonceManagerMiddleware, SignerMiddleware},
    providers::{Http, Provider},
    signers::{LocalWallet, Signer, Wallet},
    types::{Chain, Eip1559TransactionRequest},
};

use std::collections::HashMap;
use uuid::Uuid;

use crate::transaction_repository::DbTxRequestRepository;
mod chain_monitor;
use chain_monitor::ChainMonitor;
mod gas_escalation;

type ConfigedProvider = NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>;
type ConfigedMonitor = ChainMonitor<ConfigedProvider>;

#[derive(Debug)]
pub struct TransactionMonitor {
    pub tx_repo: DbTxRequestRepository,
    monitors: HashMap<Chain, ConfigedMonitor>,
}

impl TransactionMonitor {
    pub fn new(tx_repo: DbTxRequestRepository) -> Self {
        Self {
            tx_repo: tx_repo,
            monitors: HashMap::new(),
        }
    }

    pub async fn get_transaction_status(&self, id: Uuid) -> anyhow::Result<Option<(bool, String)>> {
        let request = self.tx_repo.get(id).await?;
        Ok(request.map(|req| (req.mined, req.hash)))
    }

    pub async fn send_monitored_transaction(
        &self,
        tx: Eip1559TransactionRequest,
        chain: Chain,
    ) -> anyhow::Result<Uuid> {
        let monitor = self.monitors.get(&chain);

        if let Some(monitor) = monitor {
            monitor.send_monitored_transaction(tx).await
        } else {
            anyhow::bail!("chain not supported, todo fix later")
        }
    }

    pub async fn setup_monitor(
        &mut self,
        signer: Wallet<SigningKey>,
        provider: Provider<Http>,
        chain: Chain,
        block_frequency: u8,
    ) -> anyhow::Result<()> {
        let address = signer.address();
        let provider = SignerMiddleware::new_with_provider_chain(provider, signer)
            .await
            .expect("Could not connect to provider");
        let provider = NonceManagerMiddleware::new(provider, address);
        provider
            .initialize_nonce(None)
            .await
            .expect("Could not initialize nonce");

        self.monitors.insert(
            chain,
            ChainMonitor::new(provider, chain, block_frequency, self.tx_repo.clone()),
        );

        Ok(())
    }
}
