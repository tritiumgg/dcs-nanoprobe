nanoprobe fake sidecar for macOS

No DCS server runs on macOS, so this folder holds only the tool for writing a
controller without DCS:

  nanoprobe-fake-sidecar   the real API over a simulated DCS; a universal
                           program for Apple silicon and Intel Macs, macOS 11
                           or later
  README.md, docs/, spec/  the project's documents
  skills/nanoprobe/        the nanoprobe skill for AI coding tools; see
                           skills/nanoprobe/README.md
  LICENSE

The program is not notarized by Apple. macOS refuses to open a program
downloaded by a web browser until you allow it. Either:

  - download the archive with "gh release download" or "curl -LO", which
    macOS does not mark, and check it against SHA256SUMS; or
  - after a browser download, unpack it and run, in this folder:
      xattr -dr com.apple.quarantine .
    or, after macOS refuses it once, choose "Open Anyway" in System Settings,
    Privacy & Security.

Run

  ./nanoprobe-fake-sidecar
    serves the API on 127.0.0.1:47311 until Ctrl-C.
  ./nanoprobe-fake-sidecar --listen 127.0.0.1:0 --run <your controller>
    runs your controller against it, with NANOPROBE_ADDR set to its address,
    and exits with the controller's exit code.

Run "./nanoprobe-fake-sidecar --help" for every option. Start at
docs/controller-guide.md, "Test without DCS". The simulation follows what the
project has observed of DCS; a controller that works here can still behave
differently against a real server.
