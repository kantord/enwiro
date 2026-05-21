//! End-to-end integration test for the JSON-RPC server.
//!
//! Starts an in-process server bound to a tempdir-scoped socket, drives it
//! with the SDK `Client`, asserts roundtrip behavior. No actual cookbook
//! spawning here (no plugins to find); a follow-up test will exercise the
//! cookbook handler against a fixture plugin.

use std::sync::Arc;
use std::time::Duration;

use enwiro_daemon::rpc;
use enwiro_sdk::rpc::{Client, ClientError, CookbookInvokeParams, RpcError};
use tempfile::TempDir;

/// Spawn the rpc server on a current-thread runtime in a background tokio
/// task. Returns the socket path; the test client connects to it.
async fn spawn_server(tempdir: &TempDir) -> std::path::PathBuf {
    let socket_path = tempdir.path().join("rpc.sock");
    let state = Arc::new(rpc::State::default());
    {
        let socket_path = socket_path.clone();
        tokio::spawn(async move {
            let _ = rpc::serve(socket_path, state).await;
        });
    }
    // Give the server a moment to bind before the client connects.
    for _ in 0..50 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    socket_path
}

#[tokio::test]
async fn cookbook_invoke_returns_application_error_when_cookbook_unknown() {
    let tempdir = TempDir::new().unwrap();
    let socket_path = spawn_server(&tempdir).await;

    let mut client = Client::connect_at(&socket_path).await.unwrap();
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
        ClientError::Rpc(RpcError { code, message, .. }) => {
            assert_eq!(code, RpcError::APPLICATION_ERROR);
            assert!(message.contains("not found"), "got: {message}");
        }
        other => panic!("expected RpcError::APPLICATION_ERROR, got {other:?}"),
    }
}

#[tokio::test]
async fn cookbook_invoke_refuses_cycle_in_call_chain() {
    let tempdir = TempDir::new().unwrap();
    let socket_path = spawn_server(&tempdir).await;

    let mut client = Client::connect_at(&socket_path).await.unwrap();
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
        ClientError::Rpc(RpcError { code, .. }) => {
            assert_eq!(code, RpcError::CYCLE_DETECTED);
        }
        other => panic!("expected RpcError::CYCLE_DETECTED, got {other:?}"),
    }
}
