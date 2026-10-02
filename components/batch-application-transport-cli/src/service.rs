use std::path::PathBuf;

use error_stack::{Report, ResultExt};
use remora_batch::{application::BatchService, model::BatchStep};
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

pub async fn run(command: Command, service: &BatchService) -> Result<()> {
    match command {
        Command::Run { recipe } => {
            let contents = std::fs::read_to_string(&recipe)
                .map_err(|_| Report::new(Error::ReadRecipe(recipe.clone())))?;
            let steps: Vec<BatchStep> = serde_json::from_str(&contents)
                .map_err(|_| Report::new(Error::ParseRecipe(recipe.clone())))?;
            let step_count = steps.len();

            let (sink, stream) = remora_progress::channel();
            let printer = remora_progress::print_to_stderr(stream);
            let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
            let result = service.run(steps, &ctx).await;
            drop(ctx);
            let _ = printer.await;
            result.change_context(Error::Batch)?;

            println!(
                "batch recipe {} completed ({step_count} step(s))",
                recipe.display()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
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
        let err = run(Command::Run { recipe }, &service()).await.unwrap_err();
        assert!(format!("{err:?}").contains("failed to read recipe file"));
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
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("as a batch recipe"));
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
        )
        .await
        .unwrap();
        let _ = std::fs::remove_file(&recipe);
    }
}
