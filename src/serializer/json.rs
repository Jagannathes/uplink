use crate::collector::simulator::Buffer;

use crossbeam_channel::Receiver;
use rumqtt::{MqttOptions, MqttClient, QoS, ReconnectOptions, SecurityOptions};

pub struct Serializer {
    collector_rx: Receiver<Buffer>,
    mqtt_client: rumqtt::MqttClient
}

impl Serializer {
    pub(crate) fn new(collector_rx: Receiver<Buffer>) -> Serializer {
        let bike_id = "bike-100";
        let reconnection_options = ReconnectOptions::AfterFirstSuccess(5);
        let (rsa_private, ca) = get_certs();
        let security_options = SecurityOptions::GcloudIot("cloudlinc".to_owned(), rsa_private.to_vec(), 60);
        let client_id = format!("projects/cloudlinc/locations/asia-east1/registries/iotcore/devices/{}", bike_id);

        let mqtt_options = MqttOptions::new(client_id, "mqtt.googleapis.com", 8883)
            .set_keep_alive(60)
            .set_reconnect_opts(reconnection_options)
            .set_ca(ca)
            .set_security_opts(security_options);

        let (mqtt_client, _notifications) = MqttClient::start(mqtt_options).unwrap();

        Serializer {
            collector_rx,
            mqtt_client
        }
    }

    pub(crate) fn start(&mut self) {
        let bike_id = "bike-100";
        let sample_topic = format!("/devices/{}/events/sample", bike_id);
        let qos = QoS::AtLeastOnce;

        for data in self.collector_rx.iter() {
            let buffer = &data.buffer;
            let channel = &data.channel;

            let payload = serde_json::to_string(buffer).unwrap();
            println!("Channel = {:?}, Payload = {:?}", channel, payload);
            self.mqtt_client.publish(&sample_topic, qos, false, payload).unwrap();
        }
    }
}

fn get_certs() -> (Vec<u8>, Vec<u8>) {
    let key = include_bytes!("../../certs/bike-100/rsa_private.der");
    let ca = include_bytes!("../../certs/bike-100/roots.pem");

    (key.to_vec(), ca.to_vec())
}