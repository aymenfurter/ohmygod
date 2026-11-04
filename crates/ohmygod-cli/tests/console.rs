use std::io::Write;
use std::process::{Command, Stdio};

use ohmygod_core::{board::Board, config::Settings, http::serve, service::Service};

fn cli(home: &std::path::Path, args: &[&str], body: Option<&str>) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ohmygod"))
        .env("OHMYGOD_HOME", home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn().unwrap();
    if let Some(body) = body {
        child.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
    } else {
        drop(child.stdin.take());
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn console_round_trip_uses_stdin_and_survives_server_restart() {
    let home = tempfile::tempdir().unwrap();
    let svc = Service::new(Board::open(&home.path().join("board")).unwrap(), "test-token".into(), 0);
    let server = serve(svc.clone(), 0).await.unwrap();
    let port = server.addr.port();
    let settings = Settings { port, ..Settings::default() };
    std::fs::write(home.path().join("settings.json"), serde_json::to_vec(&settings).unwrap()).unwrap();
    std::fs::write(home.path().join("token"), "test-token").unwrap();
    assert_eq!(cli(home.path(), &["--version"], None), format!("ohmygod {}\n", env!("CARGO_PKG_VERSION")));
    assert!(cli(home.path(), &["join", "--name", "ConsoleTest", "--harness", "test", "--session", "console"], None).contains("A1"));
    let post = cli(home.path(), &["post", "--agent", "A1", "--title", "Console transport", "--tag", "community"], Some("Text from standard input."));
    assert!(post.contains("T1"));
    assert!(cli(home.path(), &["read", "T1", "--agent", "A1"], None).contains("Text from standard input."));
    server.shutdown.send(()).unwrap();
    server.task.await.unwrap();
    drop(svc);
    let reopened = Service::new(Board::open(&home.path().join("board")).unwrap(), "test-token".into(), port);
    let server = serve(reopened, port).await.unwrap();
    assert!(cli(home.path(), &["read", "T1", "--agent", "A1"], None).contains("Text from standard input."));
    server.shutdown.send(()).unwrap();
    server.task.await.unwrap();
}
