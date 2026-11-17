# Contributing to Quorum Companion

Every commit must carry a Developer Certificate of Origin sign-off (`git commit -s`), certifying you have the right to submit the change under the project license. See https://developercertificate.org/.

Open an issue before large changes. Keep pull requests focused, with tests for new behaviour. PR titles start with an emoji (`✨` feature, `🐛` fix, `📖` docs, `🌱` chore).

- The companion streams to hive's companion API; its contract lives in hive `src/docs/meetings.md` ("Desktop companion") and quorum `docs/contract.md` ("Companion audio"). A change to it changes those docs first.
- Consent is not optional: nothing may be captured or sent before the person confirmed the participants were told, and the "Betty is listening" indicator stays visible for the whole capture.
- Capture modules are per OS (`capture/macos`, `capture/windows`, later `capture/linux`) and speak the same frame protocol on stdout (see `src-tauri/src/audio.rs`). They never touch the network.
- No platform's bot detection is ever bypassed: the companion only reads audio from the person's own meeting app on their own computer.

Ask in the [Hive Commons Discord](https://hivecommons.dev/discord) if in doubt.
