<!--
  Vereinfachter First-Run-Wizard. Sammelt die wichtigsten Einstellungen
  und ruft am Ende update_settings mit onboarded=true auf.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { createEventDispatcher } from "svelte";

  export let initialSettings: any;
  export let nerStatus: any;

  const dispatch = createEventDispatcher();
  let draft = { ...initialSettings };
  let step = 0;

  const isMac = navigator.platform.toLowerCase().includes("mac");
  const STEPS_TOTAL = isMac ? 7 : 6;

  let downloadStatus = nerStatus.model_files_present ? "done" : "idle";
  let downloadError = "";

  function next() {
    if (step === 2 && draft.strict_mode) {
      step = 4;
      return;
    }
    step += 1;
  }

  function back() {
    if (step === 4 && draft.strict_mode) {
      step = 2;
      return;
    }
    step = Math.max(0, step - 1);
  }

  async function startDownload() {
    downloadStatus = "running";
    downloadError = "";
    try {
      await invoke("download_ner_model");
      downloadStatus = "done";
      draft.enable_ner = true;
    } catch (e) {
      downloadStatus = "error";
      downloadError = String(e);
    }
  }

  async function finish() {
    draft.onboarded = true;
    try {
      await invoke("update_settings", { newSettings: draft });
    } catch (e) {
      console.error("save failed", e);
    }
    // Jetzt — nachdem der letzte Schritt den Schlüsselbund-Zugriff angekündigt
    // hat — den Verschlüsselungs-Schlüssel initialisieren. Genau hier zeigt
    // macOS den einmaligen Schlüsselbund-Dialog. finalize_secret_setup meldet
    // einen etwaigen Fallback selbst per Notification.
    try {
      await invoke("finalize_secret_setup");
    } catch (e) {
      console.error("secret setup failed", e);
    }
    dispatch("done");
  }
</script>

