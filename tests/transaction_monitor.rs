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
use std::sync::Once;
use tokio::time::{sleep, Duration};

static INIT: Once = Once::new();

pub fn initialize() {
    INIT.call_once(|| {
        tracing_subscriber::fmt()
            .compact()
            .with_file(true)
            .with_line_number(true)
            .with_level(true)
            .with_max_level(Level::INFO)
            .init();
    });
}

#[sqlx::test]
async fn transaction_monitor_happy_path(pool: Pool<MySql>) {
    initialize();
    let anvil = Anvil::new()
        .args(vec!["--no-mining", "--base-fee", "50"])
        .spawn();
    let provider =
        Provider::<Http>::try_from(anvil.endpoint()).expect("Should be able to connect to anvil");
    let wallet: LocalWallet = anvil.keys().first().unwrap().clone().into();
    let wallet = wallet.with_chain_id(anvil.chain_id());

    let recipient = anvil.addresses()[1];

    let mut monitor = TransactionMonitor::new(DbTxRequestRepository::new(pool));
    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    let request = Eip1559TransactionRequest::new().to(recipient).value(1);
    let id = monitor
        .send_monitored_transaction(request, Chain::AnvilHardhat)
        .await
        .unwrap();

    // Send the first request
    let result = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error");
    assert!(result.is_some());
    let (mined, hash) = result.unwrap();
    assert!(!mined);
    println!("mined {}, hash {}", mined, hash);

    println!("Mine the block");
    provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping, waiting for the monitor to process");
    sleep(Duration::from_secs(15)).await; // let some blocks get mined

    let result = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status should work");
    assert!(result.is_some());

    let (mined, hash) = result.expect("The result should be some");
    let receipt = provider
        .get_transaction_receipt(hash)
        .await
        .expect("Grabbing the transaction hash should work");
    println!("Here's the receipt to show the tx was mined\n{:?}", receipt);

    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}

#[sqlx::test]
async fn transaction_monitor_resubmission(pool: Pool<MySql>) {
    initialize();
    let anvil = Anvil::new()
        .args(vec!["--no-mining", "--base-fee", "50"])
        .spawn();
    let provider =
        Provider::<Http>::try_from(anvil.endpoint()).expect("Should be able to connect to anvil");
    let wallet: LocalWallet = anvil.keys().first().unwrap().clone().into();
    let wallet = wallet.with_chain_id(anvil.chain_id());

    let recipient = anvil.addresses()[1];

    let mut monitor = TransactionMonitor::new(DbTxRequestRepository::new(pool));
    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    let request = Eip1559TransactionRequest::new().to(recipient).value(1);
    let id = monitor
        .send_monitored_transaction(request, Chain::AnvilHardhat)
        .await
        .unwrap();

    // Send the first request
    let result = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error");
    assert!(result.is_some());
    let (mined, hash) = result.unwrap();
    assert!(!mined);
    println!("mined {}, hash {}", mined, hash);

    // Drop the transaction so it doesn't get mined
    provider
        .request::<_, U256>("anvil_dropTransaction", vec![format!("{:?}", hash)])
        .await
        .expect("dropping transaction should work");

    // Mine a block so that the monitor resubmits the tx
    provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping, waiting for the monitor to resubmit");
    sleep(Duration::from_secs(15)).await;

    println!("Mining another block");
    provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping, waiting for the monitor to process");
    sleep(Duration::from_secs(15)).await; // let some blocks get mined

    let result = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status should work");
    assert!(result.is_some());

    let (mined, hash) = result.expect("The result should be some");
    let receipt = provider
        .get_transaction_receipt(hash)
        .await
        .expect("Grabbing the transaction hash should work");
    println!("Here's the receipt to show the tx was mined\n{:?}", receipt);

    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}
