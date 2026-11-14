use std::time::{Duration, SystemTime};

use error_stack::{Report, ResultExt};
use remora_context::model::ResolvedContext;
use remora_monitor::{
    adapter::gateway::{Error, MonitorGatewayAdapter, Result},
    model::Sample,
};
use remora_platform_http::{model, status_summary, Code, Connection, Gateway, Status};
use url::Url;

/// The platform's time series store (tbss), over its REST API.
pub struct MonitorGatewayAdapterImpl;

async fn gateway(context: &ResolvedContext) -> Result<Gateway> {
    Gateway::new(&Connection::for_resolved(context))
        .await
        .change_context(Error::Unreachable)
}

fn classify(status: Status) -> Report<Error> {
    let error = match status.code {
        Code::Unauthenticated => Error::Unauthenticated,
        Code::PermissionDenied => Error::PermissionDenied,
        Code::NotFound => Error::NotFound,
        Code::Unavailable => Error::Unreachable,
        _ => Error::Call,
    };
    Report::new(error).attach(status_summary(&status))
}

/// A bucket's route: its name holds `/`, each part a path segment of its
/// own (upstream's `{bucket_name=**}`).
fn bucket_url(gateway: &Gateway, bucket: &str, what: &[&str]) -> Url {
    gateway.url(
        ["tbss", "v1", "bucket"]
            .into_iter()
            .chain(bucket.split('/').filter(|part| !part.is_empty()))
            .chain(what.iter().copied()),
    )
}

/// The bucket remora's own pipeline fills (`remora/store-device-heartbeat`):
/// heartbeats only, under the device itself.
const DEVICE_BUCKET: &str = "remora-device";

/// How much time is asked of the store at once, to start with: the store
/// refuses an answer over 4 MiB whole, and a device's documents are a few
/// kilobytes every few minutes.
const WINDOW: Duration = Duration::from_secs(6 * 3600);
/// The shortest window asked for: what is still too large by then is not a
/// matter of time.
const SHORTEST_WINDOW: Duration = Duration::from_secs(60);
/// An answer under this many bytes leaves room for a window twice as
/// long: a quarter of what the store accepts to send.
const SMALL_ANSWER: usize = 1024 * 1024;

/// A device as a bucket's routes take it: `service:type:name`, not the
/// URN it answers with. An account's pipelines store what a device sends
/// under the thing named after it; remora's own bucket, under the device.
fn resource_name(bucket: &str, device: &str) -> String {
    if bucket == DEVICE_BUCKET {
        format!("remora:device:{device}")
    } else {
        format!("iot:thing:{device}")
    }
}

/// Whether the store refused an answer for its size.
fn too_large(status: &Status) -> bool {
    status.code == Code::ResourceExhausted && status.message.contains("larger than max")
}

/// A schema as the store's routes take it, from the URN it lists
/// (`sntns:catalog:<region>:<tenant>:schema:<name>`): its name, in the
/// `managed` scope when it is the platform's own (no tenant).
fn schema_name(schema: &str) -> String {
    let parts: Vec<&str> = schema.split(':').collect();
    match parts.as_slice() {
        [_, "catalog", _, tenant, "schema", name @ ..] if !name.is_empty() => {
            let name = name.join(":");
            if tenant.is_empty() {
                format!("managed:{name}")
            } else {
                name
            }
        }
        _ => schema.to_owned(),
    }
}

/// A stored value at its timestamp; one the store did not date is of no
/// use on a time axis.
fn sample(timestamp: Option<&str>, value: serde_json::Value) -> Option<Sample> {
    Some(Sample {
        at: model::time_of(timestamp)?,
        value,
    })
}

#[async_trait::async_trait]
impl MonitorGatewayAdapter for MonitorGatewayAdapterImpl {
    async fn schemas(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
    ) -> Result<Vec<String>> {
        let gateway = gateway(context).await?;
        let mut url = bucket_url(&gateway, bucket, &["shadow", "schema"]);
        url.query_pairs_mut()
            .append_pair("resourceName", &resource_name(bucket, device));
        let response: model::ListBucketShadowSchemasResponse =
            gateway.get(url).await.map_err(classify)?;
        Ok(response.bucket_shadow_schemas)
    }

