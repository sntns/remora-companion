use std::{sync::Arc, time::SystemTime};

use error_stack::{Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{ContextOverride, ResolvedContext},
};
use remora_monitor::{
    adapter::gateway::{self, MonitorGatewayAdapterService},
    application::{Error, MonitorServiceInterface, Result},
    model::{document_name, Document},
};
use tokio::task::JoinSet;

/// The monitor vertical's use case.
pub struct MonitorControllerImpl {
    contexts: ContextService,
    gateway: MonitorGatewayAdapterService,
}

impl MonitorControllerImpl {
    pub fn new(contexts: ContextService, gateway: MonitorGatewayAdapterService) -> Self {
        Self { contexts, gateway }
    }

    /// The documents the store holds of `device`: `(document, schema)`,
    /// sorted.
    async fn documents(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
    ) -> Result<Vec<(String, String)>> {
        let failed = |report: Report<gateway::Error>| match report.current_context() {
            gateway::Error::PermissionDenied => report.change_context(Error::Denied(bucket.into())),
            // No such bucket: nothing was ever stored.
            gateway::Error::NotFound => report.change_context(Error::NoData(device.into())),
            _ => report.change_context(Error::Read(device.into())),
        };
        let mut documents: Vec<_> = self
            .gateway
            .schemas(context, bucket, device)
            .await
            .map_err(failed)?
            .into_iter()
            .filter_map(|schema| Some((document_name(&schema)?.to_owned(), schema)))
            .collect();
        if documents.is_empty() {
            return Err(Report::new(Error::NoData(device.into())));
        }
        documents.sort();
        Ok(documents)
    }
}

#[async_trait::async_trait]
impl MonitorServiceInterface for MonitorControllerImpl {
    async fn latest(
        &self,
        over: Option<&ContextOverride>,
        bucket: &str,
        device: &str,
    ) -> Result<Vec<Document>> {
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
        let documents = self.documents(&context, bucket, device).await?;

        // One call per document: all at once, they are a handful.
        let (context, resource, bucket) = (
            Arc::new(context),
            Arc::new(device.to_owned()),
            Arc::new(bucket.to_owned()),
        );
        let mut tasks = JoinSet::new();
        for (name, schema) in documents {
            let (gateway, context, resource, bucket) = (
                self.gateway.clone(),
                context.clone(),
                resource.clone(),
                bucket.clone(),
            );
            tasks.spawn(async move {
                let sample = gateway.latest(&context, &bucket, &resource, &schema).await;
                (name, schema, sample)
            });
        }
        let read = || Error::Read(device.to_owned());
        let mut latest = Vec::with_capacity(tasks.len());
        while let Some(joined) = tasks.join_next().await {
            let (name, schema, sample) = joined
                .map_err(|e| Report::new(read()).attach(format!("a document read failed: {e}")))?;
            // Listed, then gone: expired between the two calls.
            if let Some(sample) = sample.change_context_lazy(read)? {
                latest.push(Document::from_sample(&name, &schema, sample));
            }
        }
        latest.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(latest)
    }

