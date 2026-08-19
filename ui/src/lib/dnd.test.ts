import { describe, expect, it, vi } from 'vitest';

import {
  MAX_IMPORT_BYTES,
  droppedAudioFile,
  droppedAudioPath,
  encodeDrop,
  importDroppedFile,
  isAudioPath,
  parseDrop,
} from './dnd';

describe('parseDrop', () => {
  it('reads a Windows file path after the first separator', () => {
    const payload = parseDrop(encodeDrop('file', 'C:\\Samples\\kick.wav'));
    expect(payload).toEqual({ kind: 'file', ref: 'C:\\Samples\\kick.wav' });
  });

  it('still reads sampler ids that contain a colon', () => {
    expect(parseDrop('inst::sampler:salamander-piano')).toEqual({
      kind: 'inst',
      ref: 'sampler:salamander-piano',
    });
  });

  it('rejects a bare path from Explorer', () => {
    expect(parseDrop('C:\\Samples\\kick.wav')).toBeNull();
  });
});

describe('isAudioPath', () => {
  it('accepts the formats this build decodes, regardless of case', () => {
    expect(isAudioPath('loop.WAV')).toBe(true);
    expect(isAudioPath('C:\\a\\take.mp3')).toBe(true);
    expect(isAudioPath('/packs/hat.flac')).toBe(true);
    expect(isAudioPath('C:\\packs\\hat.ogg')).toBe(true);
  });

  it('refuses a non-audio name', () => {
    expect(isAudioPath('notes.txt')).toBe(false);
    expect(isAudioPath('C:\\Samples')).toBe(false);
  });
});

/** A drop event as WebView2 delivers it: text payload plus OS files. */
function dropEvent(text: string, files: File[]): DragEvent {
  return {
    dataTransfer: { getData: () => text, files },
  } as unknown as DragEvent;
}

describe('droppedAudioFile', () => {
  it('takes the OS file when its name is audio', () => {
    const file = new File([new Uint8Array([1, 2, 3])], 'kick.wav');
    expect(droppedAudioFile(dropEvent('', [file]))).toBe(file);
  });

  it('refuses a non-audio file and an empty drop', () => {
    expect(droppedAudioFile(dropEvent('', [new File([], 'notes.txt')]))).toBeNull();
    expect(droppedAudioFile(dropEvent('', []))).toBeNull();
  });
});

describe('droppedAudioPath', () => {
  it('never reads a path off an OS file — WebView2 has none', () => {
    const file = new File([], 'kick.wav') as File & { path?: string };
    file.path = 'C:\\Electron\\only.wav';
    expect(droppedAudioPath(dropEvent('', [file]))).toBeNull();
  });

  it('still resolves a library file:: payload', () => {
    expect(droppedAudioPath(dropEvent('file::C:\\Samples\\kick.wav', []))).toBe(
      'C:\\Samples\\kick.wav',
    );
  });
});

describe('importDroppedFile', () => {
  it('hands the file name and bytes to the importer and resolves with its path', async () => {
    const file = new File([new Uint8Array([7, 8, 9])], 'kick.wav');
    const importBytes = vi.fn(async () => 'C:\\app\\imports\\kick.wav');
    await expect(importDroppedFile(file, importBytes)).resolves.toBe(
      'C:\\app\\imports\\kick.wav',
    );
    expect(importBytes).toHaveBeenCalledWith('kick.wav', new Uint8Array([7, 8, 9]));
  });

  it('rejects a file over the 100 MB cap without reading it', async () => {
    const file = { name: 'set.wav', size: MAX_IMPORT_BYTES + 1 } as File;
    await expect(importDroppedFile(file, vi.fn())).rejects.toThrow(/capped at 100 MB/);
  });
});
