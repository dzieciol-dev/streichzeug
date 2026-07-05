<!--
  Widget.svelte — Inhalt des schwebenden Mini-Panels (eigenes Fenster,
  Label "widget", siehe src-tauri/src/widget.rs).

  Ein einziger runder Marker-Button, der beides kann:
   - **Klick** (Loslassen ohne Bewegung) → voller Capture-Flow
     (synthetisches Cmd+C auf die Markierung der aktiven App).
   - **Ziehen** (Bewegung über den Schwellwert) → Fenster verschieben via
     `startDragging()`. Bewusst selbst unterschieden statt
     `data-tauri-drag-region`: die Drag-Region schluckt Klicks, und eine
     separate Griff-Zone war in der Beta zu klein zum Treffen.

  Das Panel ist nicht-aktivierend: der Klick nimmt der Quell-App den Fokus
  nicht weg — deshalb funktioniert hier, was bei Dock/Tray nicht geht.

  Svelte 5 Legacy-Modus.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  // Dock-Icon der App (Kopie von src-tauri/icons/128x128@2x.png) — das Widget
  // soll aussehen wie das Dock-Symbol, nur klein und schwebend.
  import iconUrl from "./assets/widget-icon.png";

  let busy = false;

  // Drag-Erkennung: mousedown merkt die Startposition; überschreitet die
  // Bewegung den Schwellwert, übernimmt das native Fenster-Dragging (das
  // mouseup kommt dann nie im DOM an). Ein mouseup ohne Drag ist ein Klick.
  const DRAG_THRESHOLD_PX = 4;
  let pressed = false;
  let dragging = false;
  let startX = 0;
  let startY = 0;

  function onMouseDown(e: MouseEvent) {
    if (e.button !== 0) return;
    pressed = true;
    dragging = false;
    startX = e.screenX;
    startY = e.screenY;
  }

  async function onMouseMove(e: MouseEvent) {
    if (!pressed || dragging) return;
    const dx = e.screenX - startX;
    const dy = e.screenY - startY;
    if (dx * dx + dy * dy >= DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX) {
      dragging = true;
      pressed = false;
      try {
        await getCurrentWindow().startDragging();
      } catch (err) {
        console.error("startDragging failed", err);
      }
      dragging = false;
    }
  }

  // Rechtsklick → natives Kontextmenü (Backend baut und zeigt es; ein
  // HTML-Menü würde am 56-px-Fensterrand abgeschnitten).
  async function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    pressed = false;
    try {
      await invoke("widget_menu");
    } catch (err) {
      console.error("widget_menu failed", err);
    }
  }

  async function onMouseUp(e: MouseEvent) {
    if (e.button !== 0 || !pressed) return;
    pressed = false;
    if (dragging) return;
    if (busy) return;
    busy = true;
    try {
      await invoke("stage_capture");
    } catch (err) {
      console.error("stage_capture failed", err);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    // Fenster-Transparenz: das NSWindow ist bereits clear (widget.rs) —
    // hier muss auch die Seite selbst durchsichtig sein, sonst malt die
    // Webview ein weißes Quadrat um den runden Button.
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
  });
</script>

<!-- svelte-ignore a11y-no-static-element-interactions -->
<div
  class="widget"
  on:mousedown={onMouseDown}
  on:mousemove={onMouseMove}
  on:mouseup={onMouseUp}
  on:contextmenu={onContextMenu}
>
  <img
    class="app-icon"
    class:busy
    src={iconUrl}
    alt="Markierung schwärzen"
    title="Klick: Markierung schwärzen · Ziehen: verschieben"
    draggable="false"
  />
</div>

<style>
  .widget {
    width: 56px;
    height: 56px;
    display: flex;
    align-items: center;
    justify-content: center;
    user-select: none;
    -webkit-user-select: none;
  }

  /* Das Dock-Icon der App, klein und schwebend. Die Form (abgerundetes
     Rechteck) bringt das PNG selbst mit — kein eigener Kreis/Hintergrund.
     pointer-events: none, weil Klick/Drag über den .widget-Container laufen. */
  .app-icon {
    width: 50px;
    height: 50px;
    /* Das Tauri-Icon-PNG ist ein randloses Quadrat — die macOS-typische
       Rundung (Superellipse ≈ 22,5 % Radius) kommt hier aus dem CSS,
       sonst sieht man harte Ecken/Ränder. */
    border-radius: 22.5%;
    pointer-events: none;
    -webkit-user-drag: none;
    filter: drop-shadow(0 2px 6px rgba(0, 0, 0, 0.35));
    opacity: 0.88;
    transition:
      transform 120ms ease,
      opacity 120ms ease;
  }
  .widget:hover .app-icon {
    opacity: 1;
    transform: scale(1.07);
  }
  .widget:active .app-icon {
    transform: scale(0.96);
  }

  /* Während des Captures pulsiert das Icon dezent. */
  .app-icon.busy {
    animation: widget-pulse 700ms ease-in-out infinite;
  }
  @keyframes widget-pulse {
    0%, 100% { transform: scale(1); opacity: 1; }
    50% { transform: scale(0.9); opacity: 0.75; }
  }

  @media (prefers-reduced-motion: reduce) {
    .app-icon,
    .app-icon.busy {
      transition: none;
      animation: none;
    }
  }
</style>
