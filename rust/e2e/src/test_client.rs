use super::{cf::ConcurrentFutures, term::ProgressBar};
use futures::{Future, StreamExt};
use std::pin::Pin;
use tracing_subscriber::{fmt::Formatter, reload::Handle, EnvFilter};
use xaynet_client::{
    api::{ApiClient, HttpApiClient},
    mobile_client::{
        client::ClientStateMachine,
        participant::{AggregationConfig, MaxMessageSize, ParticipantSettings},
        ClientStateName, LocalModelCache, MobileClient,
    },
};
use xaynet_core::{
    common::RoundParameters,
    crypto::{ByteObject, Signature},
    mask::{FromPrimitives, Model},
    ParticipantSecretKey,
};

pub struct TestClient {
    api: HttpApiClient,
    local_model: LocalModelCache,
    client_state: ClientStateMachine,
}

impl TestClient {
    pub fn new(
        api_client: HttpApiClient,
        participant_settings: ParticipantSettings,
    ) -> anyhow::Result<Self> {
        let client_state = ClientStateMachine::new(participant_settings)?;

        Ok(Self {
            api: api_client,
            client_state,
            local_model: LocalModelCache(None),
        })
    }

    pub async fn try_to_proceed(self) -> Self {
        let TestClient {
            mut api,
            mut local_model,
            client_state,
        } = self;

        let new_state = client_state.next(&mut api, &mut local_model).await;

        Self {
            api,
            local_model,
            client_state: new_state,
        }
    }

    pub fn get_current_state(&self) -> ClientStateName {
        match self.client_state {
            ClientStateMachine::Awaiting(_) => ClientStateName::Awaiting,
            ClientStateMachine::Sum(_) => ClientStateName::Sum,
            ClientStateMachine::Update(_) => ClientStateName::Update,
            ClientStateMachine::Sum2(_) => ClientStateName::Sum2,
        }
    }

    pub fn set_local_model(&mut self, model: Model) {
        self.local_model.set_local_model(model);
    }
}

pub struct TestClientBuilderSettings {
    test_client_settings: TestClientSettings,
    number_of_sum: u64,
    number_of_update: u64,
    model_size: usize,
}

impl TestClientBuilderSettings {
    pub fn from_coordinator_settings(
        coordinator_settings: xaynet_server::settings::Settings,
        scalar: f64,
        max_message_size: MaxMessageSize,
    ) -> Self {
        let test_client_settings = TestClientSettings {
            aggregation_config: AggregationConfig {
                mask: coordinator_settings.mask.clone().into(),
                scalar,
            },
            max_message_size,
        };

        Self {
            test_client_settings,
            number_of_sum: coordinator_settings.pet.min_sum_count,
            number_of_update: coordinator_settings.pet.min_update_count,
            model_size: coordinator_settings.model.size,
        }
    }
}

pub struct TestClientBuilder {
    settings: TestClientBuilderSettings,
    api_client: HttpApiClient,
}

impl TestClientBuilder {
    pub fn new(settings: TestClientBuilderSettings, api_client: HttpApiClient) -> Self {
        Self {
            api_client,
            settings,
        }
    }

    pub async fn build_sum_clients(
        &mut self,
    ) -> anyhow::Result<ConcurrentFutures<Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>>>
    {
        let update_sender = ProgressBar::new(self.settings.number_of_sum, "sum participants");
        let round_params = self.api_client.get_round_params().await?;

        let mut sum_clients = ConcurrentFutures::<
            Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>,
        >::new(100);
        for _ in 0..self.settings.number_of_sum {
            let sum_client = build_sum_client(
                round_params.clone(),
                self.api_client.clone(),
                self.settings.test_client_settings.clone(),
            )?;
            sum_clients.push(Box::pin(async {
                let sum_client = sum_client.try_to_proceed().await;
                sum_client.try_to_proceed().await
            }));
            let _ = update_sender.send(());
        }

        Ok(sum_clients)
    }

    pub async fn build_update_clients(
        &mut self,
    ) -> anyhow::Result<ConcurrentFutures<Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>>>
    {
        let update_sender = ProgressBar::new(self.settings.number_of_update, "update participants");
        let round_params = self.api_client.get_round_params().await?;

        let mut update_clients = ConcurrentFutures::<
            Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>,
        >::new(100);

        let model = Model::from_primitives(vec![1; self.settings.model_size].into_iter())?;
        for _ in 0..self.settings.number_of_update {
            let mut update_client = build_update_client(
                round_params.clone(),
                self.api_client.clone(),
                self.settings.test_client_settings.clone(),
            )?;
            update_client.set_local_model(model.clone());
            update_clients.push(Box::pin(async {
                let update_client = update_client.try_to_proceed().await;
                update_client.try_to_proceed().await
            }));
            let _ = update_sender.send(());
        }

        Ok(update_clients)
    }

    pub async fn build_clients(
        &mut self,
        filter_handle: Handle<EnvFilter, Formatter>,
    ) -> anyhow::Result<ClientRunner> {
        let sum_clients = self.build_sum_clients().await?;
        let update_clients = self.build_update_clients().await?;
        Ok(ClientRunner::new(
            filter_handle,
            sum_clients,
            update_clients,
        ))
    }
}

