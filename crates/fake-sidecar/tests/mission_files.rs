//! `SetMissionFile` and `ListMissionFiles` over gRPC: a file is read from a
//! listed folder and checked against its type, and the refusals name the
//! field (spec/persistence.md, "Mission files").

use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::errors::detail;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::{
    AcquireLeaseRequest, CreateCampaignRequest, Idempotency, ListMissionFilesRequest,
    MissionFileKind, SetMissionFileRequest, Write,
};
use tonic::Code;

/// A PNG of `w` by `h` pixels: the signature, `IHDR` with its CRC, `IEND`.
fn png(w: u32, h: u32) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = b"IHDR".to_vec();
    chunk.extend_from_slice(&w.to_be_bytes());
    chunk.extend_from_slice(&h.to_be_bytes());
    chunk.extend_from_slice(&[8, 2, 0, 0, 0]);
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(&chunk);
    b.extend_from_slice(&crc(&chunk).to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(b"IEND");
    b.extend_from_slice(&crc(b"IEND").to_be_bytes());
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_are_read_checked_listed_and_removed() {
    let dir = tempfile::tempdir().expect("dir");
    let listed = dir.path().join("files");
    std::fs::create_dir_all(&listed).expect("listed");
    let cfg = Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
        template_dirs: vec![listed.clone()],
        ..Config::default()
    };
    let (_hub, ch) = in_process(cfg, fake_config()).await.expect("server");
    let lease = SessionServiceClient::new(ch.clone())
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("lease")
        .into_inner()
        .lease
        .expect("lease");
    let id = CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(lease.clone()),
            name: "files".into(),
            theaters: vec!["Caucasus".into()],
            idempotency: Some(Idempotency {
                key: "create".into(),
            }),
            ..CreateCampaignRequest::default()
        })
        .await
        .expect("create")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    let mut m = MissionServiceClient::new(ch.clone());
    let path = |name: &str, bytes: &[u8]| {
        let p = listed.join(name);
        std::fs::write(&p, bytes).expect("write");
        p.to_string_lossy().into_owned()
    };
    let req = |k: &str, name: &str, kind: MissionFileKind, path: String| SetMissionFileRequest {
        write: Some(Write {
            lease: Some(lease.clone()),
            idempotency: Some(Idempotency { key: k.into() }),
            campaign_id: id,
        }),
        name: name.into(),
        kind: kind as i32,
        aircraft_type: String::new(),
        path,
    };
    let good = png(16, 16);
    let set = m
        .set_mission_file(req(
            "a",
            "map.png",
            MissionFileKind::Mission,
            path("map.png", &good),
        ))
        .await
        .expect("set")
        .into_inner()
        .file
        .expect("file");
    assert_eq!(set.size_bytes, good.len() as u64);
    assert_eq!(set.sha256.len(), 32);

    // Refusals, each naming its field.
    let big = listed.join("big.png");
    std::fs::write(&big, vec![0u8; 8 * 1024 * 1024 + 1]).expect("big");
    let outside = dir.path().join("outside.png");
    std::fs::write(&outside, &good).expect("outside");
    let cases = [
        (
            req(
                "b",
                "x.png",
                MissionFileKind::Mission,
                path("x.png", b"GIF89a"),
            ),
            "path",
            "not a PNG",
        ),
        (
            req(
                "c",
                "y.png",
                MissionFileKind::Mission,
                path("y.png", &png(9000, 1)),
            ),
            "path",
            "at most 8192",
        ),
        (
            req(
                "d",
                "big.png",
                MissionFileKind::Mission,
                big.to_string_lossy().into_owned(),
            ),
            "path",
            "at most",
        ),
        (
            req(
                "e",
                "o.png",
                MissionFileKind::Mission,
                outside.to_string_lossy().into_owned(),
            ),
            "path",
            "",
        ),
        (
            req("f", "a.mp3", MissionFileKind::Mission, path("a.mp3", b"x")),
            "name",
            "",
        ),
        (
            req(
                "g",
                "../a.png",
                MissionFileKind::Mission,
                path("z.png", &good),
            ),
            "name",
            "",
        ),
        (
            req(
                "h",
                "s.ogg",
                MissionFileKind::Kneeboard,
                path("s.ogg", b"OggS"),
            ),
            "name",
            "kneeboard",
        ),
        (
            req(
                "i",
                "MAP.png",
                MissionFileKind::Mission,
                path("MAP2.png", &good),
            ),
            "name",
            "only in case",
        ),
        (
            req(
                "j",
                "k.png",
                MissionFileKind::Unspecified,
                path("k.png", &good),
            ),
            "kind",
            "",
        ),
    ];
    for (r, field, text) in cases {
        let e = m.set_mission_file(r).await.expect_err(field);
        assert_eq!(e.code(), Code::InvalidArgument, "{field}: {}", e.message());
        let d = detail(&e).expect("detail");
        assert_eq!(d.field, field, "{}", e.message());
        assert!(e.message().contains(text), "{text}: {}", e.message());
    }

    let listed_files = m
        .list_mission_files(ListMissionFilesRequest { campaign_id: id })
        .await
        .expect("list")
        .into_inner()
        .files;
    assert_eq!(listed_files.len(), 1);
    assert_eq!(listed_files[0].name, "map.png");

    // No path removes it.
    let gone = m
        .set_mission_file(req("k", "map.png", MissionFileKind::Mission, String::new()))
        .await
        .expect("remove")
        .into_inner();
    assert!(gone.file.is_none());
    let left = m
        .list_mission_files(ListMissionFilesRequest { campaign_id: id })
        .await
        .expect("list")
        .into_inner()
        .files;
    assert!(left.is_empty());
}
