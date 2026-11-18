nanoprobe for DCS World under Wine on Linux

This folder holds:

  nanoprobe           doctor, install, uninstall, inspect, top, watch, restore
  nanoprobe-sidecar   the sidecar: connects to the agent in DCS and serves the API
  install.sh          runs "nanoprobe install" with the arguments it is given
  uninstall.sh        runs "nanoprobe uninstall" with the arguments it is given
  deploy/             the sidecar's container image, a compose overlay and an
                      example sidecar.toml for it; start at deploy/README.md
  payload/            the agent that install puts in the DCS profile
  LICENSE

Both programs are static and need nothing installed.

Install

  1. Stop DCS. Keep this folder outside the DCS install and the Wine prefix.
  2. As the user who owns the DCS profile, run:
       ./install.sh --wine-prefix <the Wine prefix DCS runs in>
     It installs the agent into the DCS profile in the prefix's Saved Games,
     and nothing into the DCS install. With DCS in a container, run it on the
     host against the prefix folder the container mounts.
  3. Start DCS, let a mission start, and run:
       ./nanoprobe doctor --wine-prefix <prefix>
  4. Run the sidecar. For DCS in a Docker container follow deploy/README.md.
     Otherwise write a sidecar.toml and run:
       ./nanoprobe-sidecar run --config sidecar.toml
     The sidecar exits when DCS restarts. Run it under a service manager that
     starts it again. The container setup in deploy/ has a restart policy
     meant for that; that it brings the sidecar back after the DCS container
     restarts is not yet verified.

Uninstall

  Stop DCS and run ./uninstall.sh --wine-prefix <prefix>. It removes
  everything install added, and the whole "nanoprobe" folder in the profile:
  copy out any mission template you keep there first. The sidecar's files
  stay.

The full documents (install guide, first campaign, API) are in the project's
repository, in README.md and docs/. Run "./nanoprobe help" for every option.
