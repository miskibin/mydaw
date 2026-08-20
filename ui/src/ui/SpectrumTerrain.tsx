/**
 * A 3D frequency landscape: log-spaced bins as hills, history as depth.
 *
 * Loudness is height. Time recedes. This is the picture of the sound, not a
 * decoration around the knobs — you can read a kick, a harshness, a hole,
 * without hearing it.
 */

import { onCleanup, onMount, type JSX } from 'solid-js';
import * as THREE from 'three';

const COLS = 128;
const ROWS = 40;
const FLOOR = -90;

function heightOf(db: number): number {
  return ((Math.min(0, Math.max(FLOOR, db)) - FLOOR) / -FLOOR) * 0.62;
}

function parseCssColor(raw: string, fallback: number): number {
  const hex = raw.trim();
  if (hex.startsWith('#') && (hex.length === 7 || hex.length === 4)) {
    const full =
      hex.length === 4
        ? `#${hex[1]}${hex[1]}${hex[2]}${hex[2]}${hex[3]}${hex[3]}`
        : hex;
    const n = Number.parseInt(full.slice(1), 16);
    return Number.isFinite(n) ? n : fallback;
  }
  return fallback;
}

export function SpectrumTerrain(props: {
  bins: readonly number[] | null | undefined;
}): JSX.Element {
  let host: HTMLDivElement | undefined;

  onMount(() => {
    const el = host;
    if (!el) return;

    const scene = new THREE.Scene();
    scene.fog = new THREE.Fog(0x0b1220, 1.6, 4.2);
    scene.background = new THREE.Color(0x0b1220);

    const camera = new THREE.PerspectiveCamera(28, 1, 0.08, 20);
    camera.position.set(0, 1.15, 2.55);
    camera.lookAt(0, 0.12, 0);

    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false });
    renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
    el.appendChild(renderer.domElement);

    scene.add(new THREE.AmbientLight(0xffffff, 0.42));
    const key = new THREE.DirectionalLight(0xffffff, 1.15);
    key.position.set(-0.8, 1.6, 1.1);
    scene.add(key);
    const fill = new THREE.DirectionalLight(0xffffff, 0.35);
    fill.position.set(1.2, 0.4, 0.4);
    scene.add(fill);

    const low = new THREE.Color();
    const high = new THREE.Color();
    const accent = new THREE.Color();

    // Re-read per frame so a theme toggle recolours the landscape rather than
    // leaving the colours sampled at mount. Everything downstream only moves
    // when the values actually changed.
    let accNow = -1;
    let sunkNow = -1;
    const applyTheme = () => {
      const styles = getComputedStyle(el);
      const acc = parseCssColor(styles.getPropertyValue('--acc'), 0x7aa2f7);
      const sunk = parseCssColor(styles.getPropertyValue('--sunk'), 0x0b1220);
      if (acc === accNow && sunk === sunkNow) return;
      accNow = acc;
      sunkNow = sunk;
      // The terrain must follow the active UI surface in both dark and light
      // themes. Previously bright --sunk values were replaced with a hardcoded
      // navy background, leaving the 3D spectrum dark in light mode.
      const ink = sunk;
      (scene.fog as THREE.Fog).color.setHex(ink);
      (scene.background as THREE.Color).setHex(ink);
      renderer.setClearColor(ink, 1);
      fill.color.setHex(acc);
      accent.setHex(acc);
      low.setHex(ink).lerp(accent, 0.15);
      high.setHex(0xffffff).lerp(accent, 0.35);
    };
    applyTheme();

    const geo = new THREE.PlaneGeometry(2.7, 1.85, COLS - 1, ROWS - 1);
    geo.rotateX(-Math.PI / 2);
    const colors = new Float32Array(geo.attributes.position!.count * 3);
    geo.setAttribute('color', new THREE.BufferAttribute(colors, 3));
    const mat = new THREE.MeshLambertMaterial({
      vertexColors: true,
      side: THREE.DoubleSide,
      flatShading: true,
    });
    const mesh = new THREE.Mesh(geo, mat);
    scene.add(mesh);

    const history: number[][] = Array.from({ length: ROWS }, () => Array(COLS).fill(FLOOR));
    const pos = geo.attributes.position as THREE.BufferAttribute;
    const colAttr = geo.attributes.color as THREE.BufferAttribute;

    const fit = () => {
      const w = Math.max(1, el.clientWidth);
      const h = Math.max(1, el.clientHeight);
      renderer.setSize(w, h, false);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
    };
    fit();
    const ro = new ResizeObserver(fit);
    ro.observe(el);

    let raf = 0;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      applyTheme();
      const incoming = props.bins;
      const row = Array.from({ length: COLS }, (_, i) => {
        if (!incoming || incoming.length < 2) return FLOOR;
        const t = i / (COLS - 1);
        const src = t * (incoming.length - 1);
        const lo = Math.floor(src);
        const hi = Math.min(incoming.length - 1, lo + 1);
        const frac = src - lo;
        return incoming[lo]! * (1 - frac) + incoming[hi]! * frac;
      });
      history.pop();
      history.unshift(row);

      for (let j = 0; j < ROWS; j++) {
        const bins = history[ROWS - 1 - j]!;
        for (let i = 0; i < COLS; i++) {
          const index = j * COLS + i;
          const db = bins[i] ?? FLOOR;
          const y = heightOf(db);
          pos.setY(index, y);
          const t = Math.min(1, Math.max(0, y / 0.62));
          const c = low.clone().lerp(high, t * t);
          colAttr.setXYZ(index, c.r, c.g, c.b);
        }
      }
      pos.needsUpdate = true;
      colAttr.needsUpdate = true;
      geo.computeVertexNormals();
      renderer.render(scene, camera);
    };
    tick();

    onCleanup(() => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      geo.dispose();
      mat.dispose();
      renderer.dispose();
      renderer.domElement.remove();
    });
  });

  return (
    <div
      ref={host}
      class="wv-fx__terrain"
      title="Live spectrum as a landscape. Height is loudness, depth is the last second. Bass is left."
    />
  );
}
