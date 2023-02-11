use async_trait::async_trait;
use ethers::{
    providers::{FromErr, Middleware, PendingTransaction, StreamExt},
    types::{
        transaction::eip2718::TypedTransaction, BlockId, Eip1559TransactionRequest, TxHash, H256,
        U256,
    },
};
use futures_util::lock::Mutex;
use std::{cmp::max, pin::Pin, sync::Arc};
use thiserror::Error;
use tracing::{info, trace};

use tokio::{
    spawn,
    time::{sleep, Duration},
};
type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = H256> + Send + 'a>>;

#[derive(Debug)]
pub struct EIP1559BlockEscalator<M> {
    pub inner: Arc<M>,
    pub txs: Arc<Mutex<Vec<(TxHash, Eip1559TransactionRequest, Option<BlockId>)>>>,
    pub block_frequency: u8,
}

impl<M> Clone for EIP1559BlockEscalator<M> {
    fn clone(&self) -> Self {
        EIP1559BlockEscalator {
            inner: self.inner.clone(),
            txs: self.txs.clone(),
            block_frequency: self.block_frequency.clone(),
        }
    }
}

#[async_trait]
impl<M> Middleware for EIP1559BlockEscalator<M>
where
    M: Middleware,
{
    type Error = EIP1559BlockEscalatorError<M>;
    type Provider = M::Provider;
    type Inner = M;

    fn inner(&self) -> &M {
        &self.inner
    }

    async fn send_transaction<T: Into<TypedTransaction> + Send + Sync>(
        &self,
        tx: T,
        block: Option<BlockId>,
    ) -> Result<PendingTransaction<'_, Self::Provider>, Self::Error> {
        let tx = tx.into();

        let mut tx = match tx {
            TypedTransaction::Eip1559(inner) => inner,
            _ => return Err(EIP1559BlockEscalatorError::UnsupportedTxType),
        };

        if tx.max_fee_per_gas.is_none() || tx.max_priority_fee_per_gas.is_none() {
            let (estimate_max_fee, estimate_max_priority_fee) =
                self.estimate_eip1559_fees(None).await?;
            tx.max_fee_per_gas = Some(estimate_max_fee);
            tx.max_priority_fee_per_gas = Some(estimate_max_priority_fee);
        }

        let pending_tx = self
            .inner()
            .send_transaction(tx.clone(), block)
            .await
            .map_err(EIP1559BlockEscalatorError::MiddlewareError)?;

        // insert the tx in the pending txs
        let mut lock = self.txs.lock().await;
        lock.push((*pending_tx, tx, block));

        Ok(pending_tx)
    }
}

impl<M> EIP1559BlockEscalator<M>
where
    M: Middleware,
{
    /// Initializes the middleware with the provided gas escalator and the chosen
    /// escalation frequency (per block or per second)
    pub fn new(inner: M, block_frequency: u8) -> Self
    where
        M: Clone + 'static,
    {
        let this = Self {
            inner: Arc::new(inner),
            txs: Arc::new(Mutex::new(Vec::new())),
            block_frequency,
        };

        {
            let this2 = this.clone();
            spawn(async move {
                this2.escalate().await.unwrap();
            });
        }

        this
    }

    /// Re-broadcasts pending transactions with a gas price escalator
    pub async fn escalate(&self) -> Result<(), EIP1559BlockEscalatorError<M>> {
        info!("Monitoring for escalation!");
        let mut watcher: WatcherFuture = Box::pin(
            self.inner
                .watch_blocks()
                .await
                .map_err(EIP1559BlockEscalatorError::MiddlewareError)?
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
                .get_block_with_txs(block_hash)
                .await?
                .expect("Block must exist");
            sleep(Duration::from_secs(1)).await; // to avoid rate limiting

            let mut txs = self.txs.lock().await;

            let (estimate_max_fee, estimate_max_priority_fee) =
                self.estimate_eip1559_fees(None).await.unwrap();

            let len = txs.len();
            info!("checking transactions");
            for _ in 0..len {
                // this must never panic as we're explicitly within bounds
                let (tx_hash, mut replacement_tx, priority) =
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
                    .inner()
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
                            return Err(EIP1559BlockEscalatorError::MiddlewareError(err));
                        }
                    }
                };

                info!("Transaction {:?} replaced with {:?}", tx_hash, new_txhash);
                sleep(Duration::from_secs(1)).await; // to avoid rate limiting TODO add retries

                txs.push((new_txhash, replacement_tx, priority));
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

// Boilerplate
impl<M: Middleware> FromErr<M::Error> for EIP1559BlockEscalatorError<M> {
    fn from(src: M::Error) -> EIP1559BlockEscalatorError<M> {
        EIP1559BlockEscalatorError::MiddlewareError(src)
    }
}

#[derive(Error, Debug)]
/// Error thrown when the GasEscalator interacts with the blockchain
pub enum EIP1559BlockEscalatorError<M: Middleware> {
    #[error("{0}")]
    /// Thrown when an internal middleware errors
    MiddlewareError(M::Error),

    #[error("only 1559 transaction supported")]
    UnsupportedTxType,
}
