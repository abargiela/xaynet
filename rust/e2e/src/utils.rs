use super::term;
use chrono::{DateTime, Utc};
use influxdb::{Client, Query};
use std::time::Duration;
use tokio::time::interval;
use xaynet_client::api::{ApiClient, HttpApiClient};

pub async fn wait_until_coordinator_is_ready(mut client: HttpApiClient) {
    let spinner = term::spinner("Test connection to coordinator...", "");
    let mut intv = interval(Duration::from_millis(500));
    while client.get_round_params().await.is_err() {
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
                "No phase metrics! If you use docker-compose, try to restart the coordinator via `docker restart docker_coordinator_1`"
            )),
        }
    }

    spinner.finish_with_message(&format!("Ok",));
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
