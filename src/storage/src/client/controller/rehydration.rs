// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

//! Rehydration of storage hosts.
//!
//! Rehydration is the process of bringing a crashed `storaged` process back
//! up to date. The [`RehydratingStorageClient`] records all commands it
//! observes in a minimal form. If it observes a send or receive failure while
//! communicating with the underlying client, it will reconnect the client and
//! replay the command stream.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::anyhow;
use futures::{Stream, StreamExt};
use timely::progress::Timestamp;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::{pin, select};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::warn;

use mz_ore::retry::Retry;
use mz_repr::GlobalId;
use mz_service::client::Reconnect;

use crate::client::{IngestSourceCommand, StorageClient, StorageCommand, StorageResponse};

/// A storage client that replays the command stream on failure.
///
/// See the [module documentation](self) for details.
#[derive(Debug)]
pub struct RehydratingStorageClient<T> {
    command_tx: UnboundedSender<StorageCommand<T>>,
    response_rx: UnboundedReceiverStream<StorageResponse<T>>,
}

impl<T> RehydratingStorageClient<T>
where
    T: Timestamp,
{
    /// Creates a `RehydratingStorageClient` that wraps a reconnectable
    /// [`StorageClient`].
    pub fn new<C>(client: C) -> RehydratingStorageClient<T>
    where
        C: StorageClient<T> + Reconnect + 'static,
    {
        let (command_tx, command_rx) = unbounded_channel();
        let (response_tx, response_rx) = unbounded_channel();
        let mut task = RehydrationTask {
            command_rx,
            response_tx,
            ingestions: BTreeMap::new(),
            client,
        };
        mz_ore::task::spawn(|| "rehydration", async move { task.run().await });
        RehydratingStorageClient {
            command_tx,
            response_rx: UnboundedReceiverStream::new(response_rx),
        }
    }

    /// Sends a command to the underlying client.
    pub fn send(&mut self, cmd: StorageCommand<T>) {
        self.command_tx
            .send(cmd)
            .expect("rehydration task should not drop first");
    }

    /// Returns a stream that produces responses from the underlying client.
    pub fn response_stream(&mut self) -> impl Stream<Item = StorageResponse<T>> + '_ {
        &mut self.response_rx
    }
}

/// A task that manages rehydration.
struct RehydrationTask<C, T> {
    /// A channel upon which commands intended for the storage host are delivered.
    command_rx: UnboundedReceiver<StorageCommand<T>>,
    /// A channel upon which responses from the storage host are delivered.
    response_tx: UnboundedSender<StorageResponse<T>>,
    /// The ingestions that have been observed.
    ingestions: BTreeMap<GlobalId, IngestSourceCommand<T>>,
    /// The underlying client that communicates with the storage host.
    client: C,
}

enum RehydrationTaskState {
    /// The storage host should be (re)hydrated.
    Rehydrate,
    /// Communication with the storage host is live. Commands and responses should
    /// be forwarded until an error occurs.
    Pump,
    /// The caller has asked us to shut down communication with this storage
    /// host.
    Done,
}

impl<C, T> RehydrationTask<C, T>
where
    C: StorageClient<T> + Reconnect + 'static,
    T: Timestamp,
{
    async fn run(&mut self) {
        let mut state = RehydrationTaskState::Rehydrate;
        loop {
            state = match state {
                RehydrationTaskState::Rehydrate => self.step_rehydrate().await,
                RehydrationTaskState::Pump => self.step_pump().await,
                RehydrationTaskState::Done => break,
            }
        }
    }

    async fn step_rehydrate(&mut self) -> RehydrationTaskState {
        // Reconnect to the storage host.
        let retry = Retry::default()
            .clamp_backoff(Duration::from_secs(32))
            .into_retry_stream();
        pin!(retry);
        loop {
            match self.client.reconnect().await {
                Ok(()) => break,
                Err(e) => {
                    warn!("error connecting to storage host, retrying: {e}");
                    retry.next().await;
                }
            }
        }

        // Rehydrate all commands.
        self.send_command(StorageCommand::IngestSources(
            self.ingestions.values().cloned().collect(),
        ))
        .await
    }

    async fn step_pump(&mut self) -> RehydrationTaskState {
        select! {
            // Command from controller to forward to storage host.
            command = self.command_rx.recv() => match command {
                None => RehydrationTaskState::Done,
                Some(command) => {
                    self.absorb_command(&command);
                    self.send_command(command).await
                }
            },
            // Response from storage host to forward to controller.
            response = self.client.recv() => {
                let response = match response.transpose() {
                    None => {
                        // In the future, if a storage host politely hangs up,
                        // we might want to take it as a signal that a new
                        // controller has taken over. For now we just try to
                        // reconnect.
                        Err(anyhow!("storage host unexpectedly gracefully terminated connection"))
                    }
                    Some(response) => response,
                };
                self.send_response(response).await
            }
        }
    }

    async fn send_command(&mut self, command: StorageCommand<T>) -> RehydrationTaskState {
        match self.client.send(command).await {
            Ok(()) => RehydrationTaskState::Pump,
            Err(e) => self.send_response(Err(e)).await,
        }
    }

    async fn send_response(
        &mut self,
        response: Result<StorageResponse<T>, anyhow::Error>,
    ) -> RehydrationTaskState {
        match response {
            Ok(response) => {
                if self.response_tx.send(response).is_err() {
                    RehydrationTaskState::Done
                } else {
                    RehydrationTaskState::Pump
                }
            }
            Err(e) => {
                warn!("storage host produced error, reconnecting: {e}");
                RehydrationTaskState::Rehydrate
            }
        }
    }

    fn absorb_command(&mut self, command: &StorageCommand<T>) {
        match command {
            StorageCommand::IngestSources(ingestions) => {
                for ingestion in ingestions {
                    self.ingestions.insert(ingestion.id, ingestion.clone());
                }
            }
            StorageCommand::AllowCompaction(frontiers) => {
                for (id, frontier) in frontiers {
                    if frontier.is_empty() {
                        self.ingestions.remove(id);
                    }
                }
            }
        }
    }
}
