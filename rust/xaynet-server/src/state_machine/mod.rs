//! The state machine that controls the execution of the PET protocol.
//!
//! # Overview
//!
//! ![](https://mermaid.ink/svg/eyJjb2RlIjoic3RhdGVEaWFncmFtXG5cdFsqXSAtLT4gSWRsZVxuXG4gIElkbGUgLS0-IFN1bVxuICBTdW0gLS0-IFVwZGF0ZVxuICBVcGRhdGUgLS0-IFN1bTJcbiAgU3VtMiAtLT4gVW5tYXNrXG4gIFVubWFzayAtLT4gSWRsZVxuXG4gIFN1bSAtLT4gRXJyb3JcbiAgVXBkYXRlIC0tPiBFcnJvclxuICBTdW0yIC0tPiBFcnJvclxuICBVbm1hc2sgLS0-IEVycm9yXG4gIEVycm9yIC0tPiBJZGxlXG4gIEVycm9yIC0tPiBTaHV0ZG93blxuXG4gIFNodXRkb3duIC0tPiBbKl1cblxuXG5cblxuXG5cblxuICAiLCJtZXJtYWlkIjp7InRoZW1lIjoibmV1dHJhbCJ9fQ)
//!
//! The [`StateMachine`] is responsible for executing the individual tasks of the PET protocol.
//! The main tasks include: building the sum and seed dictionaries, aggregating the masked
//! models, determining the applicable mask and unmasking the global masked model.
//!
//! Furthermore, the [`StateMachine`] publishes protocol events and handles protocol errors.
//!
//! The [`StateMachine`] as well as the PET settings can be configured in the config file.
//! See [here][settings] for more details.
//!
//! # Phase states
//!
//! **Idle**
//!
//! Publishes [`PhaseName::Idle`], increments the `round id` by `1`, invalidates the
//! [`SumDict`], [`SeedDict`], `scalar` and `mask length`, updates the [`EncryptKeyPair`],
//! `thresholds` as well as the `seed` and publishes the [`EncryptKeyPair`] and the
//! [`RoundParameters`].
//!
//! **Sum**
//!
//! Publishes [`PhaseName::Sum`], builds and publishes the [`SumDict`], ensures that enough sum
//! messages have been submitted and initializes the [`SeedDict`].
//!
//! **Update**
//!
//! Publishes [`PhaseName::Update`], publishes the `scalar`, builds and publishes the
//! [`SeedDict`], ensures that enough update messages have been submitted and aggregates the
//! masked model.
//!
//! **Sum2**
//!
//! Publishes [`PhaseName::Sum2`], builds the [`MaskDict`], ensures that enough sum2
//! messages have been submitted and determines the applicable mask for unmasking the global
//! masked model.
//!
//! **Unmask**
//!
//! Publishes [`PhaseName::Unmask`], unmasks the global masked model and publishes the global
//! model.
//!
//! **Error**
//!
//! Publishes [`PhaseName::Error`] and handles [`PhaseStateError`]s that can occur during the
//! execution of the [`StateMachine`]. In most cases, the error is handled by restarting the round.
//! However, if a [`PhaseStateError::Channel`] occurs, the [`StateMachine`] will shut down.
//!
//! **Shutdown**
//!
//! Publishes [`PhaseName::Shutdown`] and shuts down the [`StateMachine`]. During the shutdown,
//! the [`StateMachine`] performs a clean shutdown of the [Request][requests_idx] channel by
//! closing it and consuming all remaining messages.
//!
//! # Requests
//!
//! By initiating a new [`StateMachine`] via [`StateMachine::new()`], a new
//! [StateMachineRequest][requests_idx] channel is created, the function of which is to send
//! [`StateMachineRequest`]s to the [`StateMachine`]. The sender half of that channel
//! ([`RequestSender`]) is returned back to the caller of [`StateMachine::new()`], whereas the
//! receiver half ([`RequestReceiver`]) is used by the [`StateMachine`].
//!
//! See [here][requests] for more details.
//!
//! # Events
//!
//! During the execution of the PET protocol, the [`StateMachine`] will publish various events
//! (see Phase states). Everyone who is interested in the events can subscribe to the respective
//! events via the [`EventSubscriber`]. An [`EventSubscriber`] is automatically created when a new
//! [`StateMachine`] is created through [`StateMachine::new()`].
//!
//! See [here][events] for more details.
//!
//! [settings]: ../settings/index.html
//! [`PhaseName::Idle`]: crate::state_machine::phases::PhaseName::Idle
//! [`PhaseName::Sum`]: crate::state_machine::phases::PhaseName::Sum
//! [`PhaseName::Update`]: crate::state_machine::phases::PhaseName::Update
//! [`PhaseName::Sum2`]: crate::state_machine::phases::PhaseName::Sum2
//! [`PhaseName::Unmask`]: crate::state_machine::phases::PhaseName::Unmask
//! [`PhaseName::Error`]: crate::state_machine::phases::PhaseName::Error
//! [`PhaseName::Shutdown`]: crate::state_machine::phases::PhaseName::Shutdown
//! [`SumDict`]: xaynet_core::SumDict
//! [`SeedDict`]: xaynet_core::SeedDict
//! [`EncryptKeyPair`]: xaynet_core::crypto::EncryptKeyPair
//! [`RoundParameters`]: xaynet_core::common::RoundParameters
//! [`MaskDict`]: crate::state_machine::coordinator::MaskDict
//! [`StateMachineRequest`]: crate::state_machine::requests::StateMachineRequest
//! [requests_idx]: ./requests/index.html
//! [events]: ./events/index.html

