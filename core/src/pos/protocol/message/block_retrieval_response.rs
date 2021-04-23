// Copyright 2019-2020 Conflux Foundation. All rights reserved.
// TreeGraph is free software and distributed under Apache License 2.0.
// See https://www.apache.org/licenses/LICENSE-2.0

use super::super::sync_protocol::{Context, Handleable, RpcResponse};
use crate::{
    message::RequestId,
    pos::{
        consensus::consensus_types::{
            block_retrieval::BlockRetrievalResponse, common::Payload,
        },
        protocol::{
            message::block_retrieval::BlockRetrievalRpcRequest,
            request_manager::AsAny,
        },
    },
    sync::Error,
};
use serde::{Deserialize, Serialize};
use std::any::Any;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BlockRetrievalRpcResponse<P> {
    pub request_id: RequestId,
    #[serde(bound(
        deserialize = "BlockRetrievalResponse<P>: Deserialize<'de>"
    ))]
    pub response: BlockRetrievalResponse<P>,
}

impl<P: Payload> RpcResponse for BlockRetrievalRpcResponse<P> {}

impl<P: Payload> AsAny for BlockRetrievalRpcResponse<P> {
    fn as_any(&self) -> &dyn Any { self }

    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

impl<P: Payload> Handleable<P> for BlockRetrievalRpcResponse<P> {
    fn handle(self, ctx: &Context<P>) -> Result<(), Error> {
        let mut req = ctx.match_request(self.request_id)?;
        // FIXME: There is a potential issue if downcast error happens.
        match req.downcast_mut::<BlockRetrievalRpcRequest>(
            ctx.io,
            &ctx.manager.request_manager,
        ) {
            Ok(req) => {
                let res_tx = req.response_tx.take();
                if let Some(tx) = res_tx {
                    tx.send(Ok(Box::new(self)))
                        .expect("send response tx should succeed");
                }
            }
            Err(e) => {
                return Err(e);
            }
        }
        Ok(())
    }
}
