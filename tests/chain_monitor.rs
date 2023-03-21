use ethers::{
    providers::Http,
    providers::Provider,
    signers::{LocalWallet, Signer},
    types::*,
    utils::Anvil,
};

use relay::transaction_monitor::TransactionMonitor;
use relay::transaction_repository::DbTxRequestRepository;
use sqlx::{MySql, Pool};

#[sqlx::test]
async fn chain_monitor(pool: Pool<MySql>) {
    let anvil = Anvil::new().spawn();
    let chain_id = anvil.chain_id();
    let provider = Provider::<Http>::try_from(anvil.endpoint()).unwrap();

    let wallet: LocalWallet = anvil.keys().first().unwrap().clone().into();
    let wallet = wallet.with_chain_id(chain_id);

    let recipient = anvil.addresses()[1];

    let repo = DbTxRequestRepository::new(pool);
    let mut monitor = TransactionMonitor::new(repo);
    monitor
        .setup_monitor(wallet, provider, Chain::AnvilHardhat, 1)
        .await
        .unwrap();

    let request = Eip1559TransactionRequest::new().to(recipient).value(1);
    let id = monitor
        .send_monitored_transaction(request, Chain::AnvilHardhat)
        .await
        .unwrap();

    let result = monitor.get_transaction_status(id).await.unwrap();
    println!("Here's the result {:?}", result);
    assert!(result.is_some());
}
