# Release archives on the host

| | |
|---|---|
| Step | S22, host part: the release archives staged and their contents tested; a clean install rehearsed in a made-up profile; the agent build compared with and without the path remapping |
| Date | 2026-11-18 |
| DCS | None. Host only. |
| Platform | macOS 26.6.2 on arm64; Rust 1.99.1, cargo-xwin 0.23.1, zig 0.15.2 |
| Script | `mise run package` (`tools/stage-release.sh`, `tools/stage-release-test.sh`); the comparison's commands are below |
| Code | The commit that adds this document |
| Result | PASS, with the three limits under "Not shown" |

## The archives

`mise run package` cross-builds the agent DLL, the two Windows programs and the two
static Linux programs, stages the five release files and `SHA256SUMS` in `dist/`, and
runs the test. Every line of the test passed:

- each archive lists exactly the files spec/install.md names;
- `nanoprobe.exe` and `nanoprobe-sidecar.exe` are Windows executables; `nanoprobe` and
  `nanoprobe-sidecar` are ELF executables that name no dynamic loader;
- the texts and `deploy/` are the repository's, the Windows texts with CR LF;
- `install.sh` and `uninstall.sh` pass on their arguments, one with a space in it, and
  the program's exit code, shown with a stand-in program;
- both archives carry the same payload, and its DLL is the build `tools/checkdll.sh`
  checked;
- no file in an archive names the build machine's home folder or checkout;
- `SHA256SUMS` lists the five files and matches.

`cargo test -p nanoprobe-cli` parses the example `sidecar.toml` of each archive with the
sidecar's own loader, placeholders replaced.

## The clean-install rehearsal

The test unpacks the Linux archive and, with the host's build of `nanoprobe`, installs
its payload into a made-up Wine profile that already holds an `autoexec.cfg` and has a
sidecar's folder beside it. It runs `doctor`, then uninstalls. Afterwards every path
under the prefix has the mode, size and checksum it had before, and `autoexec.cfg` holds
its one original line.

The hello campaigns run on the host in `mise run check`: the Python and the Kotlin hello
against the fake sidecar, and the Rust and the Python hello through the real sidecar
against the agent under stock Lua with the mock DCS (`lua/tests/sidecar.lua`, run by
`tools/luatest.sh`).

## What the path remapping changes in the agent

`tools/remap.sh` gives rustc `--remap-path-prefix` for the checkout, cargo's registry
and the toolchain, and tells the linker to name the PDB by its file name. To see what
that changes, the agent was built twice from one tree, without and with the flags, each
time emitting assembly. `<dir>` was empty before the first build and was kept for the
second, in which cargo compiled every crate for the target again because the flags had
changed:

```sh
T=x86_64-pc-windows-msvc
cargo xwin rustc -p nanoprobe-agent --release --target $T --target-dir <dir> -- --emit=asm
cp <dir>/$T/release/deps/nanoprobe.s plain.s
touch crates/agent/src/lib.rs
cargo xwin rustc -p nanoprobe-agent --release --target $T --target-dir <dir> \
    --config "$(sh tools/remap.sh $T)" -- --emit=asm
cp <dir>/$T/release/deps/nanoprobe.s remap.s
diff plain.s remap.s
```

The agent crate's assembly is 375,897 lines without the flags and 375,890 with them.
The lines that differ:

| Directive | Lines | What they are |
|---|---|---|
| `.cv_file` | 144 | the debug information's table of source files |
| `.asciz` | 18 | 7 source paths that panic locations hold, and the 11 location records that point at them, whose first byte is the path's length |
| `.long`, `.short`, `.byte`, `.asciz` | 11 without the flags, 4 with them | debug type records: the record that held the build's working folder is gone, and the type numbers after it shift by one |
| `.cv_inline_site_id` | 1 | a debug record, differing in white space only |

No instruction line differs. The DLL is 1,110,016 bytes without the flags and
1,107,968 with them. The flags change the agent's build ID, since the DLL's bytes
change, the source paths in its panic messages and crash records, and its debug
information. They do not change its code.

`cargo xtask deploy` builds the agent into the same target folder without these flags.
A DLL it deploys has the build machine's paths and another build ID than the staged one,
and after it runs, `mise run dll` builds the staged DLL again.

The comparison covers the agent crate. The crates it links, the standard library among
them, were built with the same flags and their assembly was not compared.

## Not shown

- The staged Linux programs did not run: the host is macOS. The rehearsal used the
  host's build of the same source. The acceptance run on the Wine server is their first
  run.
- `nanoprobe-sidecar.exe` links and is a Windows executable. It has not run. The
  acceptance run on Windows is its first run.
- The PDB names the build machine's folders on 81 lines: the linker writes the absolute
  path of each object file and library it read, and of the libraries on its command
  line. The test prints a note for it.
