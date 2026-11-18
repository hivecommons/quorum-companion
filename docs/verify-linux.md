# Verify on Linux

Status of the Linux build: prototype, built in CI, never run on a real call. This is the exact human procedure that closes [hivecommons/hive#18661](https://github.com/hivecommons/hive/issues/18661). Record each result in the template at the end and paste it into the issue.

## Setup

- An x86_64 desktop running PipeWire with WirePlumber (not a bare PulseAudio session). Run the whole procedure on **two** machines or sessions: one with an older PipeWire (0.3.48, for example Ubuntu 22.04) and one with 1.0 or newer (for example Ubuntu 24.04, Fedora 40). Record `pipewire --version` and `wireplumber --version` for each.
- A Secret Service provider unlocked (GNOME Keyring, KWallet, or KeePassXC).
- A hive with the companion enabled, a companion token, and a second participant for the call.
- Get the builds: `QuorumCompanion-X.Y.Z-linux-x64.deb` and `...AppImage` from Releases (or the `release-linux` artifact of a release-workflow dry run). Check against `SHA256SUMS` (`sha256sum -c`).

## A. Install (do both formats, on each machine)

1. `.deb`: `sudo apt install ./QuorumCompanion-X.Y.Z-linux-x64.deb`. Confirm it pulled in `pipewire-bin` and `pulseaudio-utils` and that the app starts from the launcher.
2. AppImage: `chmod +x` it and run it. It needs `pw-dump`, `pw-record`, `parec` and `pactl` on the host (`command -v` each).
3. Settings: enter the hive URL, display name and token, save.

## B. Token in the keyring

1. Open Seahorse (or your keyring tool): an entry for service `dev.hivecommons.quorum-companion` exists. Do not paste the secret anywhere.
2. Reboot the machine, log in, start the app: the token is still there and no re-entry is asked.
3. Optional negative check: with no Secret Service running, saving the token shows a clear error rather than failing silently.

## C. Call capture (Zoom, Teams for Linux, or a browser Meet; at least two apps)

1. Start a call with the second participant, microphone unmuted.
2. The companion asks "Let Betty listen to this call?" and names the right app (the executable name, for example `zoom`, `teams-for-linux`, `chrome`).
3. Consent gate: pressing Let Betty listen without the box ticked captures nothing; with it ticked the red "Betty is listening" indicator shows.
4. Per-app output only: the other person speaks (call channel) and you speak (mic channel); both appear in the hive transcript. Play a video or music in a different app: it must NOT appear in the transcript.
5. Helper log: find the helper's log (run the app from a terminal, or check the app's log output) and note which backend it used: `pw-record` (per-node) or `parec` (pipewire-pulse sink-input fallback). Expect `parec` on 0.3.48 and `pw-record` on 1.0+, but record what you actually see. Capture must work on both.
6. Betty's consent notice and replies appear in the companion window and in the hive meeting panel.
7. Stop, then a second capture ended with "betty leave" in the call: both end capture and the indicator clears.

## Results template (paste into hive#18661)

```
Linux verification
Companion version / source: (release X.Y.Z | dry-run artifact, run id)
Distro / desktop / session (Wayland or X11):
pipewire --version / wireplumber --version:
.deb install: pass/fail   AppImage: pass/fail
Token in keyring, survives reboot: pass/fail   (provider: )
App 1 (name): prompt names right app pass/fail | consent gate pass/fail | call channel pass/fail | mic channel pass/fail | other-app audio excluded pass/fail | backend in helper log: pw-record / parec | Betty reply in window pass/fail | in hive panel pass/fail | Stop pass/fail | "betty leave" pass/fail
App 2 (name): (same fields)
Second machine (other PipeWire generation): (same block)
Notes / errors:
Verdict: verified on a real call / not verified
```
