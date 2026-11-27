use anvil_core::paths::resolve_install_dir;
use anvil_core::InstallLayout;
use anvil_env::shell::{SessionEnv, Shell};
use anyhow::Result;

use crate::cli::EnvArgs;
use crate::output::Outcome;

pub fn run(args: &EnvArgs) -> Result<Outcome> {
    let layout = InstallLayout::new(resolve_install_dir(args.dest.as_deref())?);
    if !layout.state_file().is_file() {
        // stderr only: stdout must stay valid for `eval` / JSON consumers.
        eprintln!(
            "warning: no anvil installation found at {} (run `anvil install`)",
            layout.root().display()
        );
    }
    let shell = args.shell.unwrap_or_else(Shell::detect);
    let env = SessionEnv::for_layout(&layout);
    Ok(Outcome {
        json: (shell == Shell::Json).then(|| env.to_json()),
        human: env.render(shell),
        exit_code: 0,
    })
}
