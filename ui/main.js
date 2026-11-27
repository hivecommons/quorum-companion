// The companion window. Everything that touches audio or the network runs in the Rust side;
// this file only shows state and asks for the person's decisions.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let info = null;
let pending = null; // { bundle_id, app, name }
// Betty's listening chime: the samples come from the Rust side (tone.rs, the same chime as
// quorum's); this only plays them on the default output. Never into the call.
const INT16_FULL_SCALE = 32767;
let chime = null; // { ctx, buffer }

function show(section) {
  for (const id of ["idle", "prompt", "live"]) $(id).hidden = id !== section;
}

function status(text) {
  $("status").textContent = text || "";
}

async function loadSettings() {
  const s = await invoke("get_settings");
  $("hive-url").value = s.hive_url;
  $("host-name").value = s.host_name;
  $("offer").checked = s.offer_on_call;
  $("chime").checked = s.listening_chime;
  $("settings").hidden = Boolean(s.hive_url && s.has_token);
  $("settings-status").textContent = s.has_token ? "Token saved in your system's credential store." : "No token saved yet.";
  return s;
}

async function saveSettings() {
  const token = $("token").value;
  try {
    await invoke("save_settings", {
      hiveUrl: $("hive-url").value,
      hostName: $("host-name").value,
      offerOnCall: $("offer").checked,
      listeningChime: $("chime").checked,
      token: token ? token : null,
    });
    $("token").value = "";
    info = null;
    await loadSettings();
    status("Saved.");
  } catch (e) {
    status(String(e));
  }
}

async function refreshApps() {
  try {
    const apps = await invoke("list_apps");
    const sel = $("apps");
    sel.textContent = "";
    for (const a of apps) {
      const opt = document.createElement("option");
      opt.value = a.bundle_id;
      opt.dataset.app = a.app;
      opt.textContent = `${a.name}${a.in_call ? " (in a call)" : ""}`;
      sel.appendChild(opt);
    }
    $("manual-start").disabled = apps.length === 0;
  } catch (e) {
    status(String(e));
  }
}

async function ask(app) {
  try {
    info = info || (await invoke("companion_info"));
  } catch (e) {
    status(`Cannot reach your hive: ${e}`);
    $("settings").hidden = false;
    return;
  }
  pending = app;
  $("prompt-app").textContent = `${app.name || app.app} is in a call.`;
  $("consent-text").textContent = info.consent_text;
  $("attest").checked = false;
  $("start").disabled = true;
  show("prompt");
}

// Creates the audio context during a click (a webview may only start audio after one) and loads
// the chime once. A failure only costs the chime.
function prepareChime() {
  try {
    if (!chime) {
      const Ctx = window.AudioContext || window.webkitAudioContext;
      if (!Ctx) return;
      chime = { ctx: new Ctx(), buffer: null };
      invoke("listening_tone")
        .then((t) => {
          const buffer = chime.ctx.createBuffer(1, t.samples.length, t.sample_rate);
          const data = buffer.getChannelData(0);
          t.samples.forEach((s, i) => (data[i] = s / INT16_FULL_SCALE));
          chime.buffer = buffer;
        })
        .catch(() => {});
    }
    if (chime.ctx.state === "suspended") chime.ctx.resume().catch(() => {});
  } catch (e) {
    chime = null;
  }
}

function playChime() {
  if (!chime || !chime.buffer) return;
  try {
    const src = chime.ctx.createBufferSource();
    src.buffer = chime.buffer;
    src.connect(chime.ctx.destination);
    src.start();
  } catch (e) {
    // The chime is a courtesy; never let it disturb the window.
  }
}

let starting = false;

async function start() {
  if (starting || !pending || !$("attest").checked) return;
  starting = true;
  prepareChime();
  $("start").disabled = true;
  try {
    const s = await invoke("start_capture", {
      bundleId: pending.bundle_id,
      appName: pending.app,
      title: $("title").value,
      consentVersion: info.consent_version,
      consentAttested: true,
    });
    goLive(s);
  } catch (e) {
    status(String(e));
    $("start").disabled = false;
  } finally {
    starting = false;
  }
}

function goLive(session) {
  $("live-app").textContent = session.app;
  $("replies").textContent = "";
  status("");
  show("live");
}

async function copyNotice() {
  if (!info) return;
  try {
    await navigator.clipboard.writeText(info.consent_text);
    status("Notice copied. Paste it into the meeting chat.");
  } catch (e) {
    status("Copy failed; select the notice text instead.");
  }
}

function addReplies(lines) {
  const list = $("replies");
  for (const l of lines) {
    const li = document.createElement("li");
    li.textContent = l.text;
    list.appendChild(li);
  }
}

$("open-settings").addEventListener("click", () => ($("settings").hidden = !$("settings").hidden));
$("save-settings").addEventListener("click", saveSettings);
$("refresh-apps").addEventListener("click", refreshApps);
$("manual-start").addEventListener("click", () => {
  const opt = $("apps").selectedOptions[0];
  if (opt) ask({ bundle_id: opt.value, app: opt.dataset.app, name: opt.textContent });
});
$("attest").addEventListener(
  "change",
  () => ($("start").disabled = starting || !$("attest").checked),
);
$("start").addEventListener("click", start);
$("dismiss").addEventListener("click", () => {
  pending = null;
  show("idle");
});
$("stop").addEventListener("click", async () => {
  await invoke("stop_capture");
  show("idle");
  status("Stopped. Nothing more is sent to your hive.");
});
$("copy-notice").addEventListener("click", copyNotice);
$("copy-notice-live").addEventListener("click", copyNotice);

listen("call-started", (e) => {
  if (!$("live").hidden) return;
  ask(e.payload);
});
listen("call-ended", () => {
  if (!$("prompt").hidden) show("idle");
});
listen("replies", (e) => addReplies(e.payload));
listen("listening-tone", () => {
  prepareChime();
  playChime();
});
listen("meeting-ended", () => {
  show("idle");
  status("The meeting ended on your hive. Capture stopped.");
});
listen("session-stopped", () => show("idle"));
listen("capture-error", (e) => status(String(e.payload)));
listen("capture-warning", (e) => status(String(e.payload)));

(async () => {
  await loadSettings();
  await refreshApps();
  const s = await invoke("current_session");
  if (s) goLive(s);
})();
