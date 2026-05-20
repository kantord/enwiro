//! Pre-built fixture binary used by `enwiro-sdk` integration tests in place
//! of runtime-written shell scripts. Cargo builds this once before tests
//! run, so no test thread ever holds a write FD on an executable it is
//! about to spawn — eliminating the ETXTBSY race that originally motivated
//! issue #458.
//!
//! Behavior: reads a JSON file sibling to argv[0] (same path, `.config`
//! extension) shaped as `{ "<subcommand>": { "stdout": "...", "stderr":
//! "...", "exit": 0 } }`. Dispatches on argv[1]; missing subcommand entry
//! means empty output + exit 0. Drains stdin so callers that pipe a
//! payload (cookbook clients write a `CookbookPayload`) don't block.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;

#[derive(serde::Deserialize, Clone, Default)]
struct Step {
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    exit: i32,
    /// When true, echo whatever arrived on stdin to stdout before any
    /// canned `stdout`. Lets cookbook tests verify that the host actually
    /// piped `CookbookPayload` JSON into the child's stdin.
    #[serde(default)]
    echo_stdin: bool,
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let mut cfg_path = PathBuf::from(&argv[0]);
    cfg_path.set_extension("config");

    let subcommand = argv.get(1).cloned().unwrap_or_default();

    let mut stdin_buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin_buf);

    let cfg_bytes = std::fs::read(&cfg_path).unwrap_or_else(|e| {
        eprintln!(
            "test-fake-plugin: cannot read config at {}: {}",
            cfg_path.display(),
            e
        );
        std::process::exit(2);
    });
    let steps: HashMap<String, Step> =
        serde_json::from_slice(&cfg_bytes).expect("test-fake-plugin config must be valid JSON");
    let step = steps.get(&subcommand).cloned().unwrap_or_default();

    if step.echo_stdin {
        print!("{stdin_buf}");
    }
    if !step.stdout.is_empty() {
        print!("{}", step.stdout);
    }
    if !step.stderr.is_empty() {
        eprint!("{}", step.stderr);
    }
    std::process::exit(step.exit);
}
