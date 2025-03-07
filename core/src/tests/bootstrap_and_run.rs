use std::convert::TryInto;

use crate::cli_commands::bootstrap::perform_bootstrap_chained_transaction;
use crate::cli_commands::bootstrap::BootstrapConfig;
use crate::cli_commands::bootstrap::BootstrapInput;
use crate::node_interface::test_utils::{ChainSubmitTx, MockNodeApi, SubmitTxMock};
use crate::oracle_config::BASE_FEE;
use crate::oracle_types::BlockHeight;
use crate::pool_commands::test_utils::init_log_tests;
use crate::pool_config::PoolConfig;
use ergo_chain_sim::ChainSim;
use ergo_lib::chain::ergo_state_context::ErgoStateContext;
use ergo_lib::ergotree_interpreter::sigma_protocol::private_input::DlogProverInput;
use ergo_lib::ergotree_ir::chain::address::Address;
use ergo_lib::ergotree_ir::chain::address::NetworkAddress;
use ergo_lib::ergotree_ir::chain::address::NetworkPrefix;
use ergo_lib::wallet::secret_key::SecretKey;
use sigma_test_util::force_any_val;

fn bootstrap(
    secrets: Vec<SecretKey>,
    net_address: &NetworkAddress,
    chain: &mut ChainSim,
) -> PoolConfig {
    let ctx = force_any_val::<ErgoStateContext>();

    let unspent_boxes = chain.get_unspent_boxes(&net_address.address().script().unwrap());

    let bootstrap_config = BootstrapConfig::default();

    let height = BlockHeight(ctx.pre_header.height);
    let mut submit_tx_mock = ChainSubmitTx {
        chain: chain.into(),
    };
    perform_bootstrap_chained_transaction(BootstrapInput {
        oracle_address: net_address.clone(),
        config: bootstrap_config,
        node_api: &MockNodeApi {
            unspent_boxes: unspent_boxes.clone(),
            ctx: ctx.clone(),
            secrets,
            chain_submit_tx: Some(&mut submit_tx_mock),
            submitted_txs: &SubmitTxMock::default().transactions,
        },
        tx_fee: *BASE_FEE,
        erg_value_per_box: *BASE_FEE,
        change_address: net_address.address(),
        height,
    })
    .unwrap()
    .0
}

#[test]
fn test_bootstrap_and_run() {
    init_log_tests();
    let mut chain = ChainSim::new();
    let secret = force_any_val::<DlogProverInput>();
    let net_address = NetworkAddress::new(
        NetworkPrefix::Mainnet,
        &Address::P2Pk(secret.public_image()),
    );
    chain.generate_unspent_box(
        net_address.address().script().unwrap(),
        100_000_000_u64.try_into().unwrap(),
        None,
    );
    let _oracle_config = bootstrap(vec![secret.clone().into()], &net_address, &mut chain);
    assert_eq!(chain.height, 8);
}
