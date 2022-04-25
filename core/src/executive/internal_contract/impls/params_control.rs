// Copyright 2019 Conflux Foundation. All rights reserved.
// Conflux is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use cfx_state::state_trait::StateOpsTrait;
use std::convert::TryFrom;

use cfx_statedb::params_control_entries::*;
use cfx_types::{Address, U256, U512};

use crate::{
    executive::{
        internal_contract::{
            contracts::params_control::Vote, impls::staking::get_vote_power,
        },
        InternalRefContext,
    },
    vm::{self, ActionParams, Error},
};

pub fn cast_vote(
    address: Address, version: u64, votes: Vec<Vote>, params: &ActionParams,
    context: &mut InternalRefContext,
) -> vm::Result<()>
{
    // If this is called, `env.number` must be larger than the activation
    // number. And version starts from 1 to tell if an account has ever voted in
    // the first version.
    let current_voting_version = (context.env.number
        - context.spec.cip94_activation_block_number)
        / context.spec.params_dao_vote_period
        + 1;
    if version != current_voting_version {
        bail!(Error::InternalContract(format!(
            "vote version unmatch: current={} voted={}",
            current_voting_version, version
        )));
    }
    let old_version =
        context.storage_at(params, &storage_key::versions(&address))?;
    let is_new_vote = old_version.as_u64() != version;

    let mut vote_counts: [Option<[U256; OPTION_INDEX_MAX]>;
        PARAMETER_INDEX_MAX] = if is_new_vote {
        // If this is the first vote in the version, even for the not-voted
        // parameters, we still reset the account's votes from previous
        // versions.
        [Some([U256::zero(); OPTION_INDEX_MAX]); PARAMETER_INDEX_MAX]
    } else {
        [None; PARAMETER_INDEX_MAX]
    };
    for vote in votes {
        if vote.index >= PARAMETER_INDEX_MAX as u16
            || vote.opt_index >= OPTION_INDEX_MAX as u16
        {
            bail!(Error::InternalContract(
                "invalid vote index or opt_index".to_string()
            ));
        }
        let entry = &mut vote_counts[vote.index as usize];
        match entry {
            None => {
                *entry = Some([U256::zero(); OPTION_INDEX_MAX]);
                entry.as_mut().unwrap()[vote.opt_index as usize] = vote.votes;
            }
            Some(votes) => {
                votes[vote.opt_index as usize] =
                    votes[vote.opt_index as usize].saturating_add(vote.votes);
            }
        }
    }

    let vote_power = get_vote_power(
        address,
        U256::from(context.env.number),
        context.env.number,
        context.state,
    )?;
    debug!("vote_power:{}", vote_power);
    for index in 0..PARAMETER_INDEX_MAX {
        if vote_counts[index].is_none() {
            continue;
        }
        let param_vote = vote_counts[index].unwrap();
        let total_counts = param_vote[0]
            .saturating_add(param_vote[1])
            .saturating_add(param_vote[2]);
        if total_counts > vote_power {
            bail!(Error::InternalContract(format!(
                "not enough vote power: power={} votes={}",
                vote_power, total_counts
            )));
        }
        for opt_index in 0..OPTION_INDEX_MAX {
            let vote_in_storage = context.storage_at(
                params,
                &storage_key::votes(&address, index, opt_index),
            )?;
            let old_vote = if is_new_vote {
                U256::zero()
            } else {
                vote_in_storage
            };
            debug!(
                "index:{}, opt_index{}, old_vote: {}, new_vote: {}",
                index, opt_index, old_vote, param_vote[opt_index]
            );

            // Update the global votes if the account vote changes.
            if param_vote[opt_index] != old_vote {
                let old_total_votes = context.state.get_system_storage(
                    &TOTAL_VOTES_ENTRIES[index][opt_index],
                )?;
                debug!("old_total_vote: {}", old_total_votes,);
                let new_total_votes = if old_vote > param_vote[opt_index] {
                    let dec = old_vote - param_vote[opt_index];
                    // If total votes are accurate, `old_total_votes` is
                    // larger than `old_vote`.
                    old_total_votes - dec
                } else if old_vote < param_vote[opt_index] {
                    let inc = param_vote[opt_index] - old_vote;
                    old_total_votes + inc
                } else {
                    assert!(is_new_vote);
                    old_total_votes
                };
                debug!("new_total_vote:{}", new_total_votes);
                context.state.set_system_storage(
                    TOTAL_VOTES_ENTRIES[index][opt_index].to_vec(),
                    new_total_votes,
                )?;
            }

            // Overwrite the account vote entry if needed.
            if param_vote[opt_index] != vote_in_storage {
                context.set_storage(
                    params,
                    storage_key::votes(&address, index, opt_index).to_vec(),
                    param_vote[opt_index],
                )?;
            }
        }
    }
    if is_new_vote {
        context.set_storage(
            params,
            storage_key::versions(&address).to_vec(),
            U256::from(version),
        )?;
    }
    Ok(())
}

pub fn read_vote(
    address: Address, params: &ActionParams, context: &mut InternalRefContext,
) -> vm::Result<Vec<Vote>> {
    let mut votes_list = Vec::new();
    for index in 0..PARAMETER_INDEX_MAX {
        for opt_index in 0..OPTION_INDEX_MAX {
            let votes = context.storage_at(
                params,
                &storage_key::votes(&address, index, opt_index),
            )?;
            if votes != U256::zero() {
                votes_list.push(Vote {
                    index: index as u16,
                    opt_index: opt_index as u16,
                    votes,
                })
            }
        }
    }
    Ok(votes_list)
}

