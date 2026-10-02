//! A fake remora gateway, in process, for tests: releases and resumable
//! uploads held in memory, a fixed set of labelled devices, and deployments
//! that move PENDING → RUNNING → SUCCEEDED as they're polled (FAILED for a
//! device named `BROKEN`), reporting progress as log entries on the way.
//! Never built into a shipped binary (the `test-gateway` feature is only
//! enabled from `[dev-dependencies]`).

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use remora_context::model::{
    Context, Credentials, Endpoint, ResolvedContext, Secret, Selection, Tls,
};
use remora_platform_grpc::sntns::service::{remora::v1 as pb, v1::ResourceReference};
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status, Streaming};

fn reference(name: &str) -> Option<ResourceReference> {
    Some(ResourceReference {
        name: name.to_owned(),
        urn: format!("urn:test:{name}"),
        ..Default::default()
    })
}

#[derive(Default)]
struct Upload {
    bytes: Vec<u8>,
}

struct StoredDeployment {
    release: String,
    target: String,
    labels: HashMap<String, String>,
    status: String,
    polls: u32,
    logs: Vec<pb::DeploymentLogEntry>,
}

/// What the fake holds; inspect it from a test.
#[derive(Default)]
pub struct State {
    pub releases: BTreeMap<String, pb::ReleaseDescriptor>,
    uploads: HashMap<String, Upload>,
    /// The next upload stream fails after receiving this many bytes, once:
    /// a dropped connection, with what arrived kept for a resume.
    pub fail_next_upload_after: Option<usize>,
    pub devices: BTreeMap<String, HashMap<String, String>>,
    deployments: BTreeMap<String, StoredDeployment>,
}

#[derive(Clone)]
pub struct TestGateway(pub Arc<Mutex<State>>);

impl TestGateway {
    /// Serves on a free local port; returns the gateway and a context
    /// pointing at it (plaintext, any token).
    pub async fn serve() -> (Self, ResolvedContext) {
        let gateway = Self(Arc::new(Mutex::new(State::default())));
        {
            let mut state = gateway.0.lock().unwrap();
            for (name, board) in [
                ("DEV1", "rp5"),
                ("DEV2", "rp5"),
                ("DEV3", "hdc"),
                ("BROKEN", "rp5"),
            ] {
                state
                    .devices
                    .insert(name.into(), HashMap::from([("board".into(), board.into())]));
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(pb::release_service_server::ReleaseServiceServer::new(
                    gateway.clone(),
                ))
                .add_service(pb::deployment_service_server::DeploymentServiceServer::new(
                    gateway.clone(),
                ))
                .add_service(pb::device_service_server::DeviceServiceServer::new(
                    gateway.clone(),
                ))
                .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener)),
        );
        let context = ResolvedContext {
            context: Context {
                name: "test".into(),
                description: None,
                endpoint: Endpoint {
                    address,
                    tls: Tls {
                        disabled: true,
                        ..Tls::default()
                    },
                },
            },
            credentials: Credentials {
                secret: Secret::AccessKey { token: "t".into() },
                assume_role: None,
            },
            selection: Selection::Flag,
        };
        (gateway, context)
    }

    /// The bytes an artifact of `release` was uploaded with.
    pub fn artifact_bytes(&self, release: &str, file_name: &str) -> Option<Vec<u8>> {
        let state = self.0.lock().unwrap();
        let descriptor = state.releases.get(release)?;
        let artifact = descriptor
            .artifacts
            .iter()
            .find(|a| a.file_name == file_name)?;
        let id = artifact.object_reference.as_ref()?.id.clone();
        state.uploads.get(&id).map(|upload| upload.bytes.clone())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
}

fn terminal(status: &str) -> bool {
    matches!(status, "SUCCEEDED" | "FAILED" | "CANCELED" | "REJECTED")
}

#[tonic::async_trait]
impl pb::release_service_server::ReleaseService for TestGateway {
    async fn create_release(
        &self,
        request: Request<pb::ReleaseServiceCreateReleaseRequest>,
    ) -> Result<Response<pb::ReleaseServiceCreateReleaseResponse>, Status> {
        let request = request.into_inner();
        let mut state = self.state();
        if state.releases.contains_key(&request.release_name) {
            return Err(Status::already_exists(
                "a release with the same name already exists",
            ));
        }
        state.releases.insert(
            request.release_name.clone(),
            pb::ReleaseDescriptor {
                resource: reference(&request.release_name),
                labels: request.release_labels,
                version: request.release_version,
                artifacts: vec![],
            },
        );
        Ok(Response::new(pb::ReleaseServiceCreateReleaseResponse {
            release_reference: reference(&request.release_name),
        }))
    }

