/**
 * HTML5 drag payloads the browser, picker, rack and playlist share.
 *
 * One `kind::ref` string on `text/plain`, because that is the type every
 * webview actually delivers. A custom MIME would be prettier and is stripped
 * by WebView2.
 */

export type DropKind = 'inst' | 'audio' | 'pattern' | 'file';

export interface DropPayload {
  kind: DropKind;
  ref: string;
}

/** Extensions the engine can decode. Keep in lockstep with `AUDIO_EXTENSIONS`. */
const AUDIO_EXTENSIONS = ['wav', 'flac', 'ogg', 'oga', 'mp3'] as const;

/** Encode a payload. The drop side must use {@link parseDrop}, not `split`. */
export function encodeDrop(kind: DropKind, ref: string | number): string {
  return `${kind}::${ref}`;
}

/**
 * Read a payload back. `null` when the string is not one of ours — a file
 * path, a URL, or empty data from a cancelled native drag.
 *
 * Split at the first `::` rather than on every one: a sampler id is
 * `inst::sampler:salamander-piano`, and a naive `split('::')` would still
 * happen to work, but a plugin id that contained `::` would not. A Windows
 * path after `file::` is `C:\takes\loop.wav`.
 */
export function parseDrop(raw: string): DropPayload | null {
  const sep = raw.indexOf('::');
  if (sep <= 0) return null;
  const kind = raw.slice(0, sep);
  const ref = raw.slice(sep + 2);
  if (
    (kind !== 'inst' && kind !== 'audio' && kind !== 'pattern' && kind !== 'file') ||
    ref.length === 0
  ) {
    return null;
  }
  return { kind, ref };
}

/** Whether `path` has an extension this build can decode. */
export function isAudioPath(path: string): boolean {
  const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  const name = slash >= 0 ? path.slice(slash + 1) : path;
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return false;
  const ext = name.slice(dot + 1).toLowerCase();
  return (AUDIO_EXTENSIONS as readonly string[]).includes(ext);
}

/**
 * A path to load as an audio clip: a `file::` payload from the library
 * browser. An OS drop never resolves here — WebView2 has no `File.path`
 * (that is an Electron extension), so it goes through
 * {@link droppedAudioFile} instead.
 */
export function droppedAudioPath(event: DragEvent): string | null {
  const transfer = event.dataTransfer;
  if (!transfer) return null;
  const encoded = parseDrop(transfer.getData('text/plain') ?? '');
  if (encoded?.kind === 'file' && isAudioPath(encoded.ref)) return encoded.ref;
  return null;
}

/**
 * Hard cap on an OS-dropped file. The bytes travel through one JSON IPC
 * message, and a message past this stalls the webview for longer than any
 * sample is worth.
 */
export const MAX_IMPORT_BYTES = 100 * 1024 * 1024;

/**
 * The OS file in a drop, when its name says it is audio this build decodes.
 *
 * WebView2 never exposes a dropped file's path, and Tauri's native drop
 * handler is off (`dragDropEnabled: false`, deliberately — it would intercept
 * the internal HTML5 drags). The only way in is the file's *bytes*: the
 * caller must take the `File` synchronously (the `DataTransfer` is neutered
 * after the handler returns) and feed it to {@link importDroppedFile}.
 */
export function droppedAudioFile(event: DragEvent): File | null {
  const transfer = event.dataTransfer;
  if (!transfer || transfer.files.length === 0) return null;
  const file = transfer.files[0]!;
  return isAudioPath(file.name) ? file : null;
}

/**
 * Read a dropped file's bytes and import them, resolving with the absolute
 * path the backend wrote — which then rides the existing `file::` pipeline.
 *
 * `importBytes` is `Backend.importDroppedAudio`; passed in so this module
 * stays backend-free and the size/read logic stays testable.
 */
export async function importDroppedFile(
  file: File,
  importBytes: (fileName: string, bytes: Uint8Array) => Promise<string>,
): Promise<string> {
  if (file.size > MAX_IMPORT_BYTES) {
    throw new Error(
      `${file.name} is ${Math.round(file.size / (1024 * 1024))} MB — dropped files are capped at ${
        MAX_IMPORT_BYTES / (1024 * 1024)
      } MB`,
    );
  }
  return importBytes(file.name, new Uint8Array(await file.arrayBuffer()));
}

/**
 * Mark this element as a drop target.
 *
 * `preventDefault` is required or the drop never fires. `dropEffect` must
 * match the source's `effectAllowed` (`copy`); WebView2 otherwise shows the
 * "not allowed" cursor and swallows the drop.
 *
 * Tauri's native file-drop handler (`dragDropEnabled`) intercepts HTML5
 * drags on Windows. It is switched off in `app/tauri.conf.json` so this
 * path can work.
 */
export function allowDrop(event: DragEvent, effect: DataTransfer['dropEffect'] = 'copy'): void {
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = effect;
}
