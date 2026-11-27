use std::path::PathBuf;

use anvil_core::CoreError;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(
        "'{tool}' is not installed in {}: looked for {}",
        home.display(),
        searched.iter().map(|p| format!("'{}'", p.display())).collect::<Vec<_>>().join(", ")
    )]
    ToolNotFound {
        tool: String,
        home: PathBuf,
        searched: Vec<PathBuf>,
    },

    #[error("refusing to run '{}': it resolves to the anvil shim itself", path.display())]
    ShimRecursion { path: PathBuf },

    #[error("failed to run '{}': {source}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot determine {0}")]
    Environment(String),

    #[error("registry error ({context}): {source}")]
    Registry {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T, E = EnvError> = std::result::Result<T, E>;
