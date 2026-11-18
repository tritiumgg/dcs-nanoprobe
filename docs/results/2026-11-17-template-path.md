# Campaign templates by path

`SetCampaignTemplate` names a `.miz` file by its path as the sidecar opens it. The
sidecar copies the file into the campaign's store when the call is made. Host only:
the fake sim on macOS, 2026-11-17. No DCS claim is made here.

## What the call does

| Step | Rule |
|---|---|
| Path | Absolute for the sidecar's system. Under Wine with a native sidecar it is the Linux path; the path DCS has for the file (`Z:\...`) is refused. |
| Folders | The path and each folder in `[missions] template_dirs` are resolved, symbolic links and `..` included, then compared. An empty list, the default, refuses every path. On Windows a share or a device no listed folder is on is refused before it is opened. |
| Read | Once, on a worker thread, without the hub's lock; at most 16 MiB, checked against the file's size and again while reading. |
| Checks | Free space, the `theatre` member, the members the shell writer changes, platform names. |
| Copy | The bytes go to the campaign's row of the `template` table in the call's store commit. |
| After | Activation, reload, backup, restore, export and a restarted sidecar use the row. Nothing opens the path again. |

A refusal leaves the campaign's template as it was. A refusal of the path or the file
is `INVALID_FIELD` with the field `path`; a theater that is not the campaign's is
`INVALID_FIELD` with `theater`; no room is `SPACE_LOW` (spec/errors.md).

## Decisions made in this step

- **The copy is a store row, with no temporary file.** The campaign's template is a
  row of the store's `template` table. The row is written in one SQLite transaction,
  so a failed or refused set changes nothing, which is what a temporary file and a
  rename would give a file. A copy kept as a file beside the store would have to be
  added to backup, restore and export, which carry the row as it is.
- **One answer for every path that is not a listed file.** A missing file, a folder, a
  path outside the listed folders and an empty list get the same text. A controller
  cannot use the call to learn what else the sidecar's machine holds. The sidecar's
  log has the cause.
- **The path refusal comes after the lease and the campaign are checked.** The file is
  read before the call takes the lock, and a failed read is reported inside the call.
  A repeat under an answered idempotency key gets its first answer although the file is
  gone.
- **`[missions] dir` is not a template folder unless it is listed.** The list is the
  only source of allowed folders.
- **A listed folder is resolved when a call is made, not at start.** A folder that is
  mounted later works without a restart; one that does not exist allows nothing.
- **CampaignService takes messages of the default size.** No call carries a file.

## The Buf exception

`SetCampaignTemplateRequest` field 3 `miz` is deleted, with its number and name
reserved, and field 4 `path` is added. `tools/protobreak.sh` compares the branch with
main and fails on exactly this deletion. No `ignore_only` is added.

```
comparing the schema against main
proto/nanoprobe/v1alpha1/campaign.proto:209:1:Previously present field "3" with name "miz" on message "SetCampaignTemplateRequest" was deleted.
```

## Runs

macOS (aarch64), the working tree of this commit, 2026-11-18.

| Command | Result |
|---|---|
| `mise run check` | Every step passes up to `tools/protobreak.sh`, which fails as under "The Buf exception": the Windows check of every crate, `cargo fmt`, both `cargo clippy` runs, `tools/tmpleak.sh` (the workspace tests: 1,154 passed, 0 failed, 5 ignored; nothing left in the temp folder), the virtualization test, both hello examples, `tools/luatest.sh`, `buf lint`. |
| `sh tools/protobreak-test.sh`, `cargo xtask proto --check`, `sh tools/stage-schema-test.sh`, `sh tools/secretscan-test.sh` | The steps `mise run check` lists after `tools/protobreak.sh`, run one by one: each passes. |
| `cargo test -p nanoprobe-api --lib -- template sidecar_toml full_file_system` | 16 passed. |
| `cargo test -p nanoprobe-cli restore` | 2 passed. |

## Tests

| Rule | Test |
|---|---|
| Listed folders, links, `..`, a Wine path, a folder, a FIFO, a file that cannot be opened, the size limit; on Windows a drive, a share and a device | `crates/api/src/template_file.rs`, `tests` |
| `template_dirs` in `sidecar.toml`: default, relative entries, unknown keys | `crates/api/src/sidecar_toml.rs`, `tests` |
| Set by path, each refusal changes nothing, a refused replacement keeps the copy, the repeat, the file deleted, activation, reload, backup, export, restart | `hub::tests::a_template_is_copied_from_its_path_and_the_file_is_not_read_again` |
| Free space, the set-time checks | `hub::tests::a_full_file_system_refuses_a_copy_and_a_template`, `hub::tests::a_campaign_template_is_checked_kept_and_written_into_its_shell` |
| Restore brings the template back | `crates/cli/src/restore.rs`, `refuses_a_held_store_dir_and_restores_a_free_one` |
| A missing per-map template is reported | `crates/cli/tests/doctor.rs`, `the_per_map_templates_are_checked` |

## Open

- The live shell part (`cargo xtask sidecar <name> --part shell --template FILE`) puts
  the file in `own/` in the mission folder and lists that folder. It builds; it has not
  run on a server since the change.
- A file replaced by a symbolic link between the folder check and the open is not
  caught. The listed folders are the administrator's.
- The Windows test (a drive, a share, a device) is compiled for
  `x86_64-pc-windows-msvc` on macOS and has not run on Windows.
- On Windows a path written `\\?\UNC\server\share\...` is refused when the listed folder
  is written `\\server\share\...`, and the reverse: the two forms are compared as
  written before anything is opened.
- `nanoprobe doctor` does not check the folders in `template_dirs`.
