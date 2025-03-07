use ergo_lib::{
    chain::{
        ergo_box::box_builder::ErgoBoxCandidateBuilder,
        ergo_box::box_builder::ErgoBoxCandidateBuilderError,
    },
    ergo_chain_types::blake2b256_hash,
    ergotree_interpreter::sigma_protocol::prover::ContextExtension,
    ergotree_ir::chain::{
        address::Address,
        ergo_box::{NonMandatoryRegisterId},
    },
    ergotree_ir::serialization::SigmaSerializable,
    wallet::{
        box_selector::{BoxSelection, BoxSelectorError},
        signing::TxSigningError,
        tx_builder::{TxBuilder, TxBuilderError},
    },
};
use ergo_node_interface::node_interface::NodeError;
use log::{error, info};
use std::convert::TryInto;
use ergo_lib::chain::transaction::unsigned::UnsignedTransaction;
use ergo_lib::ergotree_ir::chain::address::NetworkAddress;
use ergo_lib::wallet::box_selector::{BoxSelector, SimpleBoxSelector};
use ergo_lib::wallet::signing::TransactionContext;
use crate::{
    box_kind::{
        make_pool_box_candidate_unchecked, BallotBox, CastBallotBoxVoteParameters, PoolBox,
        PoolBoxWrapper, VoteBallotBoxWrapper,
    },
    contracts::pool::PoolContract,
    explorer_api::ergo_explorer_transaction_link,
    oracle_config::BASE_FEE,
    oracle_state::{
        DataSourceError, OraclePool, PoolBoxSource, UpdateBoxSource, VoteBallotBoxesSource,
    },
    oracle_types::BlockHeight,
    pool_config::{PoolConfig, POOL_CONFIG},
    spec_token::{RewardTokenId, SpecToken, TokenIdKind},
};
use thiserror::Error;
use crate::node_interface::node_api::NodeApiTrait;
use crate::oracle_config::ORACLE_CONFIG;

