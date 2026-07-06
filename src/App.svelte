<!--
  Tauri-Frontend: Status + Anleitung. Die primäre UX ist der globale
  Hotkey, das Window dient nur als Status- und Hilfe-Anzeige.

  In der Beta-Phase haben wir die Detection-/Reverse-Test-Werkzeuge
  bewusst rausgenommen — sie waren Dev-Hilfsmittel, nicht End-User-
  Funktionalität. Bei Bedarf später im Settings-Bereich oder als
  separaten Diagnose-Modus wieder einbauen.
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import Onboarding from "./Onboarding.svelte";
  import StageView from "./StageView.svelte";
  import StashView from "./StashView.svelte";
  import type { StageJob } from "./stage";
  import { prettyHotkey } from "./hotkey";

  type Settings = {
    hotkey: string;
    auto_detection: boolean;
    enable_ner: boolean;
    enable_notifications: boolean;
    retention_minutes: number;
    strict_mode: boolean;
    onboarded: boolean;
    // Schwärz-Bühne (Vertrag 2.1)
    stage_hotkey: string;
    // "full" ist der Legacy-Wert früher gespeicherter Settings (läuft als "normal").
    stage_animation: "slow" | "normal" | "fast" | "off" | "full";
    stash_clear_on_quit: boolean;
    show_widget: boolean;
    widget_position: [number, number] | null;
  };
  type StorageStatus = { mapping_count: number; retention_minutes: number };
  type NerStatus = {
    built_with_ner_feature: boolean;
    enabled: boolean;
    ready: boolean;
    model_files_present: boolean;
    user_models_dir: string | null;
  };

  let settings: Settings = {
    hotkey: "CmdOrCtrl+Alt+B",
    auto_detection: false,
    enable_ner: false,
    enable_notifications: false,
    retention_minutes: 60,
    strict_mode: false,
    onboarded: false,
    stage_hotkey: "CmdOrCtrl+Alt+Shift+B",
    stage_animation: "normal",
    stash_clear_on_quit: false,
    show_widget: false,
    widget_position: null
  };

  // Leichtes View-Konzept (kein Router): vier Tabs, die Bühne öffnet sich
  // zusätzlich event-getrieben. Tabs statt einer langen Karten-Liste —
  // mit Bühne, Ablage, Erkennung und allen Einstellungen war die eine
  // Seite zu voll geworden.
  type Tab = "status" | "stash" | "erkennung" | "einstellungen";
  let view: Tab | "stage" = "status";
  // View, zu der „Schließen" der Bühne zurückkehrt (die vor dem Event aktive).
  let prevView: Tab = "status";

  const TABS: { key: Tab; label: string }[] = [
    { key: "status", label: "Status" },
    { key: "stash", label: "Ablage" },
    { key: "erkennung", label: "Erkennung" },
    { key: "einstellungen", label: "Einstellungen" },
  ];

  let stageJob: StageJob | null = null;
  let stageUnlisten: UnlistenFn | null = null;

  function goTab(tab: Tab) { view = tab; }
  function goStash() { view = "stash"; }
  function closeStage() { view = prevView; }
  let storageStatus: StorageStatus = { mapping_count: 0, retention_minutes: 60 };
  let purgeStatus = "";
  let initialized = false;
  // Sichtbare Fehlermeldung, wenn eine Einstellung nicht gespeichert werden
  // konnte. Ohne das glaubt der Nutzer an einen Zustand, der nie persistiert
  // wurde. Wird von allen Settings-Handlern gesetzt und beim nächsten
  // erfolgreichen Speichern wieder geleert.
  let saveError = "";

  // Onboarding-Done-Handler: nochmal alle Settings laden, dann Wizard ausblenden
  async function onOnboardingDone() {
    await loadAll();
    settings = { ...settings, onboarded: true };
  }

  const RETENTION_OPTIONS = [
    { value: 15, label: "15 Minuten — maximaler DSGVO-Schutz" },
    { value: 60, label: "1 Stunde — empfohlener Default" },
    { value: 480, label: "8 Stunden — Arbeitstag" },
    { value: 1440, label: "24 Stunden — bequem, weniger streng" },
    { value: 0, label: "Nur diese Session — Mappings nach App-Quit weg" }
  ];
  let nerStatus: NerStatus = {
    built_with_ner_feature: false,
    enabled: false,
    ready: false,
    model_files_present: false,
    user_models_dir: null
  };

  // ---------------------------------------------- Erweiterte Erkennung (NER)
  // An/Aus läuft komplett über den Erkennung-Tab: fehlt das Modell, wird es
  // beim Aktivieren zuerst geladen; set_ner_enabled persistiert, zieht das
  // Laufzeit-Gate nach und lädt die Engine sofort — kein Neustart.
  let nerBusy = false;
  let nerActionStatus = "";

  async function setNerEnabled(on: boolean) {
    nerBusy = true;
    nerActionStatus = on ? "Aktiviere …" : "Deaktiviere …";
    try {
      if (on && !nerStatus.model_files_present) {
        nerActionStatus = "Lade Modell + Runtime (~145 MB) — kann 1–3 Minuten dauern …";
        await invoke("download_ner_model");
        nerStatus = await invoke<NerStatus>("get_ner_status");
      }
      nerActionStatus = on ? "Lade Engine …" : "Deaktiviere …";
      nerStatus = await invoke<NerStatus>("set_ner_enabled", { enabled: on });
      settings = { ...settings, enable_ner: on };
      saveError = "";
      if (!on) {
        nerActionStatus = "Deaktiviert — wirkt sofort.";
      } else if (nerStatus.ready) {
        nerActionStatus = "Aktiv — Modell geladen, wirkt sofort.";
      } else {
        nerActionStatus =
          "Aktiviert, aber die Engine konnte nicht geladen werden — Details im Log (Bug melden → Log kopieren).";
      }
    } catch (e) {
      nerActionStatus = `Fehler: ${e}`;
    } finally {
      nerBusy = false;
    }
  }

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
    // get_storage_status öffnet die verschlüsselte Mapping-DB und würde damit
    // den Schlüsselbund-Zugriff auslösen. Beim allerersten Start (noch nicht
    // onboarded) deshalb überspringen — der Wizard kündigt den Zugriff an und
    // stößt ihn per finalize_secret_setup an; danach lädt onOnboardingDone
    // den Status nach.
    if (settings.onboarded) {
      try {
        storageStatus = await invoke<StorageStatus>("get_storage_status");
      } catch (e) {
        console.error("storage status load failed", e);
      }
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
      saveError = "";
    } catch (e) {
      console.error("retention change failed", e);
      // Nicht gespeichert: Select-Auswahl auf den echten Wert zurücksetzen
      // (Reassign erzwingt das Re-Rendern) und Fehler sichtbar machen.
      settings = { ...settings };
      saveError = `Aufbewahrungsdauer konnte nicht gespeichert werden: ${e}`;
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
      saveError = "";
    } catch (e) {
      console.error("mode change failed", e);
      // Radio-Auswahl auf den echten Wert zurücksetzen und Fehler zeigen.
      settings = { ...settings };
      saveError = `Verarbeitungsmodus konnte nicht gespeichert werden: ${e}`;
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
      saveError = "";
    } catch (err) {
      console.error("toggle notifications failed", err);
      // Checkbox auf den echten Wert zurücksetzen und Fehler zeigen.
      settings = { ...settings };
      saveError = `Benachrichtigungs-Einstellung konnte nicht gespeichert werden: ${err}`;
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

  // --- Schwärz-Bühne: Einstellungen ---

  // Kuratierte Capture-Hotkeys analog HOTKEY_OPTIONS. Default + eine
  // konfliktärmere Alternative; leerer String = Feature aus.
  const STAGE_HOTKEY_OPTIONS = [
    {
      value: "CmdOrCtrl+Alt+Shift+B",
      label: "Strg + Alt + Shift + B  (Default)",
      hint: "Dreifach-Modifier, in Office und Browsern nicht belegt."
    },
    {
      value: "CmdOrCtrl+Alt+G",
      label: "Strg + Alt + G  (Alternative)",
      hint: "Kürzer zu greifen, falls die Default-Kombi hakt."
    }
  ];

  type StageAnimation = "slow" | "normal" | "fast" | "off";
  const STAGE_ANIMATION_OPTIONS: { value: StageAnimation; label: string; hint: string }[] = [
    { value: "slow", label: "Langsam", hint: "Zum Zusehen und Vorführen — der Strich nimmt sich Zeit (≤ 3,5 s)." },
    { value: "normal", label: "Normal", hint: "Der Filzstift-Strich zieht gestaffelt über jede Fundstelle (≤ 2,2 s)." },
    { value: "fast", label: "Schnell", hint: "Gleiche Animation, gerafft (≤ 0,7 s)." },
    { value: "off", label: "Aus", hint: "Kein Effekt — sofort das Ergebnis." }
  ];

  let stageHotkeyChangeStatus = "";

  async function changeStageAnimation(value: StageAnimation) {
    if (value === settings.stage_animation) return;
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, stage_animation: value }
      });
      settings = { ...settings, stage_animation: value };
      saveError = "";
    } catch (e) {
      console.error("stage animation change failed", e);
      settings = { ...settings };
      saveError = `Animations-Stil konnte nicht gespeichert werden: ${e}`;
    }
  }

  async function changeStageHotkey(value: string) {
    if (value === settings.stage_hotkey) return;
    stageHotkeyChangeStatus = "…";
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, stage_hotkey: value }
      });
      settings = { ...settings, stage_hotkey: value };
      stageHotkeyChangeStatus = `Auf "${prettyHotkey(value)}" geändert — App neu starten, damit's greift.`;
    } catch (e) {
      stageHotkeyChangeStatus = `Fehler: ${e}`;
    }
  }

  async function toggleStashClearOnQuit(e: Event) {
    const checked = (e.currentTarget as HTMLInputElement).checked;
    try {
      await invoke("update_settings", {
        newSettings: { ...settings, stash_clear_on_quit: checked }
      });
      settings = { ...settings, stash_clear_on_quit: checked };
      saveError = "";
    } catch (err) {
      console.error("toggle stash_clear_on_quit failed", err);
      settings = { ...settings };
      saveError = `Ablage-Einstellung konnte nicht gespeichert werden: ${err}`;
    }
  }

  onMount(async () => {
    await loadAll();
    initialized = true;
    // Bei jedem erneuten Window-Öffnen den Status frisch ziehen — nach
    // Tray-Toggle hat sich evtl. enable_ner geändert.
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden) loadAll();
    });
    // Einziger Listener-Ort für die Bühne: Payload merken, Bühne öffnen.
    stageUnlisten = await listen<StageJob>("stage://job", (event) => {
      if (view !== "stage") prevView = view;
      stageJob = event.payload;
      view = "stage";
    });
  });

  onDestroy(() => {
    if (stageUnlisten) stageUnlisten();
  });

  // --- Klick- und Drop-Einstiege in die Bühne (ohne Hotkey) -----------
  // Button: schwärzt den aktuellen Clipboard-Inhalt (Dock-Klick → Button).
  // Drop: markierten Text aus einer beliebigen App ins Fenster ziehen —
  // der Text kommt im HTML5-Drop-Event mit, ganz ohne Kopieren. Dafür ist
  // `dragDropEnabled: false` in tauri.conf.json gesetzt (Tauri würde die
  // Drops sonst selbst abfangen und nie ans DOM durchreichen).
  let dragDepth = 0;

  async function stageFromClipboard() {
    try {
      await invoke("stage_clipboard");
    } catch (e) {
      console.error("stage_clipboard failed", e);
    }
  }

  // Widget-Toggle wirkt sofort (Fenster wird live gezeigt/versteckt und
  // die Wahl im Backend persistiert) — kein App-Neustart nötig. Genutzt
  // von der Settings-Checkbox UND dem Schnell-Toggle in der Karte oben.
  async function setWidgetVisible(visible: boolean) {
    try {
      await invoke("set_widget_visible", { visible });
      settings = { ...settings, show_widget: visible };
      saveError = "";
    } catch (err) {
      console.error("toggle widget failed", err);
      settings = { ...settings };
      saveError = `Widget-Einstellung konnte nicht übernommen werden: ${err}`;
    }
  }

  function toggleWidget(e: Event) {
    void setWidgetVisible((e.currentTarget as HTMLInputElement).checked);
  }

  // Akzeptiert Text-Markierungen (Stufe 1) und BILD-Dateien (Stufe 3).
  // Datei-Drops kommen über HTML5 `dataTransfer.files` — `dragDropEnabled`
  // ist am Main-Window aus, Tauri fängt hier nichts ab (Konzept WP-J).
  //
  // WICHTIG: `preventDefault` läuft in onDragOver/onDrop für JEDEN
  // Datei-Drag — ohne das führt die WebView beim Drop ihre Default-Aktion
  // aus und NAVIGIERT ZUR DATEI: eine gedroppte PDF ersetzt dann die
  // komplette App-UI ohne Weg zurück (Beta-Befund 2026-07-05). Die
  // Annahme-Entscheidung fällt erst im Drop-Handler; nicht schwärzbare
  // Dateien bekommen dort eine sichtbare Meldung statt einer Navigation.
  // Zusätzlich fängt ein globaler Guard in main.ts Drops außerhalb von
  // <main> ab (Onboarding-Wizard, Widget-Fenster).
  function isStageDrag(e: DragEvent): boolean {
    const dt = e.dataTransfer;
    if (!dt) return false;
    if (dt.types.includes("text/plain")) return true;
    return Array.from(dt.items ?? []).some(
      (i) => i.kind === "file" && (i.type === "" || i.type.startsWith("image/"))
    );
  }

  function isFileDrag(e: DragEvent): boolean {
    return !!e.dataTransfer?.types.includes("Files");
  }

  // Hinweis für nicht schwärzbare Drops (PDF, docx, …) — sichtbar statt
  // stillem Verwerfen, mit Selbst-Abbau.
  let dropHint = "";
  let dropHintTimer = 0;
  function showDropHint(message: string) {
    dropHint = message;
    window.clearTimeout(dropHintTimer);
    dropHintTimer = window.setTimeout(() => {
      dropHint = "";
    }, 8000);
  }

  function onDragEnter(e: DragEvent) {
    if (isStageDrag(e)) {
      dragDepth += 1;
    }
  }

  function onDragLeave() {
    dragDepth = Math.max(0, dragDepth - 1);
  }

  function onDragOver(e: DragEvent) {
    // Für schwärzbare Inhalte den Drop zulassen; für ALLE anderen
    // Datei-Drags die Default-Navigation der WebView unterbinden.
    if (isStageDrag(e) || isFileDrag(e)) {
      e.preventDefault();
    }
  }

  async function onDrop(e: DragEvent) {
    e.preventDefault();
    dragDepth = 0;

    // Bilddatei (Stufe 3): Bytes als Raw-Body an stage_image — die Bühne
    // übernimmt Texterkennung + Schwärzung.
    const files = Array.from(e.dataTransfer?.files ?? []);
    const file = files.find((f) => f.type.startsWith("image/"));
    if (file) {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer());
        await invoke("stage_image", bytes);
      } catch (err) {
        console.error("stage_image failed", err);
      }
      return;
    }
    if (files.length > 0) {
      // Datei-Drop, aber kein Bild dabei: ehrlich sagen statt still schlucken.
      showDropHint(
        "Diese Datei kann Streichzeug (noch) nicht schwärzen — aktuell werden " +
          "Bilder (PNG/JPEG) und markierter Text unterstützt. PDF-Unterstützung ist geplant."
      );
      return;
    }

    const text = e.dataTransfer?.getData("text/plain") ?? "";
    if (!text) return;
    try {
      await invoke("stage_text", { text });
    } catch (err) {
      console.error("stage_text failed", err);
    }
  }
