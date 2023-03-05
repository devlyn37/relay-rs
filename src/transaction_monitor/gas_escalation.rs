use ethers::types::{Eip1559TransactionRequest, U256};
use std::cmp::max;
use tracing::info;

pub fn bump_transaction(
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
        increase_by_minimum(prev_max_priority_fee),
    );

    let estimate_base_fee = estimate_max_fee - estimate_max_priority_fee;
    let prev_base_fee = prev_max_fee - prev_max_priority_fee;
    let new_base_fee = max(estimate_base_fee, increase_by_minimum(prev_base_fee));
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
fn increase_by_minimum(value: U256) -> U256 {
    let increase = (value * 10) / 100u64;
    value + increase + 1 // add 1 here for rounding purposes
}
