# Verify on Windows

Status of the Windows build: prototype, built in CI, never run on a real call. This is the exact human procedure that closes [hivecommons/hive#18660](https://github.com/hivecommons/hive/issues/18660). Do every step; record each result in the template at the end and paste it into the issue.

## Setup

- A Windows 10 version 2004 (build 19041) or later, or Windows 11, x64 machine. Record the exact build (`winver`).
- A hive with the companion feature enabled, a companion token created in its dashboard, and a second person (or a second device) to join the test call.
- Get the build: a release `QuorumCompanion-X.Y.Z-windows-x64.msi` and its `.zip` from Releases, or, before a release exists, the `release-windows` artifact of a dry-run of the release workflow. Check the file against `SHA256SUMS` (`certutil -hashfile <file> SHA256`).
- Settings, Privacy & security, Microphone: "Let desktop apps access your microphone" is On.

## A. Install

1. Run the `.msi`. SmartScreen warns once: More info, Run anyway. The app installs and starts.
2. Also test the portable `.zip` once: unzip to a new folder, confirm `Quorum Companion.exe` and `quorum-capture.exe` sit together, run it.
3. In Settings enter the hive URL, your display name and the token, and save.
4. Open Credential Manager, Windows Credentials, Generic Credentials: an entry under `dev.hivecommons.quorum-companion` exists. Do not copy the secret into the issue. Restart the app: it still has the token (no re-entry).

## B. Zoom or new Teams desktop app (repeat per app)

1. Start a call with the second participant and unmute the microphone.
2. Within a few seconds the companion asks "Let Betty listen to this call?". Check that the prompt names the right app (`zoom.exe` or `ms-teams.exe`), not another running app.
3. Press Let Betty listen without ticking the consent box: nothing must be captured. Then tick the box and press Let Betty listen: the red "Betty is listening" indicator appears.
4. Both channels: the other person speaks (call channel) and you speak (mic channel). Both appear in the hive transcript as the right speaker. Play a sound in another app (a browser video): it must NOT appear in the transcript.
5. Betty's consent notice and replies appear in the companion window and in the hive meeting panel. Ask Betty a question and confirm the reply shows in both.
6. Stop: press Stop, confirm the indicator goes away and no more audio reaches the hive. Start again, then say "betty leave" in the call: capture ends the same way.
7. Leave the call: the prompt does not reappear.

## C. Browser Google Meet (Chrome and Edge)

1. Join a Meet in the browser with the microphone on.
2. The prompt names the browser executable (`chrome.exe` or `msedge.exe`). Accept and run steps B3 to B6.
3. Browsers use an audio helper process: confirm the call audio is still captured (process-tree capture), and a second browser window playing unrelated audio is not transcribed if it is a separate instance.

## D. Failure notes

- If capture fails, note the exact on-screen error and whether `quorum-capture.exe` is running in Task Manager.
- Windows 10 below 2004 must show a clear unsupported message instead of a silent failure (optional check on an old build).

## Results template (paste into hive#18660)

```
Windows verification
Companion version / source: (msi release X.Y.Z | dry-run artifact, run id)
Windows build (winver):
Hive URL host (no token):
Install .msi: pass/fail   portable .zip: pass/fail
Token in Credential Manager (dev.hivecommons.quorum-companion), survives restart: pass/fail
Zoom:  prompt names zoom.exe pass/fail | consent gate pass/fail | call channel pass/fail | mic channel pass/fail | other-app audio excluded pass/fail | Betty reply in window pass/fail | in hive panel pass/fail | Stop pass/fail | "betty leave" pass/fail
Teams: (same fields, ms-teams.exe)
Meet in Chrome: (same fields, chrome.exe)
Meet in Edge: (same fields, msedge.exe)
Notes / errors:
Verdict: verified on a real call / not verified
```
