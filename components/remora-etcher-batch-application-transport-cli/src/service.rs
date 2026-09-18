use std::path::PathBuf;

use error_stack::{Report, ResultExt};
use remora_etcher_batch::{application::BatchService, model::BatchStep};
use remora_etcher_progress::OperationContext;

use super::error::{Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a batch recipe: a JSON array of steps, each shaped like one
    /// existing subcommand's own arguments (see `remora-etcher-batch`'s
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

            let (sink, stream) = remora_etcher_progress::channel();
            let printer = remora_etcher_progress::print_to_stderr(stream);
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