#[derive(Clone)]
pub struct TestClientSettings {
    aggregation_config: AggregationConfig,
    max_message_size: MaxMessageSize,
}

impl TestClientSettings {
    fn into_participant_settings(self, secret_key: ParticipantSecretKey) -> ParticipantSettings {
        ParticipantSettings {
            secret_key,
            aggregation_config: self.aggregation_config,
            max_message_size: self.max_message_size,
        }
    }
}

fn new_client(round_params: &RoundParameters) -> (ClientStateName, ParticipantSecretKey) {
    let secret_key = MobileClient::create_participant_secret_key();
    let role = determine_role(
        secret_key.clone(),
        round_params.seed.as_slice(),
        round_params.sum,
        round_params.update,
    );
    (role, secret_key)
}

fn build_client(
    participant_settings: ParticipantSettings,
    api_client: HttpApiClient,
) -> anyhow::Result<TestClient> {
    TestClient::new(api_client, participant_settings)
}

fn build_sum_client(
    round_params: RoundParameters,
    api_client: HttpApiClient,
    test_client_settings: TestClientSettings,
) -> anyhow::Result<TestClient> {
    loop {
        if let (ClientStateName::Sum, secret_key) = new_client(&round_params) {
            let sum_client = build_client(
                test_client_settings.into_participant_settings(secret_key),
                api_client,
            )?;
            break Ok(sum_client);
        }
    }
}

fn build_update_client(
    round_params: RoundParameters,
    api_client: HttpApiClient,
    test_client_settings: TestClientSettings,
) -> anyhow::Result<TestClient> {
    loop {
        if let (ClientStateName::Update, secret_key) = new_client(&round_params) {
            let update_client = build_client(
                test_client_settings.into_participant_settings(secret_key),
                api_client,
            )?;
            break Ok(update_client);
        }
    }
}

pub fn determine_role(
    secret_key: ParticipantSecretKey,
    round_seed: &[u8],
    round_sum: f64,
    round_update: f64,
) -> ClientStateName {
    let (sum_signature, update_signature) = compute_signatures(secret_key, round_seed);
    if sum_signature.is_eligible(round_sum) {
        ClientStateName::Sum
    } else if update_signature.is_eligible(round_update) {
        ClientStateName::Update
    } else {
        ClientStateName::Awaiting
    }
}

/// Compute the sum and update signatures for the given round seed.
fn compute_signatures(
    secret_key: ParticipantSecretKey,
    round_seed: &[u8],
) -> (Signature, Signature) {
    (
        secret_key.sign_detached(&[round_seed, b"sum"].concat()),
        secret_key.sign_detached(&[round_seed, b"update"].concat()),
    )
}

pub struct ClientRunner {
    sum_clients:
        Option<ConcurrentFutures<Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>>>,
    update_clients:
        Option<ConcurrentFutures<Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>>>,
    sum2_clients:
        Option<ConcurrentFutures<Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>>>,
    filter_handle: Handle<EnvFilter, Formatter>,
}

impl ClientRunner {
    pub fn new(
        filter_handle: Handle<EnvFilter, Formatter>,
        sum_clients: ConcurrentFutures<Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>>,
        update_clients: ConcurrentFutures<
            Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>,
        >,
    ) -> Self {
        Self {
            sum_clients: Some(sum_clients),
            update_clients: Some(update_clients),
            sum2_clients: None,
            filter_handle,
        }
    }

    pub async fn run_sum_clients(&mut self) -> anyhow::Result<()> {
        let mut sum2_clients = ConcurrentFutures::<
            Pin<Box<dyn Future<Output = TestClient> + Send + 'static>>,
        >::new(100);

        self.filter_handle.reload("e2e=debug,xaynet=debug")?;
        let mut sum_clients = self
            .sum_clients
            .take()
            .ok_or(anyhow::anyhow!("No sum clients available"))?;

        while let Some(sum_client) = sum_clients.next().await {
            let sum_client = sum_client?;
            sum2_clients.push(Box::pin(async { sum_client.try_to_proceed().await }));
        }

        self.sum2_clients = Some(sum2_clients);
        self.filter_handle.reload("e2e=debug")?;
        Ok(())
    }

    pub async fn run_update_clients(&mut self) -> anyhow::Result<()> {
        self.filter_handle.reload("e2e=debug,xaynet=debug")?;
        let mut update_clients = self
            .update_clients
            .take()
            .ok_or(anyhow::anyhow!("No update clients available"))?;

        while update_clients.next().await.is_some() {}
        self.filter_handle.reload("e2e=debug")?;
        Ok(())
    }
    pub async fn run_sum2_clients(&mut self) -> anyhow::Result<()> {
        self.filter_handle.reload("e2e=debug,xaynet=debug")?;
        let mut sum2_clients = self
            .sum2_clients
            .take()
            .ok_or(anyhow::anyhow!("No sum2 clients available"))?;

        while sum2_clients.next().await.is_some() {}
        self.filter_handle.reload("e2e=debug")?;
        Ok(())
    }
}
