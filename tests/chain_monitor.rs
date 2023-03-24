use ethers::{
    providers::Http,
    providers::{Middleware, Provider},
    signers::{LocalWallet, Signer},
    types::*,
    utils::Anvil,
};

use relay::transaction_monitor::TransactionMonitor;
use relay::transaction_repository::DbTxRequestRepository;
use sqlx::{MySql, Pool};
use tokio::time::{sleep, Duration};

#[sqlx::test]
async fn chain_monitor_happy_path(pool: Pool<MySql>) {
    let anvil = Anvil::new().block_time(1u64).spawn();
    let chain_id = anvil.chain_id();

    println!("The chain id is {}", chain_id);

    let provider = Provider::<Http>::try_from(anvil.endpoint()).unwrap();

    let wallet: LocalWallet = anvil.keys().first().unwrap().clone().into();
    let wallet = wallet.with_chain_id(chain_id);

    let recipient = anvil.addresses()[1];

    let repo = DbTxRequestRepository::new(pool);
    let mut monitor = TransactionMonitor::new(repo);
    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    let request = Eip1559TransactionRequest::new().to(recipient).value(1);
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

    let block_number = provider
        .get_block_number()
        .await
        .expect("Fetching block number should work");
    println!("Here's the latest block number {:?}", block_number);

    println!("Sleeping while waiting for some blocks to mine...");
    sleep(Duration::from_secs(8)).await; // let some blocks get mined
    println!("Done sleeping!");

    let formatted_hash =
        TxHash::from_slice(&hex::decode(&hash[2..]).expect("transaction hash should be valid"));
    let receipt = provider
        .get_transaction_receipt(formatted_hash)
        .await
        .expect("hash");
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

    let (mined, hash) = result.unwrap();
    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}
