use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::{select, time};
use tokio_stream::StreamExt;
use tokio_util::codec::Framed;
use tokio_util::codec::{LinesCodec, LinesCodecError};

use std::io;

use crate::base::actions::{Action, ActionResponse};
use crate::base::{Buffer, Config, Package, Partitions};
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use toml::Value;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Io error {0}")]
    Io(#[from] io::Error),
    #[error("Stream done")]
    StreamDone,
    #[error("Lines codec error {0}")]
    Codec(#[from] LinesCodecError),
    #[error("Serde error {0}")]
    Json(#[from] serde_json::error::Error),
}

// TODO Don't do any deserialization on payload. Read it a Vec<u8> which is in turn a json
// TODO which cloud will double deserialize (Batch 1st and messages next)
#[derive(Debug, Serialize, Deserialize)]
pub struct Payload {
    stream: String,
    #[serde(flatten)]
    payload: Value,
}

pub struct Bridge {
    config: Arc<Config>,
    data_tx: Sender<Box<dyn Package>>,
    actions_rx: Receiver<Action>,
    current_action: Option<String>,
}

impl Bridge {
    pub fn new(config: Arc<Config>, data_tx: Sender<Box<dyn Package>>, actions_rx: Receiver<Action>) -> Bridge {
        Bridge { config, data_tx, actions_rx, current_action: None }
    }

    pub async fn start(&mut self) {
        loop {
            let addr = format!("0.0.0.0:{}", self.config.bridge_port);
            let listener = match TcpListener::bind(&addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to bind to {}. Error = {:?}. Stopping collector", addr, e);
                    return;
                }
            };

            let (stream, addr) = loop {
                select! {
                    v = listener.accept() =>  {
                        match v {
                            Ok(s) => break s,
                            Err(e) => {
                                error!("Tcp connection error = {:?}", e);
                                continue;
                            }
                        }
                    }
                    Some(action) = self.actions_rx.recv() => {
                        error!("Bridge down!! Action ID = {}", action.id);
                        let mut status = ActionResponse::new(&action.id, "Failed");
                        status.add_error(format!("Bridge down"));

                        // Send failure notification to cloud
                        if let Err(e) = self.data_tx.send(Box::new(status)).await {
                            error!("Failed to send status. Error = {:?}", e);
                        }
                    }
                }
            };

            info!("Accepted new connection from {:?}", addr);
            let framed = Framed::new(stream, LinesCodec::new());
            if let Err(e) = self.collect(framed).await {
                error!("Bridge failed. Error = {:?}", e);
            }
        }
    }

    pub async fn collect(&mut self, mut framed: Framed<TcpStream, LinesCodec>) -> Result<(), Error> {
        let streams = self.config.streams.iter();
        let streams = streams.map(|(stream, config)| (stream.to_owned(), config.buf_size as usize)).collect();
        let mut partitions = Partitions::new(self.data_tx.clone(), streams);
        let action_timeout = time::sleep(Duration::from_secs(10));

        tokio::pin!(action_timeout);
        loop {
            select! {
                frame = framed.next() => {
                    let frame = frame.ok_or(Error::StreamDone)??;
                    info!("Received line = {:?}", frame);

                    match self.current_action.take() {
                        Some(id) => debug!("Response for action = {:?}", id),
                        None => {
                            error!("Action timed out already");
                            continue
                        }
                    }

                    let data: Payload = match serde_json::from_str(&frame) {
                        Ok(d) => d,
                        Err(e) => {
                            error!("Deserialization error = {:?}", e);
                            continue
                        }
                    };

                    // TODO remove stream clone
                    if let Err(e) = partitions.fill(&data.stream.clone(), data).await {
                        error!("Failed to send data. Error = {:?}", e);
                    }
                }
                action = self.actions_rx.recv() => {
                    let action = action.ok_or(Error::StreamDone)?;
                    self.current_action = Some(action.id.to_owned());

                    action_timeout.as_mut().reset(Instant::now() + Duration::from_secs(10));
                    let data = match serde_json::to_vec(&action) {
                        Ok(d) => d,
                        Err(e) => {
                            error!("Serialization error = {:?}", e);
                            continue
                        }
                    };

                    framed.get_mut().write_all(&data).await?;
                    framed.get_mut().write_all(b"\n").await?;
                }
                _ = &mut action_timeout, if self.current_action.is_some() => {
                    let action = self.current_action.take().unwrap();
                    error!("Timeout waiting for action response. Action ID = {}", action);

                    // Send failure response to cloud
                    let mut status = ActionResponse::new(&action, "Failed");
                    status.add_error(format!("Action timed out"));
                    if let Err(e) = self.data_tx.send(Box::new(status)).await {
                        error!("Failed to send status. Error = {:?}", e);
                    }
                }
            }
        }
    }
}

impl Package for Buffer<Payload> {
    fn stream(&self) -> String {
        return self.stream.clone();
    }
    fn serialize(&self) -> Vec<u8> {
        serde_json::to_vec(&self.buffer).unwrap()
    }
}
