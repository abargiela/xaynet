use super::term;
use chrono::{DateTime, Utc};
use influxdb::{Client, Query};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::interval;
use xaynet_client::api::{ApiClient, HttpApiClient};
use xaynet_server::settings::InfluxSettings;

pub async fn wait_until_coordinator_is_ready(client: &mut HttpApiClient) {
    let spinner = term::spinner("Test connection to coordinator...", "");
    let mut intv = interval(Duration::from_millis(500));
    while client.get_round_params().await.is_err() {
        intv.tick().await;
    }
    spinner.finish_with_message("Ok");
}

pub async fn wait_until_influx_is_ready(client: &Client) {
    let spinner = term::spinner("Test connection to influx...", "");
    let mut intv = interval(Duration::from_millis(500));
    while client.ping().await.is_err() {
        intv.tick().await;
    }
    spinner.finish_with_message("Ok");
}

pub async fn wait_until_phase(client: &Client, phase: u8) {
    let spinner = term::spinner(&format!("Wait for phase {}", phase), "");
    let mut intv = interval(Duration::from_millis(500));

    loop {
        intv.tick().await;
        let read_query = Query::raw_read_query("SELECT LAST(value) FROM phase GROUP BY *");
        let read_result = client.json_query(read_query).await.unwrap();
        match deserialize_phase(read_result) {
            Ok(current_phase) => {
                if current_phase == phase {
                    break;
                } else {
                    spinner.set_message(&format!("current phase: {}", current_phase));
                }
            }
            Err(_) => spinner.set_message(&format!(
                "No phase metrics available! If you use docker-compose, try to restart the coordinator via `docker restart docker_coordinator_1`"
            )),
        }
    }

    spinner.finish_with_message(&format!("Ok"));
}

fn deserialize_phase(
    mut result: influxdb::integrations::serde_integration::DatabaseQueryResult,
) -> anyhow::Result<u8> {
    let phase = result
        .deserialize_next::<PhaseReading>()
        .map_err(|_| anyhow::anyhow!("no phase"))?
        .series
        .first()
        .ok_or(anyhow::anyhow!("no phase"))?
        .values
        .first()
        .ok_or(anyhow::anyhow!("no phase"))?
        .value;
    Ok(phase)
}

#[derive(Debug, serde::Deserialize)]
pub struct PhaseReading {
    time: DateTime<Utc>,
    value: u8,
}

pub struct TestEnvironment {
    pub influx_client: Option<Client>,
    pub api_client: Option<HttpApiClient>,
}

impl TestEnvironment {
    pub fn new() -> Self {
        Self {
            influx_client: None,
            api_client: None,
        }
    }

    pub fn with_influx_client(mut self, influx_settings: InfluxSettings) -> Self {
        self.influx_client = Some(Client::new(influx_settings.url, influx_settings.db));
        self
    }

    #[cfg(not(feature = "tls"))]
    pub fn with_api_client(mut self, api_url: &str) -> anyhow::Result<Self> {
        self.api_client = Some(HttpApiClient::new(api_url)?);
        Ok(self)
    }

    #[cfg(feature = "tls")]
    pub fn with_api_client(
        mut self,
        api_url: &str,
        certificates: Vec<PathBuf>,
    ) -> anyhow::Result<Self> {
        self.api_client = Some(HttpApiClient::new(api_url, certificates)?);
        Ok(self)
    }

    pub async fn pre_checks(&mut self) {
        if let Some(ref influx_client) = self.influx_client {
            // check for connectivity with the influx
            wait_until_influx_is_ready(influx_client).await;
            // check for phase metrics and if the coordinator is in sum phase
            wait_until_phase(influx_client, 1).await;
        }

        if let Some(ref mut api_client) = self.api_client {
            // check for connectivity with the coordinator
            wait_until_coordinator_is_ready(api_client).await;
        }
    }

    pub fn get_influx_client(&self) -> Client {
        self.influx_client.clone().unwrap()
    }

    pub fn get_api_client(&self) -> HttpApiClient {
        self.api_client.clone().unwrap()
    }
}
