<!--
  Tauri-Frontend: Status + Anleitung. Die primäre UX ist der globale
  Hotkey, das Window dient nur als Status- und Hilfe-Anzeige.

  In der Beta-Phase haben wir die Detection-/Reverse-Test-Werkzeuge
  bewusst rausgenommen — sie waren Dev-Hilfsmittel, nicht End-User-
  Funktionalität. Bei Bedarf später im Settings-Bereich oder als
  separaten Diagnose-Modus wieder einbauen.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";

  type Settings = {
    hotkey: string;
    auto_detection: boolean;
    enable_ner: boolean;
    enable_notifications: boolean;
    retention_minutes: number;
    strict_mode: boolean;
  };
  type StorageStatus = { mapping_count: number; retention_minutes: number };
  type NerStatus = { built_with_ner_feature: boolean; enabled: boolean; ready: boolean };

  let settings: Settings = {
    hotkey: "CmdOrCtrl+Alt+B",
    auto_detection: false,
    enable_ner: false,
    enable_notifications: false,
    retention_minutes: 60,
    strict_mode: false
  };
  let storageStatus: StorageStatus = { mapping_count: 0, retention_minutes: 60 };
  let purgeStatus = "";

  const RETENTION_OPTIONS = [
    { value: 15, label: "15 Minuten — maximaler DSGVO-Schutz" },
    { value: 60, label: "1 Stunde — empfohlener Default" },
    { value: 480, label: "8 Stunden — Arbeitstag" },
    { value: 1440, label: "24 Stunden — bequem, weniger streng" },
    { value: 0, label: "Nur diese Session — Mappings nach App-Quit weg" }
  ];
  let nerStatus: NerStatus = { built_with_ner_feature: false, enabled: false, ready: false };

  async function loadAll() {
    try {
      settings = await invoke<Settings>("get_settings");
    } catch (e) {
      console.error("settings load failed", e);
    }
    try {
      nerStatus = await invoke<NerStatus>("get_ner_status");
    } catch (e) {
      console.error("ner status load failed", e);
    }
    try {
      appVersion = await invoke<string>("get_version");
    } catch (e) {
      console.error("version load failed", e);
    }
    try {
      storageStatus = await invoke<StorageStatus>("get_storage_status");
    } catch (e) {
      console.error("storage status load failed", e);
    }
  }

  async function changeRetention(newValue: number) {
    if (newValue === settings.retention_minutes) return;
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, retention_minutes: newValue }
      });
      settings = { ...settings, retention_minutes: newValue };
      storageStatus = { ...storageStatus, retention_minutes: newValue };
    } catch (e) {
      console.error("retention change failed", e);
    }
  }

  function onRetentionChange(e: Event) {
    const target = e.currentTarget as HTMLSelectElement;
    void changeRetention(Number(target.value));
  }

  async function changeMode(strict: boolean) {
    if (strict === settings.strict_mode) return;
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, strict_mode: strict }
      });
      settings = { ...settings, strict_mode: strict };
    } catch (e) {
      console.error("mode change failed", e);
    }
  }

  async function purgeAllMappings() {
    purgeStatus = "…";
    try {
      const n = await invoke<number>("clear_all_mappings");
      purgeStatus = `${n} Mapping(s) gelöscht. Tokens beim LLM sind ab jetzt anonyme Daten.`;
      storageStatus = await invoke<StorageStatus>("get_storage_status");
    } catch (e) {
      purgeStatus = `Fehler: ${e}`;
    }
  }

  // Drei mögliche Anzeige-Zustände für den NER-Layer. Reiner UI-Helper,
  // keine Logik — der Status kommt vom Backend.
  function nerStatusBadge(s: NerStatus): { label: string; cls: string } {
    if (!s.built_with_ner_feature) {
      return { label: "Erweiterte Erkennung: nicht im Build", cls: "off" };
    }
    if (!s.enabled) {
      return { label: "Erweiterte Erkennung: deaktiviert (Tray-Menü)", cls: "off" };
    }
    if (!s.ready) {
      return { label: "Erweiterte Erkennung: Modell nicht geladen", cls: "warn" };
    }
    return { label: "Erweiterte Erkennung: aktiv", cls: "on" };
  }

  let logCopyStatus = "";
  let appVersion = "?";
  let hotkeyChangeStatus = "";

  // Vordefinierte Hotkey-Optionen. Bewusst nur 2-3 kuratierte Werte
  // statt freier Eingabe — Tauri's `Shortcut::from_str` ist strikt mit
  // dem Format, freie Eingabe würde zu vielen User-Fehlern führen.
  const HOTKEY_OPTIONS = [
    {
      value: "CmdOrCtrl+Alt+B",
      label: "Strg + Alt + B  (Default, ergonomisch)",
      hint: "In Office und Browsern nicht belegt. B steht semantisch für anonyMize/Block."
    },
    {
      value: "CmdOrCtrl+Alt+Shift+V",
      label: "Strg + Alt + Shift + V  (maximale Konflikt-Freiheit)",
      hint: "Drei Modifier gleichzeitig, dafür garantiert nirgends belegt."
    }
  ];

  async function changeHotkey(newHotkey: string) {
    if (newHotkey === settings.hotkey) return;
    hotkeyChangeStatus = "…";
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, hotkey: newHotkey }
      });
      settings = { ...settings, hotkey: newHotkey };
      hotkeyChangeStatus = `Auf "${prettyHotkey(newHotkey)}" geändert — App neu starten, damit's greift.`;
    } catch (e) {
      hotkeyChangeStatus = `Fehler: ${e}`;
    }
  }

  async function toggleNotifications(e: Event) {
    const checked = (e.currentTarget as HTMLInputElement).checked;
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, enable_notifications: checked }
      });
      settings = { ...settings, enable_notifications: checked };
    } catch (err) {
      console.error("toggle notifications failed", err);
    }
  }

  async function copyLog() {
    logCopyStatus = "…";
    try {
      const n = await invoke<number>("copy_log_to_clipboard");
      logCopyStatus = `${n} Zeilen kopiert — jetzt in Mail einfügen`;
    } catch (e) {
      logCopyStatus = `Fehler: ${e}`;
    }
  }

  async function openLogFolder() {
    try {
      await invoke("open_log_folder");
    } catch (e) {
      console.error("open_log_folder failed", e);
    }
  }

  function prettyHotkey(s: string): string {
    // "CmdOrCtrl+B" → "Strg + B" auf Win, "⌘ + B" auf Mac (best effort
    // ohne UA-Sniffing).
    const isMac = navigator.platform.toLowerCase().includes("mac");
    return s
      .replace(/CmdOrCtrl/g, isMac ? "⌘" : "Strg")
      .replace(/Ctrl/g, "Strg")
      .replace(/Alt/g, isMac ? "⌥" : "Alt")
      .replace(/Shift/g, isMac ? "⇧" : "Umschalt")
      .replace(/\+/g, " + ");
  }

  onMount(() => {
    loadAll();
    // Bei jedem erneuten Window-Öffnen den Status frisch ziehen — nach
    // Tray-Toggle hat sich evtl. enable_ner geändert.
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) loadAll();
    });
  });
