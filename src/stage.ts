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

// Payload des `stage://job`-Events (Backend → Frontend, Main-Window).
// Stufe 2: bei content_kind "html" trägt annotated_html die Anzeige
// (segments ist leer) — außer die Anzeige fiel wegen des Zeichen-Caps auf
// den Plain-Preview zurück (dann truncated=true und segments gefüllt).
export type StageJob = {
  job_id: string;
  mode: "reversible" | "strict";
  stash_id: number | null;
  finding_count: number;
  truncated: boolean;
  segments: StageSegment[];
  content_kind: "plain" | "html";
  annotated_html: string | null;
};
