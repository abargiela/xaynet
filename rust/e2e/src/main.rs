use influxdb::Client;
use std::path::PathBuf;

use std::process;
use tracing_subscriber::{fmt::Formatter, reload::Handle, EnvFilter, *};
use xaynet_client::{api::HttpApiClient, mobile_client::participant::MaxMessageSize};
use xaynet_server::settings::Settings;

#[macro_use]
extern crate tracing;

mod cf;
mod test_client;
use test_client::{TestClientBuilder, TestClientBuilderSettings};
mod term;
mod utils;

#[cfg(feature = "k8s")]
mod k8s;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fmt_subscriber = FmtSubscriber::builder()
        .with_env_filter("e2e=debug")
        .with_ansi(true)
        .with_filter_reloading();
    let filter_handle = fmt_subscriber.reload_handle();
    fmt_subscriber.init();

    #[cfg(feature = "k8s")]
    let (mut coordinator_handle, mut influx_handle) = {
        let k8s_settings = k8s::K8sSettings::new(PathBuf::from("test_case/k8s.toml"))
            .unwrap_or_else(|err| {
                error!("{}", err);
                process::exit(1);
            });
        let coordinator_handle = k8s::init(k8s_settings).await?;
        let influx_handle = k8s::K8sClient::new_port_forward("influxdb-0", "8086:8086")?;
        (coordinator_handle, influx_handle)
    };

    let coordinator_settings = Settings::new(PathBuf::from("test_case/config.toml"))
        .unwrap_or_else(|err| {
            error!("{}", err);
            process::exit(1);
        });

    // check for phase metrics
    let influx_client = Client::new(
        "http://localhost:8086",
        coordinator_settings.metrics.influxdb.db.clone(),
    );
    utils::wait_until_phase(&influx_client, 1).await;

    let http_client = HttpApiClient::new("http://localhost:8081")?;
    utils::wait_until_coordinator_is_ready(http_client).await;

    let influx_client = Client::new(
        "http://localhost:8086",
        coordinator_settings.metrics.influxdb.db.clone(),
    );

    let _ = run_rounds(coordinator_settings, filter_handle, &influx_client).await;

    #[cfg(feature = "k8s")]
    {
        info!("Close port forward");
        coordinator_handle.kill()?;
        influx_handle.kill()?;
    }

    Ok(())
}

async fn run_rounds(
    coordinator_settings: Settings,
    filter_handle: Handle<EnvFilter, Formatter>,
    influx_client: &Client,
) -> anyhow::Result<()> {
    let test_client_builder_settings = TestClientBuilderSettings::from_coordinator_settings(
        coordinator_settings,
        "http://localhost:8081",
        1_f64,
        MaxMessageSize::unlimited(),
    );

    let mut test_client_builder = TestClientBuilder::new(test_client_builder_settings)?;

    for _ in 0..10 {
        let mut runner = test_client_builder
            .build_clients(filter_handle.clone())
            .await?;
        utils::wait_until_phase(influx_client, 1).await;
        info!("run sum clients...");
        runner.run_sum_clients().await?;
        utils::wait_until_phase(influx_client, 2).await;
        info!("run update clients...");
        runner.run_update_clients().await?;
        utils::wait_until_phase(influx_client, 3).await;
        info!("run sum2 clients...");
        runner.run_sum2_clients().await?;
        utils::wait_until_phase(influx_client, 1).await;
    }

    Ok(())
}
