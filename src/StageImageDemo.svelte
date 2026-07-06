<!--
  StageImageDemo — Test-Harness für die Bild-Bühne (Stufe 3, WP-J).

  Erreichbar über `?demo=stage-image` in main.ts. Baut zur Laufzeit per
  Canvas ein Brief-Bild mit Beispiel-Text, mockt einen `stage://job`-Payload
  mit passenden Schwärz-Boxen und mountet StageView damit — die
  Balken-Animation, der OCR-Warnhinweis und der Text darunter lassen sich
  so ohne Tauri (und ohne echte OCR) visuell prüfen.

  Rein zum Testen; keine echten personenbezogenen Daten. Die Buttons der
  Ergebniszeile rufen invoke() und schlagen im Browser folgenlos fehl.
-->
<script lang="ts">
  import StageView from "./StageView.svelte";
  import type { StageJob } from "./stage";

  // Brief-Bild per Canvas — Zeilen und ihre Pixel-Boxen sind damit bekannt,
  // die Mock-Schwärz-Boxen sitzen exakt auf den „PII"-Stellen.
  const W = 860;
  const H = 240;
  const canvas = document.createElement("canvas");
  canvas.width = W;
  canvas.height = H;
  const ctx = canvas.getContext("2d")!;
  ctx.fillStyle = "#ffffff";
  ctx.fillRect(0, 0, W, H);
  ctx.fillStyle = "#1a1a1a";
  ctx.font = "28px Helvetica, Arial, sans-serif";
  ctx.fillText("Sehr geehrter Herr Muster,", 30, 60);
  ctx.fillText("bitte an max@example.de antworten.", 30, 120);
  ctx.fillText("Telefon: 089-12345678", 30, 180);

  // Box um einen Teilstring einer gezeichneten Zeile (normierte Koordinaten).
  function boxFor(line: string, sub: string, baselineY: number) {
    const before = line.slice(0, line.indexOf(sub));
    const x = 30 + ctx.measureText(before).width;
    const w = ctx.measureText(sub).width;
    return { x: x / W, y: (baselineY - 26) / H, w: w / W, h: 34 / H };
  }

  const job: StageJob = {
    job_id: "demo-image-1",
    mode: "reversible",
    stash_id: 1,
    finding_count: 3,
    truncated: false,
    segments: [
      { kind: "text", content: "Sehr geehrter Herr " },
      { kind: "finding", original: "Muster", replacement: "«P_a4b»", entity_type: "person", confidence: 0.9 },
      { kind: "text", content: ",\nbitte an " },
      { kind: "finding", original: "max@example.de", replacement: "«E_k2m»", entity_type: "email", confidence: 0.97 },
      { kind: "text", content: " antworten.\nTelefon: " },
      { kind: "finding", original: "089-12345678", replacement: "«T_x9z»", entity_type: "phone", confidence: 0.85 },
    ],
    content_kind: "image",
    annotated_html: null,
    image_data_url: canvas.toDataURL("image/png"),
    boxes: [
      { ...boxFor("Sehr geehrter Herr Muster,", "Muster", 60), entity_type: "person", replacement: "«P_a4b»" },
      { ...boxFor("bitte an max@example.de antworten.", "max@example.de", 120), entity_type: "email", replacement: "«E_k2m»" },
      { ...boxFor("Telefon: 089-12345678", "089-12345678", 180), entity_type: "phone", replacement: "«T_x9z»" },
    ],
    ocr_based: true,
  };

  let mounted = true;
  let runToken = 0;
  let currentJob = job;

  function replay() {
    runToken += 1;
    currentJob = { ...job, job_id: `demo-image-${runToken}` };
  }
</script>

<main>
  <header>
    <h1>Bild-Bühne — Demo<span class="badge">?demo=stage-image</span></h1>
    <p class="sub">
      Mock-Payload ohne Tauri/OCR: Canvas-Brief, drei Schwärz-Boxen,
      OCR-Warnhinweis. „Nochmal abspielen" re-mountet die Bühne.
    </p>
    <button class="primary" on:click={replay}>Nochmal abspielen</button>
  </header>

  {#if mounted}
    {#key currentJob.job_id}
      <StageView job={currentJob} animation="slow" on:close={() => {}} on:showStash={() => {}} />
    {/key}
  {/if}

  <footer>Nur Test-Harness · keine echten personenbezogenen Daten · kein Tauri</footer>
</main>

<style>
  main { max-width: 900px; margin: 0 auto; padding: 24px; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
  h1 { font-size: 20px; }
  .badge { margin-left: 8px; font-size: 11px; background: #eef2ff; color: #4338ca; padding: 2px 8px; border-radius: 10px; vertical-align: middle; }
  .sub { color: #6b7280; font-size: 13px; }
  button.primary { background: #2563eb; color: white; border: 1px solid #2563eb; border-radius: 6px; padding: 6px 12px; cursor: pointer; margin-bottom: 12px; }
  footer { margin-top: 24px; color: #9ca3af; font-size: 12px; text-align: center; }
</style>
