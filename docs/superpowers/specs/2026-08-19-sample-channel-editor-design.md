# Sample channel editor (FL-style)

Status: implementing
Depends on: playlist audio clips, mixer v2, wave studio UI

## What

Dropping a wav on the playlist creates an **Audio Clip channel** in the rack and mixer. The same file reuses that channel. Double-clicking the playlist clip or the rack row opens one floating sample window (volume, pan, pitch, start/end). The clip plays through that channel’s strip (inserts, fader, pan, mute).

Dropping a wav on the rack creates a **Sampler (one-shot) channel**: same window, triggered by notes/steps, root key C4.

## Defaults (FL)

- Stretch mode is **none**: playlist edge-drag crops; pitch is resample.
- Channel settings (vol, pan, pitch, region) are shared by every clip of that file.
- Playlist clip length and lane stay per-instance.
- Deleting the rack channel deletes its playlist clips. Deleting clips leaves the empty channel.
- Out of this step: elastique stretch, envelopes/LFO, precomputed FX, Make unique.

## Routing

```
AudioClip / OneShot node  →  [inserts]  →  fader / pan / mute  →  mix
```

The master-bus clip player is removed. Each audio-clip track head is an `AudioClipNode` that only plays clips of that file. One-shot tracks use a note-driven player of the same file.
