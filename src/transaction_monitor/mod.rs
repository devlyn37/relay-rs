use ethers::{
    providers::{Middleware, StreamExt},
    types::{transaction::eip2718::TypedTransaction, Eip1559TransactionRequest, TxHash, U256},
};
mod transaction_repository;
use transaction_repository::{DbTxRequestRepository, Request, RequestUpdate};

use sqlx::MySqlPool;
use std::{cmp::max, pin::Pin, str::FromStr, sync::Arc};
use tracing::info;
use uuid::Uuid;

use tokio::{
    spawn,
    time::{sleep, Duration},
};

type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = TxHash> + Send + 'a>>;

#[derive(Debug)]
pub struct TransactionMonitor<M> {
    pub provider: Arc<M>,
    pub block_frequency: u8,
    pub tx_repo: DbTxRequestRepository,
}

impl<M> Clone for TransactionMonitor<M> {
    fn clone(&self) -> Self {
        TransactionMonitor {
            provider: self.provider.clone(),
            block_frequency: self.block_frequency.clone(),
            tx_repo: self.tx_repo.clone(),
        }
    }
}

impl<M> TransactionMonitor<M>
where
    M: Middleware + 'static,
{
    pub fn new(provider: M, block_frequency: u8, connection_pool: MySqlPool) -> Self {
        let this = Self {
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
        self.tx_repo.save(id, tx_hash, filled.into(), false).await?;

        Ok(id)
    }

    pub async fn get_transaction_status(&self, id: Uuid) -> anyhow::Result<(bool, String)> {
        let request = self.tx_repo.get(id).await?;
        Ok((request.mined, request.hash))
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
        self.bump_transaction(tx, estimate_max_fee, estimate_max_priority_fee);

        info!("Sending replacement transaction {:?}", tx);

        // TODO Even when tx has a nonce it will still increment it here for some reason, must be some sort of fallback thing in the provider
        // Reproduce by changing the mined to false in one of the completed requests in the db
        // this only happens when we haven't sent a transaction yet, checkout send_transaction in nonce manager if you want details!
        match self.provider.send_transaction(tx.clone(), None).await {
            Ok(new_tx_hash) => {
                info!("after tx was sent {:?}", tx);
                return Ok(Some(*new_tx_hash));
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

    fn bump_transaction(
        &self,
        tx: &mut Eip1559TransactionRequest,
        estimate_max_fee: U256,
        estimate_max_priority_fee: U256,
    ) {
        // We should never risk getting gas too low errors because we set these vals in send_monitored_transaction
        let prev_max_priority_fee = tx
            .max_priority_fee_per_gas
            .unwrap_or(estimate_max_priority_fee);
        let prev_max_fee = tx.max_fee_per_gas.unwrap_or(estimate_max_fee);

        let new_max_priority_fee = max(
            estimate_max_priority_fee,
            self.increase_by_minimum(prev_max_priority_fee),
        );

        let estimate_base_fee = estimate_max_fee - estimate_max_priority_fee;
        let prev_base_fee = prev_max_fee - prev_max_priority_fee;
        let new_base_fee = max(estimate_base_fee, self.increase_by_minimum(prev_base_fee));
        let new_max_fee = new_base_fee + new_max_priority_fee;

        info!(
            "before: max_fee: {:?}, max_priority_fee: {:?}",
            tx.max_fee_per_gas, tx.max_priority_fee_per_gas
        );

        tx.max_fee_per_gas = Some(new_max_fee);
        tx.max_priority_fee_per_gas = Some(new_max_priority_fee);

        info!(
            "after: max_fee: {:?}, max_priority_fee: {:?}",
            tx.max_fee_per_gas, tx.max_priority_fee_per_gas
        );
    }

    // Rule: both the tip and the max fee must
    // be bumped by a minimum of 10%
    // https://github.com/ethereum/go-ethereum/issues/23616#issuecomment-924657965
    fn increase_by_minimum(&self, value: U256) -> U256 {
        let increase = (value * 10) / 100u64;
        value + increase + 1 // add 1 here for rounding purposes
    }
}
