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
export type StageJob = {
  job_id: string;
  mode: "reversible" | "strict";
  stash_id: number | null;
  finding_count: number;
  truncated: boolean;
  segments: StageSegment[];
};
