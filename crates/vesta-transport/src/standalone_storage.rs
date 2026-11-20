use crate::{Reply, manage};
use http::{Method, StatusCode};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use vesta_control::{
    host::NetworkHost,
    identity::Capability,
    machine::storage::{HostStorage, measure_instance},
};

#[derive(Clone, Default)]
pub(super) struct Storage(Arc<Mutex<Option<Value>>>);

impl Storage {
    pub(super) async fn measure(
        self,
        host: NetworkHost,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) {
        let config = host.configuration().clone();
        let mut tick = tokio::time::interval(Duration::from_secs(15 * 60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !*stop.borrow() {
            tokio::select! {
                biased;
                _ = stop.changed() => return,
                _ = tick.tick() => {}
            };
            let config = config.clone();
            let now = host.database().clock().millis();
            if let Ok(Ok(snapshot)) =
                tokio::task::spawn_blocking(move || measure_instance(&config, now)).await
                && let Ok(mut current) = self.0.lock()
            {
                *current = Some(snapshot);
            }
        }
    }

    pub(super) fn reply(
        &self,
        host: &NetworkHost,
        method: &Method,
        query: Option<&str>,
        authorization: Option<&str>,
    ) -> Reply {
        let Some(token) = authorization else {
            return manage::error(StatusCode::UNAUTHORIZED, "management_session_invalid");
        };
        if let Err(error) = host
            .management()
            .authorize(token, Capability::StorageView, None)
        {
            return manage::management_error(error);
        }
        if method != Method::GET || query.is_some() {
            return manage::error(StatusCode::BAD_REQUEST, "invalid_request");
        }
        match self.0.lock() {
            Ok(snapshot) => manage::ok(
                snapshot
                    .clone()
                    .unwrap_or_else(|| HostStorage::default().server_details(uuid::Uuid::nil())),
            ),
            Err(_) => manage::error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
        }
    }
}
