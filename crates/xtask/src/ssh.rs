//! Running commands on a target over SSH.
//!
//! BatchMode keeps SSH from asking for a password: keys come from the user's
//! own agent and config. xtask stores no credential and asks for none.

use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The program every call runs.
const PROGRAM: &str = "ssh";

/// The socket of the connection that the calls to one destination share.
/// The path stays short, because a Unix socket path is limited to about 100
/// bytes.
const CONTROL_PATH: &str = "ControlPath=/tmp/nanoprobe-ssh-%C";

/// How long a connection that a plain call opened stays up with no client.
const CONTROL_PERSIST: Duration = Duration::from_secs(60);

/// What a remote command left.
#[derive(Debug)]
pub struct Output {
    /// The exit code, or None when a signal ended it.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: Vec<u8>,
    /// Standard error, as text.
    pub stderr: String,
}

/// Run `remote` on `destination`, feeding `input` on stdin. SSH joins its
/// arguments into one string for the remote shell, so `remote` must already
/// be quoted. Exit code 255 is SSH's own failure, and is an error here.
pub fn run(destination: &str, remote: &str, input: &[u8]) -> Result<Output, String> {
    let mut child = plain(OsStr::new(PROGRAM))
        .args(["--", destination, remote])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ssh: {e}"))?;

    let mut stdin = child.stdin.take().expect("stdin is piped");
    // A command that exits without reading stdin closes the pipe; its exit
    // code says what happened, so a failed write is not an error by itself.
    let _ = stdin.write_all(input);
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|e| format!("ssh failed: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.code() == Some(255) {
        return Err(format!("ssh {destination} failed: {stderr}"));
    }
    Ok(Output {
        code: output.status.code(),
        stdout: output.stdout,
        stderr,
    })
}

/// `program` with the options of a plain call: one that runs a command, and
/// opens the shared connection when none is up.
fn plain(program: &OsStr) -> Command {
    let mut command = Command::new(program);
    command.args(["-o", "BatchMode=yes"]);
    // A dead connection fails within about a minute instead of hanging: the
    // connect times out after 15 s, and 4 unanswered keepalives 15 s apart
    // end the session.
    command.args([
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=4",
    ]);
    // One deploy makes dozens of calls. Sharing one connection for a minute
    // saves a handshake on each. Windows OpenSSH has no connection sharing.
    if cfg!(unix) {
        command.args([
            "-o",
            "ControlMaster=auto",
            "-o",
            CONTROL_PATH,
            "-o",
            &format!("ControlPersist={}", CONTROL_PERSIST.as_secs()),
        ]);
    }
    command
}

/// Run `command`, which reads nothing and whose output is not wanted, for at
/// most `limit`. Returns its exit code and what it wrote to stderr.
fn bounded(command: &mut Command, limit: Duration) -> Result<(Option<i32>, String), String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ssh: {e}"))?;
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("ssh failed: {e}"))? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("ssh did not finish within {} s", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The read ends at once: the command has exited, and a master process
    // that it left behind does not keep its stderr, as `run` relies on too.
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    Ok((status.code(), stderr.trim().to_owned()))
}