</script>

{#if initialized && !settings.onboarded}
  <Onboarding initialSettings={settings} nerStatus={nerStatus} on:done={onOnboardingDone} />
{:else}
<!-- svelte-ignore a11y-no-static-element-interactions -->
<main
  on:dragenter={onDragEnter}
  on:dragleave={onDragLeave}
  on:dragover={onDragOver}
  on:drop={onDrop}
>
  {#if dragDepth > 0}
    <div class="drop-overlay" aria-hidden="true">
      <span>Loslassen zum Schwärzen</span>
    </div>
  {/if}
  <header>
    <h1>Streichzeug <span class="badge">Beta v{appVersion}</span></h1>
    <p class="sub">
      Erkennt personenbezogene Daten in der Zwischenablage und ersetzt sie
      durch reversible Pseudonyme — bevor du sie an einen LLM-Chat schickst.
    </p>
  </header>

  <nav class="view-switch" aria-label="Ansicht">
    {#each TABS as tab (tab.key)}
      <button class:active={view === tab.key} on:click={() => goTab(tab.key)}>{tab.label}</button>
    {/each}
  </nav>

  {#if saveError}
    <div class="save-error" role="alert">
      <strong>Nicht gespeichert.</strong> {saveError}
    </div>
  {/if}

  {#if dropHint}
    <div class="drop-hint" role="status">{dropHint}</div>
  {/if}

  {#if view === "stage"}
    {#if stageJob}
      {#key stageJob.job_id}
        <StageView
          job={stageJob}
          animation={settings.stage_animation}
          on:close={closeStage}
          on:showStash={goStash}
        />
      {/key}
    {/if}
  {:else if view === "stash"}
    <StashView stageHotkey={settings.stage_hotkey} />
  {:else if view === "erkennung"}

  <section class="card ner-card">
    <h2>Erweiterte Erkennung (lokales KI-Modell)</h2>
    <p class="usage">
      Zusätzlich zur eingebauten Regex- und Listen-Erkennung kann ein
      lokales NER-Modell Personen, Orte und Organisationen statistisch
      erkennen — auch ohne Anrede oder GmbH-Suffix. Läuft komplett offline;
      der einmalige Modell-Download (~145 MB) ist die einzige
      Netzverbindung, die diese Funktion je aufbaut.
    </p>

    {#if !nerStatus.built_with_ner_feature}
      <p class="warning-box">
        Dieser Build enthält das NER-Feature nicht (Slim-Build ohne
        <code>--features ner</code>) — die erweiterte Erkennung ist hier
        nicht verfügbar.
      </p>
    {:else}
      <div class="status-row">
        <span class="status-badge {settings.enable_ner ? 'on' : 'off'}">
          {settings.enable_ner ? "Aktiviert" : "Deaktiviert"}
        </span>
        <span class="status-badge {nerStatus.model_files_present ? 'on' : 'off'}">
          Modell: {nerStatus.model_files_present ? "vorhanden" : "nicht geladen"}
        </span>
        <span class="status-badge {nerStatus.ready ? 'on' : 'off'}">
          Engine: {nerStatus.ready ? "bereit" : "nicht geladen"}
        </span>
      </div>

      <div class="actions" style="margin-top: 12px;">
        {#if settings.enable_ner}
          <button on:click={() => setNerEnabled(false)} disabled={nerBusy}>
            Deaktivieren
          </button>
        {:else}
          <button class="primary" on:click={() => setNerEnabled(true)} disabled={nerBusy}>
            {nerStatus.model_files_present
              ? "Aktivieren"
              : "Aktivieren & Modell laden (~145 MB)"}
          </button>
        {/if}
      </div>
      {#if nerActionStatus}
        <p class="hint" style="margin-top: 8px;">{nerActionStatus}</p>
      {/if}
      <p class="hint">
        Wirkt sofort — kein Neustart nötig. Trade-off: ~300 ms einmalige
        Ladezeit und ~50 ms zusätzlich pro Schwärzung.
        {#if nerStatus.user_models_dir}
          Modell-Verzeichnis: <code>{nerStatus.user_models_dir}</code>
        {/if}
      </p>
    {/if}
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
          Personenbezogene Daten werden durch Pseudonyme wie
          <code>«P_a4b»</code> ersetzt. Eine lokale Mapping-Tabelle
          ermöglicht Rück-Übersetzung der LLM-Antwort. <strong>Die Daten
          beim LLM sind weiterhin personenbezogen</strong> (Art. 4(5) DSGVO)
          — der LLM-Einsatz muss separat über AVV und Drittlandtransfer
          rechtmäßig sein.
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
          Personenbezogene Daten werden durch lesbare Platzhalter wie
          <code>«Person A»</code>, <code>«Organisation B»</code> ersetzt.
          <strong>Keine Mapping-Tabelle</strong> — die Zuordnung existiert
          nirgends. Damit sind die Daten beim LLM <strong>anonym</strong>
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

  {:else if view === "einstellungen"}

  

  

  

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

  <section class="card stage-settings-card">
    <h2>Schwärz-Bühne</h2>
    <p class="usage">
      Zweiter Workflow: Text in einer beliebigen App markieren, Capture-Hotkey
      drücken — die Fundstellen werden sichtbar geschwärzt und landen als
      Eintrag in der <button class="link-btn" on:click={goStash}>Ablage</button>
      sowie in der Zwischenablage.
    </p>

    <div class="sub-block">
      <strong>Animations-Stil</strong>
      {#each STAGE_ANIMATION_OPTIONS as opt}
        <label class="hotkey-opt">
          <input
            type="radio"
            name="stage-animation"
            value={opt.value}
            checked={settings.stage_animation === opt.value ||
              (settings.stage_animation === "full" && opt.value === "normal")}
            on:change={() => changeStageAnimation(opt.value)}
          />
          <span>
            <strong>{opt.label}</strong>
            <br />
            <span class="hint-inline">{opt.hint}</span>
          </span>
        </label>
      {/each}
    </div>

    <div class="sub-block">
      <strong>Capture-Hotkey</strong>
      {#each STAGE_HOTKEY_OPTIONS as opt}
        <label class="hotkey-opt">
          <input
            type="radio"
            name="stage-hotkey"
            value={opt.value}
            checked={settings.stage_hotkey === opt.value}
            on:change={() => changeStageHotkey(opt.value)}
          />
          <span>
            <strong>{opt.label}</strong>
            <br />
            <span class="hint-inline">{opt.hint}</span>
          </span>
        </label>
      {/each}
      {#if stageHotkeyChangeStatus}
        <p class="hint" style="margin-top: 8px;">{stageHotkeyChangeStatus}</p>
      {:else}
        <p class="hint">Nach dem Wechsel ist ein App-Neustart nötig.</p>
      {/if}
    </div>

    <label class="opt-row">
      <input
        type="checkbox"
        checked={settings.stash_clear_on_quit}
        on:change={toggleStashClearOnQuit}
      />
      <span>
        <strong>Ablage beim Beenden leeren</strong>
        <br />
        <span class="hint-inline">
          Session-only-Ablage: alle geschwärzten Einträge werden beim
          App-Quit gelöscht.
        </span>
      </span>
    </label>

    <label class="opt-row">
      <input
        type="checkbox"
        checked={settings.show_widget}
        on:change={toggleWidget}
      />
      <span>
        <strong>Schwebendes Widget anzeigen</strong> <span class="hint-inline">(nur macOS)</span>
        <br />
        <span class="hint-inline">
          Ein kleiner Marker-Button, der über allen Fenstern schwebt und
          per Verschiebe-Griff frei platzierbar ist. Text in einer App
          markieren, Widget anklicken → die Markierung wird geschwärzt.
          Der Klick nimmt der App den Fokus nicht weg. Wirkt sofort, kein
          Neustart nötig.
        </span>
      </span>
    </label>
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


  {:else}

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
      Die erweiterte Erkennung verwaltest du im Tab „Erkennung", alles
      andere im Tab „Einstellungen". Auto-Detection lässt sich zusätzlich
      über das Tray-Icon umschalten (Neustart nötig).
    </p>
  </section>

  <section class="card stage-entry-card">
    <h2>Schwärzen — ohne Hotkey</h2>
    <p class="usage">
      Text irgendwo kopieren und hier klicken — oder markierten Text
      einfach <strong>in dieses Fenster ziehen</strong>.
    </p>
    <div class="actions">
      <button class="primary" on:click={stageFromClipboard}>
        Zwischenablage schwärzen
      </button>
      <button on:click={() => setWidgetVisible(!settings.show_widget)}>
        {settings.show_widget ? "Widget ausblenden" : "Widget einblenden"}
      </button>
    </div>
    <p class="hint">
      Das Ergebnis landet geschwärzt in der Zwischenablage und in der
      Ablage. Schneller geht's mit dem Hotkey
      <kbd>{prettyHotkey(settings.stage_hotkey)}</kbd> direkt auf einer
      Markierung — oder per Klick aufs schwebende Widget
      (Rechtsklick darauf: ausblenden).
    </p>
  </section>

  <section class="card">
    <h2>So nutzt du die App</h2>
    <p class="usage">
      In jeder App: Text kopieren, dann <kbd>{prettyHotkey(settings.hotkey)}</kbd>
      statt <kbd>{prettyHotkey("CmdOrCtrl+V")}</kbd> drücken.
    </p>
    <ul class="bullets">
      <li>Enthält der Text personenbezogene Daten → fügt die anonymisierte Version ein</li>
      <li>Enthält der Text Tokens aus einer LLM-Antwort → fügt die Originale wieder ein</li>
      <li>Sonst → fügt unverändert ein, wie ein normales <kbd>{prettyHotkey("CmdOrCtrl+V")}</kbd></li>
    </ul>
  </section>

  {/if}

  <footer>
    <p>
      Open Source unter MIT OR Apache-2.0. Bug-Reports und Feature-Requests:
      <a href="https://github.com/dzieciol-dev/streichzeug/issues" target="_blank" rel="noopener">github.com/dzieciol-dev/streichzeug/issues</a>.
    </p>
  </footer>
</main>
{/if}

<style>
  main { max-width: 720px; margin: 0 auto; padding: 24px; }
  header h1 { margin: 0 0 4px; font-size: 22px; }
  .badge { font-size: 11px; padding: 2px 6px; background: #eff6ff; color: #1d4ed8; border: 1px solid #bfdbfe; border-radius: 4px; vertical-align: middle; margin-left: 6px; }
  .sub { color: #6b7280; margin: 0 0 24px; }
  .view-switch { display: flex; gap: 6px; margin: 0 0 20px; }
  .view-switch button { font: inherit; padding: 6px 16px; border-radius: 6px; border: 1px solid #d1d5db; background: #f9fafb; cursor: pointer; font-size: 13px; }
  .view-switch button:hover { background: #f3f4f6; }
  .view-switch button.active { background: #2563eb; color: white; border-color: #2563eb; }
  .stage-settings-card { border-left: 3px solid #2563eb; }
  .sub-block { margin: 14px 0 0; }
  .sub-block > strong { font-size: 13px; }
  .link-btn { font: inherit; background: none; border: none; padding: 0; color: #2563eb; text-decoration: underline; cursor: pointer; }
  .link-btn:hover { background: none; color: #1d4ed8; }
  .save-error { background: #fef2f2; border: 1px solid #fecaca; color: #991b1b; border-radius: 8px; padding: 12px 14px; margin-bottom: 16px; font-size: 13px; line-height: 1.4; }
  .drop-hint { background: #fff8e1; border: 1px solid #fde68a; color: #92400e; border-radius: 8px; padding: 12px 14px; margin-bottom: 16px; font-size: 13px; line-height: 1.4; }
  .warning-box { background: #fff8e1; border-left: 3px solid #f59e0b; color: #92400e; border-radius: 3px; padding: 8px 12px; font-size: 13px; line-height: 1.4; }
  .card { background: white; border: 1px solid #e5e7eb; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .card h2 { margin: 0 0 12px; font-size: 16px; }
  .usage { font-size: 15px; line-height: 1.5; }
  .bullets { margin: 8px 0 12px 0; padding-left: 22px; color: #374151; line-height: 1.6; font-size: 13px; }
  .hint { color: #6b7280; font-size: 12px; margin: 8px 0 0; padding-top: 8px; border-top: 1px solid #f3f4f6; }
  kbd { background: #f3f4f6; border: 1px solid #d1d5db; padding: 1px 6px; border-radius: 3px; font-size: 12px; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  .mode-card { border-left: 3px solid #2563eb; }
  .stage-entry-card { border-left: 3px solid #171717; }
  /* Vollflächiges Overlay während eines Text-Drags über dem Fenster.
     pointer-events: none, damit es die dragleave/drop-Events des <main>
     nicht selbst schluckt. */
  .drop-overlay {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(23, 23, 23, 0.55);
    pointer-events: none;
  }
  .drop-overlay span {
    background: #171717;
    color: #fff;
    font-size: 16px;
    font-weight: 600;
    padding: 12px 22px;
    border-radius: 8px;
    transform: rotate(-0.5deg);
  }
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