    async fn history(
        &self,
        over: Option<&ContextOverride>,
        bucket: &str,
        device: &str,
        document: &str,
        from: SystemTime,
    ) -> Result<Vec<Document>> {
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
        let documents = self.documents(&context, bucket, device).await?;
        let (name, schema) = documents
            .into_iter()
            .find(|(name, _)| name == document)
            .ok_or_else(|| {
                Report::new(Error::NoDocument {
                    device: device.to_owned(),
                    document: document.to_owned(),
                })
            })?;
        let mut history: Vec<_> = self
            .gateway
            .points(&context, bucket, device, &schema, from, SystemTime::now())
            .await
            .change_context_lazy(|| Error::Read(device.to_owned()))?
            .into_iter()
            .map(|sample| Document::from_sample(&name, &schema, sample))
            // A device dates a document itself: one buffered while it was
            // offline is stored late, and may be older than asked.
            .filter(|document| document.captured_at >= from)
            .collect();
        history.sort_by_key(|document| document.captured_at);
        Ok(history)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use remora_context::{
        adapter::{
            credentials::CredentialStoreAdapterService,
            platform::{self, PlatformSessionAdapter, PlatformSessionAdapterService},
            store::ContextStoreAdapterService,
        },
        application::ContextServiceInterface,
        model::{Credentials, Endpoint, Principal},
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
    use remora_context_application::ContextControllerImpl;
    use remora_monitor::model::DEFAULT_BUCKET;
    use remora_monitor_adapter_http::MonitorGatewayAdapterImpl;
    use remora_platform_http::test_gateway::TestGateway;
    use serde_json::json;

    use super::*;

    const DEV1: &str = "sntns:iot:test:acme:thing:DEV1";
    const SYSTEM: &str = "sntns:catalog:test:acme:schema:remora-monitoring-system-v1";
    const HEALTH: &str = "sntns:catalog:test:acme:schema:remora-monitoring-health-v1";
    const HEARTBEAT: &str = "sntns:catalog:test:acme:schema:remora-device-heartbeat";
    const OTHER: &str = "sntns:catalog:test:acme:schema:app/reading";

    struct Platform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for Platform {
        async fn whoami(&self, _: &Endpoint, _: &Credentials) -> platform::Result<Principal> {
            Ok(Principal {
                user_urn: "urn:user:ada".into(),
                user_name: "ada".into(),
                account_name: None,
            })
        }

        async fn acting_account(
            &self,
            _: &Endpoint,
            _: &Credentials,
            _: &str,
        ) -> platform::Result<Option<String>> {
            Ok(None)
        }
    }

    /// A controller logged in to a fake gateway, and what keeps its
    /// context alive.
    async fn monitor() -> (TestGateway, MonitorControllerImpl, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (gateway, resolved) = TestGateway::serve().await;
        let contexts = ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(dir.path())),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(dir.path())),
            PlatformSessionAdapterService::new(Platform),
        );
        contexts
            .create(&resolved.name, None, resolved.endpoint.clone(), false)
            .await
            .unwrap();
        contexts.login(None, resolved.credentials).await.unwrap();
        let monitor = MonitorControllerImpl::new(
            ContextService::new(contexts),
            MonitorGatewayAdapterService::new(MonitorGatewayAdapterImpl),
        );
        (gateway, monitor, dir)
    }

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn envelope(captured_at: &str, document: serde_json::Value) -> serde_json::Value {
        json!({"captured-at": captured_at, "clock-synced": true, "document": document})
    }

    #[tokio::test]
    async fn the_latest_of_every_monitoring_document() {
        let (gateway, monitor, _dir) = monitor().await;
        let store = |schema, timestamp, value| {
            gateway.store_point(DEFAULT_BUCKET, DEV1, schema, timestamp, value)
        };
        store(
            SYSTEM,
            "1970-01-01T00:01:05Z",
            envelope("1970-01-01T00:01:00Z", json!({"n": 1})),
        );
        store(
            SYSTEM,
            "1970-01-01T00:02:05Z",
            envelope("1970-01-01T00:02:00Z", json!({"n": 2})),
        );
        store(
            HEALTH,
            "1970-01-01T00:03:05Z",
            envelope("1970-01-01T00:03:00Z", json!({"state": "ok"})),
        );
        store(HEARTBEAT, "1970-01-01T00:04:00Z", json!({"interval": 300}));
        // Not the device's own report: another kind of message.
        store(OTHER, "1970-01-01T00:05:00Z", json!({}));
        // Another device, whose name only starts the same.
        gateway.store_point(
            DEFAULT_BUCKET,
            "sntns:iot:test:acme:thing:DEV10",
            SYSTEM,
            "1970-01-01T00:09:00Z",
            json!({}),
        );

        let latest = monitor.latest(None, DEFAULT_BUCKET, "DEV1").await.unwrap();
        let read: Vec<_> = latest
            .iter()
            .map(|d| (d.name.as_str(), d.captured_at, &d.body))
            .collect();
        assert_eq!(
            read,
            [
                ("health", at(180), &json!({"state": "ok"})),
                ("heartbeat", at(240), &json!({"interval": 300})),
                ("system", at(120), &json!({"n": 2})),
            ]
        );
        assert_eq!(latest[2].schema, SYSTEM);
    }

    #[tokio::test]
    async fn a_documents_history_oldest_first() {
        let (gateway, monitor, _dir) = monitor().await;
        for (stored, captured, n) in [
            ("1970-01-01T00:03:05Z", "1970-01-01T00:03:00Z", 3),
            ("1970-01-01T00:01:05Z", "1970-01-01T00:01:00Z", 1),
            // Buffered offline, stored late: older than asked.
            ("1970-01-01T00:04:00Z", "1970-01-01T00:00:10Z", 0),
            ("1970-01-01T00:02:05Z", "1970-01-01T00:02:00Z", 2),
        ] {
            gateway.store_point(
                DEFAULT_BUCKET,
                DEV1,
                SYSTEM,
                stored,
                envelope(captured, json!({"n": n})),
            );
        }
        let history = monitor
            .history(None, DEFAULT_BUCKET, "DEV1", "system", at(30))
            .await
            .unwrap();
        let read: Vec<_> = history.iter().map(|d| d.body["n"].as_u64()).collect();
        assert_eq!(read, [Some(1), Some(2), Some(3)]);

        let report = monitor
            .history(None, DEFAULT_BUCKET, "DEV1", "network", at(0))
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NoDocument { .. }));
    }

    #[tokio::test]
    async fn no_data_and_a_refusal_are_told_apart() {
        let (gateway, monitor, _dir) = monitor().await;
        // No bucket at all.
        let report = monitor
            .latest(None, DEFAULT_BUCKET, "DEV1")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NoData(_)));

        // A bucket, without this device; then with it, but nothing of
        // monitoring.
        gateway.store_point(
            DEFAULT_BUCKET,
            "sntns:iot:test:acme:thing:DEV2",
            SYSTEM,
            "1970-01-01T00:00:00Z",
            json!({}),
        );
        let report = monitor
            .latest(None, DEFAULT_BUCKET, "DEV1")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NoData(_)));
        gateway.store_point(
            DEFAULT_BUCKET,
            DEV1,
            OTHER,
            "1970-01-01T00:00:00Z",
            json!({}),
        );
        let report = monitor
            .latest(None, DEFAULT_BUCKET, "DEV1")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NoData(_)));

        gateway.0.lock().unwrap().tbss_denied = true;
        let report = monitor
            .latest(None, DEFAULT_BUCKET, "DEV1")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Denied(_)));
    }
}
