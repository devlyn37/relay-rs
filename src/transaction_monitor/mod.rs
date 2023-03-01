use ethers::{
    providers::{Middleware, StreamExt},
    types::{
        transaction::eip2718::TypedTransaction, Eip1559TransactionRequest, TxHash, H256, U256,
    },
};
use futures_util::lock::Mutex;
use sqlx::{query, MySqlPool};
use std::{cmp::max, pin::Pin, sync::Arc};
use tracing::info;
use uuid::Uuid;

use tokio::{
    spawn,
    time::{sleep, Duration},
};
type WatcherFuture<'a> = Pin<Box<dyn futures_util::stream::Stream<Item = H256> + Send + 'a>>;

#[derive(Debug)]
pub struct TransactionMonitor<M> {
    pub provider: Arc<M>,
    pub txs: Arc<Mutex<Vec<(TxHash, Eip1559TransactionRequest, Uuid)>>>, // Is the mutex really necessary here, we're only gonna have two tasks sharing this
    pub block_frequency: u8,
    pub connection_pool: MySqlPool,
}

impl<M> Clone for TransactionMonitor<M> {
    fn clone(&self) -> Self {
        TransactionMonitor {
            provider: self.provider.clone(),
            txs: self.txs.clone(),
            block_frequency: self.block_frequency.clone(),
            connection_pool: self.connection_pool.clone(),
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
            txs: Arc::new(Mutex::new(Vec::new())),
            block_frequency,
            connection_pool,
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
    ) -> Result<Uuid, anyhow::Error> {
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

        // insert the tx in the pending txs
        let mut lock = self.txs.lock().await;
        lock.push((*pending_tx, filled.clone().into(), id));

        Ok(id)
    }

    pub async fn get_transaction_status(&self, id: Uuid) -> String {
        let request = query!("SELECT * FROM requests WHERE id = ?", id.to_string())
            .fetch_one(&self.connection_pool)
            .await
            .expect("Could not find transaction in database"); // TODO 404 and all that jazz
        format!("{:?}", request.status)
    }

    pub async fn monitor(&self) -> Result<(), anyhow::Error> {
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
            let mut txs = self.txs.lock().await;
            let len = txs.len();

            for _ in 0..len {
                // this must never panic as we're explicitly within bounds
                let (tx_hash, mut replacement_tx, id) =
                    txs.pop().expect("should have element in vector");

                let tx_has_been_included = block
                    .transactions
                    .iter()
                    .find(|tx| tx.hash == tx_hash)
                    .is_some();

                if tx_has_been_included {
                    info!("transaction {:?} was included", tx_hash);
                    continue;
                }

                if block_count % self.block_frequency != 0 {
                    info!(
                        "transaction {:?} was not included, not sending replacement yet",
                        tx_hash
                    );
                    txs.push((tx_hash, replacement_tx, id));
                    continue;
                }

                info!("Rebroadcasting {:?}", tx_hash);
                match self
                    .rebroadcast(
                        &mut replacement_tx,
                        estimate_max_fee,
                        estimate_max_priority_fee,
                    )
                    .await?
                {
                    Some(new_txhash) => {
                        info!("Transaction {:?} replaced with {:?}", tx_hash, new_txhash);
                        txs.push((new_txhash, replacement_tx, id));
                        sleep(Duration::from_secs(1)).await; // to avoid rate limiting TODO add retries
                    }
                    None => {}
                }
            }
        }

        Ok(())
    }

    async fn rebroadcast(
        &self,
        tx: &mut Eip1559TransactionRequest,
        estimate_max_fee: U256,
        estimate_max_priority_fee: U256,
    ) -> Result<Option<H256>, anyhow::Error> {
        self.bump_transaction(tx, estimate_max_fee, estimate_max_priority_fee);

        match self.provider.send_transaction(tx.clone(), None).await {
            Ok(new_tx_hash) => {
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

// CREATE TABLE requests (
//   id varchar(255) NOT NULL PRIMARY KEY,
//   to_address varchar(42) NOT NULL,
// 	value varchar(78) NOT NULL,
// 	data varchar(255) NOT NULL,
// 	tx_hash varchar(66),
// 	status varchar(255) NOT NULL DEFAULT 'pending'
// );

// INSERT INTO requests (id, to_address, value, data, tx_hash)
// VALUES ('b55dae22-dfc7-4d39-bc20-48aa5e1197f9', '0xE898BBd704CCE799e9593a9ADe2c1cA0351Ab660', '100000', '0x0', '0x8097262014c0301ff70491a74cb9585549eae1a111ffb6a33318cc6d2e1958a0')

// #[derive(FromRow)]
// struct StoredTransaction {
//     id: Uuid,
//     to_address: Address,
//     value: Numeric,
//     data: String,
//     tx_hash: Option<String>,
// }
