//! Drives `drydock serve --stdio` against a generated fixture program.

use drydock_fixtures::PeBuilder;
use iced_x86::code_asm::*;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Client {
    fn start() -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_drydock"))
            .args(["serve", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            stdin,
            stdout,
            next: 1,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next;
        self.next += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({ "id": id, "method": method, "params": params })
        )
        .unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], id, "response order");
        match v.get("error") {
            Some(e) => Err(e.clone()),
            None => Ok(v["result"].clone()),
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params).unwrap_or_else(|e| panic!("{method}: {e}"))
    }
}

/// Counts to 300 with 1 ms sleeps, then exits with the count.
fn counter_pe() -> Vec<u8> {
    PeBuilder::new()
        .import("KERNEL32.DLL", &["Sleep", "ExitProcess"])
        .bss(16)
        .build(|a, l| {
            let mut top = a.create_label();
            a.set_label(&mut top)?;
            a.inc(dword_ptr(l.data))?;
            a.push(1)?;
            a.call(dword_ptr(l.import("KERNEL32.DLL", "Sleep")))?;
            a.cmp(dword_ptr(l.data), 300)?;
            a.jb(top)?;
            a.push(dword_ptr(l.data))?;
            a.call(dword_ptr(l.import("KERNEL32.DLL", "ExitProcess")))?;
            a.int3()
        })
}

#[test]
fn protocol_session_lifecycle() {
    let dir = std::env::temp_dir().join(format!("drydock-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("COUNT.EXE"), counter_pe()).unwrap();
    let data = format!("{:#x}", drydock_fixtures::IMAGE_BASE + 0x2000);

    let mut c = Client::start();
    let hello = c.ok("hello", json!({}));
    assert_eq!(hello["protocol"], 1);
    assert!(c.call("run", json!({ "seconds": 1 })).is_err(), "run before open fails");

    c.ok(
        "open",
        json!({ "exe": "COUNT.EXE", "root": dir, "artifacts": dir.join("artifacts") }),
    );
    let r = c.ok("run", json!({ "seconds": 0.5 }));
    assert_eq!(r["stop"], "TimeReached");
    let count = |c: &mut Client| {
        let m = c.ok("memory.read", json!({ "addr": data, "len": 4 }));
        u32::from_str_radix(&m["hex"].as_str().unwrap()[..2], 16).unwrap()
    };
    let at_save = count(&mut c);
    assert!(at_save > 10);
    c.ok("savepoint.create", json!({ "name": "a" }));
    c.ok("run", json!({ "seconds": 0.5 }));
    let later = count(&mut c);
    assert!(later > at_save);
    c.ok("savepoint.restore", json!({ "name": "a" }));
    assert_eq!(count(&mut c), at_save);
    c.ok("run", json!({ "seconds": 0.5 }));
    assert_eq!(count(&mut c), later, "execution after restore is reproducible");

    // Debugger mutation: set the counter close to the end.
    c.ok("memory.write", json!({ "addr": data, "hex": "2a010000" }));
    let regs = c.ok("regs.get", json!({}));
    assert!(regs["regs"]["eip"].as_u64().unwrap() > 0);
    let st = c.ok("state", json!({}));
    assert!(!st["threads"].as_array().unwrap().is_empty());
    let r = c.ok("run", json!({ "seconds": 30 }));
    assert_eq!(r["stop"], json!({ "Exited": { "code": 300 } }));
    let rep = c.ok("report", json!({}));
    assert_eq!(rep["provenance"]["debugger_mutations"].as_array().unwrap().len(), 1);

    assert!(c.call("nonsense", json!({})).is_err());
    c.ok("shutdown", json!({}));
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
