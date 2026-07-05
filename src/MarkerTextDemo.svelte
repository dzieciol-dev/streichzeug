<!--
  MarkerTextDemo — Test-Harness fuer MarkerText (Vertrag 2.6).

  Erreichbar ueber den Querystring-Schalter `?demo=marker` in main.ts.
  Mountet drei Mock-Payloads (kurz/3 Findings mit Umlauten, lang/15 Findings,
  0 Findings) und spielt sie je Animations-Stil ab. Neu-Abspielen erfolgt per
  Re-Mount ueber {#key} — so laeuft die Animation garantiert von vorn.

  Rein zum Testen; kein Tauri, keine echten personenbezogenen Daten.
-->
<script lang="ts">
  import MarkerText from "./MarkerText.svelte";

  type Segment =
    | { kind: "text"; content: string }
    | {
        kind: "finding";
        original: string;
        replacement: string;
        entity_type: string;
        confidence: number;
      };

  type Payload = {
    title: string;
    note: string;
    segments: Segment[];
    // Stufe 2: gesetzt → HTML-Modus (segments wird ignoriert).
    annotatedHtml?: string;
  };

  // --- Payload 1: kurzer Text, 3 Findings, mit Umlauten ---
  const kurz: Segment[] = [
    { kind: "text", content: "Sehr geehrter " },
    {
      kind: "finding",
      original: "Herr Müller",
      replacement: "«P_a4b»",
      entity_type: "person",
      confidence: 0.93,
    },
    { kind: "text", content: ", Ihre IBAN " },
    {
      kind: "finding",
      original: "DE12 3456 7890 1234 5678 90",
      replacement: "«IBAN_7f2»",
      entity_type: "iban",
      confidence: 0.99,
    },
    { kind: "text", content: " wurde für die Überweisung an " },
    {
      kind: "finding",
      original: "Café Größenwahn GmbH",
      replacement: "«ORG_1c8»",
      entity_type: "org",
      confidence: 0.61,
    },
    { kind: "text", content: " verwendet. Mit freundlichen Grüßen." },
  ];

  // --- Payload 2: langer Text, 15 Findings (inkl. eines unsicheren) ---
  function buildLang(): Segment[] {
    const people = [
      "Anna Schäfer",
      "Björn Öztürk",
      "Clara Weiß",
      "Dénes Kovács",
      "Emre Yılmaz",
    ];
    const emails = [
      "anna.schaefer@example.de",
      "b.oeztuerk@example.de",
      "clara.weiss@example.de",
    ];
    const phones = ["+49 151 23456789", "0170 9876543", "089 4455667"];
    const ibans = [
      "DE44 5001 0517 5407 3249 31",
      "AT61 1904 3002 3457 3201",
    ];
    const orgs = ["Größenwahn GmbH", "Bäckerei Süß & Co."];

    const segs: Segment[] = [
      {
        kind: "text",
        content:
          "Protokoll der Besprechung vom 4. Juli. Anwesend waren unter anderem ",
      },
    ];

    let n = 0;
    const pushFinding = (
      original: string,
      replacement: string,
      entity_type: string,
      confidence: number,
    ) => {
      segs.push({ kind: "finding", original, replacement, entity_type, confidence });
      n++;
    };

    // 5 Personen
    people.forEach((name, i) => {
      pushFinding(name, `«P_${String(i + 1).padStart(2, "0")}»`, "person", i === 3 ? 0.62 : 0.94);
      segs.push({
        kind: "text",
        content: i < people.length - 1 ? ", " : ". Kontaktdaten wurden geprüft: ",
      });
    });

    // 3 E-Mails
    emails.forEach((mail, i) => {
      pushFinding(mail, `«MAIL_${i + 1}»`, "email", 0.97);
      segs.push({ kind: "text", content: i < emails.length - 1 ? ", " : ". Telefonisch erreichbar über " });
    });

    // 3 Telefonnummern
    phones.forEach((tel, i) => {
      pushFinding(tel, `«TEL_${i + 1}»`, "phone", 0.9);
      segs.push({ kind: "text", content: i < phones.length - 1 ? " bzw. " : ". Zahlungen liefen über die Konten " });
    });

    // 2 IBANs
    ibans.forEach((iban, i) => {
      pushFinding(iban, `«IBAN_${i + 1}»`, "iban", 0.99);
      segs.push({ kind: "text", content: i < ibans.length - 1 ? " und " : " zugunsten von " });
    });

    // 2 Organisationen
    orgs.forEach((org, i) => {
      pushFinding(org, `«ORG_${i + 1}»`, "org", i === 1 ? 0.68 : 0.88);
      segs.push({ kind: "text", content: i < orgs.length - 1 ? " sowie " : ". Alle Angaben wurden ins Protokoll übernommen." });
    });

    return segs; // 5 + 3 + 3 + 2 + 2 = 15 Findings
  }

  const lang = buildLang();

  // --- Payload 3: 0 Findings ---
  const leer: Segment[] = [
    {
      kind: "text",
      content:
        "Dies ist ein völlig unverfänglicher Absatz ohne personenbezogene Daten. Er dient nur dazu, den Leerlauf-Fall der Animation zu prüfen — es sollte sofort fertig gemeldet werden.",
    },
  ];

  // --- Payload 4: HTML-Modus (Stufe 2) — annotiertes Dokument wie es
  // richtext::redact liefert, inkl. Finding über eine Tag-Grenze
  // (Fortsetzungs-Span mit data-sz-cont) und Tabelle.
  const htmlAnnotated = [
    '<p>Sehr geehrter <span data-sz-finding data-original="Herr Müller" data-replacement="«P_a4b»" data-entity-type="person" data-confidence="0.93">Herr Müller</span>,</p>',
    '<p>Ihre IBAN <b><span data-sz-finding data-original="DE89370400440532013000" data-replacement="«IBAN_7f2»" data-entity-type="iban" data-confidence="0.99">DE89370400440532013000</span></b> wurde belastet.</p>',
    "<table><tbody><tr><td><b>Kontakt</b></td><td><span data-sz-finding data-original=\"anna.schaefer@example.de\" data-replacement=\"«MAIL_1»\" data-entity-type=\"email\" data-confidence=\"0.97\">anna.schaefer@example.de</span></td></tr></tbody></table>",
    '<p style="color: #6b21a8">Grüße von <span data-sz-finding data-original="Max Mustermann" data-replacement="«P_x9z»" data-entity-type="person" data-confidence="0.94">Max </span><b><span data-sz-finding data-sz-cont data-replacement="" data-entity-type="person" data-confidence="0.94">Mustermann</span></b></p>',
  ].join("");

  const payloads: Payload[] = [
    { title: "Kurz · 3 Findings (mit Umlauten)", note: "Enthält ein Finding mit niedriger Confidence (0,61) — Balken-Optik ist bewusst für alle gleich (einheitlich schwarz).", segments: kurz },
    { title: "Lang · 15 Findings", note: "Testet Wellen-Parallelisierung und das Gesamtbudget bei vielen Fundstellen.", segments: lang },
    { title: "Leer · 0 Findings", note: "Reiner Text — die Komponente meldet sofort fertig.", segments: leer },
    { title: "HTML · 4 Findings (Stufe 2)", note: "Formatiertes Dokument mit Fettdruck, Tabelle und einem Finding über eine Tag-Grenze (Max <b>Mustermann</b> — der zweite Teil kollabiert zu Leere).", segments: [], annotatedHtml: htmlAnnotated },
  ];

  type Style = "slow" | "normal" | "fast" | "off";
  const styles: { key: Style; label: string }[] = [
    { key: "slow", label: "Langsam (≤ 3,5 s)" },
    { key: "normal", label: "Normal (≤ 2,2 s)" },
    { key: "fast", label: "Schnell (≤ 0,7 s)" },
    { key: "off", label: "Aus (sofort)" },
  ];

  // Laufzustand je Payload-Index.
  let activeStyle: Style[] = payloads.map(() => "normal");
  let runToken: number[] = payloads.map(() => 0);
  let mounted: boolean[] = payloads.map(() => false);
  let status: string[] = payloads.map(() => "");
  let startedAt: number[] = payloads.map(() => 0);

  function play(index: number, style: Style) {
    activeStyle[index] = style;
    mounted[index] = true;
    startedAt[index] = performance.now();
    status[index] = "läuft …";
    runToken[index] += 1; // erzwingt Re-Mount via {#key}
    // Reaktivität anstoßen
    activeStyle = activeStyle;
    mounted = mounted;
    status = status;
    runToken = runToken;
  }

  function onDone(index: number) {
    const ms = Math.round(performance.now() - startedAt[index]);
    status[index] = `fertig nach ${ms} ms`;
    status = status;
  }
</script>

<main>
  <header>
    <h1>MarkerText — Demo<span class="badge">?demo=marker</span></h1>
    <p class="sub">
      Test-Harness für die Schwärz-Animation. Wähle je Payload einen
      Animations-Stil; die Komponente wird per Re-Mount neu abgespielt.
    </p>
  </header>

  {#each payloads as p, i (i)}
    <section class="card">
      <h2>{p.title}</h2>
      <p class="note">{p.note}</p>

      <div class="controls">
        {#each styles as s (s.key)}
          <button
            class:primary={activeStyle[i] === s.key && mounted[i]}
            on:click={() => play(i, s.key)}
          >{s.label}</button>
        {/each}
        {#if status[i]}<span class="status">{status[i]}</span>{/if}
      </div>

      <div class="stage">
        {#if mounted[i]}
          {#key runToken[i]}
            <MarkerText
              segments={p.segments}
              contentKind={p.annotatedHtml ? "html" : "plain"}
              annotatedHtml={p.annotatedHtml ?? ""}
              animation={activeStyle[i]}
              on:done={() => onDone(i)}
            />
          {/key}
        {:else}
          <span class="placeholder">Noch nicht abgespielt — Stil wählen.</span>
        {/if}
      </div>
    </section>
  {/each}

  <footer>Nur Test-Harness · keine echten personenbezogenen Daten · kein Tauri</footer>
</main>

<style>
  main { max-width: 720px; margin: 0 auto; padding: 24px; }
  header h1 { margin: 0 0 4px; font-size: 22px; }
  .badge { font-size: 11px; padding: 2px 6px; background: #eff6ff; color: #1d4ed8; border: 1px solid #bfdbfe; border-radius: 4px; vertical-align: middle; margin-left: 8px; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  .sub { color: #6b7280; margin: 0 0 24px; }
  .card { background: white; border: 1px solid #e5e7eb; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .card h2 { margin: 0 0 4px; font-size: 16px; }
  .note { color: #6b7280; font-size: 12px; margin: 0 0 12px; }
  .controls { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; margin-bottom: 14px; }
  button { font: inherit; padding: 6px 12px; border-radius: 6px; border: 1px solid #d1d5db; background: #f9fafb; cursor: pointer; }
  button:hover { background: #f3f4f6; }
  button.primary { background: #2563eb; color: white; border-color: #2563eb; }
  button.primary:hover { background: #1d4ed8; }
  .status { color: #065f46; font-size: 12px; margin-left: 4px; }
  .stage { background: #f7f7f8; border: 1px solid #eef0f2; border-radius: 6px; padding: 14px 16px; min-height: 48px; }
  .placeholder { color: #9ca3af; font-size: 13px; }
  footer { text-align: center; color: #9ca3af; font-size: 12px; margin-top: 24px; padding-top: 12px; border-top: 1px solid #f3f4f6; }
</style>
