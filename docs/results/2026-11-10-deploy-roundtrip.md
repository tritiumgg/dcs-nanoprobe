# Deploy round trip on a dedicated server

| | |
|---|---|
| Step | S1 exit test: deploy then undeploy leaves an identical tree |
| Date | 2026-11-10 |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Script | `cargo xtask roundtrip <target>` (`crates/xtask/src/main.rs`, `roundtrip`) |
| Build deployed | `29fa9effb7e6c8ff` |
| Result | PASS |

The script lists every path in the Saved Games profile with its kind and, for a file,
its size, reading no file's contents, and leaves out `Logs/`, which DCS writes while it
runs. It deploys, lists, deploys the same build again, lists, undeploys, and lists. It
passes when the first deploy writes the version and switches the manifest, the second
changes nothing, and the last listing equals the first.

```
build      29fa9effb7e6c8ff
entries    35 before, 49 installed, 35 after
result     PASS: deploy is idempotent and undeploy restores the tree
```

DCS was running and was not restarted, so the hook did not run. Run three times on the
same day with the same result. The local half of the exit test is the host test
`install::tests::deploy_then_undeploy_leaves_an_identical_tree`.