</script>

<main>
  <header>
    <h1>Streichzeug <span class="badge">Beta v{appVersion}</span></h1>
    <p class="sub">
      Erkennt personenbezogene Daten in der Zwischenablage und ersetzt sie
      durch reversible Pseudonyme — bevor du sie an einen LLM-Chat schickst.
    </p>
  </header>

  <section class="card">
    <h2>So nutzt du die App</h2>
    <p class="usage">
      In jeder App: Text kopieren, dann <kbd>{prettyHotkey(settings.hotkey)}</kbd>
      statt <kbd>{prettyHotkey("CmdOrCtrl+V")}</kbd> drücken.
    </p>
    <ul class="bullets">
      <li>Enthält der Text PII → fügt die anonymisierte Version ein</li>
      <li>Enthält der Text Tokens aus einer LLM-Antwort → fügt die Originale wieder ein</li>
      <li>Sonst → fügt unverändert ein, wie ein normales <kbd>{prettyHotkey("CmdOrCtrl+V")}</kbd></li>
    </ul>
  </section>

  <section class="card mode-card">
    <h2>Verarbeitungsmodus</h2>
    <p class="usage">
      Welche Art von DSGVO-Schutz soll die App anwenden?
    </p>
    <label class="hotkey-opt">
      <input
        type="radio"
        name="mode"
        checked={!settings.strict_mode}
        on:change={() => changeMode(false)}
      />
      <span>
        <strong>Reversibel (Default)</strong>
        <br />
        <span class="hint-inline">
          PII werden durch Pseudonyme wie <code>«P_a4b»</code> ersetzt.
          Eine lokale Mapping-Tabelle ermöglicht Rück-Übersetzung der
          LLM-Antwort. <strong>Die Daten beim LLM sind weiterhin
          personenbezogen</strong> (Art. 4(5) DSGVO) — der LLM-Einsatz
          muss separat über AVV und Drittlandtransfer rechtmäßig sein.
        </span>
      </span>
    </label>
    <label class="hotkey-opt">
      <input
        type="radio"
        name="mode"
        checked={settings.strict_mode}
        on:change={() => changeMode(true)}
      />
      <span>
        <strong>Strict — echte Anonymisierung</strong>
        <br />
        <span class="hint-inline">
          PII werden durch lesbare Platzhalter wie <code>«Person A»</code>,
          <code>«Organisation B»</code> ersetzt. <strong>Keine
          Mapping-Tabelle</strong> — die Zuordnung existiert nirgends.
          Damit sind die Daten beim LLM <strong>anonym</strong>
          (ErwGr. 26 DSGVO), kein AVV-Bedarf. Trade-off: kein
          automatisches Reverse. Für Berufsgeheimnisträger und
          LLMs ohne AVV.
        </span>
      </span>
    </label>
    <p class="hint">
      Compliance-Details: siehe <code>COMPLIANCE.md</code> im Installations-Verzeichnis.
    </p>
  </section>

  <section class="card">
    <h2>Hotkey ändern</h2>
    <p class="usage">
      Falls der aktuelle Hotkey mit einem deiner Editoren konfliktiert,
      kannst du hier auf eine konfliktärmere Variante wechseln. Nach
      dem Wechsel ist ein App-Neustart nötig.
    </p>
    {#each HOTKEY_OPTIONS as opt}
      <label class="hotkey-opt">
        <input
          type="radio"
          name="hotkey"
          value={opt.value}
          checked={settings.hotkey === opt.value}
          on:change={() => changeHotkey(opt.value)}
        />
        <span>
          <strong>{opt.label}</strong>
          <br />
          <span class="hint-inline">{opt.hint}</span>
        </span>
      </label>
    {/each}
    {#if hotkeyChangeStatus}
      <p class="hint" style="margin-top: 8px;">{hotkeyChangeStatus}</p>
    {/if}
    <label class="opt-row">
      <input
        type="checkbox"
        checked={settings.enable_notifications}
        on:change={toggleNotifications}
      />
      <span>
        <strong>Toast-Benachrichtigungen anzeigen</strong>
        <br />
        <span class="hint-inline">
          Default aus, weil Toasts auf manchen Win-Setups den Window-Focus
          klauen und damit das automatische Einfügen blockieren. Nur
          aktivieren, wenn der Hotkey sonst zuverlässig funktioniert.
        </span>
      </span>
    </label>
  </section>

  <section class="card">
    <h2>Status</h2>
    <div class="status-row">
      <span class="status-badge on">
        Hotkey: {prettyHotkey(settings.hotkey)}
      </span>
      {#each [nerStatusBadge(nerStatus)] as b}
        <span class="status-badge {b.cls}">{b.label}</span>
      {/each}
      <span class="status-badge {settings.auto_detection ? 'on' : 'off'}">
        Auto-Detection: {settings.auto_detection ? "aktiv" : "aus"}
      </span>
    </div>
    <p class="hint">
      Einstellungen über das Tray-Icon (Systemleiste unten rechts).
      Toggle-Änderungen erfordern einen App-Neustart.
    </p>
  </section>

  <section class="card">
    <h2>Datenspeicherung (DSGVO)</h2>
    {#if settings.strict_mode}
      <p class="usage" style="background: #ecfdf5; padding: 8px 10px; border-radius: 6px; color: #065f46;">
        Strict Mode aktiv: <strong>keine Mappings werden gespeichert.</strong>
        Die folgenden Einstellungen wirken nur im reversiblen Modus.
      </p>
    {:else}
    <p class="usage">
      Solange die Token→Original-Zuordnung gespeichert ist, gelten die
      Tokens beim LLM-Anbieter weiterhin als personenbezogene Daten
      (Art. 4(5) DSGVO — die Zuordnung ist die „zusätzliche Information").
      Erst nach Löschung der Mappings werden sie zu anonymen Daten.
    </p>
    {/if}
    <p class="status-line">
      Aktuell gespeichert: <strong>{storageStatus.mapping_count}</strong>
      Mapping{storageStatus.mapping_count === 1 ? "" : "s"}
    </p>

    <label class="opt-row" style="flex-direction: column; gap: 6px;">
      <strong>Aufbewahrungsdauer</strong>
      <select
        value={settings.retention_minutes}
        on:change={onRetentionChange}
      >
        {#each RETENTION_OPTIONS as opt}
          <option value={opt.value}>{opt.label}</option>
        {/each}
      </select>
      <span class="hint-inline">
        Mappings älter als die gewählte Dauer werden alle 5 Minuten
        automatisch gelöscht. Bei „Nur diese Session" geschieht die
        Löschung nur beim App-Quit.
      </span>
    </label>

    <div class="actions" style="margin-top: 12px;">
      <button class="danger" on:click={purgeAllMappings}>
        Jetzt alle Mappings löschen
      </button>
    </div>
    {#if purgeStatus}
      <p class="hint" style="margin-top: 8px;">{purgeStatus}</p>
    {/if}
    <p class="hint">
      Achtung: nach Sofort-Löschung sind die zugehörigen Tokens in
      bereits gesendeten LLM-Anfragen nicht mehr in Klartext
      rückübersetzbar — das ist im DSGVO-Sinne genau richtig, aber
      praktisch ein Punkt-of-no-Return.
    </p>
  </section>

  <section class="card">
    <h2>Sicherheit</h2>
    <ul class="bullets">
      <li>Alle Verarbeitung läuft <strong>lokal auf deinem Gerät</strong> — keine Daten verlassen den Rechner.</li>
      <li>Originaltexte werden ausschließlich in einer lokalen SQLite-DB für das Reverse-Mapping gehalten.</li>
      <li>Die App stellt <strong>keine</strong> Outbound-Verbindungen her (keine Telemetrie, kein Update-Check).</li>
    </ul>
  </section>

  <section class="card">
    <h2>Bug melden</h2>
    <p class="usage">
      Wenn was nicht funktioniert: zwei Klicks und du hast einen
      verwertbaren Bug-Report.
    </p>
    <div class="actions">
      <button class="primary" on:click={copyLog}>
        Log in Zwischenablage kopieren
      </button>
      <button on:click={openLogFolder}>
        Log-Ordner öffnen
      </button>
    </div>
    {#if logCopyStatus}
      <p class="hint" style="margin-top: 8px;">{logCopyStatus}</p>
    {/if}
    <p class="hint">
      Dann ein <a href="https://github.com/dzieciol-dev/streichzeug/issues/new" target="_blank" rel="noopener">GitHub-Issue eröffnen</a>,
      Log einfügen, kurz beschreiben was nicht geklappt hat, abschicken.
    </p>
  </section>

  <footer>
    <p>
      Open Source unter MIT OR Apache-2.0. Bug-Reports und Feature-Requests:
      <a href="https://github.com/dzieciol-dev/streichzeug/issues" target="_blank" rel="noopener">github.com/dzieciol-dev/streichzeug/issues</a>.
    </p>
  </footer>
</main>

<style>
  main { max-width: 720px; margin: 0 auto; padding: 24px; }
  header h1 { margin: 0 0 4px; font-size: 22px; }
  .badge { font-size: 11px; padding: 2px 6px; background: #eff6ff; color: #1d4ed8; border: 1px solid #bfdbfe; border-radius: 4px; vertical-align: middle; margin-left: 6px; }
  .sub { color: #6b7280; margin: 0 0 24px; }
  .card { background: white; border: 1px solid #e5e7eb; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .card h2 { margin: 0 0 12px; font-size: 16px; }
  .usage { font-size: 15px; line-height: 1.5; }
  .bullets { margin: 8px 0 12px 0; padding-left: 22px; color: #374151; line-height: 1.6; font-size: 13px; }
  .hint { color: #6b7280; font-size: 12px; margin: 8px 0 0; padding-top: 8px; border-top: 1px solid #f3f4f6; }
  kbd { background: #f3f4f6; border: 1px solid #d1d5db; padding: 1px 6px; border-radius: 3px; font-size: 12px; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  .mode-card { border-left: 3px solid #2563eb; }
  .opt-row { display: flex; gap: 10px; padding: 8px 10px; margin: 8px 0 0; border-radius: 6px; border: 1px solid #e5e7eb; cursor: pointer; align-items: flex-start; font-size: 13px; }
  .opt-row input { margin-top: 4px; }
  .hotkey-opt { display: flex; gap: 10px; padding: 8px 10px; margin: 4px 0; border-radius: 6px; border: 1px solid #e5e7eb; cursor: pointer; align-items: flex-start; font-size: 13px; }
  .hotkey-opt:hover { background: #f9fafb; }
  .hotkey-opt input { margin-top: 4px; }
  .hint-inline { color: #6b7280; font-size: 12px; }
  .actions { display: flex; gap: 8px; margin-top: 8px; }
  button { font: inherit; padding: 6px 12px; border-radius: 6px; border: 1px solid #d1d5db; background: #f9fafb; cursor: pointer; }
  button.primary { background: #2563eb; color: white; border-color: #2563eb; }
  button.danger { background: #fef2f2; color: #991b1b; border-color: #fecaca; }
  button:hover { background: #f3f4f6; }
  button.primary:hover { background: #1d4ed8; }
  button.danger:hover { background: #fee2e2; }
  select { font: inherit; padding: 6px 10px; border-radius: 6px; border: 1px solid #d1d5db; background: white; width: 100%; }
  .status-line { color: #374151; font-size: 14px; margin: 8px 0; }
  .status-row { display: flex; gap: 8px; flex-wrap: wrap; margin: 0 0 8px; }
  .status-badge { font-size: 11px; padding: 3px 8px; border-radius: 12px; font-weight: 500; }
  .status-badge.on { background: #ecfdf5; color: #065f46; border: 1px solid #a7f3d0; }
  .status-badge.off { background: #f3f4f6; color: #6b7280; border: 1px solid #e5e7eb; }
  .status-badge.warn { background: #fffbeb; color: #92400e; border: 1px solid #fde68a; }
  footer { text-align: center; color: #9ca3af; font-size: 12px; margin-top: 24px; padding-top: 12px; border-top: 1px solid #f3f4f6; }
  footer a { color: #6b7280; text-decoration: underline; }
</style>
