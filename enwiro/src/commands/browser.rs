//! Browser-extension integration: `enw browser host` is the native
//! messaging host the browser spawns (via the wrapper script written by
//! `enw browser install`), serving the extension's client-side URL router.
//!
//! The host answers two messages: `getRules` returns every URL rule from
//! the daemon's recipe cache (see `enwiro_sdk::url_rule`), and `activate`
//! runs the normal activation path for a rule-derived recipe name. All
//! smarts stay host-side; the extension is a dumb matcher, because a Web
//! Store release cadence is much slower than a cargo release.

use std::io::Write;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::commands::activate::{ActivateArgs, activate};
use crate::context::CommandContext;
use enwiro_sdk::browser;
use enwiro_sdk::client::CachedEntry;

/// Bumped when the message shapes change incompatibly; the extension
/// refuses rule sets whose version it does not know.
const PROTOCOL_VERSION: u32 = 1;

#[derive(clap::Args)]
#[command(about = "Browser extension integration (native messaging host)")]
pub struct BrowserArgs {
    #[command(subcommand)]
    command: BrowserCommand,
}

#[derive(clap::Subcommand)]
enum BrowserCommand {
    /// Run the native messaging host loop (spawned by the browser, not
    /// meant for interactive use).
    Host,
    /// Install the native messaging manifest for every detected
    /// Chromium-family browser.
    Install,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Request {
    GetRules,
    Activate { recipe: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Response {
    Rules {
        version: u32,
        rules: Vec<RuleEntry>,
    },
    ActivateResult {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Error {
        error: String,
    },
}

/// One routable rule: the URL rule plus the anchored name claim the
/// extension re-checks derived names against before showing a badge.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuleEntry {
    cookbook: String,
    name_pattern: String,
    url_pattern: String,
    recipe_template: String,
}

pub fn browser<W: Write>(context: &mut CommandContext<W>, args: BrowserArgs) -> anyhow::Result<()> {
    match args.command {
        BrowserCommand::Host => host(context),
        BrowserCommand::Install => install(context),
    }
}

fn install<W: Write>(context: &mut CommandContext<W>) -> anyhow::Result<()> {
    let enw_binary =
        browser::resolve_enw_binary().context("Could not locate the enw binary to point at")?;
    let outcome = browser::install(&enw_binary)?;
    if outcome.manifests.is_empty() {
        writeln!(
            context.writer,
            "No Chromium-family browser config directory found; nothing installed."
        )?;
        return Ok(());
    }
    for manifest in &outcome.manifests {
        writeln!(context.writer, "Installed {}", manifest.display())?;
    }
    Ok(())
}

fn host<W: Write>(context: &mut CommandContext<W>) -> anyhow::Result<()> {
    let (mut input, mut output) = steal_stdio_channel()?;
    while let Some(payload) = browser::read_message(&mut input)? {
        let response = handle_message(context, &payload);
        let encoded = serde_json::to_vec(&response).context("Could not encode response")?;
        browser::write_message(&mut output, &encoded)?;
    }
    Ok(())
}

/// Take exclusive ownership of the native messaging channel: duplicate the
/// real stdin/stdout and point fds 0/1 at /dev/null. Activation spawns
/// child processes (adapter, cookbooks via the daemon, hooks) that inherit
/// stdio; a child writing to the inherited stdout would corrupt the
/// length-prefixed framing, and one reading stdin could eat a browser
/// message. The duplicated fds are CLOEXEC so children never see the
/// channel at all.
fn steal_stdio_channel() -> anyhow::Result<(std::fs::File, std::fs::File)> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let devnull = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .context("Could not open /dev/null")?;
    let channel = unsafe {
        let input = libc::dup(libc::STDIN_FILENO);
        let output = libc::dup(libc::STDOUT_FILENO);
        anyhow::ensure!(input >= 0 && output >= 0, "Could not duplicate stdio");
        libc::fcntl(input, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(output, libc::F_SETFD, libc::FD_CLOEXEC);
        anyhow::ensure!(
            libc::dup2(devnull.as_raw_fd(), libc::STDIN_FILENO) >= 0
                && libc::dup2(devnull.as_raw_fd(), libc::STDOUT_FILENO) >= 0,
            "Could not redirect stdio to /dev/null"
        );
        (
            std::fs::File::from_raw_fd(input),
            std::fs::File::from_raw_fd(output),
        )
    };
    Ok(channel)
}

fn handle_message<W: Write>(context: &mut CommandContext<W>, payload: &[u8]) -> Response {
    let request: Request = match serde_json::from_slice(payload) {
        Ok(request) => request,
        Err(e) => {
            return Response::Error {
                error: format!("Unsupported message: {e}"),
            };
        }
    };
    match request {
        Request::GetRules => match collect_rules(context) {
            Ok(rules) => Response::Rules {
                version: PROTOCOL_VERSION,
                rules,
            },
            Err(e) => Response::Error {
                error: format!("{e:#}"),
            },
        },
        Request::Activate { recipe } => match handle_activate(context, &recipe) {
            Ok(()) => Response::ActivateResult {
                ok: true,
                error: None,
            },
            Err(error) => Response::ActivateResult {
                ok: false,
                error: Some(error),
            },
        },
    }
}

/// Every URL rule in the daemon's recipe cache. The cache is
/// priority-sorted at build time, so the extension can use first-match-wins
/// ordering as-is.
fn collect_rules<W: Write>(context: &CommandContext<W>) -> anyhow::Result<Vec<RuleEntry>> {
    let cache = match &context.cache_dir {
        Some(dir) => enwiro_daemon::DaemonCache::with_runtime_dir(dir.clone()),
        None => enwiro_daemon::DaemonCache::open()?,
    };
    let content = cache
        .read_recipes()?
        .context("No recipe cache found - is enwiro-daemon running?")?;
    let rules = content
        .lines()
        .filter_map(|line| serde_json::from_str::<CachedEntry>(line).ok())
        .filter_map(|entry| match entry {
            CachedEntry::Pattern(pattern) => pattern.url.map(|url| RuleEntry {
                cookbook: pattern.cookbook,
                name_pattern: pattern.pattern,
                url_pattern: url.pattern,
                recipe_template: url.recipe,
            }),
            CachedEntry::Concrete(_) => None,
        })
        .collect();
    Ok(rules)
}

/// Activate a rule-derived recipe name, but only when the cache claims it
/// (or the environment already exists): `activate` switches workspaces
/// before cooking, so an unvalidated name would materialize an empty
/// workspace for whatever a buggy client sends.
fn handle_activate<W: Write>(context: &mut CommandContext<W>, recipe: &str) -> Result<(), String> {
    let exists = context
        .get_all_environments()
        .map(|envs| envs.values().any(|env| env.name == recipe))
        .unwrap_or(false);
    if !exists && !context.find_recipe_in_cache_by_name(recipe) {
        return Err(format!(
            "'{recipe}' is neither a cached recipe nor an existing environment"
        ));
    }
    activate(
        context,
        ActivateArgs {
            name: recipe.to_string(),
            no_hooks: false,
        },
    )
    .map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_utilities::context_object;

    fn url_pattern_cache_line(
        cookbook: &str,
        name_pattern: &str,
        url: &str,
        recipe: &str,
    ) -> String {
        serde_json::to_string(&enwiro_sdk::client::CachedPatternRecipe {
            cookbook: cookbook.to_string(),
            pattern: enwiro_sdk::recipe_pattern::anchor(name_pattern),
            description: None,
            url: Some(enwiro_sdk::url_rule::UrlRule {
                pattern: url.to_string(),
                recipe: recipe.to_string(),
            }),
        })
        .unwrap()
    }

    fn plain_pattern_cache_line(cookbook: &str, name_pattern: &str) -> String {
        serde_json::to_string(&enwiro_sdk::client::CachedPatternRecipe {
            cookbook: cookbook.to_string(),
            pattern: enwiro_sdk::recipe_pattern::anchor(name_pattern),
            description: None,
            url: None,
        })
        .unwrap()
    }

    #[test]
    fn get_rules_returns_url_rules_only() {
        let (_temp, mut context, _adapter, _notifications) = context_object();
        context.write_cache_lines(&[
            serde_json::to_string(&enwiro_sdk::client::CachedRecipe {
                cookbook: "git".to_string(),
                name: "my-project".to_string(),
                description: None,
                sort_order: 0,
                equivalent_to: vec![],
                scores: None,
            })
            .unwrap(),
            plain_pattern_cache_line("git", "my-project@(?P<branch>.+)"),
            url_pattern_cache_line(
                "github",
                "enwiro#(?P<number>[0-9]{1,19})",
                "https://github.com/kantord/enwiro/:kind(pull|issues)/:number([0-9]+){/*}?",
                "enwiro#{number}",
            ),
        ]);

        let response = handle_message(&mut context, br#"{"type":"getRules"}"#);
        let Response::Rules { version, rules } = response else {
            panic!("expected a rules response, got {response:?}");
        };
        assert_eq!(version, PROTOCOL_VERSION);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].cookbook, "github");
        assert_eq!(rules[0].recipe_template, "enwiro#{number}");
        assert!(rules[0].name_pattern.starts_with("^(?:"));
    }

    #[test]
    fn get_rules_without_cache_reports_error() {
        let (_temp, mut context, _adapter, _notifications) = context_object();
        let response = handle_message(&mut context, br#"{"type":"getRules"}"#);
        assert!(matches!(response, Response::Error { .. }), "{response:?}");
    }

    #[test]
    fn activate_rejects_unknown_recipe_without_switching() {
        let (_temp, mut context, adapter, _notifications) = context_object();
        context.write_cache_lines(&[plain_pattern_cache_line("git", "repo@(?P<branch>.+)")]);

        let response = handle_message(&mut context, br#"{"type":"activate","recipe":"garbage"}"#);
        let Response::ActivateResult { ok, error } = response else {
            panic!("expected an activate result");
        };
        assert!(!ok);
        assert!(error.unwrap().contains("garbage"));
        assert!(adapter.borrow().is_empty(), "workspace must not switch");
    }

    #[test]
    fn activate_runs_for_claimed_recipe() {
        let (_temp, mut context, adapter, _notifications) = context_object();
        context.write_cache_entry("fake_cookbook", "my-recipe");

        let response = handle_message(&mut context, br#"{"type":"activate","recipe":"my-recipe"}"#);
        let Response::ActivateResult { ok, .. } = response else {
            panic!("expected an activate result");
        };
        assert!(ok);
        assert_eq!(adapter.borrow().as_slice(), ["my-recipe"]);
    }

    #[test]
    fn unknown_message_type_reports_error() {
        let (_temp, mut context, _adapter, _notifications) = context_object();
        let response = handle_message(&mut context, br#"{"type":"selfDestruct"}"#);
        assert!(matches!(response, Response::Error { .. }));
    }

    #[test]
    fn responses_use_camel_case_tags() {
        let encoded = serde_json::to_string(&Response::ActivateResult {
            ok: true,
            error: None,
        })
        .unwrap();
        assert_eq!(encoded, r#"{"type":"activateResult","ok":true}"#);
    }
}