    async fn get_release(
        &self,
        request: Request<pb::ReleaseServiceGetReleaseRequest>,
    ) -> Result<Response<pb::ReleaseServiceGetReleaseResponse>, Status> {
        let name = request.into_inner().release_name;
        let descriptor = self
            .state()
            .releases
            .get(&name)
            .cloned()
            .ok_or_else(|| Status::not_found("the release has not been found"))?;
        Ok(Response::new(pb::ReleaseServiceGetReleaseResponse {
            release_descriptor: Some(descriptor),
        }))
    }

    async fn update_release(
        &self,
        request: Request<pb::ReleaseServiceUpdateReleaseRequest>,
    ) -> Result<Response<pb::ReleaseServiceUpdateReleaseResponse>, Status> {
        let request = request.into_inner();
        let mut state = self.state();
        let release = state
            .releases
            .get_mut(&request.release_name)
            .ok_or_else(|| Status::not_found("the release has not been found"))?;
        if let Some(labels) = request.release_labels {
            if !labels.merge {
                release.labels.clear();
            }
            release.labels.extend(labels.set);
            for key in labels.unset {
                release.labels.remove(&key);
            }
        }
        Ok(Response::new(pb::ReleaseServiceUpdateReleaseResponse {
            release_reference: reference(&request.release_name),
        }))
    }

    async fn list_releases(
        &self,
        request: Request<pb::ReleaseServiceListReleasesRequest>,
    ) -> Result<Response<pb::ReleaseServiceListReleasesResponse>, Status> {
        let wanted = request.into_inner().release_labels;
        let summaries = self
            .state()
            .releases
            .values()
            .filter(|r| wanted.iter().all(|(k, v)| r.labels.get(k) == Some(v)))
            .map(|r| pb::ReleaseSummary {
                resource: r.resource.clone(),
                version: r.version.clone(),
            })
            .collect();
        Ok(Response::new(pb::ReleaseServiceListReleasesResponse {
            release_summaries: summaries,
        }))
    }

    async fn delete_release(
        &self,
        request: Request<pb::ReleaseServiceDeleteReleaseRequest>,
    ) -> Result<Response<pb::ReleaseServiceDeleteReleaseResponse>, Status> {
        let name = request.into_inner().release_name;
        self.state()
            .releases
            .remove(&name)
            .ok_or_else(|| Status::not_found("the release has not been found"))?;
        Ok(Response::new(pb::ReleaseServiceDeleteReleaseResponse {
            release_reference: reference(&name),
        }))
    }

    async fn upload_release_artifact(
        &self,
        request: Request<Streaming<pb::ReleaseServiceUploadReleaseArtifactRequest>>,
    ) -> Result<Response<pb::ReleaseServiceUploadReleaseArtifactResponse>, Status> {
        use pb::release_service_upload_release_artifact_request::Request as In;
        let mut incoming = request.into_inner();
        let Some(Ok(pb::ReleaseServiceUploadReleaseArtifactRequest {
            request: Some(In::InitialRequest(initial)),
        })) = incoming.next().await
        else {
            return Err(Status::invalid_argument("no initial request"));
        };
        if !self.state().releases.contains_key(&initial.release_name) {
            return Err(Status::not_found("the release has not been found"));
        }
        let fail_after = self.state().fail_next_upload_after.take();
        let mut received = 0;
        while let Some(message) = incoming.next().await {
            let message = message?;
            if let Some(In::SubsequentRequest(chunk)) = message.request {
                let mut state = self.state();
                let upload = state
                    .uploads
                    .entry(initial.artifact_content_id.clone())
                    .or_default();
                let mut bytes = chunk.artifact_chunk.as_slice();
                if let Some(limit) = fail_after {
                    let room = limit.saturating_sub(received);
                    if bytes.len() > room {
                        upload.bytes.extend_from_slice(&bytes[..room]);
                        return Err(Status::unavailable("connection reset"));
                    }
                }
                received += bytes.len();
                upload.bytes.extend_from_slice(std::mem::take(&mut bytes));
            }
        }
        let mut state = self.state();
        let length = state
            .uploads
            .get(&initial.artifact_content_id)
            .map_or(0, |upload| upload.bytes.len());
        let release = state.releases.get_mut(&initial.release_name).unwrap();
        release
            .artifacts
            .retain(|a| a.file_name != initial.artifact_file_name);
        release.artifacts.push(pb::ReleaseArtifact {
            object_reference: Some(ResourceReference {
                id: initial.artifact_content_id.clone(),
                ..Default::default()
            }),
            file_name: initial.artifact_file_name,
            content_type: initial.artifact_content_type,
            content_length: length as i64,
            checksum_sha256: String::new(),
            tag_condition: initial.artifact_tag_condition,
        });
        Ok(Response::new(
            pb::ReleaseServiceUploadReleaseArtifactResponse {
                release_reference: reference(&initial.release_name),
            },
        ))
    }