/// Run `script` on `destination` through `remote`, feeding the script on
/// stdin, and return its standard output. Any non-zero exit is an error.
pub fn script(destination: &str, remote: &str, script: &str) -> Result<String, String> {
    let output = run(destination, remote, script.as_bytes())?;
    if output.code != Some(0) {
        return Err(format!(
            "ssh {destination} failed ({:?}): {}",
            output.code, output.stderr
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A request to the master process of the shared connection (`ssh -O`). A
/// request talks to the control socket only: when no connection is up it
/// fails, and it never opens one.
#[derive(Clone, Copy, Debug)]
enum Control<'a> {
    /// Whether a connection is up.
    Check,
    /// Add the local forward `spec`. Adding one that is there succeeds.
    Forward(&'a str),
    /// Remove the local forward `spec`.
    Cancel(&'a str),
}

/// How long the master process gets to answer a request.
const CONTROL_LIMIT: Duration = Duration::from_secs(10);

fn control_args(request: Control, destination: &str) -> Vec<String> {
    let mut args = vec!["-o", "BatchMode=yes", "-o", CONTROL_PATH, "-O"];
    match request {
        Control::Check => args.push("check"),
        Control::Forward(spec) => args.extend(["forward", "-L", spec]),
        Control::Cancel(spec) => args.extend(["cancel", "-L", spec]),
    }
    args.extend(["--", destination]);
    args.into_iter().map(str::to_owned).collect()
}

/// Make `request` of the shared connection to `destination`. It fails
/// within [`CONTROL_LIMIT`] when the master process does not answer.
fn control(program: &OsStr, destination: &str, request: Control) -> Result<(), String> {
    let mut command = Command::new(program);
    command.args(control_args(request, destination));
    match bounded(&mut command, CONTROL_LIMIT)? {
        (Some(0), _) => Ok(()),
        (_, stderr) => Err(stderr),
    }
}

/// How long a plain call gets to open the shared connection: the connect
/// timeout, and as long again for the key agent to sign or refuse.
const OPEN_LIMIT: Duration = Duration::from_secs(30);

/// Open the shared connection to `destination` the way a plain call does,
/// with a command that does nothing.
fn open(program: &OsStr, destination: &str) -> Result<(), String> {
    let mut command = plain(program);
    command.args(["--", destination, "exit"]);
    // Exit code 255 is SSH's own failure, as in `run`.
    match bounded(&mut command, OPEN_LIMIT) {
        Ok((Some(255), stderr)) => Err(format!("ssh {destination} failed: {stderr}")),
        Ok(_) => Ok(()),
        Err(e) => Err(format!("ssh {destination} failed: {e}")),
    }
}

/// A loopback port here forwarded over SSH to `ip:port` on the destination's
/// side, for as long as the value lives.
///
/// Where SSH shares connections, the forward is a forward of the shared
/// connection: starting one opens no connection when one is up, so it needs
/// no new signature from the key agent. Two forwards cannot share a local
/// port: stop the one before starting the other. Stopping one waits for the
/// keeper's request in flight and for the cancel, each at most
/// [`CONTROL_LIMIT`].
///
/// A process that is killed does not remove its forward. The forward then
/// stays, unused, until the shared connection closes.
pub(crate) struct Forward(pub(crate) Option<Link>);

/// What carries a [`Forward`].
pub(crate) enum Link {
    /// A forward of the shared connection, and the thread that keeps both up.
    Shared {
        program: OsString,
        destination: String,
        spec: String,
        /// Dropping the sender ends the thread.
        stop: mpsc::Sender<()>,
        keeper: JoinHandle<()>,
    },
    /// An `ssh -N -L` process with a connection of its own, where SSH has no
    /// connection sharing.
    Own(Child),
}

/// The times a [`Forward`] works to.
#[derive(Clone, Copy)]
struct Pace {
    /// How long the forward gets to accept a connection.
    up: Duration,
    /// How often the keeper makes its request. The master process closes
    /// the connection once no client has talked to it for `ControlPersist`
    /// ([`CONTROL_PERSIST`] when a plain call opened it), and a forward is
    /// not a client. A request every 15 s is a client four times within
    /// that.
    keep: Duration,
}

const PACE: Pace = Pace {
    up: Duration::from_secs(20),
    keep: Duration::from_secs(15),
};

const _: () = assert!(PACE.keep.as_secs() * 4 <= CONTROL_PERSIST.as_secs());

impl Forward {
    pub(crate) fn start(dest: &str, local: u16, ip: &str, port: u16) -> Result<Forward, String> {
        Forward::start_with(OsStr::new(PROGRAM), PACE, dest, local, ip, port)
    }

    fn start_with(
        program: &OsStr,
        pace: Pace,
        dest: &str,
        local: u16,
        ip: &str,
        port: u16,
    ) -> Result<Forward, String> {
        let spec = format!("127.0.0.1:{local}:{ip}:{port}");
        let link = if cfg!(unix) {
            shared(program, pace, dest, spec)?
        } else {
            own(program, dest, &spec)?
        };
        let mut f = Forward(Some(link));
        let deadline = Instant::now() + pace.up;
        while std::net::TcpStream::connect(("127.0.0.1", local)).is_err() {
            if Instant::now() > deadline {
                f.stop();
                return Err("the SSH forward did not come up".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(f)
    }

    pub(crate) fn stop(&mut self) {
        match self.0.take() {
            Some(Link::Shared {
                program,
                destination,
                spec,
                stop,
                keeper,
            }) => {
                // The keeper ends first, so that it cannot add the forward
                // again after the cancel. The connection stays, and closes
                // by its own `ControlPersist` like after any plain call.
                drop(stop);
                let _ = keeper.join();
                let _ = control(&program, &destination, Control::Cancel(&spec));
            }
            Some(Link::Own(mut child)) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            None => {}
        }
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Add the forward `spec` to the shared connection to `dest`, and start its
/// keeper.
fn shared(program: &OsStr, pace: Pace, dest: &str, spec: String) -> Result<Link, String> {
    if control(program, dest, Control::Check).is_err() {
        // No connection is up. A plain call opens one, as it does for any
        // other command. This is the only place a forward opens a
        // connection, and so the only place it can need a signature.
        open(program, dest).map_err(|e| format!("the SSH forward did not come up: {e}"))?;
    }
    control(program, dest, Control::Forward(&spec))
        .map_err(|e| format!("the SSH forward did not come up: {e}"))?;
    let (stop, stopped) = mpsc::channel::<()>();
    let keeper = {
        let (program, dest, spec) = (program.to_owned(), dest.to_owned(), spec.clone());
        std::thread::spawn(move || keep(&program, &dest, &spec, pace.keep, &stopped))
    };
    Ok(Link::Shared {
        program: program.to_owned(),
        destination: dest.to_owned(),
        spec,
        stop,
        keeper,
    })
}

/// Keep the shared connection and the forward up until `stopped` closes, by
/// asking for the forward again every `every`. The request is a client of
/// the master process, which holds the connection open through any pause in
/// plain calls, for a run of any length. It also puts the forward back when
/// a plain call had to open a new connection. It never opens one itself.
fn keep(program: &OsStr, dest: &str, spec: &str, every: Duration, stopped: &mpsc::Receiver<()>) {
    let mut up = true;
    while stopped.recv_timeout(every) == Err(mpsc::RecvTimeoutError::Timeout) {
        let asked = control(program, dest, Control::Forward(spec));
        match &asked {
            Ok(()) if !up => eprintln!("the SSH forward {spec} is back"),
            Err(e) if up => eprintln!("the SSH forward {spec} is down: {e}"),
            _ => {}
        }
        up = asked.is_ok();
    }
}

/// Start an `ssh -N -L` process for `spec`, with a connection of its own.
fn own(program: &OsStr, dest: &str, spec: &str) -> Result<Link, String> {
    Command::new(program)
        .args([
            "-N",
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
            spec,
            "--",
            dest,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Link::Own)
        .map_err(|e| format!("ssh -L: {e}"))
}

/// Quote `value` as one word for a POSIX shell.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_a_single_quote() {
        assert_eq!(quote("a'b c"), r"'a'\''b c'");
    }

    #[test]
    fn a_control_request_names_the_shared_socket_and_no_way_to_connect() {
        let line = |r| control_args(r, "<target>").join(" ");
        assert_eq!(
            line(Control::Check),
            "-o BatchMode=yes -o ControlPath=/tmp/nanoprobe-ssh-%C -O check -- <target>"
        );
        assert_eq!(
            line(Control::Forward("127.0.0.1:1:10.0.0.2:2")),
            "-o BatchMode=yes -o ControlPath=/tmp/nanoprobe-ssh-%C -O forward \
             -L 127.0.0.1:1:10.0.0.2:2 -- <target>"
        );
        assert_eq!(
            line(Control::Cancel("127.0.0.1:1:10.0.0.2:2")),
            "-o BatchMode=yes -o ControlPath=/tmp/nanoprobe-ssh-%C -O cancel \
             -L 127.0.0.1:1:10.0.0.2:2 -- <target>"
        );
    }

    /// A stand-in `ssh` in a folder of its own. It appends each call's
    /// arguments to `calls`. A connection is up while the file `master`
    /// exists: a control request fails without it, and a plain call makes
    /// it, unless the file `locked` exists. The file `refuse` makes the
    /// master refuse a forward. It forwards nothing: a test that needs the
    /// local port to answer listens on it itself.
    #[cfg(unix)]
    struct StandIn(std::path::PathBuf);

    #[cfg(unix)]
    impl StandIn {
        fn new(test: &str, master: bool) -> StandIn {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = std::env::temp_dir()
                .join(format!("nanoprobe-forward-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let program = dir.join("ssh");
            std::fs::write(
                &program,
                "#!/bin/sh\n\
                 [ \"$1\" != --warm ] || exit 0\n\
                 d=$(dirname \"$0\")\n\
                 echo \"$*\" >> \"$d/calls\"\n\
                 case \" $* \" in\n\
                 *' -O forward '*)\n\
                 [ -e \"$d/master\" ] || { echo 'no control socket' >&2; exit 255; }\n\
                 [ ! -e \"$d/refuse\" ] || { echo 'Port forwarding failed' >&2; exit 255; } ;;\n\
                 *' -O '*)\n\
                 [ -e \"$d/master\" ] || { echo 'no control socket' >&2; exit 255; } ;;\n\
                 *)\n\
                 [ ! -e \"$d/locked\" ] || { echo 'agent refused operation' >&2; exit 255; }\n\
                 : > \"$d/master\" ;;\n\
                 esac\n",
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            // A process that another test forked while the script was being
            // written holds it open for writing until it execs, and Linux
            // refuses to run a file that is open for writing. Once one run
            // starts, nothing holds it, and no later run is refused.
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match Command::new(&program).arg("--warm").status() {
                    Err(e)
                        if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    ran => {
                        assert!(ran.unwrap().success());
                        break;
                    }
                }
            }
            let stand_in = StandIn(dir);
            if master {
                stand_in.set("master");
            }
            stand_in
        }

        fn set(&self, flag: &str) {
            std::fs::write(self.0.join(flag), "").unwrap();
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.0.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// Start a forward of `local` with the stand-in.
        fn forward(&self, pace: Pace, local: u16) -> Result<Forward, String> {
            let program = self.0.join("ssh");
            Forward::start_with(program.as_os_str(), pace, "<target>", local, "10.0.0.2", 7)
        }
    }

    #[cfg(unix)]
    impl Drop for StandIn {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A keeper that stays out of a test's way.
    #[cfg(unix)]
    const QUIET: Pace = Pace {
        up: Duration::from_secs(5),
        keep: Duration::from_secs(3600),
    };

    /// A loopback port that answers, and its listener.
    #[cfg(unix)]
    fn listening() -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    /// A loopback port that refuses connections and that no other socket
    /// can take while the returned sockets live: the local port of a
    /// connected stream, which does not listen.
    #[cfg(unix)]
    fn silent() -> ((std::net::TcpListener, std::net::TcpStream), u16) {
        let (listener, port) = listening();
        let stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let port = stream.local_addr().unwrap().port();
        ((listener, stream), port)
    }

    #[cfg(unix)]
    fn request(op: &str, local: u16) -> String {
        let spec = format!("127.0.0.1:{local}:10.0.0.2:7");
        let request = match op {
            "forward" => Control::Forward(&spec),
            _ => Control::Cancel(&spec),
        };
        control_args(request, "<target>").join(" ")
    }

    #[cfg(unix)]
    #[test]
    fn a_forward_rides_the_connection_that_is_up_and_leaves_on_drop() {
        let ssh = StandIn::new("rides", true);
        let (_listener, local) = listening();
        let forward = ssh.forward(QUIET, local).unwrap();
        let check = control_args(Control::Check, "<target>").join(" ");
        assert_eq!(ssh.calls(), [check.clone(), request("forward", local)]);
        drop(forward);
        // Every call was a control request: none could open a connection.
        assert_eq!(
            ssh.calls(),
            [check, request("forward", local), request("cancel", local)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_forward_opens_a_connection_with_a_plain_call_when_none_is_up() {
        let ssh = StandIn::new("opens", false);
        let (_listener, local) = listening();
        let _forward = ssh.forward(QUIET, local).unwrap();
        let calls = ssh.calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[0].contains("-O check"));
        assert!(
            calls[1].contains("-o ControlMaster=auto -o ControlPath=/tmp/nanoprobe-ssh-%C")
                && calls[1].ends_with("-- <target> exit")
                && !calls[1].contains("-O"),
            "{calls:?}"
        );
        assert_eq!(calls[2], request("forward", local));
    }

    #[cfg(unix)]
    #[test]
    fn a_forward_fails_when_no_connection_can_be_opened() {
        let ssh = StandIn::new("locked", false);
        ssh.set("locked");
        let (_listener, local) = listening();
        let error = ssh.forward(QUIET, local).err().unwrap();
        assert_eq!(
            error,
            "the SSH forward did not come up: ssh <target> failed: agent refused operation"
        );
        // One attempt, and no request after it.
        assert_eq!(ssh.calls().len(), 2, "{:?}", ssh.calls());
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_forward_fails_with_the_master_s_reason() {
        let ssh = StandIn::new("refused", true);
        ssh.set("refuse");
        let (_listener, local) = listening();
        let error = ssh.forward(QUIET, local).err().unwrap();
        assert_eq!(
            error,
            "the SSH forward did not come up: Port forwarding failed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_forward_that_does_not_answer_fails_within_its_bound_and_is_removed() {
        let ssh = StandIn::new("silent", true);
        let (_held, local) = silent();
        let pace = Pace {
            up: Duration::from_millis(300),
            ..QUIET
        };
        let started = Instant::now();
        let error = ssh.forward(pace, local).err().unwrap();
        assert_eq!(error, "the SSH forward did not come up");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(ssh.calls().last().unwrap(), &request("cancel", local));
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_does_not_finish_is_stopped_at_its_limit() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        let error = bounded(&mut command, Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn the_keeper_asks_again_until_the_forward_is_dropped() {
        let ssh = StandIn::new("keeper", true);
        let (_listener, local) = listening();
        let pace = Pace {
            keep: Duration::from_millis(50),
            ..QUIET
        };
        let forward = ssh.forward(pace, local).unwrap();
        let asked = || {
            ssh.calls()
                .iter()
                .filter(|c| **c == request("forward", local))
                .count()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while asked() < 4 {
            assert!(
                Instant::now() < deadline,
                "the keeper asked {} times",
                asked()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(forward);
        let calls = ssh.calls();
        assert_eq!(calls.last().unwrap(), &request("cancel", local));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(ssh.calls(), calls, "the keeper asked after the drop");
    }

    #[cfg(unix)]
    #[test]
    fn two_forwards_live_together_and_one_starts_again_on_its_port() {
        let ssh = StandIn::new("two", true);
        let (_a, first) = listening();
        let (_b, second) = listening();
        let mut one = ssh.forward(QUIET, first).unwrap();
        let two = ssh.forward(QUIET, second).unwrap();
        // A sidecar restart: the old forward stops, then a new one starts on
        // the same port.
        one.stop();
        one = ssh.forward(QUIET, first).unwrap();
        drop(two);
        drop(one);
        let count = |line: String| ssh.calls().iter().filter(|c| **c == line).count();
        assert_eq!(count(request("forward", first)), 2);
        assert_eq!(count(request("cancel", first)), 2);
        assert_eq!(count(request("forward", second)), 1);
        assert_eq!(count(request("cancel", second)), 1);
    }

    /// The forward on a real destination. It needs two variables:
    /// `NANOPROBE_FORWARD_DEST`, an SSH destination, and
    /// `NANOPROBE_FORWARD_TO`, an `ip:port` on its side where a service
    /// sends first, such as an SSH server. `NANOPROBE_FORWARD_HOLD_S` is how
    /// long the forward is held with no plain call. The default is 15 s
    /// more than [`CONTROL_PERSIST`]: a connection that a plain call opened
    /// lasts that long only if the keeper holds it. Run:
    /// `cargo test -p xtask -- --ignored --nocapture a_forward_on_a_real`.
    #[cfg(unix)]
    #[test]
    #[ignore = "needs an SSH destination"]
    fn a_forward_on_a_real_destination_carries_a_connection() {
        let dest = std::env::var("NANOPROBE_FORWARD_DEST").expect("NANOPROBE_FORWARD_DEST");
        let to = std::env::var("NANOPROBE_FORWARD_TO").expect("NANOPROBE_FORWARD_TO");
        let (ip, port) = to.rsplit_once(':').expect("ip:port");
        let port: u16 = port.parse().expect("a port");
        let hold = std::env::var("NANOPROBE_FORWARD_HOLD_S")
            .map_or(CONTROL_PERSIST.as_secs() + 15, |s| {
                s.parse().expect("seconds")
            });
        let free = || listening().1;
        // The first bytes the service sends through the forward.
        let greeting = |local: u16| -> std::io::Result<Vec<u8>> {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", local))?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut bytes = [0; 64];
            let n = stream.read(&mut bytes)?;
            Ok(bytes[..n].to_vec())
        };
        let carries = |local: u16| greeting(local).is_ok_and(|bytes| !bytes.is_empty());
        let up = || control(OsStr::new(PROGRAM), &dest, Control::Check).is_ok();

        let (first, second) = (free(), free());
        let mut one = Forward::start(&dest, first, ip, port).unwrap();
        assert!(carries(first), "the forward carried nothing");
        let two = Forward::start(&dest, second, ip, port).unwrap();
        assert!(carries(second) && carries(first));
        drop(two);
        assert!(greeting(second).is_err(), "a dropped forward still listens");
        assert!(carries(first), "dropping one forward broke the other");

        println!("holding the forward for {hold} s");
        std::thread::sleep(Duration::from_secs(hold));
        assert!(up(), "the shared connection closed under the forward");
        assert!(carries(first), "the forward did not last");

        one.stop();
        assert!(greeting(first).is_err());
        one = Forward::start(&dest, first, ip, port).unwrap();
        assert!(carries(first), "the forward did not start again");
        drop(one);
        assert!(greeting(first).is_err());
        assert!(up(), "dropping the forward closed the shared connection");
    }
}
