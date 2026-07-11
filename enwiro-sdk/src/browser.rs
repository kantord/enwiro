//! Browser-extension integration: the native-messaging wire framing and the
//! per-browser host manifest installer.
//!
//! Browsers talk to native programs through "native messaging": the browser
//! spawns the executable named by a small manifest file in its config
//! directory and exchanges length-prefixed JSON messages with it over
//! stdio. The manifest can carry no arguments, so it points at a wrapper
//! script that execs `enw browser host`. Shared by `enw browser install`
//! and the daemon's idempotent auto-install at startup.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;

/// Native messaging host name; the extension addresses the host by this
/// name, and the manifest file is named after it.
pub const NATIVE_HOST_NAME: &str = "ro.enwi.browser_host";

/// Extension IDs allowed to spawn the native host. The first is the
/// development (load-unpacked) ID, pinned by the `key` field in the
/// extension's manifest.json. A Chrome Web Store listing gets its own ID;
/// it is appended here once one exists.
pub const EXTENSION_IDS: &[&str] = &["fiigfehpoiaamipdficcboaljopdopcb"];

/// Chromium-family browser config directories (relative to the XDG config
/// dir) probed for manifest installation. Only browsers whose directory
/// already exists get a manifest.
const CHROMIUM_CONFIG_DIRS: &[&str] = &[
    "google-chrome",
    "google-chrome-beta",
    "google-chrome-unstable",
    "chromium",
    "BraveSoftware/Brave-Browser",
    "microsoft-edge",
    "vivaldi",
];

/// Upper bound on a single native message. Chrome caps extension-to-host
/// messages at 4 GB but nothing legitimate comes close; a corrupt length
/// prefix must not make us allocate gigabytes.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// Read one length-prefixed message. `Ok(None)` means the peer closed the
/// stream cleanly (EOF on the length prefix), i.e. the browser shut the
/// port down and the host should exit.
pub fn read_message(reader: &mut impl Read) -> anyhow::Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    if let Err(e) = reader.read_exact(&mut length) {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Ok(None);
        }
        return Err(e).context("Could not read native message length");
    }
    let length = u32::from_le_bytes(length) as usize;
    anyhow::ensure!(
        length <= MAX_MESSAGE_BYTES,
        "native message of {} bytes exceeds the {} byte cap",
        length,
        MAX_MESSAGE_BYTES,
    );
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .context("Could not read native message payload")?;
    Ok(Some(payload))
}

/// Write one length-prefixed message and flush it.
pub fn write_message(writer: &mut impl Write, payload: &[u8]) -> anyhow::Result<()> {
    let length = u32::try_from(payload.len()).context("Native message too large")?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|_| writer.write_all(payload))
        .and_then(|_| writer.flush())
        .context("Could not write native message")?;
    Ok(())
}

/// What [`install`] wrote: the wrapper script plus one manifest per
/// detected browser (empty when no Chromium-family config dir exists).
#[derive(Debug)]
pub struct InstallOutcome {
    pub wrapper: PathBuf,
    pub manifests: Vec<PathBuf>,
}

/// Locate the `enw` binary the wrapper script should exec: the running
/// executable if it is `enw` itself, else a sibling named `enw` (both live
/// in `~/.cargo/bin` under a cargo install), else a `$PATH` lookup.
pub fn resolve_enw_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if exe.file_name().is_some_and(|name| name == "enw") {
        return Some(exe);
    }
    let sibling = exe.parent()?.join("enw");
    if sibling.is_file() {
        return Some(sibling);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("enw"))
        .find(|candidate| candidate.is_file())
}

/// Idempotently install the native messaging host for every detected
/// Chromium-family browser, using the XDG data and config directories.
pub fn install(enw_binary: &Path) -> anyhow::Result<InstallOutcome> {
    let data_dir = dirs::data_dir()
        .context("Could not determine data directory (is $HOME set?)")?
        .join("enwiro");
    let config_dir = dirs::config_dir().context("Could not determine config directory")?;
    install_at(enw_binary, &data_dir, &config_dir)
}

