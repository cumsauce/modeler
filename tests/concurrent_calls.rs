//! A slow `apply_ops` runs its heavy part off the app thread, so a quick
//! call on another document is answered while it is still working (and the
//! slow one still lands in its own document).

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn call(id: u64, name: &str, args: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
           "params": {"name": name, "arguments": args}})
    .to_string()
}

#[test]
fn a_slow_apply_does_not_block_a_quick_call_on_another_doc() {
    let dir = tempfile_dir();
    let mut child = Command::new(env!("CARGO_BIN_EXE_modeler"))
        .arg("--headless")
        .env("FRIENDSLOP_DATA_DIR", &dir)
        .env_remove("MODELER_PROJECT")
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn: {e}"));
    let Some(mut stdin) = child.stdin.take() else {
        panic!("no stdin")
    };
    let Some(stdout) = child.stdout.take() else {
        panic!("no stdout")
    };
    let (tx, rx) = std::sync::mpsc::channel::<(Instant, Value)>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                let _ = tx.send((Instant::now(), v));
            }
        }
    });
    let mut send = |s: String| {
        writeln!(stdin, "{s}").unwrap_or_else(|e| panic!("write: {e}"));
        stdin.flush().unwrap_or_else(|e| panic!("flush: {e}"));
    };
    let wait = |id: u64| -> (Instant, Value) {
        loop {
            let (t, v) = rx
                .recv_timeout(Duration::from_secs(120))
                .unwrap_or_else(|e| panic!("no reply to #{id}: {e}"));
            if v["id"] == json!(id) {
                return (t, v);
            }
        }
    };
    send(
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}}})
        .to_string(),
    );
    wait(0);
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string());
    let box_item = |name: &str| {
        json!({"type": "primitive", "name": name, "kind": "box", "size": [0.2, 0.2, 0.2],
               "translation": [0, 0.1, 0]})
    };
    send(call(1, "new_model", json!({"name": "slow"})));
    wait(1);
    send(call(
        2,
        "apply_ops",
        json!({"doc": "slow", "ops": [box_item("seed")]}),
    ));
    wait(2);
    send(call(3, "new_model", json!({"name": "quick"})));
    wait(3);
    send(call(
        4,
        "apply_ops",
        json!({"doc": "quick", "ops": [box_item("a")]}),
    ));
    wait(4);
    // A fine sdf: seconds of meshing.
    let sdf = json!({"type": "sdf", "name": "s", "resolution": 0.006, "shapes": [
        {"shape": "sphere", "radius": 0.5, "at": [0, 0.5, 0]},
        {"shape": "torus", "size": [0.5, 0.12], "at": [0, 0.5, 0]}],
        "warps": [{"mode": "displace", "amount": 0.03, "scale": 0.2, "seed": 3}]});
    let start = Instant::now();
    send(call(5, "apply_ops", json!({"doc": "slow", "ops": [sdf]})));
    std::thread::sleep(Duration::from_millis(200));
    send(call(
        6,
        "apply_ops",
        json!({"doc": "quick", "ops": [box_item("b")]}),
    ));
    let (quick_at, quick) = wait(6);
    let (slow_at, slow) = wait(5);
    assert_ne!(quick["result"]["isError"], json!(true), "{quick}");
    assert_ne!(slow["result"]["isError"], json!(true), "{slow}");
    let slow_ms = (slow_at - start).as_millis();
    let quick_ms = (quick_at - start).as_millis();
    assert!(
        quick_at < slow_at && slow_ms > 1000,
        "quick call answered at {quick_ms} ms, slow at {slow_ms} ms"
    );
    // Each batch landed in its own document.
    send(call(7, "scene_summary", json!({"doc": "slow"})));
    let (_, s) = wait(7);
    let text = s.to_string();
    assert!(
        text.contains("model `slow`") && !text.contains("+b"),
        "{text}"
    );
    send(call(8, "ops", json!({"doc": "quick"})));
    let (_, q) = wait(8);
    let text = q.to_string();
    assert!(text.contains("`b`"), "{text}");
    assert!(!text.contains("sdf"), "{text}");
    drop(send);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempfile_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("modeler-concurrent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    d
}
