// Copyright 2020 Conflux Foundation. All rights reserved.
// Conflux is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

use super::super::types::LocalizedBlockTrace;
use crate::{
    common::delegate_convert::into_jsonrpc_result,
    rpc::{
        traits::{eth_space::trace::Trace as EthTrace, trace::Trace},
        types::{
            eth::{
                BlockNumber, LocalizedTrace as EthLocalizedTrace,
                Res as EthRes, TraceFilter as EthTraceFilter,
            },
            Action as RpcAction, LocalizedTrace as RpcLocalizedTrace,
            LocalizedTrace, TraceFilter as RpcTraceFilter,
        },
        RpcResult,
    },
};
use cfx_addr::Network;
use cfx_types::{Space, H256};
use cfxcore::{
    block_data_manager::DataVersionTuple,
    observer::trace_filter::TraceFilter as PrimitiveTraceFilter,
    BlockDataManager, ConsensusGraph, SharedConsensusGraph,
};
use jsonrpc_core::{Error as JsonRpcError, Result as JsonRpcResult};
use std::{convert::TryInto, sync::Arc};

pub struct TraceHandler {
    data_man: Arc<BlockDataManager>,
    consensus: SharedConsensusGraph,
    network: Network,
}

impl TraceHandler {
    pub fn new(
        data_man: Arc<BlockDataManager>, network: Network,
        consensus: SharedConsensusGraph,
    ) -> Self
    {
        TraceHandler {
            data_man,
            consensus,
            network,
        }
    }

    fn consensus_graph(&self) -> &ConsensusGraph {
        self.consensus
            .as_any()
            .downcast_ref::<ConsensusGraph>()
            .expect("downcast should succeed")
    }

    fn block_traces_impl(
        &self, block_hash: H256,
    ) -> RpcResult<Option<LocalizedBlockTrace>> {
        // Note: an alternative to `into_jsonrpc_result` is the delegate! macro.
        let transaction_hashes = match self
            .data_man
            .block_by_hash(&block_hash, true /* update_cache */)
        {
            None => return Ok(None),
            Some(block) => {
                block.transactions.iter().map(|tx| tx.hash()).collect()
            }
        };

        match self.data_man.block_traces_by_hash(&block_hash) {
            None => Ok(None),
            Some(DataVersionTuple(pivot_hash, traces)) => {
                let traces = traces.filter_space(Space::Native);
                let epoch_number = self
                    .data_man
                    .block_height_by_hash(&pivot_hash)
                    .ok_or("pivot block missing")?;
                match LocalizedBlockTrace::from(
                    traces,
                    block_hash,
                    pivot_hash,
                    epoch_number,
                    transaction_hashes,
                    self.network,
                ) {
                    Ok(t) => Ok(Some(t)),
                    Err(e) => bail!(format!(
                        "Traces not found for block {:?}: {:?}",
                        block_hash, e
                    )),
                }
            }
        }
    }

    fn filter_traces_impl(
        &self, filter: PrimitiveTraceFilter,
    ) -> RpcResult<Option<Vec<RpcLocalizedTrace>>> {
        let consensus_graph = self.consensus_graph();
        let traces: Vec<_> = consensus_graph
            .filter_traces(filter)?
            .into_iter()
            .map(|trace| {
                RpcLocalizedTrace::from(trace, self.network)
                    .expect("Local address conversion should succeed")
            })
            .collect();
        if traces.is_empty() {
            Ok(None)
        } else {
            Ok(Some(traces))
        }
    }

