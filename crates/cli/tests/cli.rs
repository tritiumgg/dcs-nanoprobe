//! Runs the built binary on a real mapped file.

use std::process::Command;
use std::sync::atomic::Ordering;

use nanoprobe_transport::layout::{kind, status as s};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::Lifecycle;
use nanoprobe_transport::ring::{Producer, Ring};
use nanoprobe_transport::status::{Header, start};

const CAP: u64 = 256 << 10;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nanoprobe"))
}

#[test]
fn the_binary_inspects_and_tops_a_real_file() {
    let header = Header {
        interface: 1,
        agent_boot: 0x55,
        build: 1,
        created_ms: 1,
        clock_hz: 1_000_000_000,
        out_capacity: CAP,
        in_capacity: CAP,
    };
    let path = std::env::temp_dir().join(format!("nanoprobe-cli-test-{}.map", std::process::id()));
    let map = MappedFile::create(&path, header.file_size()).expect("create");
    start(&map, &header, 3);
    let mut out = Producer::new(Ring::outbound(CAP), 0);
    let life = Lifecycle {
        state: 3,
        mission_gen: 1,
        theater: "Caucasus".into(),
        mission: "m".into(),
    };
    out.push(&map, kind::LIFECYCLE as u16, &[&life.encode()])
        .expect("push");
    out.publish(&map);
    map.store(s::FRAME, 7, Ordering::Release);

    let inspect = bin().arg("inspect").arg(&path).output().expect("run");
    let text = String::from_utf8_lossy(&inspect.stdout);
    assert_eq!(inspect.status.code(), Some(0), "{text}");
    assert!(text.contains("header check: ok"), "{text}");
    assert!(text.contains("LIFECYCLE"), "{text}");
    assert!(text.contains("theater=\"Caucasus\""), "{text}");

    let top = bin()
        .arg("top")
        .arg(&path)
        .arg("--once")
        .output()
        .expect("run");
    let text = String::from_utf8_lossy(&top.stdout);
    assert_eq!(top.status.code(), Some(0), "{text}");
    assert!(text.contains("frame            7"), "{text}");
    assert!(!text.contains('\x1b'), "--once does not clear the screen");

    let missing = bin()
        .arg("inspect")
        .arg(path.with_extension("none"))
        .output()
        .expect("run");
    assert_eq!(missing.status.code(), Some(1));
    let usage = bin().arg("top").output().expect("run");
    assert_eq!(usage.status.code(), Some(2));

    drop(map);
    let _ = std::fs::remove_file(&path);
}
