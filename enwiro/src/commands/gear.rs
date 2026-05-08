use std::io::Write;
use std::path::Path;
use std::process::Command;

use anyhow::Context;

use crate::CommandContext;

#[derive(clap::Args)]
pub struct GearArgs {
    pub name: Option<String>,
    #[clap(skip = "xdg-open")]
    pub open_command: &'static str,
}

pub fn gear<W: Write>(context: &mut CommandContext<W>, args: GearArgs) -> anyhow::Result<()> {
    let env_name = context
        .adapter
        .get_active_environment_name()
        .context("Could not determine active environment")?;
    let flat_name = env_name.replace('/', "-");
    let gear_path = Path::new(&context.config.workspaces_directory)
        .join(&flat_name)
        .join("gear.json");

    let contents = std::fs::read_to_string(&gear_path)
        .with_context(|| format!("Could not read gear.json at {}", gear_path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&contents).context("gear.json is not valid JSON")?;
    let obj = value
        .as_object()
        .context("gear.json must be a JSON object")?;

    match args.name {
        None => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            for key in keys {
                let url = obj[key]
                    .get("open")
                    .and_then(|v| v.as_str())
                    .with_context(|| format!("Item '{}' has no 'open' string", key))?;
                writeln!(context.writer, "{}: {}", key, url)
                    .context("Could not write to output")?;
            }
        }
        Some(name) => {
            let item = obj
                .get(&name)
                .with_context(|| format!("Gear item '{}' not found", name))?;
            let url = item
                .get("open")
                .and_then(|v| v.as_str())
                .with_context(|| format!("Item '{}' has no 'open' string", name))?;
            Command::new(args.open_command)
                .arg(url)
                .spawn()
                .with_context(|| format!("Failed to spawn '{}'", args.open_command))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use tempfile::TempDir;

    use crate::{
        config::ConfigurationValues,
        context::CommandContext,
        test_utils::test_utilities::{EnwiroAdapterMock, MockNotifier},
    };

    use super::{GearArgs, gear};

    fn make_context(temp_dir: &TempDir, active_env: &str) -> CommandContext<Cursor<Vec<u8>>> {
        let mut config = ConfigurationValues::default();
        config.workspaces_directory = temp_dir.path().to_str().unwrap().to_string();

        let adapter = EnwiroAdapterMock::new(active_env);
        let notifier = MockNotifier::new();

        CommandContext {
            config,
            writer: Cursor::new(vec![]),
            adapter: Box::new(adapter),
            notifier: Box::new(notifier),
            cookbooks: vec![],
            cache_dir: Some(temp_dir.path().join("daemon")),
        }
    }

    fn write_gear_json(env_dir: &std::path::Path, json: &str) {
        fs::create_dir_all(env_dir).unwrap();
        fs::write(env_dir.join("gear.json"), json).unwrap();
    }

    fn get_output(context: &mut CommandContext<Cursor<Vec<u8>>>) -> String {
        use std::io::Read;
        let mut output = String::new();
        context.writer.set_position(0);
        context.writer.read_to_string(&mut output).unwrap();
        output
    }

    #[test]
    fn test_gear_list_all_items_from_gear_json() {
        let temp_dir = TempDir::new().unwrap();
        let env_dir = temp_dir.path().join("my-env");
        write_gear_json(
            &env_dir,
            r#"{"pull-request": {"open": "https://github.com/org/repo/pull/1"}, "issue": {"open": "https://github.com/org/repo/issues/1"}}"#,
        );

        let mut context = make_context(&temp_dir, "my-env");
        let result = gear(
            &mut context,
            GearArgs {
                name: None,
                open_command: "echo",
            },
        );

        assert!(result.is_ok(), "Expected Ok, got {:?}", result);
        let output = get_output(&mut context);
        assert!(
            output.contains("pull-request: https://github.com/org/repo/pull/1"),
            "Output should contain pull-request line, got: {}",
            output
        );
        assert!(
            output.contains("issue: https://github.com/org/repo/issues/1"),
            "Output should contain issue line, got: {}",
            output
        );
    }

    #[test]
    fn test_gear_open_item_spawns_open_command_with_url() {
        let temp_dir = TempDir::new().unwrap();
        let env_dir = temp_dir.path().join("my-env");
        write_gear_json(
            &env_dir,
            r#"{"pull-request": {"open": "https://github.com/org/repo/pull/1"}}"#,
        );

        let mut context = make_context(&temp_dir, "my-env");
        let result = gear(
            &mut context,
            GearArgs {
                name: Some("pull-request".to_string()),
                open_command: "echo",
            },
        );

        assert!(
            result.is_ok(),
            "Expected Ok when opening known item, got {:?}",
            result
        );
    }

    #[test]
    fn test_gear_missing_gear_json_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let mut context = make_context(&temp_dir, "my-env");
        let result = gear(
            &mut context,
            GearArgs {
                name: None,
                open_command: "echo",
            },
        );

        assert!(result.is_err(), "Expected Err when gear.json is missing");
    }

    #[test]
    fn test_gear_open_unknown_item_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let env_dir = temp_dir.path().join("my-env");
        write_gear_json(
            &env_dir,
            r#"{"pull-request": {"open": "https://github.com/org/repo/pull/1"}}"#,
        );

        let mut context = make_context(&temp_dir, "my-env");
        let result = gear(
            &mut context,
            GearArgs {
                name: Some("nonexistent".to_string()),
                open_command: "echo",
            },
        );

        assert!(
            result.is_err(),
            "Expected Err when named gear item does not exist"
        );
    }
}
