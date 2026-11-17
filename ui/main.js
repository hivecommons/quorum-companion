// The companion window. Everything that touches audio or the network runs in the Rust side;
// this file only shows state and asks for the person's decisions.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let info = null;
let pending = null; // { bundle_id, app, name }

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
  $("settings").hidden = Boolean(s.hive_url && s.has_token);
  $("settings-status").textContent = s.has_token ? "Token saved in the keychain." : "No token saved yet.";
  return s;
}

async function saveSettings() {
  const token = $("token").value;
  try {
    await invoke("save_settings", {
      hiveUrl: $("hive-url").value,
      hostName: $("host-name").value,
      offerOnCall: $("offer").checked,
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

async function start() {
  if (!pending || !$("attest").checked) return;
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
$("attest").addEventListener("change", () => ($("start").disabled = !$("attest").checked));
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