/// [`install`] against explicit directories, for tests.
pub fn install_at(
    enw_binary: &Path,
    data_dir: &Path,
    config_dir: &Path,
) -> anyhow::Result<InstallOutcome> {
    let wrapper = write_wrapper_script(enw_binary, data_dir)?;
    let manifest = host_manifest(&wrapper);
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).expect("host manifest is always serializable");

    let mut manifests = Vec::new();
    for browser in CHROMIUM_CONFIG_DIRS {
        let browser_dir = config_dir.join(browser);
        if !browser_dir.is_dir() {
            continue;
        }
        let hosts_dir = browser_dir.join("NativeMessagingHosts");
        std::fs::create_dir_all(&hosts_dir)
            .with_context(|| format!("Could not create {}", hosts_dir.display()))?;
        let manifest_path = hosts_dir.join(format!("{}.json", NATIVE_HOST_NAME));
        crate::fs::atomic_write(&manifest_path, &manifest_bytes)
            .with_context(|| format!("Could not write {}", manifest_path.display()))?;
        manifests.push(manifest_path);
    }
    Ok(InstallOutcome { wrapper, manifests })
}

fn host_manifest(wrapper: &Path) -> serde_json::Value {
    let allowed_origins: Vec<String> = EXTENSION_IDS
        .iter()
        .map(|id| format!("chrome-extension://{}/", id))
        .collect();
    serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "enwiro browser integration host",
        "path": wrapper,
        "type": "stdio",
        "allowed_origins": allowed_origins,
    })
}

/// The manifest cannot pass arguments, so it points at a two-line shell
/// script that execs the hidden host subcommand.
fn write_wrapper_script(enw_binary: &Path, data_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("Could not create {}", data_dir.display()))?;
    let wrapper = data_dir.join("browser-host");
    let script = format!(
        "#!/bin/sh\nexec {} browser host\n",
        shell_single_quote(&enw_binary.to_string_lossy()),
    );
    crate::fs::atomic_write(&wrapper, script.as_bytes())
        .with_context(|| format!("Could not write {}", wrapper.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("Could not make {} executable", wrapper.display()))?;
    }
    Ok(wrapper)
}

fn shell_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trips() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, br#"{"type":"getRules"}"#).unwrap();
        let mut reader = buffer.as_slice();
        let payload = read_message(&mut reader).unwrap().unwrap();
        assert_eq!(payload, br#"{"type":"getRules"}"#);
        assert!(read_message(&mut reader).unwrap().is_none(), "clean EOF");
    }

    #[test]
    fn read_message_rejects_oversized_length_prefix() {
        let length_prefix = u32::MAX.to_le_bytes();
        let mut reader = length_prefix.as_slice();
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn install_writes_wrapper_and_manifest_per_detected_browser() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(config_dir.join("chromium")).unwrap();
        std::fs::create_dir_all(config_dir.join("BraveSoftware/Brave-Browser")).unwrap();

        let outcome = install_at(Path::new("/opt/bin/enw"), &data_dir, &config_dir).unwrap();

        assert_eq!(outcome.manifests.len(), 2);
        let script = std::fs::read_to_string(&outcome.wrapper).unwrap();
        assert!(
            script.contains("exec '/opt/bin/enw' browser host"),
            "{script}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&outcome.wrapper)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "wrapper must be executable");
        }
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(config_dir.join(format!(
                "chromium/NativeMessagingHosts/{NATIVE_HOST_NAME}.json"
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["name"], NATIVE_HOST_NAME);
        assert_eq!(manifest["type"], "stdio");
        assert_eq!(manifest["path"], outcome.wrapper.to_string_lossy().as_ref());
        assert_eq!(
            manifest["allowed_origins"][0],
            format!("chrome-extension://{}/", EXTENSION_IDS[0])
        );
    }

    #[test]
    fn install_without_browsers_writes_no_manifests() {
        let temp = tempfile::tempdir().unwrap();
        let outcome = install_at(
            Path::new("/opt/bin/enw"),
            &temp.path().join("data"),
            &temp.path().join("config"),
        )
        .unwrap();
        assert!(outcome.manifests.is_empty());
    }

    #[test]
    fn install_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let config_dir = temp.path().join("config");
        std::fs::create_dir_all(config_dir.join("chromium")).unwrap();

        let first = install_at(Path::new("/opt/bin/enw"), &data_dir, &config_dir).unwrap();
        let second = install_at(Path::new("/opt/bin/enw"), &data_dir, &config_dir).unwrap();
        assert_eq!(first.manifests, second.manifests);
    }
}