    async fn get_release_artifact_upload_offset(
        &self,
        request: Request<pb::ReleaseServiceGetReleaseArtifactUploadOffsetRequest>,
    ) -> Result<Response<pb::ReleaseServiceGetReleaseArtifactUploadOffsetResponse>, Status> {
        let id = request.into_inner().artifact_content_id;
        let offset = self.state().uploads.get(&id).map_or(0, |u| u.bytes.len());
        Ok(Response::new(
            pb::ReleaseServiceGetReleaseArtifactUploadOffsetResponse {
                artifact_content_offset: offset as i64,
            },
        ))
    }
}

#[tonic::async_trait]
impl pb::device_service_server::DeviceService for TestGateway {
    async fn get_device(
        &self,
        request: Request<pb::DeviceServiceGetDeviceRequest>,
    ) -> Result<Response<pb::DeviceServiceGetDeviceResponse>, Status> {
        let name = request.into_inner().device_name;
        let labels = self
            .state()
            .devices
            .get(&name)
            .cloned()
            .ok_or_else(|| Status::not_found("the device has not been found"))?;
        Ok(Response::new(pb::DeviceServiceGetDeviceResponse {
            device_descriptor: Some(pb::DeviceDescriptor {
                resource: reference(&name),
                labels,
                certificates: vec![],
            }),
        }))
    }

    async fn list_devices(
        &self,
        request: Request<pb::DeviceServiceListDevicesRequest>,
    ) -> Result<Response<pb::DeviceServiceListDevicesResponse>, Status> {
        let wanted = request.into_inner().device_labels;
        let summaries = self
            .state()
            .devices
            .iter()
            .filter(|(_, labels)| wanted.iter().all(|(k, v)| labels.get(k) == Some(v)))
            .map(|(name, _)| pb::DeviceSummary {
                resource: reference(name),
            })
            .collect();
        Ok(Response::new(pb::DeviceServiceListDevicesResponse {
            device_summaries: summaries,
        }))
    }
}

#[tonic::async_trait]
impl pb::deployment_service_server::DeploymentService for TestGateway {
    async fn create_deployment(
        &self,
        request: Request<pb::DeploymentServiceCreateDeploymentRequest>,
    ) -> Result<Response<pb::DeploymentServiceCreateDeploymentResponse>, Status> {
        let request = request.into_inner();
        let mut state = self.state();
        if !state
            .releases
            .contains_key(&request.deployment_release_name)
        {
            return Err(Status::not_found("the release has not been found"));
        }
        if !state.devices.contains_key(&request.deployment_target_name) {
            return Err(Status::not_found("the device has not been found"));
        }
        if state.deployments.contains_key(&request.deployment_name) {
            return Err(Status::failed_precondition(
                "a deployment with the same name already exists",
            ));
        }
        if state
            .deployments
            .values()
            .any(|d| d.target == request.deployment_target_name && !terminal(&d.status))
        {
            return Err(Status::failed_precondition(
                "the target device already has a non-terminal deployment",
            ));
        }
        state.deployments.insert(
            request.deployment_name.clone(),
            StoredDeployment {
                release: request.deployment_release_name,
                target: request.deployment_target_name,
                labels: request.deployment_labels,
                status: "PENDING".into(),
                polls: 0,
                logs: vec![],
            },
        );
        Ok(Response::new(
            pb::DeploymentServiceCreateDeploymentResponse {
                deployment_reference: reference(&request.deployment_name),
            },
        ))
    }

    /// Every poll of a running deployment moves it on: 50 %, 100 %, then
    /// done -- failed for a device named BROKEN.
    async fn get_deployment(
        &self,
        request: Request<pb::DeploymentServiceGetDeploymentRequest>,
    ) -> Result<Response<pb::DeploymentServiceGetDeploymentResponse>, Status> {
        let name = request.into_inner().deployment_name;
        let mut state = self.state();
        let deployment = state
            .deployments
            .get_mut(&name)
            .ok_or_else(|| Status::not_found("the deployment has not been found"))?;
        if deployment.status == "RUNNING" {
            deployment.polls += 1;
            let progress = |current| pb::DeploymentLogEntry {
                execution: "proceeding".into(),
                details: vec![format!("installing {current}%")],
                progress: Some(pb::DeploymentLogEntryProgress {
                    current,
                    max: 100,
                    unit: "%".into(),
                    speed: 0.0,
                }),
                ..Default::default()
            };
            match deployment.polls {
                1 => deployment.logs.push(progress(50)),
                2 => deployment.logs.push(progress(100)),
                _ => {
                    let failed = deployment.target == "BROKEN";
                    deployment.status = if failed { "FAILED" } else { "SUCCEEDED" }.into();
                    deployment.logs.push(pb::DeploymentLogEntry {
                        execution: "closed".into(),
                        result: if failed { "failure" } else { "success" }.into(),
                        details: vec![if failed {
                            "bundle signature invalid"
                        } else {
                            "installed, rebooting"
                        }
                        .into()],
                        ..Default::default()
                    });
                }
            }
        }
        if deployment.status == "CANCELING" {
            deployment.status = "CANCELED".into();
        }
        let details = if deployment.status == "FAILED" {
            HashMap::from([("reason".to_owned(), "bundle signature invalid".to_owned())])
        } else {
            HashMap::new()
        };
        Ok(Response::new(pb::DeploymentServiceGetDeploymentResponse {
            deployment_descriptor: Some(pb::DeploymentDescriptor {
                resource: reference(&name),
                labels: deployment.labels.clone(),
                release_reference: reference(&deployment.release),
                target_reference: reference(&deployment.target),
                status_value: deployment.status.clone(),
                status_details: details,
                status_timestamp: Some(prost_types::Timestamp {
                    seconds: 1_790_000_000,
                    nanos: 0,
                }),
            }),
        }))
    }

