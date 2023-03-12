use ethers::{
    providers::{Middleware, StreamExt},
    types::{
        transaction::eip2718::TypedTransaction, Chain, Eip1559TransactionRequest, TxHash, U256,
    },
};

use sqlx::MySqlPool;
use std::{pin::Pin, str::FromStr, sync::Arc};
use tracing::info;
use uuid::Uuid;

use tokio::{
    spawn,
    time::{sleep, Duration},
};

mod transaction_repository;
use transaction_repository::{DbTxRequestRepository, Request, RequestUpdate};

mod gas_escalation;
use gas_escalation::bump_transaction;

type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = TxHash> + Send + 'a>>;

#[derive(Debug)]
pub struct TransactionMonitor<M> {
    pub provider: Arc<M>,
    pub chain: Chain,
    pub block_frequency: u8,
    pub tx_repo: DbTxRequestRepository,
}

impl<M> Clone for TransactionMonitor<M> {
    fn clone(&self) -> Self {
        TransactionMonitor {
            provider: self.provider.clone(),
            chain: self.chain.clone(),
            block_frequency: self.block_frequency.clone(),
            tx_repo: self.tx_repo.clone(),
        }
    }
}

impl<M> TransactionMonitor<M>
where
    M: Middleware + 'static,
{
    pub fn new(provider: M, chain: Chain, block_frequency: u8, connection_pool: MySqlPool) -> Self {
        let this = Self {
            chain,
            provider: Arc::new(provider),
            block_frequency,
            tx_repo: DbTxRequestRepository::new(connection_pool),
        };

        {
            let this2 = this.clone();
            spawn(async move {
                this2.monitor().await.unwrap();
            });
        }

        this
    }

    pub async fn send_monitored_transaction(
        &self,
        tx: Eip1559TransactionRequest,
    ) -> anyhow::Result<Uuid> {
        let mut with_gas = tx.clone();
        if with_gas.max_fee_per_gas.is_none() || with_gas.max_priority_fee_per_gas.is_none() {
            let (estimate_max_fee, estimate_max_priority_fee) =
                self.provider.estimate_eip1559_fees(None).await?;
            with_gas.max_fee_per_gas = Some(estimate_max_fee);
            with_gas.max_priority_fee_per_gas = Some(estimate_max_priority_fee);
        }
        let mut filled: TypedTransaction = with_gas.clone().into();
        self.provider.fill_transaction(&mut filled, None).await?;
        info!("Filled Transaction {:?}", filled);

        let pending_tx = self.provider.send_transaction(filled.clone(), None).await?;
        let id = Uuid::new_v4();
        let tx_hash = pending_tx.tx_hash();
        self.tx_repo
            .save(id, tx_hash, filled.into(), false, self.chain as u32)
            .await?;

        Ok(id)
    }

    pub async fn get_transaction_status(&self, id: Uuid) -> anyhow::Result<Option<(bool, String)>> {
        let request = self.tx_repo.get(id).await?;
        Ok(request.map(|req| (req.mined, req.hash)))
    }

    pub async fn monitor(&self) -> anyhow::Result<()> {
        info!("Monitoring for escalation!");
        let mut watcher: WatcherFuture =
            Box::pin(self.provider.watch_blocks().await?.map(|hash| (hash)));
        let mut block_count = 0;

        while let Some(block_hash) = watcher.next().await {
            // We know the block exists at this point
            info!("Block {:?} has been mined", block_hash);
            block_count = block_count + 1;

            let block = self.provider.get_block_with_txs(block_hash).await?.unwrap();
            sleep(Duration::from_secs(1)).await; // to avoid rate limiting

            let (estimate_max_fee, estimate_max_priority_fee) =
                self.provider.estimate_eip1559_fees(None).await?;
            let requests = self.tx_repo.get_pending().await?;
            let mut updates: Vec<RequestUpdate> = Vec::new();

            for request in requests {
                let Request { hash, id, .. } = request;
                let hash = TxHash::from_str(&hash)?;
                let id = Uuid::from_str(&id)?;
                let mut replacement_tx: Eip1559TransactionRequest = request.tx.0.into();

                let tx_has_been_included = block
                    .transactions
                    .iter()
                    .find(|tx| tx.hash == hash)
                    .is_some();

                if tx_has_been_included {
                    info!("transaction {:?} was included", hash);
                    updates.push((id, true, hash));
                    continue;
                }

                if block_count % self.block_frequency != 0 {
                    info!(
                        "transaction {:?} was not included, not sending replacement yet",
                        hash
                    );
                    continue;
                }

                info!("Rebroadcasting {:?}", hash);
                match self
                    .rebroadcast(
                        &mut replacement_tx,
                        estimate_max_fee,
                        estimate_max_priority_fee,
                    )
                    .await?
                {
                    Some(new_hash) => {
                        info!("Transaction {:?} replaced with {:?}", hash, new_hash);
                        updates.push((id, false, new_hash));
                        sleep(Duration::from_secs(1)).await; // to avoid rate limiting TODO add retries
                    }
                    None => {
                        updates.push((id, true, hash));
                    }
                }
            }

            self.tx_repo.update_many(updates).await?;
        }

        Ok(())
    }

    async fn rebroadcast(
        &self,
        tx: &mut Eip1559TransactionRequest,
        estimate_max_fee: U256,
        estimate_max_priority_fee: U256,
    ) -> anyhow::Result<Option<TxHash>> {
        bump_transaction(tx, estimate_max_fee, estimate_max_priority_fee);

        info!("Sending replacement transaction {:?}", tx);
        match self.provider.send_transaction(tx.clone(), None).await {
            Ok(pending) => {
                info!("after tx was sent {:?}", tx);
                return Ok(Some(pending.tx_hash()));
            }
            Err(err) => {
                if err.to_string().contains("nonce too low") {
                    info!("transaction has already been included");
                    return Ok(None);
                }

                return Err(anyhow::anyhow!(err));
            }
        };
    }
}
