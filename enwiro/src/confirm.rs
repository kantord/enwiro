use std::io::{self, IsTerminal};

pub fn confirm(prompt: &str) -> anyhow::Result<bool> {
    if !io::stdin().is_terminal() {
        anyhow::bail!("cannot prompt for confirmation (stdin is not a tty); pass -y to confirm");
    }
    Ok(dialoguer::Confirm::new()
        .with_prompt(prompt)
        .default(false)
        .interact()?)
}
