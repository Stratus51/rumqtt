mod state;
mod network;
mod connection;

use std::thread;
use std::sync::Arc;
use std::result::Result;
use std::time::Duration;

use futures::sync::mpsc::{self, Sender};
use futures::{Future, Sink};
use mqtt3::*;

use MqttOptions;
use ReconnectOptions;
use packet;

use error::{ConnectError, ClientError};
use crossbeam_channel::{bounded, self};

/// Interface on which clients can receive messages
pub type Notification<T> = crossbeam_channel::Receiver<T>;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ConnectCount {
    InitialConnect,
    ConnectedBefore(u32)
}

#[derive(Clone)]
pub enum Command {
    Mqtt(Packet),
    Halt,
}

pub struct MqttClient {
    nw_request_tx: Sender<Command>,
    max_packet_size: usize,
}

impl MqttClient {
    /// Connects to the broker and starts an event loop in a new thread.
    /// Returns 'Request' and handles reqests from it.
    /// Also handles network events, reconnections and retransmissions.
    pub fn start(opts: MqttOptions) -> (Self, Notification<Packet>) {
        let (commands_tx, commands_rx) = mpsc::channel(10);
        let (notifier_tx, notifier_rx) = bounded(50);

        let max_packet_size = opts.max_packet_size;
        let reconnect_config = opts.reconnect;
        let mut sleep_duration = Duration::new(10, 0);

        thread::spawn( move || {
            let mut connection = connection::Connection::new(opts, commands_rx, notifier_tx);

            'reconnect: loop {
                if let Err((e, connection_count)) = connection.start() {
                    match e {
                        ConnectError::Halt => {error!("Halting connection thread"); break 'reconnect},
                        _ => (),
                    }

                    error!("Network connection failed. Error = {:?}, Connection count = {:?}", e, connection_count);
                    match reconnect_config {
                        ReconnectOptions::Never => break 'reconnect,
                        ReconnectOptions::AfterFirstSuccess(d) if connection_count != ConnectCount::InitialConnect => sleep_duration = Duration::new(u64::from(d), 0),
                        ReconnectOptions::AfterFirstSuccess(_) => break 'reconnect,
                        ReconnectOptions::Always(d) =>  sleep_duration = Duration::new(u64::from(d), 0),
                    }
                }

                info!("Will sleep for {:?} seconds before reconnecting", sleep_duration);
                thread::sleep(sleep_duration);
            };
        });

        let client = MqttClient { nw_request_tx: commands_tx, max_packet_size: max_packet_size};
        (client, notifier_rx)
    }

    pub fn publish<S: Into<String>>(&mut self, topic: S, qos: QoS, payload: Vec<u8>) -> Result<(), ClientError>{
        let payload_len = payload.len();

        if payload_len > self.max_packet_size {
            return Err(ClientError::PacketSizeLimitExceeded)
        }

        let payload = Arc::new(payload);

        let tx = &mut self.nw_request_tx;
        let publish = packet::gen_publish_packet(topic.into(), qos, None, false, false, payload);
        let packet = Packet::Publish(publish);

        tx.send(Command::Mqtt(packet)).wait()?;

        Ok(())
    }

    // TODO: Add userdata publish

    pub fn subscribe<S: Into<String>>(&mut self, topics: Vec<(S, QoS)>) -> Result<(), ClientError> {
        if topics.len() == 0 {
            return Err(ClientError::ZeroSubscriptions);
        }

        let sub_topics: Vec<_> = topics.into_iter().map(|t| {
            SubscribeTopic{topic_path: t.0.into(), qos: t.1}
        }).collect();

        let tx = &mut self.nw_request_tx;
        let subscribe = Subscribe {pid: PacketIdentifier::zero(), topics: sub_topics};
        let packet = Packet::Subscribe(subscribe);

        tx.send(Command::Mqtt(packet)).wait()?;
        Ok(())
    }
}

impl Drop for MqttClient {
    fn drop(&mut self) {
        let tx = &mut self.nw_request_tx;
        let _ = tx.send(Command::Halt).wait();
    }
}