    async fn latest(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
        schema: &str,
    ) -> Result<Option<Sample>> {
        let gateway = gateway(context).await?;
        let mut url = bucket_url(&gateway, bucket, &["shadow"]);
        url.query_pairs_mut()
            .append_pair("resourceName", &resource_name(bucket, device))
            .append_pair("schemaName", &schema_name(schema));
        let response = match gateway.get::<model::GetBucketShadowResponse>(url).await {
            Ok(response) => response,
            Err(status) if status.code == Code::NotFound => return Ok(None),
            Err(status) => return Err(classify(status)),
        };
        Ok(response
            .bucket_shadow
            .and_then(|shadow| sample(shadow.timestamp.as_deref(), shadow.value)))
    }

    async fn points(
        &self,
        context: &ResolvedContext,
        bucket: &str,
        device: &str,
        schema: &str,
        from: SystemTime,
        to: SystemTime,
    ) -> Result<Vec<Sample>> {
        let gateway = gateway(context).await?;
        let (resource, schema) = (resource_name(bucket, device), schema_name(schema));
        // A window of time at a time, oldest first: twice as long after a
        // small answer (heartbeats are a month in one), and by halves
        // after one the store finds too large -- never again as long as
        // one it refused. A window starts
        // where the previous one ended, so that no point falls between
        // two: one stored at that very instant comes twice, and is kept
        // once.
        let mut samples: Vec<Sample> = Vec::new();
        let (mut start, mut window) = (from, WINDOW);
        let mut refused: Option<Duration> = None;
        loop {
            let end = (start + window).min(to);
            let mut url = bucket_url(&gateway, bucket, &["point"]);
            url.query_pairs_mut()
                .append_pair("resourceName", &resource)
                .append_pair("schemaName", &schema)
                .append_pair("from", &model::timestamp(start))
                .append_pair("to", &model::timestamp(end));
            match gateway
                .get_once::<model::GetBucketPointsResponse>(url)
                .await
            {
                Ok(response) => {
                    let bytes: usize = response
                        .data
                        .iter()
                        .map(|point| point.value.to_string().len())
                        .sum();
                    samples.extend(
                        response
                            .data
                            .into_iter()
                            .filter_map(|point| sample(point.timestamp.as_deref(), point.value)),
                    );
                    if end >= to {
                        break;
                    }
                    start = end;
                    if bytes < SMALL_ANSWER && refused.is_none_or(|refused| window * 2 < refused) {
                        window *= 2;
                    }
                }
                Err(status) if too_large(&status) && window > SHORTEST_WINDOW => {
                    refused = Some(window);
                    window /= 2;
                }
                Err(status) => return Err(classify(status)),
            }
        }
        samples.sort_by_key(|sample| sample.at);
        samples.dedup();
        Ok(samples)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use remora_platform_http::test_gateway::TestGateway;
    use serde_json::json;

    use super::*;

    const BUCKET: &str = "thing-data";
    const DEV1: &str = "sntns:iot:test:acme:thing:DEV1";
    const SYSTEM: &str = "sntns:catalog:test:acme:schema:remora-monitoring-system-v1";
    const HEARTBEAT: &str = "sntns:catalog:test:acme:schema:remora-device-heartbeat";

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_listed_schema_is_asked_for_by_its_name() {
        assert_eq!(schema_name(HEARTBEAT), "remora-device-heartbeat");
        assert_eq!(
            schema_name("sntns:catalog:eu2::schema:iot/measurement/sample/temperature"),
            "managed:iot/measurement/sample/temperature"
        );
        assert_eq!(
            schema_name("remora-device-heartbeat"),
            "remora-device-heartbeat"
        );
    }

    #[tokio::test]
    async fn reads_a_bucket() {
        let (gateway, context) = TestGateway::serve().await;
        gateway.store_point(
            BUCKET,
            DEV1,
            SYSTEM,
            "1970-01-01T00:01:00Z",
            json!({"n": 1}),
        );
        gateway.store_point(
            BUCKET,
            DEV1,
            SYSTEM,
            "1970-01-01T00:02:00Z",
            json!({"n": 2}),
        );
        gateway.store_point(
            BUCKET,
            "sntns:iot:test:acme:thing:DEV2",
            HEARTBEAT,
            "1970-01-01T00:02:00Z",
            json!({"interval": 300}),
        );
        let adapter = MonitorGatewayAdapterImpl;

        assert_eq!(
            adapter.schemas(&context, BUCKET, "DEV1").await.unwrap(),
            [SYSTEM]
        );
        assert!(adapter
            .schemas(&context, BUCKET, "NOPE")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            adapter
                .latest(&context, BUCKET, "DEV1", SYSTEM)
                .await
                .unwrap(),
            Some(Sample {
                at: at(120),
                value: json!({"n": 2})
            })
        );
        assert_eq!(
            adapter
                .latest(&context, BUCKET, "DEV1", HEARTBEAT)
                .await
                .unwrap(),
            None
        );
        let all = adapter
            .points(&context, BUCKET, "DEV1", SYSTEM, at(0), at(600))
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        let recent = adapter
            .points(&context, BUCKET, "DEV1", SYSTEM, at(90), at(600))
            .await
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].value, json!({"n": 2}));
        let early = adapter
            .points(&context, BUCKET, "DEV1", SYSTEM, at(0), at(90))
            .await
            .unwrap();
        assert_eq!(early.len(), 1);
        assert_eq!(early[0].value, json!({"n": 1}));
    }

    #[tokio::test]
    async fn remoras_own_bucket_names_the_device_itself() {
        let (gateway, context) = TestGateway::serve().await;
        gateway.store_point(
            "remora-device",
            "sntns:remora:test:acme:device:DEV1",
            HEARTBEAT,
            "1970-01-01T00:01:00Z",
            json!({"interval": 300}),
        );
        assert_eq!(
            MonitorGatewayAdapterImpl
                .schemas(&context, "remora-device", "DEV1")
                .await
                .unwrap(),
            [HEARTBEAT]
        );
    }

    #[tokio::test]
    async fn a_long_history_is_read_a_window_at_a_time_each_point_once() {
        let (gateway, context) = TestGateway::serve().await;
        // Two days, a point every 10 minutes, the last one on the hour.
        for n in 0..=288u64 {
            gateway.store_point(
                BUCKET,
                DEV1,
                SYSTEM,
                &model::timestamp(at(n * 600)),
                json!({"n": n}),
            );
        }
        // 6 hours hold 36 or 37 points: too large, read by halves.
        gateway.0.lock().unwrap().tbss_max_points = Some(20);
        let points = MonitorGatewayAdapterImpl
            .points(&context, BUCKET, "DEV1", SYSTEM, at(0), at(288 * 600))
            .await
            .unwrap();
        let mut read: Vec<u64> = points
            .iter()
            .filter_map(|p| p.value["n"].as_u64())
            .collect();
        read.sort_unstable();
        assert_eq!(read, (0..=288).collect::<Vec<_>>());
        // Once refused, then 3 hours at a time: no retry of a refusal.
        assert_eq!(gateway.0.lock().unwrap().tbss_point_calls, 1 + 16);

        // Nothing fits: an error, not a loop.
        gateway.0.lock().unwrap().tbss_max_points = Some(0);
        let report = MonitorGatewayAdapterImpl
            .points(&context, BUCKET, "DEV1", SYSTEM, at(0), at(3600))
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Call));
    }

    #[tokio::test]
    async fn a_refusal_or_a_missing_bucket_says_which() {
        let (gateway, context) = TestGateway::serve().await;
        let adapter = MonitorGatewayAdapterImpl;
        let report = adapter.schemas(&context, BUCKET, "DEV1").await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotFound));

        gateway.0.lock().unwrap().tbss_denied = true;
        let report = adapter.schemas(&context, BUCKET, "DEV1").await.unwrap_err();
        assert!(matches!(report.current_context(), Error::PermissionDenied));
    }
}