    fn transaction_trace_impl(
        &self, tx_hash: &H256,
    ) -> RpcResult<Option<Vec<RpcLocalizedTrace>>> {
        Ok(self
            .data_man
            .transaction_index_by_hash(tx_hash, true /* update_cache */)
            .and_then(|tx_index| {
                // FIXME(thegaram): do we support traces for phantom txs?
                if tx_index.is_phantom {
                    return None;
                }

                self.data_man
                    .transactions_traces_by_block_hash(&tx_index.block_hash)
                    .and_then(|(pivot_hash, traces)| {
                        traces
                            .into_iter()
                            .nth(tx_index.real_index)
                            .map(|tx_trace| {
                                tx_trace.filter_space(Space::Native).0
                            })
                            .map(|traces| {
                                traces
                                    .into_iter()
                                    .map(|trace| RpcLocalizedTrace {
                                        action: RpcAction::try_from(
                                            trace.action,
                                            self.network,
                                        )
                                        .expect("local address convert error"),
                                        valid: trace.valid,
                                        epoch_hash: Some(pivot_hash),
                                        epoch_number: Some(
                                            self.data_man
                                                .block_height_by_hash(
                                                    &pivot_hash,
                                                )
                                                .expect("pivot block missing")
                                                .into(),
                                        ),
                                        block_hash: Some(tx_index.block_hash),
                                        transaction_position: Some(
                                            tx_index.real_index.into(),
                                        ),
                                        transaction_hash: Some(*tx_hash),
                                    })
                                    .collect()
                            })
                    })
            }))
    }
}

impl Trace for TraceHandler {
    fn block_traces(
        &self, block_hash: H256,
    ) -> JsonRpcResult<Option<LocalizedBlockTrace>> {
        into_jsonrpc_result(self.block_traces_impl(block_hash))
    }

    fn filter_traces(
        &self, filter: RpcTraceFilter,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>> {
        let primitive_filter = filter.into_primitive()?;
        into_jsonrpc_result(self.filter_traces_impl(primitive_filter))
    }

    fn transaction_traces(
        &self, tx_hash: H256,
    ) -> JsonRpcResult<Option<Vec<LocalizedTrace>>> {
        into_jsonrpc_result(self.transaction_trace_impl(&tx_hash))
    }
}

pub struct EthTraceHandler {
    pub trace_handler: TraceHandler,
}

impl EthTrace for EthTraceHandler {
    fn block_traces(
        &self, block_number: BlockNumber,
    ) -> JsonRpcResult<Option<Vec<EthLocalizedTrace>>> {
        let epoch_hashes = self
            .trace_handler
            .consensus
            .get_block_hashes_by_epoch(block_number.try_into()?)
            .map_err(JsonRpcError::invalid_params)?;
        let eth_block_hash = *epoch_hashes.last().unwrap();
        let eth_block_number = self
            .trace_handler
            .consensus
            .get_data_manager()
            .block_height_by_hash(&eth_block_hash)
            .unwrap();
        let mut eth_traces = Vec::new();
        for block_hash in epoch_hashes {
            match self
                .trace_handler
                .data_man
                .block_traces_by_hash(&block_hash)
            {
                None => return Ok(None),
                Some(DataVersionTuple(pivot_hash_for_trace, traces)) => {
                    if eth_block_hash != pivot_hash_for_trace {
                        return Ok(None);
                    }
                    for tx_traces in traces.0 {
                        for paired_trace in tx_traces
                            .filter_trace_pairs(
                                &PrimitiveTraceFilter::space_filter(
                                    Space::Ethereum,
                                ),
                            )
                            .map_err(|_| JsonRpcError::internal_error())?
                        {
                            let mut eth_trace = EthLocalizedTrace {
                                action: RpcAction::try_from(
                                    paired_trace.0.action,
                                    self.trace_handler.network,
                                )
                                .map_err(|_| JsonRpcError::internal_error())?
                                .try_into()
                                .map_err(|_| JsonRpcError::internal_error())?,
                                result: EthRes::None,
                                trace_address: vec![],
                                subtraces: 0,
                                // FIXME(lpl): follow the value of tx index?
                                transaction_position: None,
                                transaction_hash: None,
                                block_number: eth_block_number,
                                block_hash: eth_block_hash,
                            };
                            eth_trace.set_result(
                                RpcAction::try_from(
                                    paired_trace.1.action,
                                    self.trace_handler.network,
                                )
                                .map_err(|_| JsonRpcError::internal_error())?,
                            )?;
                            eth_traces.push(eth_trace);
                        }
                    }
                }
            }
        }
        Ok(Some(eth_traces))
    }

