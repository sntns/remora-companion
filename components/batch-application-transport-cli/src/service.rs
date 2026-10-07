use std::path::PathBuf;

use error_stack::ResultExt;
use remora_batch::{application::BatchService, model::BatchStep};
use remora_context::model::ContextOverride;
use remora_progress::OperationContext;

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a batch recipe: a JSON array of steps, each shaped like one
    /// existing subcommand's own arguments (see `remora-batch`'s
    /// `BatchStep` for the exact fields each step kind takes, e.g.
    /// `{"step": "convert-to-raw", "image": "...", "output": "..."}`).
    Run {
        /// Path to a JSON file containing an array of steps.
        #[arg(long)]
        recipe: PathBuf,
    },
}

/// `over`: the context factory-provision steps manufacture as, unless a
/// step names its own.
pub async fn run(
    command: Command,
    service: &BatchService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::Run { recipe } => {
            let contents = std::fs::read_to_string(&recipe)
                .change_context_lazy(|| Error::ReadRecipe(recipe.clone()))?;
            let steps: Vec<BatchStep> = serde_json::from_str(&contents)
                .change_context_lazy(|| Error::ParseRecipe(recipe.clone()))?;
            let step_count = steps.len();

            let (sink, stream) = remora_progress::channel();
            let follow = remora_tui::follow(stream);
            let ctx = OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c());
            let result = service.run(steps, over, &ctx).await;
            drop(ctx);
            follow.finish(&result).await;
            result.change_context(Error::Batch)?;

            remora_tui::success(format!(
                "Ran {} ({step_count} step(s))",
                remora_tui::accent(recipe.display())
            ));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use remora_image::model::{PartitionRole, PartitionSelector};

    use super::*;

    /// Never actually run in these tests -- they only exercise the
    /// read-recipe-file/parse-JSON path, which returns before `service` is
    /// ever touched, except for the empty-recipe success case.
    struct AlwaysSucceeds;

    #[async_trait::async_trait]
    impl remora_batch::application::BatchServiceInterface for AlwaysSucceeds {
        async fn run(
            &self,
            _steps: Vec<BatchStep>,
            _over: Option<&ContextOverride>,
            _ctx: &OperationContext,
        ) -> remora_batch::application::Result<()> {
            Ok(())
        }
    }

    fn service() -> BatchService {
        BatchService::new(AlwaysSucceeds)
    }

    fn temp_path(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-batch-cli-test-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        path
    }

    #[tokio::test]
    async fn fails_clearly_when_the_recipe_file_does_not_exist() {
        let recipe = temp_path("missing.json");
        let err = run(Command::Run { recipe }, &service(), None)
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("failed to read recipe file"));
        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound),
            "the io error is kept as the cause"
        );
    }

    #[tokio::test]
    async fn fails_clearly_on_malformed_json() {
        let recipe = temp_path("bad.json");
        std::fs::write(&recipe, "not json").unwrap();
        let err = run(
            Command::Run {
                recipe: recipe.clone(),
            },
            &service(),
            None,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("as a batch recipe"));
        // The parser's own error is kept as the cause: it says where.
        let cause = err
            .downcast_ref::<serde_json::Error>()
            .expect("the serde error is kept as the cause");
        assert_eq!((cause.line(), cause.column()), (1, 2));
        let _ = std::fs::remove_file(&recipe);
    }

    #[tokio::test]
    async fn runs_an_empty_recipe_successfully() {
        let recipe = temp_path("empty.json");
        std::fs::write(&recipe, "[]").unwrap();
        run(
            Command::Run {
                recipe: recipe.clone(),
            },
            &service(),
            None,
        )
        .await
        .unwrap();
        let _ = std::fs::remove_file(&recipe);
    }

    #[test]
    fn still_parses_a_recipe_naming_a_boot_mode() {
        // Roles used to need one; they're now found from the image's own
        // layout, and a recipe written before that keeps working.
        let steps: Vec<BatchStep> = serde_json::from_str(
            r#"[{"step": "image-inject", "image": "remora.wic", "source": "a.json",
                 "dest_path": "/a.json", "partition": {"role": "data"},
                 "boot_mode": "efi", "mode": 420}]"#,
        )
        .unwrap();
        let [BatchStep::ImageInject(request)] = steps.as_slice() else {
            panic!("expected one image-inject step, got {steps:?}");
        };
        assert_eq!(
            request.partition,
            PartitionSelector::Role(PartitionRole::Data)
        );
    }
}
