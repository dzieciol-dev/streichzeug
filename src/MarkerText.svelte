<!--
  MarkerText — wiederverwendbare Schwärz-Animation der Bühne.

  Vertrag 2.6: rendert die Anzeige-Segmente aus dem `stage://job`-Event
  (Format siehe Vertrag 2.2). Text-Segmente werden normal gesetzt,
  Finding-Segmente zeigen zuerst das Original; darüber zieht ein
  Filzstift-Strich (transform: scaleX, transform-origin links, minimale
  Rotation, raue Kante via clip-path), danach Crossfade zum Ersatztext in
  code-Optik.

  Bewusst Svelte 5 im Legacy-Modus: `export let` + `createEventDispatcher`,
  KEINE Runes. Keine Tauri-API — läuft standalone mit Mock-Daten.
  Es werden ausschließlich `transform`/`opacity` animiert (kein Layout-Thrash):
  Original und Ersatz liegen im selben Grid-Feld gestapelt, die Zellenbreite
  entspricht dem breiteren der beiden — so reflowt der umgebende Text nie.
-->
<script lang="ts">
  import { createEventDispatcher, onMount, onDestroy } from "svelte";

  // Segment-Format exakt nach Vertrag 2.2.
  type TextSegment = { kind: "text"; content: string };
  type FindingSegment = {
    kind: "finding";
    original: string;
    replacement: string;
    entity_type: string;
    confidence: number;
  };
  type Segment = TextSegment | FindingSegment;

  // --- Props (Vertrag 2.6, Stufe-2-Erweiterung) ---
  export let segments: Segment[] = [];
  // "full" ist der Legacy-Wert früher gespeicherter Settings und wird wie
  // "normal" behandelt (siehe Normalisierung in onMount).
  export let animation: "slow" | "normal" | "fast" | "off" | "full" = "normal";
  // Stufe 2: bei "html" wird annotatedHtml gerendert (backend-sanitisiert,
  // Fundstellen als <span data-sz-finding …>) und dieselbe Marker-Animation
  // läuft über den Spans. segments wird dann ignoriert.
  export let contentKind: "plain" | "html" = "plain";
  export let annotatedHtml: string = "";

  const dispatch = createEventDispatcher<{ done: null }>();

  // Wurzel des {@html}-Renderings — die Finding-Spans werden nach Mount per
  // querySelectorAll eingesammelt und mit den Animations-Layern bestückt.
  let htmlRoot: HTMLElement | null = null;

  // Timing je Stil in ms. Nur transform/opacity werden hierüber angesteuert.
  //  - stroke  : Dauer, in der der Strich von links über das Original zieht
  //  - cross   : Dauer des Crossfade Original -> Ersatztext
  //  - stagger : Grund-Versatz pro Finding
  //  - budget  : hartes Gesamtbudget bis `on:done`
  const TIMING = {
    slow: { stroke: 650, cross: 420, stagger: 200, budget: 3500 },
    normal: { stroke: 420, cross: 280, stagger: 130, budget: 2200 },
    fast: { stroke: 180, cross: 110, stagger: 45, budget: 700 },
  } as const;

  // Phase je Segment-Index: "idle" | "strike" | "reveal" | "done".
  let phases: string[] = segments.map(() => "idle");

  // CSS-Transition-Dauern (an den gewählten Stil gebunden; 0 = Endzustand sofort).
  let strokeMs: number = TIMING.normal.stroke;
  let crossMs: number = TIMING.normal.cross;

  let timers: number[] = [];

  function prefersReducedMotion(): boolean {
    return (
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches
    );
  }

  function setPhase(index: number, phase: string): void {
    phases[index] = phase;
    phases = phases; // Reaktivität in Svelte-5-Legacy anstoßen
  }

  function later(ms: number, fn: () => void): void {
    timers.push(window.setTimeout(fn, ms));
  }

  // HTML-Modus: sammelt die backend-annotierten Spans ein und bestückt sie
  // mit denselben drei Layern wie ein Plain-Finding (Original / Ersatz /
  // Strich). Fortsetzungs-Spans (`data-sz-cont`, Finding über Tag-Grenze)
  // bekommen ein leeres Replacement — ihr Balken bleibt stehen, der Token
  // erscheint nur auf dem ersten Teil.
  function prepareHtmlFindings(): HTMLElement[] {
    if (!htmlRoot) return [];
    const spans = Array.from(
      htmlRoot.querySelectorAll<HTMLElement>("[data-sz-finding]")
    );
    for (const span of spans) {
      const original = span.textContent ?? "";
      const replacement = span.hasAttribute("data-sz-cont")
        ? ""
        : (span.getAttribute("data-replacement") ?? "");
      span.textContent = "";
      const orig = document.createElement("span");
      orig.className = "layer original";
      orig.textContent = original;
      const repl = document.createElement("code");
      repl.className = "layer replacement";
      repl.textContent = replacement;
      const stroke = document.createElement("span");
      stroke.className = "layer stroke";
      stroke.setAttribute("aria-hidden", "true");
      span.append(orig, repl, stroke);
      span.classList.add("finding", "idle");
      const entity = span.getAttribute("data-entity-type");
      if (entity) span.title = entity;
    }
    return spans;
  }

  function setElementPhase(el: HTMLElement, phase: string): void {
    el.classList.remove("idle", "strike", "reveal", "done");
    el.classList.add(phase);
  }

  onMount(() => {
    // prefers-reduced-motion wird wie "off" behandelt: sofort Endzustand.
    // Legacy-Wert "full" (frühere Settings) läuft als "normal".
    const normalized: "slow" | "normal" | "fast" | "off" =
      animation === "full" ? "normal" : animation;
    const effective: "slow" | "normal" | "fast" | "off" =
      normalized === "off" || prefersReducedMotion() ? "off" : normalized;

    // Einheitliche Sicht auf die animierbaren Fundstellen: Segment-Indizes
    // (plain) bzw. DOM-Spans (html) — die Choreografie darunter ist identisch.
    // HTML: Fortsetzungs-Spans (data-sz-cont, Finding über Tag-Grenze) werden
    // ihrem Primär-Span zugeschlagen — EIN Finding ist EINE Unit, alle Teile
    // wechseln die Phase gleichzeitig. Sonst animiert „Max <b>Mustermann</b>"
    // als zwei zeitversetzte Findings und verzerrt die Stagger-Stauchung.
    let units: ((phase: string) => void)[];
    if (contentKind === "html") {
      const groups: HTMLElement[][] = [];
      for (const span of prepareHtmlFindings()) {
        if (span.hasAttribute("data-sz-cont") && groups.length > 0) {
          groups[groups.length - 1].push(span);
        } else {
          groups.push([span]);
        }
      }
      units = groups.map(
        (group) => (p: string) => group.forEach((el) => setElementPhase(el, p))
      );
    } else {
      units = segments
        .map((seg, i) => (seg.kind === "finding" ? i : -1))
        .filter((i) => i >= 0)
        .map((i) => (p: string) => setPhase(i, p));
    }

    if (effective === "off" || units.length === 0) {
      strokeMs = 0;
      crossMs = 0;
      phases = segments.map(() => "done");
      units.forEach((set) => set("done"));
      later(0, () => dispatch("done"));
      return;
    }

    const t = TIMING[effective];
    strokeMs = t.stroke;
    crossMs = t.cross;

    // Staffelung: Grund-Versatz 70 ms (bzw. 24 ms). Bei vielen Findings wird
    // der Versatz so gestaucht, dass das Gesamtbudget hält — dadurch laufen
    // späte Findings in überlappenden Wellen parallel (ab ~10 Findings sichtbar).
    const perFinding = t.stroke + t.cross;
    const maxSpread = Math.max(0, t.budget - perFinding);
    const n = units.length;
    const stagger = n > 1 ? Math.min(t.stagger, maxSpread / (n - 1)) : 0;

    let lastEnd = 0;
    units.forEach((set, k) => {
      const start = Math.round(k * stagger);
      later(start, () => set("strike"));
      later(start + t.stroke, () => set("reveal"));
      later(start + perFinding, () => set("done"));
      lastEnd = Math.max(lastEnd, start + perFinding);
    });

    later(lastEnd, () => dispatch("done"));
  });

  onDestroy(() => {
    timers.forEach((id) => clearTimeout(id));
    timers = [];
  });
