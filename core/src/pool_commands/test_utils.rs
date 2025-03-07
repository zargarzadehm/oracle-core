//! This module contains common code used for testing the various commands
use std::convert::TryFrom;
use std::convert::TryInto;
use std::option::Option;

use ergo_lib::chain::transaction::unsigned::UnsignedTransaction;
use ergo_lib::chain::transaction::TxId;
use ergo_lib::ergo_chain_types::Digest32;
use ergo_lib::ergo_chain_types::EcPoint;
use ergo_lib::ergotree_ir::chain::ergo_box::box_value::BoxValue;
use ergo_lib::ergotree_ir::chain::ergo_box::BoxTokens;
use ergo_lib::ergotree_ir::chain::ergo_box::ErgoBox;
use ergo_lib::ergotree_ir::chain::ergo_box::NonMandatoryRegisterId;
use ergo_lib::ergotree_ir::chain::ergo_box::NonMandatoryRegisters;
use ergo_lib::ergotree_ir::chain::token::Token;
use ergo_lib::ergotree_ir::ergo_tree::ErgoTree;
use ergo_lib::ergotree_ir::mir::constant::Constant;
use ergo_lib::ergotree_ir::mir::expr::Expr;
use ergo_lib::ergotree_ir::sigma_protocol::sigma_boolean::ProveDlog;
use sigma_test_util::force_any_val;

use crate::box_kind::BallotBoxWrapper;
use crate::box_kind::BuybackBoxWrapper;
use crate::box_kind::OracleBoxWrapper;
use crate::box_kind::OracleBoxWrapperInputs;
use crate::box_kind::PoolBoxWrapper;
use crate::box_kind::PoolBoxWrapperInputs;
use crate::box_kind::UpdateBoxWrapper;
use crate::box_kind::VoteBallotBoxWrapper;
use crate::contracts::oracle::OracleContract;
use crate::contracts::oracle::OracleContractError;
use crate::contracts::oracle::OracleContractInputs;
use crate::contracts::oracle::OracleContractParameters;
use crate::contracts::pool::PoolContract;
use crate::contracts::pool::PoolContractInputs;
use crate::contracts::pool::PoolContractParameters;
use crate::oracle_state::BuybackBoxSource;
use crate::oracle_state::LocalBallotBoxSource;
use crate::oracle_state::UpdateBoxSource;
use crate::oracle_state::VoteBallotBoxesSource;
use crate::oracle_state::{DataSourceError, LocalDatapointBoxSource, PoolBoxSource};
use crate::oracle_types::EpochCounter;
use crate::pool_config::TokenIds;
use crate::spec_token::BallotTokenId;
use crate::spec_token::OracleTokenId;
use crate::spec_token::PoolTokenId;
use crate::spec_token::RefreshTokenId;
use crate::spec_token::RewardTokenId;
use crate::spec_token::TokenIdKind;
use crate::spec_token::UpdateTokenId;

use super::*;

#[derive(Clone)]
pub(crate) struct PoolBoxMock {
    pub pool_box: PoolBoxWrapper,
}

impl PoolBoxSource for PoolBoxMock {
    fn get_pool_box(&self) -> std::result::Result<PoolBoxWrapper, DataSourceError> {
        Ok(self.pool_box.clone())
    }
}

#[derive(Clone)]
pub(crate) struct OracleBoxMock {
    pub oracle_box: OracleBoxWrapper,
}

impl LocalDatapointBoxSource for OracleBoxMock {
    fn get_local_oracle_datapoint_box(
        &self,
    ) -> std::result::Result<Option<OracleBoxWrapper>, DataSourceError> {
        Ok(Some(self.oracle_box.clone()))
    }
}

#[derive(Clone)]
pub(crate) struct BallotBoxMock {
    pub ballot_box: BallotBoxWrapper,
}

impl LocalBallotBoxSource for BallotBoxMock {
    fn get_ballot_box(&self) -> std::result::Result<Option<BallotBoxWrapper>, DataSourceError> {
        Ok(Some(self.ballot_box.clone()))
    }
}

pub struct BallotBoxesMock {
    pub ballot_boxes: Vec<VoteBallotBoxWrapper>,
}

impl VoteBallotBoxesSource for BallotBoxesMock {
    fn get_ballot_boxes(&self) -> std::result::Result<Vec<VoteBallotBoxWrapper>, DataSourceError> {
        Ok(self.ballot_boxes.clone())
    }
}

pub(crate) struct UpdateBoxMock {
    pub update_box: UpdateBoxWrapper,
}

impl UpdateBoxSource for UpdateBoxMock {
    fn get_update_box(&self) -> crate::oracle_state::Result<crate::box_kind::UpdateBoxWrapper> {
        Ok(self.update_box.clone())
    }
}

pub(crate) struct BuybackBoxSourceMock {
    pub buyback_box: BuybackBoxWrapper,
}
impl BuybackBoxSource for BuybackBoxSourceMock {
    fn get_buyback_box(
        &self,
    ) -> crate::oracle_state::Result<Option<crate::box_kind::BuybackBoxWrapper>> {
        Ok(Some(self.buyback_box.clone()))
    }
}

