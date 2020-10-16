use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use kube::{
    api::{
        Api,
        DeleteParams,
        ListParams,
        Meta,
        PatchParams,
        PatchStrategy,
        PostParams,
        WatchEvent,
    },
    Client,
};
use serde::Deserialize;
use serde_json::json;
use std::{fmt, path::PathBuf, process::Stdio};
use tokio::{
    fs,
    process::{Child, Command},
};

use config::{Config, ConfigError, Environment};

#[derive(Deserialize, Clone)]
pub struct K8sSettings {
    namespace: String,
    app_label: String,
    config: String,
    #[serde(default = "default_image")]
    image: String,
}

fn default_image() -> String {
    "xaynetwork/xaynet:development".to_string()
}

impl K8sSettings {
    pub fn new(path: PathBuf) -> anyhow::Result<Self> {
        let settings: K8sSettings = Self::load(path)?;
        Ok(settings)
    }

    fn load(path: PathBuf) -> anyhow::Result<Self> {
        let mut config = Config::new();
        config.merge(config::File::from(path))?;
        config
            .try_into()
            .map_err(|e| anyhow::anyhow!("config error: {}", e))
    }
}

pub struct K8sClient {
    client: Client,
    settings: K8sSettings,
}

impl K8sClient {
    pub async fn new(settings: K8sSettings) -> anyhow::Result<Self> {
        let client = Client::try_default().await?;
        Ok(Self { client, settings })
    }

    pub async fn find_coordinator_pod(&self) -> anyhow::Result<Pod> {
        self.find_pod(&format!("app={}", &self.settings.app_label))
            .await
    }

    pub async fn find_pod(&self, label: &str) -> anyhow::Result<Pod> {
        info!("Find pod with label: {}", label);
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), &self.settings.namespace);
        let lp = ListParams::default().labels(label);

        let pods = pod_api.list(&lp).await?;
        let found_pod = pods
            .into_iter()
            .take(1)
            .next()
            .ok_or(anyhow::anyhow!("Cannot find pod with label: {}", label))?;

        Ok(found_pod)
    }

    pub async fn patch_config_map(&self, config_id: &str) -> anyhow::Result<()> {
        info!("Patching config map: {}", config_id);
        let cm_api: Api<ConfigMap> = Api::namespaced(self.client.clone(), &self.settings.namespace);
        let config_content = fs::read_to_string(&self.settings.config).await?;

        let config_patch = json!(
            {
                "data": {
                    "config.toml":  config_content
                }
            }
        );

        let pp = PatchParams {
            patch_strategy: PatchStrategy::Strategic,
            ..Default::default()
        };
        cm_api
            .patch(config_id, &pp, serde_json::to_vec(&config_patch)?)
            .await?;
        info!("Patched config map: {}", config_id);
        Ok(())
    }

    pub async fn restart_pod(&self, pod_id: &str) -> anyhow::Result<String> {
        info!("Restarting pod: {}", pod_id);
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), &self.settings.namespace);

        let dp = DeleteParams::default();
        pod_api.delete(pod_id, &dp).await?.map_left(|pdel| {
            info!("Deleting pod: {}", Meta::name(&pdel));
        });

        let new_pod_id = self.wait_until_restarted(pod_api).await?;

        info!("New pod id: {}", new_pod_id);
        Ok(new_pod_id)
    }

    pub async fn patch_image(&self, pod_id: &str) -> anyhow::Result<String> {
        info!("Patching image of pod: {}", pod_id);
        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), &self.settings.namespace);

        let pp = PatchParams {
            patch_strategy: PatchStrategy::Strategic,
            ..Default::default()
        };
        let image_patch = json!(
            {
                "spec": {
                    "containers": [
                        {
                            "name": "coordinator",
                            "image": &self.settings.image
                        }
                    ]
                }
            }
        );

        pod_api
            .patch(pod_id, &pp, serde_json::to_vec(&image_patch)?)
            .await?;

        let new_pod_id = self.wait_until_restarted(pod_api).await?;

        info!("Patched pod: {}", new_pod_id);
        Ok(new_pod_id)
    }

    async fn wait_until_restarted(&self, pod_api: Api<Pod>) -> anyhow::Result<String> {
        let lp = ListParams::default().labels(&format!("app={}", &self.settings.app_label));
        let mut stream = pod_api.watch(&lp, "0").await?.boxed();

        loop {
            if let Some(status) = stream.try_next().await? {
                match status {
                    WatchEvent::Added(o) => info!("Added {}", Meta::name(&o)),
                    WatchEvent::Modified(o) => {
                        let s = o.status.as_ref().expect("status exists on pod");
                        let phase = s.phase.clone().unwrap_or_default();
                        info!("Modified: {} with phase: {}", Meta::name(&o), phase);
                        if phase == "Running" {
                            break Ok(Meta::name(&o));
                        }
                    }
                    WatchEvent::Deleted(o) => info!("Deleted {}", Meta::name(&o)),
                    WatchEvent::Error(e) => error!("Error {}", e),
                    _ => {}
                }
            }
        }
    }

    pub fn reveal_pod_and_config_map_id(pod: Pod) -> anyhow::Result<(String, String)> {
        let pod_id = Meta::name(&pod);
        let config_map_id = pod
            .spec
            .ok_or(anyhow::anyhow!("Cannot `spec` field",))?
            .volumes
            .ok_or(anyhow::anyhow!("Cannot `volumes` field",))?
            .get(1)
            .ok_or(anyhow::anyhow!("Cannot `config_map` field",))?
            .config_map
            .clone()
            .ok_or(anyhow::anyhow!("Cannot `config_map` field",))?
            .name
            .ok_or(anyhow::anyhow!("Cannot `name` field",))?;
        info!("Pod id {}, config map id {}", pod_id, config_map_id);
        Ok((pod_id, config_map_id))
    }

    pub fn new_port_forward(pod_id: &str, port_mapping: &str) -> anyhow::Result<Child> {
        info!(
            "New port forward for pod: {} with port mapping: {}",
            pod_id, port_mapping
        );
        let handle = Command::new("kubectl")
            .arg("port-forward")
            .arg(pod_id)
            .arg(port_mapping)
            .stdout(Stdio::null())
            .spawn()?;
        Ok(handle)
    }
}

pub async fn init(settings: K8sSettings) -> anyhow::Result<Child> {
    let client = K8sClient::new(settings).await?;
    let pod = client.find_coordinator_pod().await?;
    let (pod_id, config_map_id) = K8sClient::reveal_pod_and_config_map_id(pod)?;
    client.patch_config_map(&config_map_id).await?;
    let new_pod_id = client.restart_pod(&pod_id).await?;
    K8sClient::new_port_forward(&new_pod_id, "8081:8081")
}
