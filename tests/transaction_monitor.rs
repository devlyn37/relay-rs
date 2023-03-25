use ethers::{
    providers::{Middleware, Provider, Ws},
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

    let anvil = Anvil::new().block_time(1u64).spawn();
    let chain_id = anvil.chain_id();

    println!("The chain id is {}", chain_id);

    let provider = Provider::<Ws>::connect(anvil.ws_endpoint())
        .await
        .expect("Should be able to connect to anvil");

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

    let (mined, hash) = result.unwrap();
    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}
