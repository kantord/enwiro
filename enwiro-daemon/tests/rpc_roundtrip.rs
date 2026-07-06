//! End-to-end integration test for the JSON-RPC server.
//!
//! Starts an in-process server bound to a tempdir-scoped socket, drives
//! it with the SDK client + typed `EnwiroRpcClient` extension trait,
//! asserts roundtrip behaviour.

use std::os::unix::fs::PermissionsExt;

use std::sync::{Arc, Mutex};

use enwiro_daemon::rpc;
use enwiro_sdk::rpc::{
    APPLICATION_ERROR_CODE, CYCLE_DETECTED_CODE, CookbookInvokeParams, EnwiroRpcClient, connect_at,
};
use jsonrpsee::core::client::{Client, Error as ClientError};
use jsonrpsee::types::ErrorObjectOwned;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// Spawn the RPC server in a background tokio task. Signals readiness
/// via a `oneshot` after the socket has bound, so the test never races
/// on existence polling and bind errors surface on the join handle.
async fn spawn_server(tempdir: &TempDir) -> std::path::PathBuf {
    let socket_path = tempdir.path().join("rpc.sock");

    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind rpc socket in test");
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod 0600 rpc socket in test");

    let active_env: Arc<Mutex<Option<rpc::ActiveEnvState>>> = Arc::new(Mutex::new(None));
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let socket_path_clone = socket_path.clone();
    tokio::spawn(async move {
        let _ = ready_tx.send(());
        let _ = rpc::serve_listener(
            listener,
            socket_path_clone,
            active_env,
            std::path::PathBuf::from("/tmp"),
            None,
        )
        .await;
    });
    ready_rx.await.expect("rpc server ready signal");
    socket_path
}

/// Set up a temp-dir-backed server + connected client. Returns both so
/// the caller keeps the TempDir alive (drop = remove socket + dir).
async fn setup() -> (TempDir, Client) {
    let tempdir = TempDir::new().unwrap();
    let socket_path = spawn_server(&tempdir).await;
    let client = connect_at(&socket_path).await.unwrap();
    (tempdir, client)
}

/// Assert `err` is `ClientError::Call(...)` with the expected JSON-RPC
/// error code; return the inner `ErrorObject` for further assertions.
fn assert_call_error(err: ClientError, expected_code: i32) -> ErrorObjectOwned {
    match err {
        ClientError::Call(e) => {
            assert_eq!(e.code(), expected_code, "rpc error code");
            e
        }
        other => panic!("expected ClientError::Call({expected_code}), got {other:?}"),
    }
}

/// Security-relevant invariant: socket must be 0600.
#[tokio::test]
async fn socket_is_owner_only() {
    let tempdir = TempDir::new().unwrap();
    let socket_path = spawn_server(&tempdir).await;
    let mode = std::fs::metadata(&socket_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "rpc socket must be mode 0600, got {mode:o}");
}

#[tokio::test]
async fn cookbook_invoke_returns_application_error_when_cookbook_unknown() {
    let (_tempdir, client) = setup().await;
    let err = client
        .cookbook_invoke(CookbookInvokeParams {
            cookbook: "this-cookbook-does-not-exist-anywhere".into(),
            op: "list-recipes".into(),
            args: vec![],
            payload: serde_json::Value::Null,
            call_chain: vec![],
        })
        .await
        .unwrap_err();

    let e = assert_call_error(err, APPLICATION_ERROR_CODE);
    assert!(e.message().contains("not found"), "got: {}", e.message());
}

#[tokio::test]
async fn cookbook_invoke_refuses_cycle_in_call_chain() {
    let (_tempdir, client) = setup().await;
    let err = client
        .cookbook_invoke(CookbookInvokeParams {
            cookbook: "beta".into(),
            op: "cook".into(),
            args: vec![],
            payload: serde_json::Value::Null,
            call_chain: vec!["alpha".into(), "beta".into()],
        })
        .await
        .unwrap_err();

    let _ = assert_call_error(err, CYCLE_DETECTED_CODE);
}

#[tokio::test]
async fn env_current_returns_none_when_no_switch_seen() {
    let (_tempdir, client) = setup().await;
    let result = client.env_current().await.unwrap();
    assert!(result.env_name.is_none());
    assert!(result.timestamp.is_none());
}

#[tokio::test]
async fn env_current_returns_state_when_set() {
    let tempdir = TempDir::new().unwrap();
    let socket_path = tempdir.path().join("rpc.sock");
    let _ = std::fs::remove_file(&socket_path);
    std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let active_env: Arc<Mutex<Option<rpc::ActiveEnvState>>> =
        Arc::new(Mutex::new(Some(rpc::ActiveEnvState {
            env_name: "my-project".into(),
            timestamp: 1700000000,
        })));
    let active_env_clone = active_env.clone();
    let socket_path_clone = socket_path.clone();
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = ready_tx.send(());
        let _ = rpc::serve_listener(
            listener,
            socket_path_clone,
            active_env_clone,
            std::path::PathBuf::from("/tmp"),
            None,
        )
        .await;
    });
    ready_rx.await.unwrap();

    let client = connect_at(&socket_path).await.unwrap();
    let result = client.env_current().await.unwrap();
    assert_eq!(result.env_name.as_deref(), Some("my-project"));
    assert!(result.timestamp.is_some());
}