pub mod coordinator;
pub mod events;
pub mod phases;
pub mod requests;

use self::{
    coordinator::CoordinatorState,
    events::{EventPublisher, EventSubscriber, ModelUpdate},
    phases::{
        Idle,
        Phase,
        PhaseName,
        PhaseState,
        PhaseStateError,
        Shared,
        Shutdown,
        Sum,
        Sum2,
        Unmask,
        Update,
    },
    requests::{RequestReceiver, RequestSender},
};
use crate::{
    settings::{MaskSettings, ModelSettings, PetSettings},
    storage::{redis, MaskDictIncrError, RedisError, SeedDictUpdateError, SumDictAddError},
};
use derive_more::From;
use thiserror::Error;
use xaynet_core::mask::UnmaskingError;

#[cfg(feature = "model-persistence")]
use xaynet_core::mask::Model;

#[cfg(feature = "metrics")]
use crate::metrics::MetricsSender;

#[cfg(feature = "model-persistence")]
use crate::storage::s3;

/// Error returned when the state machine fails to handle a request
#[derive(Debug, Error)]
pub enum RequestError {
    #[error("the message was rejected")]
    MessageRejected,

    #[error("invalid update: the model or scalar sent by the participant could not be aggregated")]
    AggregationFailed,

    #[error("the request could not be processed due to an internal error: {0}")]
    InternalError(&'static str),

    #[error("redis request failed: {0}")]
    Redis(#[from] RedisError),

    #[error(transparent)]
    SeedDictUpdate(#[from] SeedDictUpdateError),

    #[error(transparent)]
    SumDictAdd(#[from] SumDictAddError),

    #[error(transparent)]
    MaskDictIncr(#[from] MaskDictIncrError),
}

pub type StateMachineResult = Result<(), RequestError>;

/// Error that occurs when unmasking of the global model fails.
#[derive(Error, Debug, Eq, PartialEq)]
pub enum UnmaskGlobalModelError {
    #[error("ambiguous masks were computed by the sum participants")]
    AmbiguousMasks,
    #[error("no mask found")]
    NoMask,
    #[error("unmasking error: {0}")]
    Unmasking(#[from] UnmaskingError),
}

/// The state machine with all its states.
#[derive(From)]
pub enum StateMachine {
    Idle(PhaseState<Idle>),
    Sum(PhaseState<Sum>),
    Update(PhaseState<Update>),
    Sum2(PhaseState<Sum2>),
    Unmask(PhaseState<Unmask>),
    Error(PhaseState<PhaseStateError>),
    Shutdown(PhaseState<Shutdown>),
}

impl StateMachine
where
    PhaseState<Idle>: Phase,
    PhaseState<Sum>: Phase,
    PhaseState<Update>: Phase,
    PhaseState<Sum2>: Phase,
    PhaseState<Unmask>: Phase,
    PhaseState<PhaseStateError>: Phase,
    PhaseState<Shutdown>: Phase,
{
    /// Moves the [`StateMachine`] to the next state and consumes the current one.
    /// Returns the next state or `None` if the [`StateMachine`] reached the state [`Shutdown`].
    pub async fn next(self) -> Option<Self> {
        match self {
            StateMachine::Idle(state) => state.run_phase().await,
            StateMachine::Sum(state) => state.run_phase().await,
            StateMachine::Update(state) => state.run_phase().await,
            StateMachine::Sum2(state) => state.run_phase().await,
            StateMachine::Unmask(state) => state.run_phase().await,
            StateMachine::Error(state) => state.run_phase().await,
            StateMachine::Shutdown(state) => state.run_phase().await,
        }
    }

    /// Runs the state machine until it shuts down.
    /// The [`StateMachine`] shuts down once all [`RequestSender`] have been dropped.
    pub async fn run(mut self) -> Option<()> {
        loop {
            self = self.next().await?;
        }
    }
}

type StateMachineInitializationResult<T> = Result<T, StateMachineInitializationError>;

#[derive(Debug, Error)]
pub enum StateMachineInitializationError {
    #[error("redis request failed: {0}")]
    Redis(#[from] RedisError),
    #[error("failed to initialize crypto library")]
    Crypto,
    #[error("GlobalModelUnavailable")]
    GlobalModelUnavailable,
    #[error("GlobalModelInvalid")]
    GlobalModelInvalid(String),
}

pub struct StateMachineInitializer {
    pet_settings: PetSettings,
    mask_settings: MaskSettings,
    model_settings: ModelSettings,
    redis_handle: redis::Client,

    #[cfg(feature = "model-persistence")]
    s3_handle: s3::Client,
    #[cfg(feature = "metrics")]
    metrics_handle: MetricsSender,
}

impl StateMachineInitializer {
    pub fn new(
        pet_settings: PetSettings,
        mask_settings: MaskSettings,
        model_settings: ModelSettings,
        redis_handle: redis::Client,
        #[cfg(feature = "model-persistence")] s3_handle: s3::Client,
        #[cfg(feature = "metrics")] metrics_handle: MetricsSender,
    ) -> Self {
        Self {
            pet_settings,
            mask_settings,
            model_settings,
            redis_handle,
            #[cfg(feature = "model-persistence")]
            s3_handle,
            #[cfg(feature = "metrics")]
            metrics_handle,
        }
    }

    #[cfg(not(feature = "model-persistence"))]
    /// Initializes a new [`StateMachine`] from scratch.
    pub async fn init(
        self,
    ) -> StateMachineInitializationResult<(StateMachine, RequestSender, EventSubscriber)> {
        // crucial: init must be called before anything else in this module
        // maybe we should create a wrapper around that function and put this into the xaynet::core crate
        sodiumoxide::init().or(Err(StateMachineInitializationError::Crypto))?;

        let (coordinator_state, global_model) = { self.from_settings().await? };

        Ok(self.init_state_machine(coordinator_state, global_model))
    }

    async fn from_settings(
        &self,
    ) -> StateMachineInitializationResult<(CoordinatorState, ModelUpdate)> {
        // clear everything in the redis database
        // should only be called for the first start or if we need to perform a
        // hard reset.
        self.redis_handle.connection().await.flush_db().await?;
        Ok((
            CoordinatorState::new(self.pet_settings, self.mask_settings, self.model_settings),
            ModelUpdate::Invalidate,
        ))
    }

    fn init_state_machine(
        self,
        coordinator_state: CoordinatorState,
        global_model: ModelUpdate,
    ) -> (StateMachine, RequestSender, EventSubscriber) {
        let (event_publisher, event_subscriber) = EventPublisher::init(
            coordinator_state.round_id,
            coordinator_state.keys.clone(),
            coordinator_state.round_params.clone(),
            PhaseName::Idle,
            global_model,
        );

        let (req_receiver, req_sender) = RequestReceiver::new();

        let shared = Shared::new(
            coordinator_state,
            event_publisher,
            req_receiver,
            self.redis_handle,
            #[cfg(feature = "model-persistence")]
            self.s3_handle,
            #[cfg(feature = "metrics")]
            self.metrics_handle,
        );

        let state_machine = StateMachine::from(PhaseState::<Idle>::new(shared));
        (state_machine, req_sender, event_subscriber)
    }
}

#[cfg(feature = "model-persistence")]
impl StateMachineInitializer {
    /// Initializes a new [`StateMachine`] by trying to restore the
    /// coordinator state along with the latest global model.
    pub async fn init(
        self,
        no_restore: bool,
    ) -> StateMachineInitializationResult<(StateMachine, RequestSender, EventSubscriber)> {
        // crucial: init must be called before anything else in this module
        // maybe we should create a wrapper around that function and put this into the xaynet::core crate
        sodiumoxide::init().or(Err(StateMachineInitializationError::Crypto))?;

        let (coordinator_state, global_model) = if no_restore {
            info!("requested not to restore the coordinator state");
            info!("initialize state machine from settings");
            self.from_settings().await?
        } else {
            self.from_previous_state().await?
        };

        Ok(self.init_state_machine(coordinator_state, global_model))
    }

    pub async fn from_previous_state(
        &self,
    ) -> StateMachineInitializationResult<(CoordinatorState, ModelUpdate)> {
        let (coordinator_state, global_model) = if let Some(coordinator_state) = self
            .redis_handle
            .connection()
            .await
            .get_coordinator_state()
            .await?
        {
            self.try_restore_state(coordinator_state).await?
        } else {
            // no state in redis available seems to be a fresh start
            self.from_settings().await?
        };

        Ok((coordinator_state, global_model))
    }

    async fn try_restore_state(
        &self,
        coordinator_state: CoordinatorState,
    ) -> StateMachineInitializationResult<(CoordinatorState, ModelUpdate)> {
        let latest_global_model_id = self
            .redis_handle
            .connection()
            .await
            .get_latest_global_model_id()
            .await?;

        let global_model_id = match latest_global_model_id {
            // the state machine was shut down before completing a round
            // we cannot use the round_id here because we increment the round_id after each restart
            // that means even if the round id is larger than one, it doesn't mean that a
            // round has ever been completed
            None => {
                debug!("apparently no round has been completed yet");
                debug!("restore coordinator without a global model");
                return Ok((coordinator_state, ModelUpdate::Invalidate));
            }
            Some(global_model_id) => global_model_id,
        };

        let global_model = self
            .download_global_model(&coordinator_state, &global_model_id)
            .await?;

        debug!(
            "restore coordinator with global model id: {}",
            global_model_id
        );
        Ok((
            coordinator_state,
            ModelUpdate::New(std::sync::Arc::new(global_model)),
        ))
    }

    async fn download_global_model(
        &self,
        coordinator_state: &CoordinatorState,
        global_model_id: &str,
    ) -> StateMachineInitializationResult<Model> {
        if let Ok(global_model) = self.s3_handle.download_global_model(&global_model_id).await {
            if Self::model_properties_matches_settings(coordinator_state, &global_model) {
                Ok(global_model)
            } else {
                let error_msg = format!(
                    "the size of global model with the id {} does not match with the value of the model size setting {} != {}",
                    &global_model_id,
                    global_model.len(),
                    coordinator_state.model_size);

                Err(StateMachineInitializationError::GlobalModelInvalid(
                    error_msg,
                ))
            }
        } else {
            warn!("cannot find global model {}", &global_model_id);
            // model id exists but we cannot find it in S3 / Minio
            // here we better fail because if we restart a coordinator with an empty model
            // the clients will throw a way there current global model and start from scratch
            Err(StateMachineInitializationError::GlobalModelUnavailable)
        }
    }

    fn model_properties_matches_settings(
        coordinator_state: &CoordinatorState,
        global_model: &Model,
    ) -> bool {
        // can we test more here?
        coordinator_state.model_size == global_model.len()
    }
}

#[cfg(test)]
pub(crate) mod tests;