pub(crate) fn make_pool_box(
    datapoint: i64,
    epoch_counter: EpochCounter,
    value: BoxValue,
    creation_height: BlockHeight,
    pool_contract_parameters: &PoolContractParameters,
    token_ids: &TokenIds,
) -> PoolBoxWrapper {
    let pool_contract_inputs = PoolContractInputs::build_with(
        pool_contract_parameters.clone(),
        token_ids.refresh_nft_token_id.clone(),
        token_ids.update_nft_token_id.clone(),
    )
    .unwrap();
    let pool_box_wrapper_inputs = PoolBoxWrapperInputs {
        contract_inputs: pool_contract_inputs.clone(),
        pool_nft_token_id: token_ids.pool_nft_token_id.clone(),
        reward_token_id: token_ids.reward_token_id.clone(),
    };
    let tokens = vec![
        Token::from((
            token_ids.pool_nft_token_id.token_id(),
            1u64.try_into().unwrap(),
        )),
        Token::from((
            token_ids.reward_token_id.token_id(),
            100u64.try_into().unwrap(),
        )),
    ]
    .try_into()
    .unwrap();
    PoolBoxWrapper::new(
        ErgoBox::new(
            value,
            PoolContract::build_with(&pool_contract_inputs)
                .unwrap()
                .ergo_tree(),
            Some(tokens),
            NonMandatoryRegisters::new(
                vec![
                    (NonMandatoryRegisterId::R4, Constant::from(datapoint)),
                    (
                        NonMandatoryRegisterId::R5,
                        Constant::from(epoch_counter.0 as i32),
                    ),
                ]
                .into_iter()
                .collect(),
            )
            .unwrap(),
            creation_height.0,
            force_any_val::<TxId>(),
            0,
        )
        .unwrap(),
        &pool_box_wrapper_inputs,
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn make_datapoint_box(
    pub_key: EcPoint,
    datapoint: i64,
    epoch_counter: EpochCounter,
    token_ids: &TokenIds,
    value: BoxValue,
    creation_height: BlockHeight,
    reward_token_amount: u64,
) -> ErgoBox {
    let tokens = vec![
        Token::from((
            token_ids.oracle_token_id.token_id(),
            1u64.try_into().unwrap(),
        )),
        Token::from((
            token_ids.reward_token_id.token_id(),
            reward_token_amount.try_into().unwrap(),
        )),
    ]
    .try_into()
    .unwrap();
    let parameters = OracleContractParameters::default();
    let oracle_contract_inputs =
        OracleContractInputs::build_with(parameters, token_ids.pool_nft_token_id.clone()).unwrap();
    ErgoBox::new(
        value,
        OracleContract::checked_load(&oracle_contract_inputs)
            .unwrap()
            .ergo_tree(),
        Some(tokens),
        NonMandatoryRegisters::new(
            vec![
                (NonMandatoryRegisterId::R4, Constant::from(pub_key)),
                (
                    NonMandatoryRegisterId::R5,
                    Constant::from(epoch_counter.0 as i32),
                ),
                (NonMandatoryRegisterId::R6, Constant::from(datapoint)),
            ]
            .into_iter()
            .collect(),
        )
        .unwrap(),
        creation_height.0,
        force_any_val::<TxId>(),
        0,
    )
    .unwrap()
}

pub(crate) fn make_wallet_unspent_box(
    pub_key: ProveDlog,
    value: BoxValue,
    tokens: Option<BoxTokens>,
) -> ErgoBox {
    let c: Constant = pub_key.into();
    let expr: Expr = c.into();
    ErgoBox::new(
        value,
        ErgoTree::try_from(expr).unwrap(),
        tokens,
        NonMandatoryRegisters::empty(),
        1,
        force_any_val::<TxId>(),
        0,
    )
    .unwrap()
}

pub(crate) fn find_input_boxes(
    tx: UnsignedTransaction,
    available_boxes: Vec<ErgoBox>,
) -> Vec<ErgoBox> {
    tx.inputs
        .mapped(|i| {
            available_boxes
                .clone()
                .into_iter()
                .find(|b| b.box_id() == i.box_id)
                .unwrap()
        })
        .as_vec()
        .clone()
}

pub fn init_log_tests() {
    // set log level via RUST_LOG=info env var
    env_logger::builder().is_test(true).try_init().unwrap();
}

pub fn generate_token_ids() -> TokenIds {
    TokenIds {
        pool_nft_token_id: PoolTokenId::from_token_id_unchecked(force_any_val::<Digest32>().into()),
        refresh_nft_token_id: RefreshTokenId::from_token_id_unchecked(
            force_any_val::<Digest32>().into(),
        ),
        update_nft_token_id: UpdateTokenId::from_token_id_unchecked(
            force_any_val::<Digest32>().into(),
        ),
        oracle_token_id: OracleTokenId::from_token_id_unchecked(force_any_val::<Digest32>().into()),
        reward_token_id: RewardTokenId::from_token_id_unchecked(force_any_val::<Digest32>().into()),
        ballot_token_id: BallotTokenId::from_token_id_unchecked(force_any_val::<Digest32>().into()),
    }
}

impl TryFrom<(OracleContractParameters, &TokenIds)> for OracleBoxWrapperInputs {
    type Error = OracleContractError;
    fn try_from(
        (contract_parameters, token_ids): (OracleContractParameters, &TokenIds),
    ) -> Result<Self, Self::Error> {
        let contract_inputs = OracleContractInputs::build_with(
            contract_parameters,
            token_ids.pool_nft_token_id.clone(),
        )?;
        Ok(Self {
            contract_inputs,
            oracle_token_id: token_ids.oracle_token_id.clone(),
            reward_token_id: token_ids.reward_token_id.clone(),
        })
    }
}
