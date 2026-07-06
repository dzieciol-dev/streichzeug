// Gemeinsame Typen der Schwärz-Bühne (Vertrag 2.2).
// In eine eigene Datei gezogen, damit App.svelte und StageView.svelte denselben
// Payload-Typ teilen (Svelte-Instanz-Skripte können keine Typen re-exportieren).

export type StageSegment =
  | { kind: "text"; content: string }
  | {
      kind: "finding";
      original: string;
      replacement: string;
      entity_type: string;
      confidence: number;
    };

// Schwärz-Box der Bild-Bühne (Stufe 3): normierte Koordinaten (0–1,
// Ursprung oben links) — direkt als %-Werte fürs CSS-Overlay nutzbar.
export type StageBox = {
  x: number;
  y: number;
  w: number;
  h: number;
  entity_type: string;
  replacement: string;
};

// Payload des `stage://job`-Events (Backend → Frontend, Main-Window).
// Stufe 2: bei content_kind "html" trägt annotated_html die Anzeige
// (segments ist leer) — außer die Anzeige fiel wegen der Caps auf den
// Plain-Preview zurück (dann truncated=true und segments gefüllt).
// Stufe 3: bei "image" tragen image_data_url + boxes die Anzeige,
// segments den erkannten Text darunter; ocr_based schaltet den
// Prüf-Warnhinweis ein.
export type StageJob = {
  job_id: string;
  mode: "reversible" | "strict";
  stash_id: number | null;
  finding_count: number;
  truncated: boolean;
  segments: StageSegment[];
  content_kind: "plain" | "html" | "image";
  annotated_html: string | null;
  image_data_url: string | null;
  boxes: StageBox[];
  ocr_based: boolean;
};