#[derive(Debug, Error)]
pub enum UpdatePoolError {
    #[error("Update pool: Not enough votes for {2:?}, expected {0}, found {1}")]
    NotEnoughVotes(usize, usize, CastBallotBoxVoteParameters),
    #[error("Update pool: Pool parameters (refresh NFT, update NFT) unchanged")]
    PoolUnchanged,
    #[error("Update pool: ErgoBoxCandidateBuilderError {0}")]
    ErgoBoxCandidateBuilder(#[from] ErgoBoxCandidateBuilderError),
    #[error("Update pool: box selector error {0}")]
    BoxSelector(#[from] BoxSelectorError),
    #[error("Update pool: tx builder error {0}")]
    TxBuilder(#[from] TxBuilderError),
    #[error("Update pool: tx context error {0}")]
    TxSigningError(#[from] TxSigningError),
    #[error("Update pool: data source error {0}")]
    DataSourceError(#[from] DataSourceError),
    #[error("Update pool: node error {0}")]
    Node(#[from] NodeError),
    #[error("No change address in node")]
    NoChangeAddressSetInNode,
    #[error("Update pool: pool contract error {0}")]
    PoolContractError(#[from] crate::contracts::pool::PoolContractError),
    #[error("Update pool: io error {0}")]
    IoError(#[from] std::io::Error),
    #[error("Update pool: yaml error {0}")]
    YamlError(#[from] serde_yaml::Error),
    #[error("Update pool: could not find unspent wallot boxes that do not contain ballot tokens")]
    NoUsableWalletBoxes,
}

pub fn update_pool(
    op: &OraclePool,
    node_api: &dyn NodeApiTrait,
    new_reward_tokens: Option<SpecToken<RewardTokenId>>,
    height: BlockHeight,
) -> Result<(), anyhow::Error> {
    info!("Opening pool_config_updated.yaml");
    let s = std::fs::read_to_string("pool_config_updated.yaml")?;
    let new_pool_config: PoolConfig = serde_yaml::from_str(&s)?;
    if let Some(ref reward_token) = new_reward_tokens {
        assert_eq!(
            reward_token.token_id,
            new_pool_config.token_ids.reward_token_id,
            "Reward token id in pool_config_updated.yaml does not match the one from the command line"
        );
    }
    let oracle_address = ORACLE_CONFIG.oracle_address.clone();
    let (change_address, network_prefix) = {
        let net_addr = ORACLE_CONFIG.change_address.clone().unwrap();
        (net_addr.address(), net_addr.network())
    };

    let new_pool_contract =
        PoolContract::checked_load(&new_pool_config.pool_box_wrapper_inputs.contract_inputs)?;
    let new_pool_box_hash = blake2b256_hash(
        &new_pool_contract
            .ergo_tree()
            .sigma_serialize_bytes()
            .unwrap(),
    );

    display_update_diff(
        &POOL_CONFIG,
        &new_pool_config,
        op.get_pool_box_source().get_pool_box()?,
        new_reward_tokens.clone(),
    );

    let context = build_update_pool_box_tx(
        op.get_pool_box_source(),
        op.get_ballot_boxes_source(),
        node_api,
        op.get_update_box_source(),
        new_reward_tokens.clone(),
        height,
        oracle_address,
        change_address,
        new_pool_contract,
    )?;

    log::debug!("Signing update pool box tx: {:#?}", context);
    let signed_tx = node_api.sign_transaction(context)?;

    println!(
        "YOU WILL BE SUBMITTING AN UPDATE TO THE POOL CONTRACT:\
           - Hash of new pool box contract: {}",
        String::from(new_pool_box_hash),
    );
    if let Some(reward_token) = new_reward_tokens {
        println!(
            "  - Reward token Id: {}\
               - Reward token amount: {}",
            String::from(reward_token.token_id.token_id()),
            reward_token.amount.as_u64(),
        );
    }
    println!("TYPE 'YES' TO SUBMIT THE TRANSACTION.");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    if input.trim_end() == "YES" {
        let tx_id_str = node_api.submit_transaction(&signed_tx)?;
        crate::explorer_api::wait_for_tx_confirmation(signed_tx.id());
        println!(
            "Update pool box transaction submitted: view here, {}",
            ergo_explorer_transaction_link(tx_id_str, network_prefix)
        );
        println!("Send the new pool_config_updated.yaml to the oracle operators.");
        println!("The operators should import it with `import-pool-update` command.");
        remind_send_minted_tokens_to_oracles(&POOL_CONFIG, &new_pool_config);
    } else {
        println!("Aborting the transaction.")
    }
    Ok(())
}

fn display_update_diff(
    old_pool_config: &PoolConfig,
    new_pool_config: &PoolConfig,
    old_pool_box: PoolBoxWrapper,
    new_reward_tokens: Option<SpecToken<RewardTokenId>>,
) {
    let new_pool_contract =
        PoolContract::checked_load(&new_pool_config.pool_box_wrapper_inputs.contract_inputs)
            .unwrap();
    println!("Pool Parameters: ");
    let pool_box_hash = blake2b256_hash(
        &new_pool_contract
            .ergo_tree()
            .sigma_serialize_bytes()
            .unwrap(),
    );
    println!("Pool Box Hash (new): {}", String::from(pool_box_hash));
    if old_pool_config.token_ids.reward_token_id != new_pool_config.token_ids.reward_token_id {
        println!(
            "Reward Token ID (old): {}",
            String::from(old_pool_config.token_ids.reward_token_id.token_id())
        );
        println!(
            "Reward Token ID (new): {}",
            String::from(new_pool_config.token_ids.reward_token_id.token_id())
        );
        println!(
            "Reward Token Amount (old): {}",
            old_pool_box.reward_token().amount.as_u64()
        );
        println!(
            "Reward Token Amount (new): {}",
            new_reward_tokens.unwrap().amount.as_u64()
        );
    }
    if old_pool_config.token_ids.update_nft_token_id
        != new_pool_config.token_ids.update_nft_token_id
    {
        println!(
            "Update NFT ID (old): {}",
            String::from(old_pool_config.token_ids.update_nft_token_id.token_id())
        );
        println!(
            "Update NFT ID (new): {}",
            String::from(new_pool_config.token_ids.update_nft_token_id.token_id())
        );
    }
    if old_pool_config.token_ids.refresh_nft_token_id
        != new_pool_config.token_ids.refresh_nft_token_id
    {
        println!(
            "Refresh NFT ID (old): {}",
            String::from(old_pool_config.token_ids.refresh_nft_token_id.token_id())
        );
        println!(
            "Refresh NFT ID (new): {}",
            String::from(new_pool_config.token_ids.refresh_nft_token_id.token_id())
        );
    }
    if old_pool_config.token_ids.oracle_token_id != new_pool_config.token_ids.oracle_token_id {
        println!(
            "Oracle Token ID (old): {}",
            String::from(old_pool_config.token_ids.oracle_token_id.token_id())
        );
        println!(
            "Oracle Token ID (new): {}",
            String::from(new_pool_config.token_ids.oracle_token_id.token_id())
        );
    }
    if old_pool_config.token_ids.ballot_token_id != new_pool_config.token_ids.ballot_token_id {
        println!(
            "Ballot Token ID (old): {}",
            String::from(old_pool_config.token_ids.ballot_token_id.token_id())
        );
        println!(
            "Ballot Token ID (new): {}",
            String::from(new_pool_config.token_ids.ballot_token_id.token_id())
        );
    }
    if old_pool_config.token_ids.pool_nft_token_id != new_pool_config.token_ids.pool_nft_token_id {
        println!(
            "Pool NFT ID (old): {}",
            String::from(old_pool_config.token_ids.pool_nft_token_id.token_id())
        );
        println!(
            "Pool NFT ID (new): {}",
            String::from(new_pool_config.token_ids.pool_nft_token_id.token_id())
        );
    }
}

fn remind_send_minted_tokens_to_oracles(
    old_pool_config: &PoolConfig,
    new_pool_config: &PoolConfig,
) {
    if old_pool_config.token_ids.reward_token_id != new_pool_config.token_ids.reward_token_id {
        println!("Send the minted reward token (one) to the oracle operators.");
    }
    if old_pool_config.token_ids.oracle_token_id != new_pool_config.token_ids.oracle_token_id {
        println!("Send the minted oracle tokens to the oracle operators.");
    }
    if old_pool_config.token_ids.ballot_token_id != new_pool_config.token_ids.ballot_token_id {
        println!("Send the minted ballot tokens to the oracle operators.");
    }
}

#[allow(clippy::too_many_arguments)]
fn build_update_pool_box_tx(
    pool_box_source: &dyn PoolBoxSource,
    ballot_boxes: &dyn VoteBallotBoxesSource,
    node_api: &dyn NodeApiTrait,
    update_box: &dyn UpdateBoxSource,
    new_reward_tokens: Option<SpecToken<RewardTokenId>>,
    height: BlockHeight,
    oracle_address: NetworkAddress,
    change_address: Address,
    new_pool_contract: PoolContract,
) -> Result<TransactionContext<UnsignedTransaction>, UpdatePoolError> {
    let update_box = update_box.get_update_box()?;
    let min_votes = update_box.min_votes();
    let old_pool_box = pool_box_source.get_pool_box()?;
    let pool_box_hash = blake2b256_hash(
        &new_pool_contract
            .ergo_tree()
            .sigma_serialize_bytes()
            .unwrap(),
    );
    let vote_parameters = CastBallotBoxVoteParameters {
        pool_box_address_hash: pool_box_hash,
        reward_token_opt: new_reward_tokens.clone(),
        update_box_creation_height: update_box.get_box().creation_height as i32,
    };
    let reward_tokens = new_reward_tokens.unwrap_or_else(|| old_pool_box.reward_token());
    // Find ballot boxes that are voting for the new pool hash
    let mut sorted_ballot_boxes = ballot_boxes.get_ballot_boxes()?;
    // Sort in descending order of ballot token amounts. If two boxes have the same amount of ballot tokens, also compare box value, in case some boxes were incorrectly created below minStorageRent
    sorted_ballot_boxes.sort_by(|b1, b2| {
        (
            *b1.ballot_token().amount.as_u64(),
            *b1.get_box().value.as_u64(),
        )
            .cmp(&(
                *b2.ballot_token().amount.as_u64(),
                *b2.get_box().value.as_u64(),
            ))
    });
    sorted_ballot_boxes.reverse();

    let mut votes_cast = 0;
    let vote_ballot_boxes: Vec<VoteBallotBoxWrapper> = ballot_boxes
        .get_ballot_boxes()?
        .into_iter()
        .filter(|ballot_box| *ballot_box.vote_parameters() == vote_parameters)
        .scan(&mut votes_cast, |votes_cast, ballot_box| {
            **votes_cast += *ballot_box.ballot_token().amount.as_u64();
            Some(ballot_box)
        })
        .collect();
    if votes_cast < min_votes as u64 {
        return Err(UpdatePoolError::NotEnoughVotes(
            min_votes as usize,
            vote_ballot_boxes.len(),
            vote_parameters,
        ));
    }

    let pool_box_candidate = make_pool_box_candidate_unchecked(
        &new_pool_contract,
        old_pool_box.rate(),
        old_pool_box.epoch_counter(),
        old_pool_box.pool_nft_token(),
        reward_tokens.clone(),
        old_pool_box.get_box().value,
        height,
    )?;
    let mut update_box_candidate =
        ErgoBoxCandidateBuilder::new(update_box.get_box().value, update_box.ergo_tree(), height.0);
    update_box_candidate.add_token(update_box.update_nft());
    let update_box_candidate = update_box_candidate.build()?;

    let target_balance = *BASE_FEE;
    let target_tokens =
        if reward_tokens.token_id.token_id() != old_pool_box.reward_token().token_id() {
            vec![reward_tokens.clone().into()]
        } else {
            vec![]
        };

    // Find unspent boxes without ballot token, see: https://github.com/ergoplatform/oracle-core/pull/80#issuecomment-1200258458
    let unspent_boxes = node_api
        .get_unspent_boxes_by_address_with_token_filter_option(
            &oracle_address.to_base58(),
            target_balance,
            target_tokens.clone(),
            vec![update_box.ballot_token_id()]
        )?;

    if unspent_boxes.is_empty() {
        error!("Could not find unspent wallet boxes that do not contain ballot token. Please move ballot tokens to another address");
        return Err(UpdatePoolError::NoUsableWalletBoxes);
    }

    let box_selector = SimpleBoxSelector::new();
    let selection = box_selector.select(unspent_boxes, target_balance, &target_tokens)?;
    let mut input_boxes = vec![old_pool_box.get_box().clone(), update_box.get_box().clone()];
    input_boxes.extend(
        vote_ballot_boxes
            .iter()
            .map(|ballot_box| ballot_box.get_box())
            .cloned(),
    );
    input_boxes.extend_from_slice(selection.boxes.as_vec());
    let box_selection = BoxSelection {
        boxes: input_boxes.clone().try_into().unwrap(),
        change_boxes: selection.change_boxes,
    };

    let mut outputs = vec![pool_box_candidate, update_box_candidate];
    for ballot_box in vote_ballot_boxes.iter() {
        let mut ballot_box_candidate = ErgoBoxCandidateBuilder::new(
            ballot_box.get_box().value, // value must be preserved or increased
            ballot_box.get_box().ergo_tree.clone(),
            height.0,
        );
        ballot_box_candidate.add_token(ballot_box.ballot_token().into());
        ballot_box_candidate.set_register_value(
            NonMandatoryRegisterId::R4,
            ballot_box.ballot_token_owner().into(),
        );
        outputs.push(ballot_box_candidate.build()?)
    }

    let mut tx_builder = TxBuilder::new(
        box_selection,
        outputs.clone(),
        height.0,
        *BASE_FEE,
        change_address,
    );

    if reward_tokens.token_id.token_id() != old_pool_box.reward_token().token_id() {
        tx_builder.set_token_burn_permit(vec![old_pool_box.reward_token().into()]);
    }

    for (i, input_ballot) in vote_ballot_boxes.iter().enumerate() {
        tx_builder.set_context_extension(
            input_ballot.get_box().box_id(),
            ContextExtension {
                values: IntoIterator::into_iter([(0, ((i + 2) as i32).into())]).collect(), // first 2 outputs are pool and update box, ballot indexes start at 2
            },
        )
    }
    let unsigned_tx = tx_builder.build()?;
    let context = match TransactionContext::new(unsigned_tx, input_boxes, vec![]) {
        Ok(ctx) => ctx,
        Err(e) => return Err(UpdatePoolError::TxSigningError(e)),
    };
    Ok(context)
}

#[cfg(test)]
mod tests {
    use ergo_lib::{
        chain::{
            ergo_box::box_builder::ErgoBoxCandidateBuilder, ergo_state_context::ErgoStateContext,
            transaction::TxId,
        },
        ergo_chain_types::blake2b256_hash,
        ergotree_interpreter::sigma_protocol::private_input::DlogProverInput,
        ergotree_ir::{
            chain::{
                address::AddressEncoder,
                ergo_box::ErgoBox,
                token::{Token, TokenId},
            },
            serialization::SigmaSerializable,
        },
    };
    use sigma_test_util::force_any_val;
    use std::convert::TryInto;

    use crate::{
        box_kind::{
            make_local_ballot_box_candidate, make_pool_box_candidate, PoolBoxWrapper,
            PoolBoxWrapperInputs, UpdateBoxWrapper, UpdateBoxWrapperInputs, VoteBallotBoxWrapper,
        },
        contracts::{
            ballot::{BallotContract, BallotContractInputs, BallotContractParameters},
            pool::{PoolContract, PoolContractInputs},
            update::{UpdateContract, UpdateContractInputs, UpdateContractParameters},
        },
        oracle_config::BASE_FEE,
        oracle_types::{BlockHeight, EpochCounter},
        pool_commands::test_utils::{
            generate_token_ids, make_wallet_unspent_box, BallotBoxesMock, PoolBoxMock,
            UpdateBoxMock,
        },
        spec_token::{RefreshTokenId, RewardTokenId, SpecToken, TokenIdKind},
    };
    use crate::cli_commands::bootstrap::tests::SubmitTxMock;
    use crate::node_interface::node_api::NodeApiTrait;
    use crate::node_interface::test_utils::MockNodeApi;
    use super::build_update_pool_box_tx;

    fn force_any_tokenid() -> TokenId {
        use proptest::strategy::Strategy;
        proptest::arbitrary::any_with::<TokenId>(
            ergo_lib::ergotree_ir::chain::token::arbitrary::ArbTokenIdParam::Arbitrary,
        )
        .new_tree(&mut Default::default())
        .unwrap()
        .current()
    }

    #[test]
    fn test_update_pool_box() {
        let ctx = force_any_val::<ErgoStateContext>();
        let height = BlockHeight(ctx.pre_header.height);

        let token_ids = generate_token_ids();
        dbg!(&token_ids);
        let reward_tokens = SpecToken {
            token_id: token_ids.reward_token_id.clone(),
            amount: 1500.try_into().unwrap(),
        };
        let new_reward_tokens = SpecToken {
            token_id: RewardTokenId::from_token_id_unchecked(force_any_tokenid()),
            amount: force_any_val(),
        };
        dbg!(&new_reward_tokens);

        let default_update_contract_parameters = UpdateContractParameters::default();
        let update_contract_parameters = UpdateContractParameters::build_with(
            default_update_contract_parameters.ergo_tree_bytes(),
            default_update_contract_parameters.pool_nft_index(),
            default_update_contract_parameters.ballot_token_index(),
            default_update_contract_parameters.min_votes_index(),
            6,
        )
        .unwrap();
        let update_contract_inputs = UpdateContractInputs::build_with(
            update_contract_parameters,
            token_ids.pool_nft_token_id.clone(),
            token_ids.ballot_token_id.clone(),
        )
        .unwrap();
        let update_contract = UpdateContract::checked_load(&update_contract_inputs).unwrap();
        let mut update_box_candidate =
            ErgoBoxCandidateBuilder::new(*BASE_FEE, update_contract.ergo_tree(), height.0);
        update_box_candidate.add_token(Token {
            token_id: token_ids.update_nft_token_id.token_id(),
            amount: 1.try_into().unwrap(),
        });
        let update_box = ErgoBox::from_box_candidate(
            &update_box_candidate.build().unwrap(),
            force_any_val::<TxId>(),
            0,
        )
        .unwrap();

        let pool_contract_parameters = Default::default();
        let pool_contract_inputs = PoolContractInputs::build_with(
            pool_contract_parameters,
            token_ids.refresh_nft_token_id,
            token_ids.update_nft_token_id.clone(),
        )
        .unwrap();

        let pool_contract = PoolContract::build_with(&pool_contract_inputs).unwrap();
        let pool_box_candidate = make_pool_box_candidate(
            &pool_contract,
            0,
            EpochCounter(0),
            SpecToken {
                token_id: token_ids.pool_nft_token_id.clone(),
                amount: 1.try_into().unwrap(),
            },
            reward_tokens.clone(),
            *BASE_FEE,
            height,
        )
        .unwrap();
        let pool_box =
            ErgoBox::from_box_candidate(&pool_box_candidate, force_any_val::<TxId>(), 0).unwrap();

        let new_refresh_token_id = force_any_tokenid();
        let mut new_pool_contract_inputs = pool_contract_inputs.clone();
        new_pool_contract_inputs.refresh_nft_token_id =
            RefreshTokenId::from_token_id_unchecked(new_refresh_token_id);
        let new_pool_contract = PoolContract::build_with(&new_pool_contract_inputs).unwrap();

        let pool_box_bytes = new_pool_contract
            .ergo_tree()
            .sigma_serialize_bytes()
            .unwrap();
        let pool_box_hash = blake2b256_hash(&pool_box_bytes);

        let ballot_contract_parameters = BallotContractParameters::default();
        let ballot_contract_inputs = BallotContractInputs::build_with(
            ballot_contract_parameters.clone(),
            token_ids.update_nft_token_id.clone(),
        )
        .unwrap();

        let mut ballot_boxes = vec![];

        for _ in 0..6 {
            let secret = DlogProverInput::random();
            let ballot_box_candidate = make_local_ballot_box_candidate(
                BallotContract::checked_load(&ballot_contract_inputs)
                    .unwrap()
                    .ergo_tree(),
                secret.public_image().h.as_ref(),
                BlockHeight(update_box.creation_height),
                SpecToken {
                    token_id: token_ids.ballot_token_id.clone(),
                    amount: 1.try_into().unwrap(),
                },
                pool_box_hash,
                Some(new_reward_tokens.clone()),
                ballot_contract_parameters.min_storage_rent(),
                height,
            )
            .unwrap();
            let ballot_box =
                ErgoBox::from_box_candidate(&ballot_box_candidate, force_any_val::<TxId>(), 0)
                    .unwrap();
            ballot_boxes.push(
                VoteBallotBoxWrapper::new(
                    ballot_box,
                    &crate::box_kind::BallotBoxWrapperInputs {
                        ballot_token_id: token_ids.ballot_token_id.clone(),
                        contract_inputs: ballot_contract_inputs.clone(),
                    },
                )
                .unwrap(),
            );
        }
        let ballot_boxes_mock = BallotBoxesMock { ballot_boxes };

        let secret = DlogProverInput::random();
        let wallet_unspent_box = make_wallet_unspent_box(
            // create a wallet box with new reward tokens
            secret.public_image(),
            BASE_FEE.checked_mul_u32(4_000_000_000).unwrap(),
            Some(vec![new_reward_tokens.clone().into()].try_into().unwrap()),
        );
        let address = AddressEncoder::unchecked_parse_network_address_from_str(
            "9iHyKxXs2ZNLMp9N9gbUT9V8gTbsV7HED1C1VhttMfBUMPDyF7r",
        )
        .unwrap();
        let mock_node_api = &MockNodeApi {
            unspent_boxes: vec![wallet_unspent_box],
            ctx: ctx.clone(),
            secrets: vec![secret.clone().into()],
            submitted_txs: &SubmitTxMock::default().transactions,
            chain_submit_tx: None
        };
        let update_mock = UpdateBoxMock {
            update_box: UpdateBoxWrapper::new(
                update_box,
                &UpdateBoxWrapperInputs {
                    contract_inputs: update_contract_inputs.clone(),
                    update_nft_token_id: token_ids.update_nft_token_id,
                },
            )
            .unwrap(),
        };
        let pool_mock = PoolBoxMock {
            pool_box: PoolBoxWrapper::new(
                pool_box,
                &PoolBoxWrapperInputs {
                    contract_inputs: pool_contract_inputs,
                    pool_nft_token_id: token_ids.pool_nft_token_id,
                    reward_token_id: token_ids.reward_token_id,
                },
            )
            .unwrap(),
        };

        let tx_context = build_update_pool_box_tx(
            &pool_mock,
            &ballot_boxes_mock,
            mock_node_api,
            &update_mock,
            Some(new_reward_tokens),
            BlockHeight(height.0 + 1),
            address.clone(),
            address.address(),
            new_pool_contract,
        )
        .unwrap();

        mock_node_api.sign_transaction(tx_context).unwrap();
    }
}
