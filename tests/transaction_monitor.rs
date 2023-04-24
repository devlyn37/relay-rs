use ethers::{
    prelude::k256::ecdsa::SigningKey,
    providers::{Http, Middleware, Provider},
    signers::{LocalWallet, Signer, Wallet},
    types::*,
    utils::{Anvil, AnvilInstance},
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
    let mut monitor = TransactionMonitor::new(DbTxRequestRepository::new(pool));

    let (anvil, provider, wallet) = setup_chain(31337, 8545).await;
    let recipient = anvil.addresses()[1];

    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    // Send a request to one chain

    let id = monitor
        .send_monitored_transaction(
            Eip1559TransactionRequest::new().to(recipient).value(1),
            Chain::AnvilHardhat,
        )
        .await
        .unwrap();

    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    assert!(!mined);
    println!("mined {}, hash {}", mined, hash);

    // Send a request to the other
    let id = monitor
        .send_monitored_transaction(
            Eip1559TransactionRequest::new().to(recipient).value(1),
            Chain::AnvilHardhat,
        )
        .await
        .unwrap();

    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    assert!(!mined);
    println!("mined {}, hash {}", mined, hash);

    println!("Mine the block");
    provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping, waiting for the monitor to process");
    sleep(Duration::from_secs(15)).await; // let some blocks get mined

    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");

    let receipt = provider
        .get_transaction_receipt(hash)
        .await
        .expect("Grabbing the transaction hash should work");
    println!("Here's the receipt to show the tx was mined\n{:?}", receipt);

    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}

#[sqlx::test]
async fn transaction_monitor_multiple_chains(pool: Pool<MySql>) {
    initialize();
    let mut monitor = TransactionMonitor::new(DbTxRequestRepository::new(pool));

    let (anvil, provider, wallet) = setup_chain(31337, 8545).await;
    let recipient = anvil.addresses()[1];
    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .expect("monitor setup should work");

    let (mock_goerli, mock_goerli_provider, mock_goerli_wallet) = setup_chain(5, 3006).await;
    let mock_goerli_recipient = mock_goerli.addresses()[1];

    monitor
        .setup_monitor(
            mock_goerli_wallet,
            mock_goerli_provider.clone(),
            Chain::Goerli,
            1,
        )
        .await
        .expect("monitor setup should work");

    // Send a request on the first chain
    let id = monitor
        .send_monitored_transaction(
            Eip1559TransactionRequest::new().to(recipient).value(1),
            Chain::AnvilHardhat,
        )
        .await
        .unwrap();
    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    println!("mined {}, hash {:?}", mined, hash);

    // Send a request on the second chain
    let goerli_request_id = monitor
        .send_monitored_transaction(
            Eip1559TransactionRequest::new()
                .to(mock_goerli_recipient)
                .value(1),
            Chain::Goerli,
        )
        .await
        .expect("Sending the transaction should work");
    let (goerli_mined, goerli_hash) = monitor
        .get_transaction_status(goerli_request_id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    assert!(!goerli_mined);
    println!("goerli: mined {}, hash {:?}", goerli_mined, goerli_hash);

    // Drop the transactions on both chains so they must be resubmitted
    println!(
        "Dropping transaction {:?} on chain one, and {:?} on chain two",
        hash, goerli_hash
    );

    mock_goerli_provider
        .request::<_, U256>("anvil_dropTransaction", vec![format!("{:?}", goerli_hash)])
        .await
        .expect("dropping transaction should work");

    println!(
        "Dropped the first transaction, dropping {:?} now",
        format!("{:?}", hash)
    );

    provider
        .request::<_, U256>("anvil_dropTransaction", vec![format!("{:?}", hash)])
        .await
        .expect("dropping transaction should work");

    println!("Mining blocks with dropped transactions on both chains");

    provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    mock_goerli_provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping, waiting for the monitor to resubmit");
    sleep(Duration::from_secs(15)).await;

    println!("mining a block on both chains");
    provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");
    mock_goerli_provider
        .request::<_, U256>("evm_mine", None::<()>)
        .await
        .expect("mining should work");

    println!("Sleeping, waiting for the monitor to process");
    sleep(Duration::from_secs(15)).await; // let some blocks get mined

    // Check that transactions have been mined on the correct chains
    // and that the monitor has marked them accordingly

    println!(
        "Checking that tx {:?} has been mined on chain {:?}",
        hash,
        Chain::AnvilHardhat
    );
    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    let receipt = provider
        .get_transaction_receipt(hash)
        .await
        .expect("Grabbing the transaction hash should work");
    assert!(receipt.is_some());
    assert!(mined);

    let (goerli_mined, goerli_hash) = monitor
        .get_transaction_status(goerli_request_id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    println!("mined {}, hash {}", goerli_mined, goerli_hash);
    println!(
        "Checking that tx {:?} has been mined on chain {:?}",
        goerli_hash,
        Chain::Goerli
    );
    let goerli_receipt = mock_goerli_provider
        .get_transaction_receipt(goerli_hash)
        .await
        .expect("Grabbing the transaction hash should work");
    assert!(goerli_receipt.is_some());
    assert!(goerli_mined);
}

#[sqlx::test]
async fn transaction_monitor_resubmission(pool: Pool<MySql>) {
    initialize();
    let mut monitor = TransactionMonitor::new(DbTxRequestRepository::new(pool));

    let (anvil, provider, wallet) = setup_chain(31337, 8545).await;
    let recipient = anvil.addresses()[1];

    monitor
        .setup_monitor(wallet, provider.clone(), Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    let id = monitor
        .send_monitored_transaction(
            Eip1559TransactionRequest::new().to(recipient).value(1),
            Chain::AnvilHardhat,
        )
        .await
        .unwrap();

    // Send the first request
    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
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

    let (mined, hash) = monitor
        .get_transaction_status(id)
        .await
        .expect("Grabbing transaction status not error")
        .expect("Status should exist");
    let receipt = provider
        .get_transaction_receipt(hash)
        .await
        .expect("Grabbing the transaction hash should work");
    println!("Here's the receipt to show the tx was mined\n{:?}", receipt);

    println!("mined {}, hash {}", mined, hash);
    assert!(mined);
}

async fn setup_chain(
    chain_id: u64,
    port: u16,
) -> (AnvilInstance, Provider<Http>, Wallet<SigningKey>) {
    let anvil = Anvil::new()
        .chain_id(chain_id)
        .port(port)
        .args(vec!["--no-mining", "--base-fee", "50"])
        .spawn();
    let provider =
        Provider::<Http>::try_from(anvil.endpoint()).expect("Should be able to connect to anvil");
    let wallet: LocalWallet = anvil.keys().first().unwrap().clone().into();
    let wallet = wallet.with_chain_id(anvil.chain_id());

    return (anvil, provider, wallet);
}
