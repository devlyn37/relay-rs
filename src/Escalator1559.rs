use anyhow::Context;
use ethers::{
    prelude::{NonceManagerMiddleware, SignerMiddleware},
    providers::{Http, Middleware, Provider, StreamExt},
    signers::LocalWallet,
    types::{
        transaction::eip2718::TypedTransaction, BlockId, Eip1559TransactionRequest, TxHash, H256,
        U256,
    },
};
use futures_util::lock::Mutex;
use std::{cmp::max, pin::Pin, sync::Arc};
use tracing::{info, trace};
use uuid::Uuid;

use tokio::{
    spawn,
    time::{sleep, Duration},
};
type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = H256> + Send + 'a>>;

#[derive(Debug)]
pub struct TransactionMonitor {
    pub provider: Arc<NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>>,
    pub txs: Arc<Mutex<Vec<(TxHash, Eip1559TransactionRequest, Option<BlockId>, Uuid)>>>,
    pub block_frequency: u8,
}

impl Clone for TransactionMonitor {
    fn clone(&self) -> Self {
        TransactionMonitor {
            provider: self.provider.clone(),
            txs: self.txs.clone(),
            block_frequency: self.block_frequency.clone(),
        }
    }
}

impl TransactionMonitor {
    pub fn new(
        inner: NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>,
        block_frequency: u8,
    ) -> Self
    where
        NonceManagerMiddleware<SignerMiddleware<Provider<Http>, LocalWallet>>: 'static,
    {
        let this = Self {
            provider: Arc::new(inner),
            txs: Arc::new(Mutex::new(Vec::new())),
            block_frequency,
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
        block: Option<BlockId>,
    ) -> Result<Uuid, anyhow::Error> {
        let mut with_gas = tx.clone();
        if with_gas.max_fee_per_gas.is_none() || with_gas.max_priority_fee_per_gas.is_none() {
            let (estimate_max_fee, estimate_max_priority_fee) = self
                .provider
                .estimate_eip1559_fees(None)
                .await
                .with_context(|| "error estimating gas")?;
            with_gas.max_fee_per_gas = Some(estimate_max_fee);
            with_gas.max_priority_fee_per_gas = Some(estimate_max_priority_fee);
        }
        let mut filled: TypedTransaction = with_gas.clone().into();
        self.provider
            .fill_transaction(&mut filled, None)
            .await
            .with_context(|| "error while filling transaction")?;

        info!("Filled Transaction {:?}", filled);

        let pending_tx = self
            .provider
            .send_transaction(filled.clone(), block)
            .await
            .with_context(|| "error sending transaction")?;

        let id = Uuid::new_v4();

        // insert the tx in the pending txs
        let mut lock = self.txs.lock().await;
        lock.push((*pending_tx, filled.clone().into(), block, id));

        Ok(id)
    }

    pub async fn monitor(&self) -> Result<(), anyhow::Error> {
        info!("Monitoring for escalation!");
        let mut watcher: WatcherFuture = Box::pin(
            self.provider
                .watch_blocks()
                .await
                .with_context(|| "Block streaming failure")?
                .map(|hash| (hash)),
        );
        let mut block_count = 0;

        while let Some(block_hash) = watcher.next().await {
            // We know the block exists at this point
            info!("Block {:?} has been mined", block_hash);
            block_count = block_count + 1;

            if block_count % self.block_frequency != 0 {
                info!("Not checking on transaction this block");
                continue;
            }

            let block = self
                .provider
                .get_block_with_txs(block_hash)
                .await
                .with_context(|| "error while fetching block")?
                .unwrap();
            sleep(Duration::from_secs(1)).await; // to avoid rate limiting

            let mut txs = self.txs.lock().await;
            let (estimate_max_fee, estimate_max_priority_fee) = self
                .provider
                .estimate_eip1559_fees(None)
                .await
                .with_context(|| "error estimating gas prices")?;

            let len = txs.len();
            info!("checking transactions");
            for _ in 0..len {
                // this must never panic as we're explicitly within bounds
                let (tx_hash, mut replacement_tx, priority, id) =
                    txs.pop().expect("should have element in vector");

                let receipt = block.transactions.iter().find(|tx| tx.hash == tx_hash);
                info!("checking if transaction {:?} was included", tx_hash);

                if receipt.is_some() {
                    info!("transaction {:?} was included", tx_hash);
                    continue;
                }

                // gas values here will always exist because we set them in `send_transaction`
                info!(
                    "transaction {:?} was not included, sending replacement",
                    tx_hash
                );
                let prev_max_priority_fee = replacement_tx
                    .max_priority_fee_per_gas
                    .expect("max priority fee per gas must be set");
                let prev_max_fee = replacement_tx
                    .max_fee_per_gas
                    .expect("max fee per gas must be set");

                let (new_max_fee, new_max_priority_fee) = replacement_gas_values(
                    prev_max_fee,
                    prev_max_priority_fee,
                    estimate_max_fee,
                    estimate_max_priority_fee,
                );

                replacement_tx.max_priority_fee_per_gas = Some(new_max_priority_fee);
                replacement_tx.max_fee_per_gas = Some(new_max_fee);
                trace!("replacement transaction {:?}", replacement_tx);
                info!(
                    "old: max priority fee {prev_max_priority_fee}, max fee: {prev_max_fee}\nnew: max priority fee {new_max_priority_fee}, max fee {new_max_fee}"
                );

                // the tx hash will be different so we need to update it
                let new_txhash = match self
                    .provider
                    .send_transaction(replacement_tx.clone(), priority)
                    .await
                {
                    Ok(new_tx_hash) => {
                        let new_tx_hash = *new_tx_hash;
                        new_tx_hash
                    }
                    Err(err) => {
                        if err.to_string().contains("nonce too low") {
                            // ignore "nonce too low" errors because they
                            // may happen if we try to broadcast a higher
                            // gas price tx when one of the previous ones
                            // was already mined (meaning we also do not
                            // push it back to the pending txs vector)
                            info!(
                                "transaction ${:?} was included, replacement not needed",
                                tx_hash
                            );
                            continue;
                        } else {
                            return Err(anyhow::anyhow!(err));
                        }
                    }
                };

                info!("Transaction {:?} replaced with {:?}", tx_hash, new_txhash);
                sleep(Duration::from_secs(1)).await; // to avoid rate limiting TODO add retries

                txs.push((new_txhash, replacement_tx, priority, id));
            }
        }

        Ok(())
    }
}

fn replacement_gas_values(
    prev_max_fee: U256,
    prev_max_priority_fee: U256,
    estimate_max_fee: U256,
    estimate_max_priority_fee: U256,
) -> (U256, U256) {
    let new_max_priority_fee = max(
        estimate_max_priority_fee,
        increase_by_minimum(prev_max_priority_fee),
    );

    let estimate_base_fee = estimate_max_fee - estimate_max_priority_fee;
    let prev_base_fee = prev_max_fee - prev_max_priority_fee;
    let new_base_fee = max(estimate_base_fee, increase_by_minimum(prev_base_fee));

    return (new_base_fee + new_max_priority_fee, new_max_priority_fee);
}

// Rule: both the tip and the max fee must
// be bumped by a minimum of 10%
// https://github.com/ethereum/go-ethereum/issues/23616#issuecomment-924657965
fn increase_by_minimum(value: U256) -> U256 {
    let increase = (value * 10) / 100u64;
    value + increase + 1 // add 1 here for rounding purposes
}
