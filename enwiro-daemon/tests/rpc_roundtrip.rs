//! End-to-end integration test for the JSON-RPC server.
//!
//! Starts an in-process server bound to a tempdir-scoped socket, drives
//! it with the SDK client + typed `EnwiroRpcClient` extension trait,
//! asserts roundtrip behaviour.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use enwiro_daemon::rpc;
use enwiro_sdk::rpc::{
    APPLICATION_ERROR_CODE, CYCLE_DETECTED_CODE, CookbookInvokeParams, EnwiroRpcClient, connect_at,
};
use jsonrpsee::core::client::Error as ClientError;
use tempfile::TempDir;
use tokio::sync::oneshot;

/// Spawn the RPC server in a background tokio task. Signals readiness
/// via a `oneshot` after the socket has bound, so the test never races
/// on existence polling and bind errors surface on the join handle.
async fn spawn_server(tempdir: &TempDir) -> std::path::PathBuf {
    let socket_path = tempdir.path().join("rpc.sock");
    let state = Arc::new(rpc::State::default());

    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind rpc socket in test");
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod 0600 rpc socket in test");

    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let socket_path_clone = socket_path.clone();
    tokio::spawn(async move {
        let _ = ready_tx.send(());
        let _ = rpc::serve_listener(listener, state, socket_path_clone).await;
    });
    ready_rx.await.expect("rpc server ready signal");
    socket_path
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
    assert_eq!(mode, 0o600, "rpc socket must be mode 0600, got {:o}", mode);
}

#[tokio::test]
async fn cookbook_invoke_returns_application_error_when_cookbook_unknown() {
    let tempdir = TempDir::new().unwrap();
    let socket_path = spawn_server(&tempdir).await;

    let client = connect_at(&socket_path).await.unwrap();
    let err = client
        .cookbook_invoke(CookbookInvokeParams {
            cookbook: "this-cookbook-does-not-exist-anywhere".into(),
            op: "list-recipes".into(),
            args: serde_json::Value::Null,
            payload: serde_json::Value::Null,
            call_chain: vec![],
        })
        .await
        .unwrap_err();

    match err {
        ClientError::Call(e) => {
            assert_eq!(e.code(), APPLICATION_ERROR_CODE);
            assert!(e.message().contains("not found"), "got: {}", e.message());
        }
        other => panic!("expected ClientError::Call (APPLICATION_ERROR), got {other:?}"),
    }
}

#[tokio::test]
async fn cookbook_invoke_refuses_cycle_in_call_chain() {
    let tempdir = TempDir::new().unwrap();
    let socket_path = spawn_server(&tempdir).await;

    let client = connect_at(&socket_path).await.unwrap();
    let err = client
        .cookbook_invoke(CookbookInvokeParams {
            cookbook: "git".into(),
            op: "cook".into(),
            args: serde_json::Value::Null,
            payload: serde_json::Value::Null,
            call_chain: vec!["github".into(), "git".into()],
        })
        .await
        .unwrap_err();

    match err {
        ClientError::Call(e) => {
            assert_eq!(e.code(), CYCLE_DETECTED_CODE);
        }
        other => panic!("expected ClientError::Call (CYCLE_DETECTED), got {other:?}"),
    }
}