<div class="wizard">
  <div class="progress">
    Schritt {Math.min(step + 1, STEPS_TOTAL)} von {STEPS_TOTAL}
  </div>

  {#if step === 0}
    <h1>Willkommen bei Streichzeug</h1>
    <p>
      Streichzeug erkennt personenbezogene Daten in deiner Zwischenablage
      und ersetzt sie durch Pseudonyme — bevor du sie in einen LLM-Chat
      pastest.
    </p>
    <p>
      Die App läuft im Hintergrund als Tray-Icon. Du kopierst Text wie
      gewohnt, drückst einen Hotkey statt Strg+V, und der eingefügte Text
      ist anonymisiert.
    </p>
    <p>Kurzer Setup-Walk — eine Minute, dann kanns losgehen.</p>
  {:else if step === 1}
    <h1>Hotkey wählen</h1>
    <p>In Office und Browsern nicht belegt:</p>
    <div class="options">
      <label class="option" class:selected={draft.hotkey === "CmdOrCtrl+Alt+B"}>
        <input type="radio" bind:group={draft.hotkey} value="CmdOrCtrl+Alt+B" />
        <div>
          <span class="opt-label">{isMac ? "Cmd + Option + B" : "Strg + Alt + B"} <span class="opt-tag">Default</span></span>
          <p class="opt-hint">Ergonomisch. B steht semantisch fur anonyMize/Block.</p>
        </div>
      </label>
      <label class="option" class:selected={draft.hotkey === "CmdOrCtrl+Alt+Shift+V"}>
        <input type="radio" bind:group={draft.hotkey} value="CmdOrCtrl+Alt+Shift+V" />
        <div>
          <span class="opt-label">{isMac ? "Cmd + Option + Shift + V" : "Strg + Alt + Umschalt + V"}</span>
          <p class="opt-hint">Drei Modifier — garantiert nirgends belegt, aber etwas sperrig.</p>
        </div>
      </label>
    </div>
  {:else if step === 2}
    <h1>Verarbeitungsmodus</h1>
    <p>Zwei Wege im Umgang mit personenbezogenen Daten:</p>
    <div class="options">
      <label class="option" class:selected={!draft.strict_mode}>
        <input type="radio" name="mode" checked={!draft.strict_mode} on:change={() => draft.strict_mode = false} />
        <div>
          <span class="opt-label">Reversibel <span class="opt-tag">Default</span></span>
          <p class="opt-hint">
            Personenbezogene Daten werden durch Tokens ersetzt. Mapping bleibt
            lokal, du kannst die LLM-Antwort zurückübersetzen. Tokens beim
            LLM sind pseudonyme (= personenbezogene) Daten — DSGVO-Pflichten
            bleiben.
          </p>
          <p class="opt-caution">
            Ehrlich dazu: Es entsteht eine lokale Mapping-DB. Sie liegt
            <strong>verschlüsselt</strong> auf der Platte (SQLCipher; der
            Schlüssel steckt im OS-Schlüsselbund, nicht in der Datei). Solange
            dein Benutzerkonto entsperrt ist, kann die App — und damit potenziell
            Schadsoftware unter deinem Konto — sie lesen. Willst du gar kein
            lokales Mapping, nimm Strict.
          </p>
        </div>
      </label>
      <label class="option" class:selected={draft.strict_mode}>
        <input type="radio" name="mode" checked={draft.strict_mode} on:change={() => draft.strict_mode = true} />
        <div>
          <span class="opt-label">Strict (Anonymisierung)</span>
          <p class="opt-hint">
            Personenbezogene Daten werden durch lesbare Platzhalter ersetzt.
            Kein Mapping wird angelegt, Daten beim LLM sind anonym
            (ErwGr. 26 DSGVO). Trade-off: kein Reverse — manuelle Zuordnung
            der LLM-Antwort.
          </p>
        </div>
      </label>
    </div>
  {:else if step === 3}
    <h1>Aufbewahrungsdauer</h1>
    <p>
      Wie lange sollen Pseudonym-Mappings lokal gespeichert werden? Nach
      Ablauf werden sie automatisch gelöscht.
    </p>
    <div class="options">
      <label class="option" class:selected={draft.retention_minutes === 15}>
        <input type="radio" bind:group={draft.retention_minutes} value={15} />
        <div><span class="opt-label">15 Minuten</span><p class="opt-hint">Maximaler DSGVO-Schutz.</p></div>
      </label>
      <label class="option" class:selected={draft.retention_minutes === 60}>
        <input type="radio" bind:group={draft.retention_minutes} value={60} />
        <div><span class="opt-label">1 Stunde <span class="opt-tag">Default</span></span><p class="opt-hint">Reicht für typische LLM-Chat-Sessions.</p></div>
      </label>
      <label class="option" class:selected={draft.retention_minutes === 480}>
        <input type="radio" bind:group={draft.retention_minutes} value={480} />
        <div><span class="opt-label">8 Stunden</span><p class="opt-hint">Arbeitstag — Reverse über den Tag verfügbar.</p></div>
      </label>
      <label class="option" class:selected={draft.retention_minutes === 1440}>
        <input type="radio" bind:group={draft.retention_minutes} value={1440} />
        <div><span class="opt-label">24 Stunden</span><p class="opt-hint">Bequem, weniger streng.</p></div>
      </label>
      <label class="option" class:selected={draft.retention_minutes === 0}>
        <input type="radio" bind:group={draft.retention_minutes} value={0} />
        <div><span class="opt-label">Nur diese Session</span><p class="opt-hint">Mappings nach App-Quit weg, Tokens beim LLM danach anonym.</p></div>
      </label>
    </div>
  {:else if step === 4}
    <h1>Erweiterte Erkennung (NER)</h1>
    <p>
      Optional: ein lokales KI-Modell für statistische Erkennung von
      Personen, Orten und Organisationen, die unsere Regex- und
      Listen-basierte Detection verpasst.
    </p>
    <div class="info-box">
      <h3>Pro</h3>
      <ul>
        <li>Bessere Erkennung in unstrukturierten Texten</li>
        <li>Fängt Namen ohne Anrede-Kontext</li>
        <li>Fängt Organisationen ohne GmbH/AG-Suffix</li>
      </ul>
      <h3>Contra</h3>
      <ul>
        <li>145 MB Download (einmalig, von HuggingFace direkt)</li>
        <li>300 ms Latenz beim ersten Hotkey-Druck</li>
        <li>50 ms zusätzlich pro Hotkey-Druck</li>
      </ul>
      <p class="hint">Modell läuft komplett offline. Keine Daten verlassen den Rechner.</p>
    </div>
    {#if !nerStatus.built_with_ner_feature}
      <p class="warning">Dieser Build enthält das NER-Feature nicht (Slim-Build). Schritt überspringen.</p>
    {:else if downloadStatus === "done"}
      <p class="success">Modell ist bereits geladen.</p>
    {:else if downloadStatus === "running"}
      <p>Lade Modell + ONNX Runtime — kann 1 bis 3 Minuten dauern…</p>
    {:else if downloadStatus === "error"}
      <p class="warning">Fehler: {downloadError}</p>
      <button on:click={startDownload}>Erneut versuchen</button>
    {:else}
      <button class="primary" on:click={startDownload}>Modell jetzt laden (145 MB)</button>
      <p class="hint">Oder überspringen — später aus dem App-Fenster aktivierbar.</p>
    {/if}
  {:else if step === 5 && isMac}
    <h1>macOS-Berechtigung erforderlich</h1>
    <p>
      Beim ersten Hotkey-Druck fragt macOS nach Eingabehilfen-Permission.
      Diese braucht Streichzeug, um das synthetische Cmd+V an die Ziel-App
      zu schicken.
    </p>
    <p>
      Erscheint der Dialog: Systemeinstellungen öffnen, in der Liste
      Streichzeug aktivieren.
    </p>
    <p class="hint">
      Falls der Dialog nicht erscheint: Systemeinstellungen, Datenschutz
      und Sicherheit, Bedienungshilfen, Häkchen bei Streichzeug.
    </p>
  {:else}
    <h1>Fast fertig</h1>
    {#if isMac}
      <p>
        Streichzeug erscheint ab jetzt im Dock. Das rote X schließt die App
        nicht, sondern versteckt nur das Fenster — sie läuft im Hintergrund
        weiter. Ein Klick aufs Dock-Icon holt das Fenster zurück; zusätzlich
        findest du sie über das Icon in der Menüleiste oben rechts.
      </p>
      <div class="info-box">
        <h3>Ein letzter Schritt: Schlüsselbund</h3>
        <p style="margin: 0;">
          Streichzeug legt seinen Verschlüsselungs-Schlüssel sicher im
          macOS-Schlüsselbund ab — nicht als lose Datei. Wenn du auf
          <strong>Fertig</strong> klickst, fragt macOS dich dafür
          <strong>einmalig</strong> um Erlaubnis. Bitte wähle dort
          <strong>„Immer erlauben"</strong>, sonst fragt macOS bei jedem Start
          erneut.
        </p>
      </div>
    {:else}
      <p>
        Streichzeug läuft weiter im Hintergrund. Das Fenster erreichst du
        jederzeit über das Dock/die Taskleiste und das Tray-Icon (unten
        rechts, evtl. unter dem Pfeil-Aufklapper).
      </p>
      <p class="hint">
        Den Verschlüsselungs-Schlüssel legt Streichzeug sicher im
        Windows-Anmeldeinformationsspeicher ab, nicht als lose Datei.
      </p>
    {/if}
  {/if}

  <div class="nav">
    {#if step > 0}
      <button on:click={back}>Zurück</button>
    {:else}
      <span></span>
    {/if}
    {#if step < STEPS_TOTAL - 1}
      <button class="primary" on:click={next}>Weiter</button>
    {:else}
      <button class="primary" on:click={finish}>Fertig</button>
    {/if}
  </div>
</div>

<style>
  .wizard { max-width: 640px; margin: 0 auto; padding: 32px 24px; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
  .progress { font-size: 12px; color: #888; text-align: right; margin-bottom: 16px; }
  h1 { font-size: 22px; margin: 0 0 12px; }
  h3 { font-size: 14px; margin: 16px 0 6px; color: #555; }
  p { line-height: 1.5; color: #444; }
  p.hint { font-size: 13px; color: #777; }
  p.success { color: #2563eb; font-weight: 600; }
  p.warning { background: #fff8e1; border-left: 3px solid #f59e0b; padding: 8px 12px; border-radius: 3px; }
  .options { display: flex; flex-direction: column; gap: 8px; margin: 16px 0; }
  .option { display: flex; align-items: flex-start; gap: 12px; padding: 12px 14px; border: 1px solid #ddd; border-radius: 6px; cursor: pointer; }
  .option:hover { background: #f5f5f5; }
  .option.selected { border-color: #2563eb; background: #eff6ff; }
  .option input { margin-top: 4px; }
  .opt-label { font-weight: 600; }
  .opt-tag { display: inline-block; margin-left: 6px; font-size: 11px; background: #2563eb; color: white; padding: 1px 6px; border-radius: 3px; vertical-align: middle; }
  .opt-hint { font-size: 13px; color: #666; margin: 4px 0 0; }
  .opt-caution { font-size: 12px; color: #92400e; background: #fffbeb; border-left: 3px solid #f59e0b; padding: 6px 10px; border-radius: 3px; margin: 8px 0 0; }
  .info-box { background: #f8fafc; border: 1px solid #e2e8f0; border-radius: 6px; padding: 12px 16px; margin: 12px 0; }
  .info-box ul { margin: 6px 0; padding-left: 20px; }
  .info-box li { margin: 4px 0; font-size: 13px; }
  .nav { display: flex; justify-content: space-between; margin-top: 24px; padding-top: 16px; border-top: 1px solid #eee; }
  button { padding: 8px 18px; font-size: 14px; border: 1px solid #ccc; border-radius: 5px; background: white; cursor: pointer; }
  button.primary { background: #2563eb; border-color: #2563eb; color: white; }
  button.primary:hover { background: #1d4ed8; }
</style>
