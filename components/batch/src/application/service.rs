use remora_context::model::ContextOverride;
use remora_progress::OperationContext;

use crate::model::BatchStep;

use super::error::Result;

/// The batch vertical's application-facing port: run an arbitrary,
/// caller-supplied sequence of other verticals' operations as one call,
/// with one unified progress/log stream across all of them.
///
/// Stops at the first failing step -- no automatic cleanup, no rollback of
/// steps already done (a deliberate choice: simpler, and matches how the
/// same sequence run by hand as separate CLI commands already behaves).
/// The error names which step (index + kind) failed; the image/config/etc.
/// files any earlier steps already produced are left exactly as they were.
#[async_trait::async_trait]
pub trait BatchServiceInterface: Send + Sync {
    /// `over` is the context a `FactoryProvision` step manufactures as,
    /// unless the step names its own.
    async fn run(
        &self,
        steps: Vec<BatchStep>,
        over: Option<&ContextOverride>,
        ctx: &OperationContext,
    ) -> Result<()>;
}

/// Injectable handle to whatever `BatchServiceInterface` implementation was
/// wired at startup (normally `remora-batch-application`'s
/// `BatchControllerImpl`).
#[derive(Clone)]
pub struct BatchService(busybody::Service<Box<dyn BatchServiceInterface>>);

impl BatchService {
    pub fn new<T: BatchServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for BatchService {
    type Target = busybody::Service<Box<dyn BatchServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