</script>

{#if contentKind === "html"}
  <!-- annotated_html ist backend-sanitisiert (ammonia-Allowlist); die CSP
       (kein Remote-Load) ist die zweite Verteidigungslinie. -->
  <div
    class="marker-text marker-html"
    bind:this={htmlRoot}
    style="--stroke-ms:{strokeMs}ms; --cross-ms:{crossMs}ms"
  >
    {@html annotatedHtml}
  </div>
{:else}<span class="marker-text" style="--stroke-ms:{strokeMs}ms; --cross-ms:{crossMs}ms">{#each segments as seg, i (i)}{#if seg.kind === "text"}<span class="txt">{seg.content}</span>{:else}<span class="finding {phases[i] ?? 'idle'}" title={seg.entity_type}><span class="layer original">{seg.original}</span><code class="layer replacement">{seg.replacement}</code><span class="layer stroke" aria-hidden="true"></span></span>{/if}{/each}</span>{/if}

<style>
  .marker-text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    line-height: 1.75;
    font-size: 14px;
    color: #1a1a1a;
  }

  /* Finding: Original und Ersatz im selben Grid-Feld gestapelt -> Zellenbreite
     = breiteres der beiden, kein Reflow beim Crossfade. */
  .finding {
    position: relative;
    display: inline-grid;
    vertical-align: baseline;
    white-space: pre-wrap;
  }
  .finding > .layer {
    grid-area: 1 / 1;
  }

  .original {
    justify-self: start;
    opacity: 1;
    transition: opacity var(--cross-ms) ease;
  }

  /* Ersatztext: weiß AUF dem stehenbleibenden Balken — wie eine Prägung auf
     echter Schwärzung. Kein eigener Hintergrund, der Balken ist die Bühne. */
  .replacement {
    justify-self: center;
    align-self: center;
    z-index: 1;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.85em;
    line-height: 1;
    color: #ffffff;
    padding: 0 4px;
    white-space: nowrap;
    opacity: 0;
    transition: opacity var(--cross-ms) ease;
  }

  /* Filzstift-Strich: Der Balken hat von Anfang an seine fertige Form
     (feste Höhe, ungleichmäßig gerundete Enden, dezente Streifen-Textur mit
     FESTEN Pixelmaßen) und wird per clip-path-Wipe von links aufgedeckt —
     nichts wird gestreckt, wie bei einem echten Markerstrich, dessen Spitze
     über das Papier zieht. Vorher wurde per scaleX skaliert, wodurch die
     prozentbasierte Zackenkante bei breiten Fundstellen zu hässlichen
     Wellen/Lücken auseinandergezogen wurde. */
  .stroke {
    justify-self: stretch;
    align-self: center;
    height: 1.15em;
    margin: 0 -3px;
    background:
      repeating-linear-gradient(
        180deg,
        rgba(255, 255, 255, 0.05) 0 2px,
        transparent 2px 5px
      ),
      #171717;
    border-radius: 0.35em 0.5em 0.4em 0.55em;
    transform: rotate(-0.5deg);
    pointer-events: none;
    opacity: 0;
    clip-path: inset(-10% 100% -10% -2%);
    transition:
      clip-path var(--stroke-ms) cubic-bezier(0.35, 0.55, 0.3, 1),
      opacity 130ms ease;
    will-change: clip-path, opacity;
  }

  /* Phasen ------------------------------------------------------------ */
  /* strike: Strich zieht über das Original (Wipe von links) */
  .finding.strike .stroke {
    opacity: 1;
    clip-path: inset(-10% -2% -10% -2%);
  }

  /* reveal/done: Der Balken BLEIBT deckend stehen (echte Schwärzung — nichts
     verblasst), das Original verschwindet darunter, der Ersatztext erscheint
     in Weiß auf dem Balken. Das Original bleibt unsichtbar im Fluss stehen,
     damit die Zellenbreite konstant bleibt — kein Reflow, weder während
     noch nach der Animation. */
  .finding.reveal .stroke,
  .finding.done .stroke {
    opacity: 1;
    clip-path: inset(-10% -2% -10% -2%);
  }
  .finding.reveal .original,
  .finding.done .original {
    opacity: 0;
  }
  .finding.reveal .replacement,
  .finding.done .replacement {
    opacity: 1;
  }

  @media (prefers-reduced-motion: reduce) {
    .original,
    .replacement,
    .stroke {
      transition: none;
    }
  }

  /* ------------------------------------------------------ HTML-Modus
     Die Finding-Spans und ihre Layer entstehen per JS aus dem {@html}-
     Inhalt — Svelte-Scoping greift dort nicht, daher :global unter dem
     .marker-html-Scope. Die Regeln spiegeln 1:1 die Plain-Regeln oben. */
  .marker-html {
    white-space: normal; /* HTML bringt seine eigene Block-Struktur mit */
  }
  .marker-html :global(img) {
    max-width: 100%;
    height: auto;
  }
  .marker-html :global(table) {
    border-collapse: collapse;
  }
  .marker-html :global(.finding) {
    position: relative;
    display: inline-grid;
    vertical-align: baseline;
    white-space: pre-wrap;
  }
  .marker-html :global(.finding > .layer) {
    grid-area: 1 / 1;
  }
  .marker-html :global(.finding .original) {
    justify-self: start;
    opacity: 1;
    transition: opacity var(--cross-ms) ease;
  }
  .marker-html :global(.finding .replacement) {
    justify-self: center;
    align-self: center;
    z-index: 1;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.85em;
    line-height: 1;
    color: #ffffff;
    padding: 0 4px;
    white-space: nowrap;
    opacity: 0;
    transition: opacity var(--cross-ms) ease;
  }
  .marker-html :global(.finding .stroke) {
    justify-self: stretch;
    align-self: center;
    height: 1.15em;
    margin: 0 -3px;
    background:
      repeating-linear-gradient(
        180deg,
        rgba(255, 255, 255, 0.05) 0 2px,
        transparent 2px 5px
      ),
      #171717;
    border-radius: 0.35em 0.5em 0.4em 0.55em;
    transform: rotate(-0.5deg);
    pointer-events: none;
    opacity: 0;
    clip-path: inset(-10% 100% -10% -2%);
    transition:
      clip-path var(--stroke-ms) cubic-bezier(0.35, 0.55, 0.3, 1),
      opacity 130ms ease;
    will-change: clip-path, opacity;
  }
  .marker-html :global(.finding.strike .stroke),
  .marker-html :global(.finding.reveal .stroke),
  .marker-html :global(.finding.done .stroke) {
    opacity: 1;
    clip-path: inset(-10% -2% -10% -2%);
  }
  .marker-html :global(.finding.reveal .original),
  .marker-html :global(.finding.done .original) {
    opacity: 0;
  }
  .marker-html :global(.finding.reveal .replacement),
  .marker-html :global(.finding.done .replacement) {
    opacity: 1;
  }
  @media (prefers-reduced-motion: reduce) {
    .marker-html :global(.finding .original),
    .marker-html :global(.finding .replacement),
    .marker-html :global(.finding .stroke) {
      transition: none;
    }
  }
</style>
