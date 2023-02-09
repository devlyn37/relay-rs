use async_trait::async_trait;
use ethers::{
    providers::{FromErr, Middleware, PendingTransaction, StreamExt},
    types::{transaction::eip2718::TypedTransaction, BlockId, Eip1559TransactionRequest, TxHash},
};
use futures_util::lock::Mutex;
use std::{pin::Pin, sync::Arc};
use thiserror::Error;
use tracing::info;

use tokio::spawn;
type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = ()> + Send + 'a>>;

#[derive(Debug)]
pub struct Escalator1559Middleware<M> {
    pub inner: Arc<M>,
    pub txs: Arc<Mutex<Vec<(TxHash, Eip1559TransactionRequest, Option<BlockId>)>>>,
}

impl<M> Clone for Escalator1559Middleware<M> {
    fn clone(&self) -> Self {
        Escalator1559Middleware {
            inner: self.inner.clone(),
            txs: self.txs.clone(),
        }
    }
}

#[async_trait]
impl<M> Middleware for Escalator1559Middleware<M>
where
    M: Middleware,
{
    type Error = GasEscalatorError<M>;
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

        info!("Hi sending a transaction from the escalator");
        info!("{:?}", tx);

        let pending_tx = self
            .inner()
            .send_transaction(tx.clone(), block)
            .await
            .map_err(GasEscalatorError::MiddlewareError)?;

        let tx = match tx {
            TypedTransaction::Eip1559(inner) => inner,
            _ => return Err(GasEscalatorError::UnsupportedTxType),
        };

        // insert the tx in the pending txs
        let mut lock = self.txs.lock().await;
        lock.push((*pending_tx, tx, block));

        Ok(pending_tx)
    }
}

impl<M> Escalator1559Middleware<M>
where
    M: Middleware,
{
    /// Initializes the middleware with the provided gas escalator and the chosen
    /// escalation frequency (per block or per second)
    pub fn new(inner: M) -> Self
    where
        M: Clone + 'static,
    {
        let this = Self {
            inner: Arc::new(inner),
            txs: Arc::new(Mutex::new(Vec::new())),
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
    pub async fn escalate(&self) -> Result<(), GasEscalatorError<M>> {
        info!("Monitoring for escalation!");
        let mut watcher: WatcherFuture = Box::pin(
            self.inner
                .watch_blocks()
                .await
                .map_err(GasEscalatorError::MiddlewareError)?
                .map(|_| ()),
        );

        while watcher.next().await.is_some() {
            let mut txs = self.txs.lock().await;
            let len = txs.len();

            for _ in 0..len {
                // this must never panic as we're explicitly within bounds
                let (tx_hash, mut replacement_tx, priority) =
                    txs.pop().expect("should have element in vector");

                let receipt = self.get_transaction_receipt(tx_hash).await?;
                info!(tx_hash = ?tx_hash, "checking if exists");

                if receipt.is_none() {
                    info!("Transaction wasn't mined in the latest block, we're going to send a replacement out");
                    let old_priority_fee = replacement_tx
                        .max_priority_fee_per_gas
                        .expect("max priority fee per gas needs to be set");
                    let old_max_fee_per_gas = replacement_tx
                        .max_fee_per_gas
                        .expect("max fee per gas needs to be set");
                    let old_base_fee = old_max_fee_per_gas - old_priority_fee;

                    let (max_fee_estimate, priority_fee_estimate) =
                        self.estimate_eip1559_fees(None).await.unwrap();
                    let base_fee_estimate = max_fee_estimate - priority_fee_estimate;

                    // Rule: both the tip and the max fee must be bumped by a minimum of 10% https://github.com/ethereum/go-ethereum/issues/23616#issuecomment-924657965
                    let replacement_priority_fee_minimum = old_priority_fee * 110 / 100; // TODO fix possible rounding issues here
                    let mut new_priority_fee = replacement_priority_fee_minimum; // TODO use some greater(x, y) type thing here, idk what the meta is in rust
                    if priority_fee_estimate > new_priority_fee {
                        new_priority_fee = priority_fee_estimate;
                    }

                    let replacement_base_fee_minimum = old_base_fee * 110 / 100;
                    let mut new_base_fee = replacement_base_fee_minimum;
                    if base_fee_estimate > new_base_fee {
                        new_base_fee = base_fee_estimate;
                    }

                    replacement_tx.max_priority_fee_per_gas = Some(new_priority_fee);
                    replacement_tx.max_fee_per_gas = Some(new_base_fee + new_priority_fee);
                    info!("{:?}", replacement_tx);
                    info!("old max priority fee {}", old_priority_fee);
                    info!("new max priority fee {}", new_priority_fee);

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
                                continue;
                            } else {
                                return Err(GasEscalatorError::MiddlewareError(err));
                            }
                        }
                    };

                    txs.push((new_txhash, replacement_tx, priority));
                }
            }
        }

        Ok(())
    }
}

// Boilerplate
impl<M: Middleware> FromErr<M::Error> for GasEscalatorError<M> {
    fn from(src: M::Error) -> GasEscalatorError<M> {
        GasEscalatorError::MiddlewareError(src)
    }
}

#[derive(Error, Debug)]
/// Error thrown when the GasEscalator interacts with the blockchain
pub enum GasEscalatorError<M: Middleware> {
    #[error("{0}")]
    /// Thrown when an internal middleware errors
    MiddlewareError(M::Error),

    #[error("only 1559 transaction supported")]
    UnsupportedTxType,
}
