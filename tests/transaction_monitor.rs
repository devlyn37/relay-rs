use ethers::{
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::*,
    utils::Anvil,
};
use tracing::Level;

use relay::transaction_monitor::TransactionMonitor;
use relay::transaction_repository::DbTxRequestRepository;
use sqlx::{MySql, Pool};
use tokio::time::{sleep, Duration};

#[sqlx::test]
async fn chain_monitor_happy_path(pool: Pool<MySql>) {
    tracing_subscriber::fmt()
        .compact()
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .with_max_level(Level::INFO)
        .init();

    // Need to either set the gas values such the test txn doesn't mine
    // fallback to dropping it from the mempool
    let anvil = Anvil::new()
        .args(vec![
            "--no-mining",
            "--gas-price",
            "500",
            "--base-fee",
            "50",
        ])
        .spawn();
    let chain_id = anvil.chain_id();

    let provider =
        Provider::<Http>::try_from(anvil.endpoint()).expect("Should be able to connect to anvil");
    let wallet: LocalWallet = anvil.keys().first().unwrap().clone().into();
    let wallet = wallet.with_chain_id(chain_id);

    let recipient = anvil.addresses()[1];

    let repo = DbTxRequestRepository::new(pool);
    let mut monitor = TransactionMonitor::new(repo);
    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    let request = Eip1559TransactionRequest::new()
        .to(recipient)
        .value(1)
        .max_fee_per_gas(100)
        .max_priority_fee_per_gas(1);
    let id = monitor
        .send_monitored_transaction(request, Chain::AnvilHardhat)
        .await
        .unwrap();

    let result = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error");
    assert!(result.is_some());
    let (mined, hash) = result.unwrap();
    assert!(!mined);
    println!("mined {}, hash {}", mined, hash);

    println!("Mining a block");
    let test = provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping while waiting for some blocks to mine...");
    sleep(Duration::from_secs(15)).await; // let some blocks get mined
    println!("Done sleeping!");
    let receipt = provider
        .get_transaction_receipt(hash)
        .await
        .expect("Grabbing the transaction hash should work");
    println!("{:?}", receipt);

    let block_number = provider
        .get_block_number()
        .await
        .expect("Fetching the block number should work");
    println!("Here's the latest block number {:?}", block_number);

    let result = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status should work");
    assert!(result.is_some());

    let (mined, hash) = result.expect("The result should be some");
    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}
