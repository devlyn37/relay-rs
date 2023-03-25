use anyhow::Context;
use ethers::{
    prelude::{
        k256::ecdsa::SigningKey, MiddlewareBuilder, NonceManagerMiddleware, SignerMiddleware,
    },
    providers::{Middleware, Provider, Ws},
    signers::{LocalWallet, Signer, Wallet},
    types::{Chain, Eip1559TransactionRequest, TxHash},
};

use std::collections::HashMap;
use uuid::Uuid;

use crate::transaction_repository::{DbTxRequestRepository, TransactionRepository};
mod chain_monitor;
use chain_monitor::ChainMonitor;
mod gas_escalation;

type ConfigedProvider = NonceManagerMiddleware<SignerMiddleware<Provider<Ws>, LocalWallet>>;
type ConfigedMonitor = ChainMonitor<ConfigedProvider, DbTxRequestRepository>;

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

    pub async fn get_transaction_status(&self, id: Uuid) -> anyhow::Result<Option<(bool, TxHash)>> {
        let request = self.tx_repo.get(id).await?;
        Ok(request.map(|req| (req.mined, req.hash)))
    }

    pub async fn send_monitored_transaction(
        &self,
        tx: Eip1559TransactionRequest,
        chain: Chain,
    ) -> anyhow::Result<Uuid> {
        let monitor = self
            .monitors
            .get(&chain)
            .expect(&format!("monitor for chain {} not defined", chain));
        monitor.send_monitored_transaction(tx).await
    }

    pub async fn setup_monitor(
        &mut self,
        signer: Wallet<SigningKey>,
        provider: Provider<Ws>,
        chain: Chain,
        block_frequency: u8,
    ) -> anyhow::Result<()> {
        let address = signer.address();
        let chain_id = provider.get_chainid().await?;
        let signer = signer.with_chain_id(chain_id.as_u64());
        let configed = provider.with_signer(signer).nonce_manager(address);
        configed
            .initialize_nonce(None)
            .await
            .with_context(|| "Could not init nonce")?;

        self.monitors.insert(
            chain,
            ChainMonitor::new(configed, chain, block_frequency, self.tx_repo.clone()),
        );

        Ok(())
    }
}
