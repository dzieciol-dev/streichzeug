<!--
  StageView — die Schwärz-Bühne (WP-D, Bild-Modus aus WP-J).

  Zeigt einen `stage://job`-Payload an: erst die Marker-Animation (MarkerText
  bzw. Balken-Overlays über dem Bild), danach eine Ergebniszeile mit Aktionen.
  Die Komponente entscheidet nichts — Detection, Clipboard-Write und
  Ablage-Eintrag sind beim Eintreffen des Events bereits erledigt
  (Vertrag 2.5). Hier wird nur angezeigt.

  Payload-Zustände (Vertrag 2.2 / WP-D / Stufe 3):
   - normal    : finding_count >= 1, stash_id gesetzt → Erfolgszeile + Buttons.
   - 0 Findings: „Keine personenbezogenen Daten gefunden — Clipboard unverändert".
   - Fehler    : weder Segmente noch HTML noch Bild → nichts markiert/leer/zu groß.
   - image     : Original-Bild (metadaten-freie data:-URL) + Balken-Overlays,
                 darunter der erkannte Text; IMMER mit OCR-Prüf-Warnhinweis.

  Svelte 5 Legacy-Modus: export let + createEventDispatcher, keine Runes.
-->
<script lang="ts">
  import { createEventDispatcher, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import MarkerText from "./MarkerText.svelte";
  import type { StageJob } from "./stage";

  // --- Props ---
  export let job: StageJob;
  export let animation: "slow" | "normal" | "fast" | "off" | "full" = "normal";

  const dispatch = createEventDispatcher<{ close: null; showStash: null }>();

  // Fehler-State: nichts anzuzeigen — weder Segmente (plain) noch
  // annotiertes HTML (Stufe 2) noch Bild (Stufe 3). Direkt, ohne Animation.
  $: isError =
    (!job.segments || job.segments.length === 0) &&
    !job.annotated_html &&
    !job.image_data_url;
  $: isImage = job.content_kind === "image" && !!job.image_data_url;
  $: hasFindings = job.finding_count >= 1 && job.stash_id != null;

  // Bei Fehler-State ist die Animation sofort „durch"; sonst wartet die
  // Ergebniszeile auf on:done von MarkerText bzw. auf das Ende der
  // Balken-Choreografie der Bild-Bühne.
  let animationDone = false;
  $: if (isError) animationDone = true;

  let copyStatus = "";

  function onDone() {
    animationDone = true;
  }

  // ------------------------------------------------ Bild-Bühne (Stufe 3)
  // Balken statt Strich, gleiche Marker-Sprache und Staffelungs-Logik wie
  // MarkerText — aber als absolut positionierte Overlays über dem Bild.
  // Bewusst OHNE Rotation: die Overlays spiegeln exakt, was das geschwärzte
  // PNG in Clipboard/Ablage abdeckt.
  const BOX_TIMING = {
    slow: { stroke: 650, stagger: 200, budget: 3500 },
    normal: { stroke: 420, stagger: 130, budget: 2200 },
    fast: { stroke: 180, stagger: 45, budget: 700 },
  } as const;

  let boxShown: boolean[] = [];
  let boxStrokeMs: number = BOX_TIMING.normal.stroke;
  let boxTimers: number[] = [];
  let scheduledJobId = "";

  function prefersReducedMotion(): boolean {
    return (
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
    );
  }

  function scheduleBoxes(currentJob: StageJob) {
    boxTimers.forEach((t) => clearTimeout(t));
    boxTimers = [];
    const normalized = animation === "full" ? "normal" : animation;
    const effective =
      normalized === "off" || prefersReducedMotion() ? "off" : normalized;
    const n = currentJob.boxes.length;

    if (effective === "off" || n === 0) {
      boxStrokeMs = 0;
      boxShown = currentJob.boxes.map(() => true);
      boxTimers.push(window.setTimeout(() => onDone(), 0));
      return;
    }
    const t = BOX_TIMING[effective];
    boxStrokeMs = t.stroke;
    boxShown = currentJob.boxes.map(() => false);
    const maxSpread = Math.max(0, t.budget - t.stroke);
    const stagger = n > 1 ? Math.min(t.stagger, maxSpread / (n - 1)) : 0;
    let lastEnd = 0;
    for (let k = 0; k < n; k++) {
      const start = Math.round(k * stagger);
      boxTimers.push(
        window.setTimeout(() => {
          boxShown[k] = true;
          boxShown = boxShown; // Svelte-5-Legacy-Reaktivität
        }, start)
      );
      lastEnd = Math.max(lastEnd, start + t.stroke);
    }
    boxTimers.push(window.setTimeout(() => onDone(), lastEnd));
  }

  // Bei jedem neuen Bild-Job die Balken-Choreografie neu starten.
  $: if (isImage && job.job_id !== scheduledJobId) {
    scheduledJobId = job.job_id;
    animationDone = false;
    scheduleBoxes(job);
  }

  onDestroy(() => boxTimers.forEach((t) => clearTimeout(t)));

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
    {#if isImage}
      <div class="stage">
        <div class="image-wrap" style="--box-stroke-ms:{boxStrokeMs}ms">
          <img src={job.image_data_url} alt="Zu schwärzendes Bild" />
          {#each job.boxes as b, i (i)}
            <span
              class="img-bar"
              class:shown={boxShown[i]}
              style="left:{b.x * 100}%; top:{b.y * 100}%; width:{b.w * 100}%; height:{b.h * 100}%"
              title={b.entity_type}
              aria-hidden="true"
            ></span>
          {/each}
        </div>
        {#if job.segments.length > 0}
          <div class="ocr-text">
            <p class="ocr-label">Erkannter Text:</p>
            {#key job.job_id}
              <MarkerText segments={job.segments} {animation} on:done={() => {}} />
            {/key}
          </div>
        {:else}
          <p class="hint" style="margin-top: 10px;">
            Die Texterkennung hat in diesem Bild keinen Text gelesen.
          </p>
        {/if}
      </div>
      <p class="ocr-warning">
        Automatisch per Texterkennung geschwärzt — bitte <strong>visuell
        prüfen</strong>, bevor du das Bild teilst. Was die Texterkennung nicht
        liest (Handschrift, Logos, ungewöhnliche Schriften), bleibt sichtbar.
      </p>
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
    {/if}

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
          {#if job.content_kind === "image"}
            <p class="hint">
              Im Clipboard liegen das geschwärzte Bild und der geschwärzte Text.
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
            {#if isImage}
              Keine personenbezogenen Daten im erkannten Text gefunden —
              Clipboard unverändert.
            {:else}
              Keine personenbezogenen Daten gefunden — Clipboard unverändert.
            {/if}
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

  /* ------------------------------------------------ Bild-Bühne (Stufe 3) */
  .image-wrap { position: relative; display: inline-block; max-width: 100%; line-height: 0; }
  .image-wrap img { max-width: 100%; height: auto; border-radius: 4px; }
  /* Balken: fertige Form, per clip-path-Wipe von links aufgedeckt — gleiche
     Marker-Sprache wie der Strich in MarkerText, aber ohne Rotation (die
     Overlays spiegeln exakt die Abdeckung des geschwärzten PNGs). */
  .img-bar {
    position: absolute;
    background: #171717;
    border-radius: 2px;
    opacity: 0;
    clip-path: inset(-5% 100% -5% -2%);
    transition:
      clip-path var(--box-stroke-ms, 420ms) cubic-bezier(0.35, 0.55, 0.3, 1),
      opacity 130ms ease;
    pointer-events: none;
  }
  .img-bar.shown {
    opacity: 1;
    clip-path: inset(-5% -2% -5% -2%);
  }
  @media (prefers-reduced-motion: reduce) {
    .img-bar { transition: none; }
  }
  .ocr-text { margin-top: 12px; line-height: normal; }
  .ocr-label { font-size: 12px; color: #6b7280; margin: 0 0 4px; }
  .ocr-warning {
    margin-top: 10px;
    background: #fff8e1;
    border-left: 3px solid #f59e0b;
    padding: 8px 12px;
    border-radius: 3px;
    font-size: 13px;
    color: #92400e;
  }
</style>
