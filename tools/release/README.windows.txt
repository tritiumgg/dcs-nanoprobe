nanoprobe for DCS World on Windows

This folder holds:

  nanoprobe.exe           doctor, install, uninstall, inspect, top, watch, restore
  nanoprobe-sidecar.exe   the sidecar: connects to the agent in DCS and serves the API
  install.cmd             runs "nanoprobe.exe install"
  uninstall.cmd           runs "nanoprobe.exe uninstall"
  sidecar.toml            example settings for the sidecar
  payload\                the agent that install puts in the DCS profile
  LICENSE

Install

  1. Quit DCS. Keep this folder outside the DCS install and Saved Games.
  2. Double-click install.cmd. It installs the agent into the DCS profile in
     Saved Games, and nothing into the DCS install. With several profiles,
     run: nanoprobe.exe install --profile "<profile folder>"
  3. Start DCS, start a mission, and run: nanoprobe.exe doctor
  4. Copy sidecar.toml to a folder of its own, replace its placeholders, and
     run: nanoprobe-sidecar.exe run --config sidecar.toml
     The sidecar exits when DCS restarts. Start it again, or run it as a
     service or a scheduled task that does.

Uninstall

  Quit DCS and double-click uninstall.cmd. It removes everything install
  added, and the whole "nanoprobe" folder in the profile: copy out any
  mission template you keep there first. The sidecar's folder stays.

The full documents (install guide, first campaign, API) are in the project's
repository, in README.md and docs\. Run "nanoprobe.exe help" for every option.
