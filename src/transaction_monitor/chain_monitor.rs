use ethers::{
    providers::{Middleware, StreamExt},
    types::{
        transaction::eip2718::TypedTransaction, Chain, Eip1559TransactionRequest, TxHash, U256,
    },
};

use std::{pin::Pin, sync::Arc};
use tracing::info;
use uuid::Uuid;

use tokio::{
    spawn,
    time::{sleep, Duration},
};

use super::gas_escalation::bump_transaction;
use crate::transaction_repository::{Request, RequestUpdate, TransactionRepository};

type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = TxHash> + Send + 'a>>;

#[derive(Debug)]
pub struct ChainMonitor<M, T> {
    pub provider: Arc<M>,
    pub chain: Chain,
    pub block_frequency: u8,
    pub tx_repo: Arc<T>,
}

impl<M, T> Clone for ChainMonitor<M, T> {
    fn clone(&self) -> Self {
        ChainMonitor {
            provider: self.provider.clone(),
            chain: self.chain,
            block_frequency: self.block_frequency,
            tx_repo: self.tx_repo.clone(),
        }
    }
}

impl<M, T> ChainMonitor<M, T>
where
    M: Middleware + 'static,
    T: TransactionRepository + 'static,
{
    pub fn new(provider: M, chain: Chain, block_frequency: u8, tx_repo: T) -> Self {
        let this = Self {
            chain,
            provider: Arc::new(provider),
            block_frequency,
            tx_repo: Arc::new(tx_repo),
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
            .save(id, tx_hash, filled.into(), false, self.chain)
            .await?;

        Ok(id)
    }

    pub async fn monitor(&self) -> anyhow::Result<()> {
        info!("Monitoring for escalation! chain = {}", self.chain);
        let mut watcher: WatcherFuture =
            Box::pin(self.provider.watch_blocks().await?.map(|hash| (hash)));
        let mut block_count = 0;

        while let Some(block_hash) = watcher.next().await {
            // We know the block exists at this point
            info!(
                "Block {:?} has been mined, chain = {}",
                block_hash, self.chain
            );
            block_count += 1;

            let block = self.provider.get_block_with_txs(block_hash).await?.unwrap();
            sleep(Duration::from_secs(1)).await; // to avoid rate limiting

            let (estimate_max_fee, estimate_max_priority_fee) =
                self.provider.estimate_eip1559_fees(None).await?;
            let requests = self.tx_repo.get_pending(self.chain).await?;
            let mut updates: Vec<RequestUpdate> = Vec::new();

            for request in requests {
                let Request { hash, id, .. } = request;
                let mut replacement_tx: Eip1559TransactionRequest = request.tx;

                let tx_has_been_included = block.transactions.iter().any(|tx| tx.hash == hash);

                if tx_has_been_included {
                    info!("transaction {:?} was included", hash);
                    updates.push(RequestUpdate {
                        id,
                        mined: true,
                        hash,
                    });
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
                        updates.push(RequestUpdate {
                            id,
                            mined: false,
                            hash: new_hash,
                        });
                        sleep(Duration::from_secs(1)).await; // to avoid rate limiting TODO add retries
                    }
                    None => {
                        updates.push(RequestUpdate {
                            id,
                            mined: true,
                            hash,
                        });
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
                Ok(Some(pending.tx_hash()))
            }
            Err(err) => {
                if err.to_string().contains("nonce too low") {
                    info!("transaction has already been included");
                    return Ok(None);
                }

                Err(anyhow::anyhow!(err))
            }
        }
    }
}