    fn filter_traces(
        &self, filter: EthTraceFilter,
    ) -> JsonRpcResult<Option<Vec<EthLocalizedTrace>>> {
        // TODO(lpl): Use `TransactionExecTraces::filter_trace_pairs` to avoid
        // pairing twice.
        let primitive_filter = filter.into_primitive()?;
        let traces =
            match self.trace_handler.filter_traces_impl(primitive_filter)? {
                None => return Ok(None),
                Some(traces) => traces,
            };
        let mut eth_traces: Vec<EthLocalizedTrace> = Vec::new();
        let mut stack_index = Vec::new();
        for trace in traces {
            match &trace.action {
                RpcAction::Call(_) | RpcAction::Create(_) => {
                    stack_index.push(eth_traces.len());
                    eth_traces.push(trace.try_into().map_err(|e| {
                        error!("eth trace conversion error: {:?}", e);
                        JsonRpcError::internal_error()
                    })?);
                }
                RpcAction::CallResult(_) | RpcAction::CreateResult(_) => {
                    let index = stack_index
                        .pop()
                        .ok_or(JsonRpcError::internal_error())?;
                    eth_traces[index].set_result(trace.action)?;
                }
                RpcAction::InternalTransferAction(_) => {}
            }
        }
        if !stack_index.is_empty() {
            error!("eth::filter_traces: actions left unmatched");
            bail!(JsonRpcError::internal_error());
        }
        Ok(Some(eth_traces))
    }

    fn transaction_traces(
        &self, tx_hash: H256,
    ) -> JsonRpcResult<Option<Vec<EthLocalizedTrace>>> {
        Ok(self
            .trace_handler
            .data_man
            .transaction_index_by_hash(&tx_hash, true /* update_cache */)
            .and_then(|tx_index| {
                // FIXME(thegaram): do we support traces for phantom txs?
                if tx_index.is_phantom {
                    return None;
                }

                self.trace_handler
                    .data_man
                    .transactions_traces_by_block_hash(&tx_index.block_hash)
                    .and_then(|(pivot_hash, traces)| {
                        let pivot_epoch_number = self
                            .trace_handler
                            .data_man
                            .block_height_by_hash(&pivot_hash)
                            .unwrap();
                        traces
                            .into_iter()
                            .nth(tx_index.real_index)
                            .and_then(|tx_trace| {
                                tx_trace
                                    .filter_trace_pairs(
                                        &PrimitiveTraceFilter::space_filter(
                                            Space::Ethereum,
                                        ),
                                    )
                                    .ok()
                            })
                            .map(|traces| {
                                traces
                                    .into_iter()
                                    .map(|paired_trace| {
                                        let mut eth_trace = EthLocalizedTrace {
                                            action: RpcAction::try_from(
                                                paired_trace.0.action,
                                                self.trace_handler.network,
                                            )
                                            .unwrap()
                                            .try_into()
                                            .unwrap(),
                                            result: EthRes::None,
                                            trace_address: vec![],
                                            subtraces: 0,
                                            // FIXME(lpl): follow the value of
                                            // tx index?
                                            transaction_position: tx_index
                                                .rpc_index,
                                            transaction_hash: None,
                                            block_number: pivot_epoch_number,
                                            block_hash: pivot_hash,
                                        };
                                        eth_trace
                                            .set_result(
                                                RpcAction::try_from(
                                                    paired_trace.1.action,
                                                    self.trace_handler.network,
                                                )
                                                .unwrap(),
                                            )
                                            .unwrap();
                                        eth_trace
                                    })
                                    .collect()
                            })
                    })
            }))
    }
}
