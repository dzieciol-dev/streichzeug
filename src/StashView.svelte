<!--
  StashView — die Ablage (WP-D).

  Listet die geschwärzten Einträge (`stash_list`, neueste zuerst) mit Titel,
  lokalisiertem Datum und Entity-Chips. Pro Eintrag: Kopieren / Löschen.
  „Alle löschen" als Danger-Aktion. Leerer Zustand erklärt den Capture-Hotkey.

  Es wird ausschließlich der geschwärzte Text gespeichert (Vertrag 2.4) — die
  Ablage enthält nie Originaltext.

  Svelte 5 Legacy-Modus: export let + onMount, keine Runes.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { prettyHotkey } from "./hotkey";

  type StashMeta = {
    id: number;
    created_at: string; // ISO-8601 UTC
    mode: "reversible" | "strict";
    title: string;
    entity_counts: Record<string, number>;
    char_len: number;
  };

  // Capture-Hotkey für den Hinweis im leeren Zustand.
  export let stageHotkey = "";

  // Deutsche Labels je Entity-Typ (snake_case-Werte aus dem Backend).
  const ENTITY_LABELS: Record<string, string> = {
    person: "Person",
    location: "Ort",
    organization: "Organisation",
    email: "E-Mail",
    phone: "Telefon",
    iban: "IBAN",
    credit_card: "Kreditkarte",
    steuer_id: "Steuer-ID",
    date: "Datum",
    url: "URL",
  };

  let entries: StashMeta[] = [];
  let loaded = false;
  let loadError = "";
  let actionStatus = "";

  function entityLabel(key: string): string {
    return ENTITY_LABELS[key] ?? key;
  }

  function formatDate(iso: string): string {
    // created_at ist ISO-8601 UTC (Vertrag 2.3). Lokalisiert anzeigen.
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleString("de-DE", {
      day: "2-digit",
      month: "2-digit",
      year: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  // entity_counts als stabile Liste [ [typ, anzahl], … ] für das Rendering.
  function chips(counts: Record<string, number>): [string, number][] {
    return Object.entries(counts ?? {}).sort((a, b) => a[0].localeCompare(b[0]));
  }

  async function load() {
    try {
      entries = await invoke<StashMeta[]>("stash_list");
      loadError = "";
    } catch (e) {
      loadError = `Ablage konnte nicht geladen werden: ${e}`;
    } finally {
      loaded = true;
    }
  }

  async function copyEntry(id: number) {
    actionStatus = "…";
    try {
      await invoke("stash_copy", { id });
      actionStatus = "In die Zwischenablage kopiert.";
    } catch (e) {
      actionStatus = `Fehler beim Kopieren: ${e}`;
    }
  }

  async function deleteEntry(id: number) {
    actionStatus = "…";
    try {
      await invoke("stash_delete", { id });
      await load();
      actionStatus = "Eintrag gelöscht.";
    } catch (e) {
      actionStatus = `Fehler beim Löschen: ${e}`;
    }
  }

  async function clearAll() {
    actionStatus = "…";
    try {
      const n = await invoke<number>("stash_clear");
      await load();
      actionStatus = `${n} Eintrag${n === 1 ? "" : "e"} gelöscht.`;
    } catch (e) {
      actionStatus = `Fehler beim Löschen: ${e}`;
    }
  }

  onMount(load);
</script>

<section class="card">
  <div class="head">
    <h2>Ablage</h2>
    {#if entries.length > 0}
      <button class="danger" on:click={clearAll}>Alle löschen</button>
    {/if}
  </div>

  {#if loadError}
    <p class="save-error" role="alert">{loadError}</p>
  {/if}

  {#if !loaded}
    <p class="hint">Wird geladen …</p>
  {:else if entries.length === 0}
    <p class="usage empty">
      Noch nichts abgelegt. Markiere Text in einer beliebigen App und drücke
      {#if stageHotkey}
        <kbd>{prettyHotkey(stageHotkey)}</kbd>
      {:else}
        den Capture-Hotkey
      {/if}
      — die geschwärzte Fassung landet dann hier und in der Zwischenablage.
    </p>
  {:else}
    <ul class="stash-list">
      {#each entries as e (e.id)}
        <li class="stash-item">
          <div class="item-head">
            <span class="title" title={e.title}>{e.title || "(leer)"}</span>
            {#if e.mode === "strict"}
              <span class="strict-badge" title="Strict-Modus — kein Mapping gespeichert">Strict</span>
            {/if}
          </div>
          <div class="meta">
            <span class="date">{formatDate(e.created_at)}</span>
            <div class="chips">
              {#each chips(e.entity_counts) as [typ, anzahl] (typ)}
                <span class="status-badge on">{entityLabel(typ)} · {anzahl}</span>
              {/each}
            </div>
          </div>
          <div class="actions">
            <button on:click={() => copyEntry(e.id)}>Kopieren</button>
            <button class="danger" on:click={() => deleteEntry(e.id)}>Löschen</button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}

  {#if actionStatus}
    <p class="hint" style="margin-top: 8px;">{actionStatus}</p>
  {/if}
</section>

<style>
  .head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .head h2 { margin: 0; font-size: 16px; }
  .usage { font-size: 15px; line-height: 1.5; }
  .empty { color: #6b7280; }
  .save-error { background: #fef2f2; border: 1px solid #fecaca; color: #991b1b; border-radius: 8px; padding: 10px 12px; font-size: 13px; }
  .hint { color: #6b7280; font-size: 12px; margin: 8px 0 0; }
  kbd { background: #f3f4f6; border: 1px solid #d1d5db; padding: 1px 6px; border-radius: 3px; font-size: 12px; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }

  .stash-list { list-style: none; margin: 12px 0 0; padding: 0; }
  .stash-item { border: 1px solid #e5e7eb; border-radius: 8px; padding: 12px; margin-bottom: 10px; }
  .item-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
  .title { font-size: 14px; font-weight: 500; color: #1a1a1a; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .strict-badge { flex-shrink: 0; font-size: 10px; padding: 2px 6px; border-radius: 10px; background: #f3f4f6; color: #6b7280; border: 1px solid #e5e7eb; font-weight: 500; }
  .meta { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 10px; }
  .date { color: #6b7280; font-size: 12px; }
  .chips { display: flex; gap: 6px; flex-wrap: wrap; }
  .status-badge { font-size: 11px; padding: 3px 8px; border-radius: 12px; font-weight: 500; }
  .status-badge.on { background: #ecfdf5; color: #065f46; border: 1px solid #a7f3d0; }
  .actions { display: flex; gap: 8px; }
  button { font: inherit; padding: 5px 12px; border-radius: 6px; border: 1px solid #d1d5db; background: #f9fafb; cursor: pointer; }
  button:hover { background: #f3f4f6; }
  button.danger { background: #fef2f2; color: #991b1b; border-color: #fecaca; }
  button.danger:hover { background: #fee2e2; }
</style>
