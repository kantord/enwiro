use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use clap::Args;

use crate::commands::ls::status_label;
use crate::context::CommandContext;
use crate::environments::Environment;
use crate::usage_stats::load_env_meta;
use enwiro_sdk::gear::{Gear, LoadedGear};

#[derive(Args)]
#[command(author, version, about = "Show information about an environment")]
pub struct EnvInfoArgs {
    /// Name of the environment to query. Defaults to the active environment.
    pub name: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

fn resolve_env_name_from_daemon() -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async {
        let client = enwiro_sdk::rpc::connect().await.ok()?;
        let result = enwiro_sdk::rpc::EnwiroRpcClient::env_current(&client)
            .await
            .ok()?;
        result.env_name
    })
}

fn classify_env<W: Write>(ctx: &CommandContext<W>, name: &str) -> Option<String> {
    if Environment::get_one(&ctx.config.workspaces_directory, name).is_ok() {
        return Some("environment".into());
    }
    if ctx.find_recipe_in_cache_by_name(name) {
        return Some("recipe".into());
    }
    None
}

const SKIP_FIELDS: &[&str] = &[
    "activation_buffer",
    "switch_buffer",
    "prep_buffer",
    "event_log",
    "status",
];

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| format!("{}: {}", k, format_value(v)))
                .collect();
            parts.join(", ")
        }
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(format_value).collect();
            parts.join(", ")
        }
    }
}

fn load_gear(env_dir: &Path) -> HashMap<String, Gear> {
    match LoadedGear::from_env_dir(env_dir) {
        Ok(loaded) => loaded.into_map(),
        Err(_) => HashMap::new(),
    }
}

fn write_text_fields<W: Write>(
    writer: &mut W,
    extra_fields: &[(&str, Option<String>)],
    meta_value: &serde_json::Value,
) -> anyhow::Result<()> {
    let mut fields: Vec<(String, String)> = Vec::new();

    for (key, value) in extra_fields {
        if let Some(v) = value {
            fields.push((key.to_string(), v.clone()));
        }
    }

    if let serde_json::Value::Object(map) = meta_value {
        for (k, v) in map {
            if SKIP_FIELDS.contains(&k.as_str()) {
                continue;
            }
            if v.is_null() {
                continue;
            }
            if extra_fields.iter().any(|(ek, _)| *ek == k.as_str()) {
                continue;
            }
            let formatted = format_value(v);
            if !formatted.is_empty() {
                fields.push((k.clone(), formatted));
            }
        }
    }

    let max_key = fields.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in &fields {
        writeln!(
            writer,
            "{:<width$} {}",
            format!("{}:", k),
            v,
            width = max_key + 1
        )?;
    }

    Ok(())
}

fn write_gear_section<W: Write>(
    writer: &mut W,
    gear: &HashMap<String, Gear>,
) -> anyhow::Result<()> {
    if gear.is_empty() {
        return Ok(());
    }

    let mut entries: Vec<(&str, &str)> = gear
        .iter()
        .map(|(name, g)| (name.as_str(), g.description.as_str()))
        .collect();
    entries.sort_by_key(|(name, _)| *name);

    let max_name = entries.iter().map(|(n, _)| n.len()).max().unwrap_or(0);

    writeln!(writer)?;
    writeln!(writer, "Gear:")?;
    for (name, desc) in &entries {
        writeln!(writer, "  {:<width$}  {}", name, desc, width = max_name)?;
    }

    Ok(())
}