    async fn list_deployments(
        &self,
        request: Request<pb::DeploymentServiceListDeploymentsRequest>,
    ) -> Result<Response<pb::DeploymentServiceListDeploymentsResponse>, Status> {
        let filter = request.into_inner();
        let summaries = self
            .state()
            .deployments
            .iter()
            .filter(|(_, d)| {
                (filter.deployment_target_name.is_empty()
                    || d.target == filter.deployment_target_name)
                    && (filter.deployment_release_name.is_empty()
                        || d.release == filter.deployment_release_name)
            })
            .map(|(name, d)| pb::DeploymentSummary {
                resource: reference(name),
                status_value: d.status.clone(),
            })
            .collect();
        Ok(Response::new(
            pb::DeploymentServiceListDeploymentsResponse {
                deployment_summaries: summaries,
            },
        ))
    }

    async fn start_deployment(
        &self,
        request: Request<pb::DeploymentServiceStartDeploymentRequest>,
    ) -> Result<Response<pb::DeploymentServiceStartDeploymentResponse>, Status> {
        let name = request.into_inner().deployment_name;
        let mut state = self.state();
        let deployment = state
            .deployments
            .get_mut(&name)
            .ok_or_else(|| Status::not_found("the deployment has not been found"))?;
        if deployment.status != "PENDING" || deployment.polls != 0 || !deployment.logs.is_empty() {
            return Err(Status::failed_precondition("it is not in a draft state"));
        }
        deployment.status = "RUNNING".into();
        Ok(Response::new(
            pb::DeploymentServiceStartDeploymentResponse {
                deployment_reference: reference(&name),
            },
        ))
    }

    async fn cancel_deployment(
        &self,
        request: Request<pb::DeploymentServiceCancelDeploymentRequest>,
    ) -> Result<Response<pb::DeploymentServiceCancelDeploymentResponse>, Status> {
        let name = request.into_inner().deployment_name;
        let mut state = self.state();
        let deployment = state
            .deployments
            .get_mut(&name)
            .ok_or_else(|| Status::not_found("the deployment has not been found"))?;
        if terminal(&deployment.status) {
            return Err(Status::failed_precondition(
                "it is already in a terminal state",
            ));
        }
        deployment.status = "CANCELING".into();
        Ok(Response::new(
            pb::DeploymentServiceCancelDeploymentResponse {
                deployment_reference: reference(&name),
            },
        ))
    }

    async fn delete_deployment(
        &self,
        request: Request<pb::DeploymentServiceDeleteDeploymentRequest>,
    ) -> Result<Response<pb::DeploymentServiceDeleteDeploymentResponse>, Status> {
        let name = request.into_inner().deployment_name;
        self.state()
            .deployments
            .remove(&name)
            .ok_or_else(|| Status::not_found("the deployment has not been found"))?;
        Ok(Response::new(
            pb::DeploymentServiceDeleteDeploymentResponse {
                deployment_reference: reference(&name),
            },
        ))
    }

    async fn list_deployment_log_entries(
        &self,
        request: Request<pb::DeploymentServiceListDeploymentLogEntriesRequest>,
    ) -> Result<Response<pb::DeploymentServiceListDeploymentLogEntriesResponse>, Status> {
        let name = request.into_inner().deployment_name;
        let logs = self
            .state()
            .deployments
            .get(&name)
            .map(|d| d.logs.clone())
            .ok_or_else(|| Status::not_found("the deployment has not been found"))?;
        Ok(Response::new(
            pb::DeploymentServiceListDeploymentLogEntriesResponse {
                deployment_log_entries: logs,
            },
        ))
    }
}