/// If the vote counts are not initialized, all counts will be zero, and the
/// parameters will be unchanged.
pub fn settled_param_vote_count<T: StateOpsTrait>(
    state: &T,
) -> vm::Result<AllParamsVoteCount> {
    let pow_base_reward = ParamVoteCount {
        unchange: state.get_system_storage(
            &SETTLED_TOTAL_VOTES_ENTRIES[POW_BASE_REWARD_INDEX as usize]
                [OPTION_UNCHANGE_INDEX as usize],
        )?,
        increase: state.get_system_storage(
            &SETTLED_TOTAL_VOTES_ENTRIES[POW_BASE_REWARD_INDEX as usize]
                [OPTION_INCREASE_INDEX as usize],
        )?,
        decrease: state.get_system_storage(
            &SETTLED_TOTAL_VOTES_ENTRIES[POW_BASE_REWARD_INDEX as usize]
                [OPTION_DECREASE_INDEX as usize],
        )?,
    };
    let pos_reward_interest = ParamVoteCount {
        unchange: state.get_system_storage(
            &SETTLED_TOTAL_VOTES_ENTRIES
                [POS_REWARD_INTEREST_RATE_INDEX as usize]
                [OPTION_UNCHANGE_INDEX as usize],
        )?,
        increase: state.get_system_storage(
            &SETTLED_TOTAL_VOTES_ENTRIES
                [POS_REWARD_INTEREST_RATE_INDEX as usize]
                [OPTION_INCREASE_INDEX as usize],
        )?,
        decrease: state.get_system_storage(
            &SETTLED_TOTAL_VOTES_ENTRIES
                [POS_REWARD_INTEREST_RATE_INDEX as usize]
                [OPTION_DECREASE_INDEX as usize],
        )?,
    };
    Ok(AllParamsVoteCount {
        pow_base_reward,
        pos_reward_interest,
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ParamVoteCount {
    unchange: U256,
    increase: U256,
    decrease: U256,
}

impl ParamVoteCount {
    pub fn new(unchange: U256, increase: U256, decrease: U256) -> Self {
        Self {
            unchange,
            increase,
            decrease,
        }
    }

    pub fn compute_next_params(&self, old_value: U256) -> U256 {
        // `VoteCount` only counts valid votes, so this will not overflow.
        let total = self.unchange + self.increase + self.decrease;
        if total == U256::zero() {
            // If no one votes, we just keep the value unchanged.
            return old_value;
        }
        let weighted_total =
            self.unchange + self.increase * 2u64 + self.decrease / 2u64;
        let new_value = U512::from(old_value) * U512::from(weighted_total)
            / U512::from(total);
        U256::try_from(new_value).unwrap()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AllParamsVoteCount {
    pub pow_base_reward: ParamVoteCount,
    pub pos_reward_interest: ParamVoteCount,
}

/// Solidity variable sequences.
/// ```solidity
/// struct VoteInfo {
///     uint version,
///     uint[3] pow_base_reward dynamic,
///     uint[3] pos_interest_rate dynamic,
/// }
/// mapping(address => VoteInfo) votes;
/// ```
mod storage_key {
    use super::{Address, U256};
    use cfx_types::BigEndianHash;
    use hash::H256;
    use keccak_hash::keccak;

    const VOTES_SLOT: usize = 0;

    // General function for solidity storage rule
    fn mapping_slot(base: U256, index: U256) -> U256 {
        let mut input = [0u8; 64];
        base.to_big_endian(&mut input[32..]);
        index.to_big_endian(&mut input[..32]);
        let hash = keccak(input);
        U256::from_big_endian(hash.as_ref())
    }

    #[allow(dead_code)]
    // General function for solidity storage rule
    fn vector_slot(base: U256, index: usize, size: usize) -> U256 {
        let start_slot = dynamic_slot(base);
        return array_slot(start_slot, index, size);
    }

    fn dynamic_slot(base: U256) -> U256 {
        let mut input = [0u8; 32];
        base.to_big_endian(&mut input);
        let hash = keccak(input);
        return U256::from_big_endian(hash.as_ref());
    }

    // General function for solidity storage rule
    fn array_slot(base: U256, index: usize, element_size: usize) -> U256 {
        // Solidity will apply an overflowing add here.
        // However, if this function is used correctly, the overflowing will
        // happen with a negligible exception, so we let it panic when
        // overflowing happen.
        base + index * element_size
    }

    fn u256_to_array(input: U256) -> [u8; 32] {
        let mut answer = [0u8; 32];
        input.to_big_endian(answer.as_mut());
        answer
    }

    // TODO: add cache to avoid duplicated hash computing
    pub fn versions(address: &Address) -> [u8; 32] {
        // Position of `votes`
        let base = U256::from(VOTES_SLOT);

        // Position of `votes[address]`
        let address_slot = mapping_slot(base, H256::from(*address).into_uint());

        // Position of `votes[address].version`
        let version_slot = address_slot;

        return u256_to_array(version_slot);
    }

    pub fn votes(
        address: &Address, index: usize, opt_index: usize,
    ) -> [u8; 32] {
        const TOPIC_OFFSET: [usize; 2] = [1, 2];

        // Position of `votes`
        let base = U256::from(VOTES_SLOT);

        // Position of `votes[address]`
        let address_slot = mapping_slot(base, H256::from(*address).into_uint());

        // Position of `votes[address].<topic>` (static slot)
        let topic_slot = address_slot + TOPIC_OFFSET[index];

        // Position of `votes[address].<topic>` (dynamic slot)
        let topic_slot = dynamic_slot(topic_slot);

        // Position of `votes[address].<topic>[opt_index]`
        let opt_slot = array_slot(topic_slot, opt_index, 1);

        return u256_to_array(opt_slot);
    }
}
