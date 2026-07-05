<!--
  StageView — die Schwärz-Bühne (WP-D).

  Zeigt einen `stage://job`-Payload an: erst die Marker-Animation (MarkerText),
  danach eine Ergebniszeile mit Aktionen. Die Komponente entscheidet nichts —
  Detection, Clipboard-Write und Ablage-Eintrag sind beim Eintreffen des Events
  bereits erledigt (Vertrag 2.5). Hier wird nur angezeigt.

  Drei Payload-Zustände (Vertrag 2.2 / WP-D):
   - normal    : finding_count >= 1, stash_id gesetzt → Erfolgszeile + Buttons.
   - 0 Findings: finding_count === 0, stash_id === null, segments = Text →
                 „Keine personenbezogenen Daten gefunden — Clipboard unverändert".
   - Fehler    : segments === [] → nichts markiert / leer / zu groß.

  Svelte 5 Legacy-Modus: export let + createEventDispatcher, keine Runes.
-->
<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import MarkerText from "./MarkerText.svelte";
  import type { StageJob } from "./stage";

  // --- Props ---
  export let job: StageJob;
  export let animation: "slow" | "normal" | "fast" | "off" | "full" = "normal";

  const dispatch = createEventDispatcher<{ close: null; showStash: null }>();

  // Fehler-State: nichts anzuzeigen — weder Segmente (plain) noch
  // annotiertes HTML (Stufe 2). Wird direkt gezeigt, ohne Animation.
  $: isError =
    (!job.segments || job.segments.length === 0) && !job.annotated_html;
  $: hasFindings = job.finding_count >= 1 && job.stash_id != null;

  // Bei Fehler-State ist die Animation sofort „durch"; sonst wartet die
  // Ergebniszeile auf on:done von MarkerText.
  let animationDone = false;
  $: if (isError) animationDone = true;

  let copyStatus = "";

  function onDone() {
    animationDone = true;
  }

  async function copyAgain() {
    if (job.stash_id == null) return;
    copyStatus = "…";
    try {
      await invoke("stash_copy", { id: job.stash_id });
      copyStatus = "In die Zwischenablage kopiert.";
    } catch (e) {
      copyStatus = `Fehler beim Kopieren: ${e}`;
    }
  }
</script>

<section class="card stage-card">
  <h2>Schwärz-Bühne</h2>

  {#if isError}
    <p class="usage">
      Nichts zu schwärzen — es war kein Text markiert, die Zwischenablage war
      leer oder der Inhalt war zu groß. Die Zwischenablage wurde nicht verändert.
    </p>
    <div class="actions">
      <button class="primary" on:click={() => dispatch("close")}>Schließen</button>
    </div>
  {:else}
    <div class="stage">
      {#key job.job_id}
        <MarkerText
          segments={job.segments}
          contentKind={job.annotated_html ? "html" : "plain"}
          annotatedHtml={job.annotated_html ?? ""}
          {animation}
          on:done={onDone}
        />
      {/key}
    </div>

    {#if animationDone}
      <div class="result">
        {#if hasFindings}
          <p class="result-line ok">
            <strong>{job.finding_count}</strong>
            {job.finding_count === 1 ? "Stelle" : "Stellen"} geschwärzt —
            liegt im Clipboard und in der Ablage.
          </p>
          {#if job.truncated}
            <p class="hint">
              {#if job.content_kind === "html"}
                Anzeige gekürzt und ohne Formatierung — Clipboard und Ablage
                enthalten die vollständige formatierte Fassung.
              {:else}
                Anzeige gekürzt — Clipboard und Ablage enthalten den vollständigen Text.
              {/if}
            </p>
          {/if}
          {#if job.content_kind === "html" && !job.truncated}
            <p class="hint">
              Formatierung bleibt erhalten — im Clipboard liegen die
              formatierte und eine Text-Fassung.
            </p>
          {/if}
          <div class="actions">
            <button class="primary" on:click={copyAgain}>Nochmal kopieren</button>
            <button on:click={() => dispatch("showStash")}>Zur Ablage</button>
            <button on:click={() => dispatch("close")}>Schließen</button>
          </div>
          {#if copyStatus}
            <p class="hint" style="margin-top: 8px;">{copyStatus}</p>
          {/if}
        {:else}
          <p class="result-line neutral">
            Keine personenbezogenen Daten gefunden — Clipboard unverändert.
          </p>
          <div class="actions">
            <button class="primary" on:click={() => dispatch("close")}>Schließen</button>
          </div>
        {/if}
      </div>
    {/if}
  {/if}
</section>

<style>
  .stage-card { border-left: 3px solid #2563eb; }
  .usage { font-size: 15px; line-height: 1.5; }
  .stage { background: #f7f7f8; border: 1px solid #eef0f2; border-radius: 6px; padding: 14px 16px; min-height: 48px; }
  .result { margin-top: 14px; }
  .result-line { font-size: 15px; line-height: 1.5; margin: 0 0 4px; }
  .result-line.ok { color: #065f46; }
  .result-line.neutral { color: #374151; }
  .hint { color: #6b7280; font-size: 12px; margin: 4px 0 0; }
  .actions { display: flex; gap: 8px; margin-top: 12px; flex-wrap: wrap; }
  button { font: inherit; padding: 6px 12px; border-radius: 6px; border: 1px solid #d1d5db; background: #f9fafb; cursor: pointer; }
  button.primary { background: #2563eb; color: white; border-color: #2563eb; }
  button:hover { background: #f3f4f6; }
  button.primary:hover { background: #1d4ed8; }
</style>
