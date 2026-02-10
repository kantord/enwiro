use anyhow::Context;
use clap::Parser;
use tokio_i3ipc::I3;

#[derive(Parser)]
enum EnwiroAdapterI3WmCLI {
    GetActiveWorkspaceId(GetActiveWorkspaceIdArgs),
}

#[derive(clap::Args)]
pub struct GetActiveWorkspaceIdArgs {}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = EnwiroAdapterI3WmCLI::parse();

    match args {
        EnwiroAdapterI3WmCLI::GetActiveWorkspaceId(_) => {
            let mut i3 = I3::connect().await?;
            let workspaces = i3.get_workspaces().await?;
            let focused_workspace = workspaces
                .into_iter()
                .find(|workspace| workspace.focused)
                .context("No active workspace. This should never happen.")?;
            let mut environment_name: String = "".to_string();
            let is_active_environment = focused_workspace.id.to_string() != focused_workspace.name;

            if is_active_environment {
                environment_name = focused_workspace
                    .name
                    .replace(':', "")
                    .replace(&focused_workspace.num.to_string(), "")
                    .trim()
                    .to_string()
            }

            print!("{}", environment_name);
        }
    };

    Ok(())
}