pub fn env_info<W: Write>(ctx: &mut CommandContext<W>, args: EnvInfoArgs) -> anyhow::Result<()> {
    let env_name = args
        .name
        .or_else(|| ctx.resolve_environment_name(&None).ok())
        .or_else(resolve_env_name_from_daemon);
    let env_type = env_name.as_deref().and_then(|name| classify_env(ctx, name));

    let env_dir = env_name
        .as_deref()
        .map(|name| Path::new(&ctx.config.workspaces_directory).join(name));

    let meta = env_dir.as_deref().map(load_env_meta);

    let status_str = meta
        .as_ref()
        .map(|m| status_label(m.status.as_ref()))
        .filter(|s| *s != "-");

    let gear = env_dir.as_deref().map(load_gear).unwrap_or_default();

    if args.json {
        let mut json_value = meta
            .as_ref()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        if let serde_json::Value::Object(ref mut map) = json_value {
            if let Some(name) = &env_name {
                map.insert("name".to_string(), serde_json::Value::String(name.clone()));
            }
            if let Some(t) = &env_type {
                map.insert("type".to_string(), serde_json::Value::String(t.clone()));
            }
            if !gear.is_empty() {
                let mut gear_names: Vec<&str> = gear.keys().map(|s| s.as_str()).collect();
                gear_names.sort();
                map.insert(
                    "gear".to_string(),
                    serde_json::Value::Array(
                        gear_names
                            .iter()
                            .map(|n| serde_json::Value::String(n.to_string()))
                            .collect(),
                    ),
                );
            }
        }

        let json = serde_json::to_string(&json_value).context("Failed to serialize output")?;
        write!(ctx.writer, "{json}")?;
    } else {
        let meta_value = meta
            .as_ref()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let extra_fields: Vec<(&str, Option<String>)> = vec![
            ("name", env_name.clone()),
            ("type", env_type.clone()),
            ("status", status_str.map(|s| s.to_string())),
        ];

        write_text_fields(&mut ctx.writer, &extra_fields, &meta_value)?;
        write_gear_section(&mut ctx.writer, &gear)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    use crate::test_utils::test_utilities::{
        AdapterLog, FakeContext, NotificationLog, context_object,
    };
    use enwiro_daemon::meta::{CookedPhase, EnvStats, Status, UserIntentSignals};

    #[rstest]
    fn test_env_info_text_shows_name_and_type(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            output.contains("name:"),
            "output should contain name field: {output}"
        );
        assert!(
            output.contains("my-env"),
            "output should contain env name: {output}"
        );
        assert!(
            output.contains("type:"),
            "output should contain type field: {output}"
        );
        assert!(
            output.contains("environment"),
            "output should show type as environment: {output}"
        );
    }

    #[rstest]
    fn test_env_info_text_shows_metadata_fields(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let meta = EnvStats {
            description: Some("Fix auth bug".to_string()),
            cookbook: Some("github".to_string()),
            recipe: Some("owner/repo#42".to_string()),
            status: Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            }),
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            output.contains("github"),
            "output should contain cookbook: {output}"
        );
        assert!(
            output.contains("owner/repo#42"),
            "output should contain recipe: {output}"
        );
        assert!(
            output.contains("Fix auth bug"),
            "output should contain description: {output}"
        );
    }

    #[rstest]
    fn test_env_info_text_omits_absent_fields(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            !output.contains("cookbook:"),
            "absent cookbook should be omitted: {output}"
        );
        assert!(
            !output.contains("recipe:"),
            "absent recipe should be omitted: {output}"
        );
        assert!(
            !output.contains("description:"),
            "absent description should be omitted: {output}"
        );
    }

    #[rstest]
    fn test_env_info_text_omits_signal_buffers(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let meta = EnvStats {
            signals: UserIntentSignals {
                activation_buffer: vec![(1_700_000_000, 1.0)],
                switch_buffer: vec![(1_700_000_000, 1.0)],
                prep_buffer: vec![(1_700_000_000, 1.0)],
            },
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            !output.contains("activation_buffer"),
            "signal buffers should be hidden: {output}"
        );
        assert!(
            !output.contains("switch_buffer"),
            "signal buffers should be hidden: {output}"
        );
        assert!(
            !output.contains("prep_buffer"),
            "signal buffers should be hidden: {output}"
        );
    }

    #[rstest]
    fn test_env_info_json_includes_all_metadata(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let meta = EnvStats {
            description: Some("Fix auth bug".to_string()),
            cookbook: Some("github".to_string()),
            recipe: Some("owner/repo#42".to_string()),
            status: Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            }),
            signals: UserIntentSignals {
                activation_buffer: vec![(1_700_000_000, 1.0)],
                ..Default::default()
            },
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: true,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["name"], "my-env");
        assert_eq!(parsed["type"], "environment");
        assert_eq!(parsed["cookbook"], "github");
        assert_eq!(parsed["recipe"], "owner/repo#42");
        assert_eq!(parsed["description"], "Fix auth bug");
        assert_eq!(parsed["status"]["type"], "cooked");
        assert_eq!(parsed["status"]["phase"], "active");
        assert!(
            parsed["activation_buffer"].is_array(),
            "JSON should include activation_buffer"
        );
    }

    #[rstest]
    fn test_env_info_json_includes_name_and_type(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: true,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["name"], "my-env");
        assert_eq!(parsed["type"], "environment");
    }

    #[rstest]
    fn test_env_info_text_no_error_without_json_flag(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let result = env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        );
        assert!(
            result.is_ok(),
            "plain text output should work without --json"
        );
    }

    #[rstest]
    fn test_env_info_classifies_recipe(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.write_cache_entries(&[("git", "some-recipe", None)]);

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("some-recipe".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(output.contains("recipe"), "type should be recipe: {output}");
    }

    #[test]
    fn test_format_value_string() {
        let v = serde_json::Value::String("hello".into());
        assert_eq!(format_value(&v), "hello");
    }

    #[test]
    fn test_format_value_object() {
        let v = serde_json::json!({"type": "cooked", "phase": "active"});
        let result = format_value(&v);
        assert!(result.contains("type: cooked"));
        assert!(result.contains("phase: active"));
    }

    #[test]
    fn test_format_value_object_skips_null() {
        let v = serde_json::json!({"type": "done", "outcome": null});
        let result = format_value(&v);
        assert!(result.contains("type: done"));
        assert!(!result.contains("outcome"));
    }

    #[test]
    fn test_write_text_fields_alignment() {
        let mut buf = Vec::new();
        let meta = serde_json::json!({"cookbook": "github"});
        let extra = vec![("name", Some("my-env".to_string()))];

        write_text_fields(&mut buf, &extra, &meta).unwrap();

        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        let value_positions: Vec<usize> = lines
            .iter()
            .filter_map(|l| {
                let after_colon = l.find(':')? + 1;
                let rest = &l[after_colon..];
                let trimmed_start = rest.len() - rest.trim_start().len();
                Some(after_colon + trimmed_start)
            })
            .collect();
        assert!(
            value_positions.windows(2).all(|w| w[0] == w[1]),
            "values should be aligned: {:?}",
            lines
        );
    }

    #[test]
    fn test_write_text_fields_skips_none_extra() {
        let mut buf = Vec::new();
        let meta = serde_json::json!({});
        let extra = vec![("name", Some("my-env".to_string())), ("type", None)];

        write_text_fields(&mut buf, &extra, &meta).unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("name:"));
        assert!(!output.contains("type:"));
    }

    #[rstest]
    fn test_env_info_text_shows_status_as_label(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let meta = EnvStats {
            status: Some(Status::Cooked {
                phase: Some(CookedPhase::Active),
                detail: None,
            }),
            ..Default::default()
        };
        std::fs::write(
            temp_dir.path().join("my-env").join("meta.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            output.contains("status:") && output.contains("active"),
            "status should show as 'active', not raw JSON: {output}"
        );
        assert!(
            !output.contains("cooked"),
            "status should not show raw 'cooked' tag: {output}"
        );
        assert!(
            !output.contains("phase:"),
            "status should not show raw 'phase:' key: {output}"
        );
    }

    #[rstest]
    fn test_env_info_text_shows_gear_with_descriptions(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let gear_dir = temp_dir.path().join("my-env").join("gear.d");
        std::fs::create_dir_all(&gear_dir).unwrap();
        std::fs::write(
            gear_dir.join("cookbook-github.json"),
            r#"{"version":1,"gear":{"issue":{"description":"Issue page"}}}"#,
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            output.contains("Gear:"),
            "output should have a Gear section: {output}"
        );
        assert!(
            output.contains("issue") && output.contains("Issue page"),
            "gear should show name and description: {output}"
        );
    }

    #[rstest]
    fn test_env_info_text_gear_section_separated(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let gear_dir = temp_dir.path().join("my-env").join("gear.d");
        std::fs::create_dir_all(&gear_dir).unwrap();
        std::fs::write(
            gear_dir.join("cookbook-github.json"),
            r#"{"version":1,"gear":{"issue":{"description":"Issue page"}}}"#,
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        let gear_pos = output.find("Gear:").expect("should have Gear: section");
        let before_gear = &output[..gear_pos];
        assert!(
            before_gear.ends_with("\n\n"),
            "gear section should be separated by a blank line: {output:?}"
        );
    }

    #[rstest]
    fn test_env_info_text_omits_gear_when_no_gear_dir(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (_temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: false,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        assert!(
            !output.contains("Gear:"),
            "gear section should be omitted when no gear.d dir: {output}"
        );
    }

    #[rstest]
    fn test_env_info_json_includes_gear(
        context_object: (tempfile::TempDir, FakeContext, AdapterLog, NotificationLog),
    ) {
        let (temp_dir, mut ctx, _, _) = context_object;
        ctx.create_mock_environment("my-env");

        let gear_dir = temp_dir.path().join("my-env").join("gear.d");
        std::fs::create_dir_all(&gear_dir).unwrap();
        std::fs::write(
            gear_dir.join("cookbook-github.json"),
            r#"{"version":1,"gear":{"issue":{"description":"Issue page"}}}"#,
        )
        .unwrap();

        env_info(
            &mut ctx,
            EnvInfoArgs {
                name: Some("my-env".to_string()),
                json: true,
            },
        )
        .unwrap();

        let output = ctx.get_output();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let gear = parsed["gear"].as_array().expect("gear should be an array");
        assert!(
            gear.iter().any(|v| v == "issue"),
            "gear array should contain 'issue': {output}"
        );
    }
}
