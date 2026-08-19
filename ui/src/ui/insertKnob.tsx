/**
 * One insert's parameters, bound to knobs.
 *
 * Two surfaces now edit the same chain -- the inspector's compact card and the
 * fullscreen effect editor -- and both need the identical drag contract:
 * `set_insert_param` per frame, one whole-mixer refetch on commit, and the local
 * value in between. Writing that twice would be two subtly different opinions
 * about what a drag does, which is exactly what `Knob` being one component
 * exists to prevent, so it is written once here.
 *
 * The accessors are passed in rather than a props object because the caller's
 * `insert` moves under it: a chain refetch replaces the whole `InsertDto`, and a
 * snapshot taken at setup would pin every knob to the values it had when the
 * card was first drawn.
 */

import { createSignal, type JSX } from 'solid-js';

import type { InsertDto, ParamDefDto } from '../api/types';
import { shortParamName } from '../lib/param';
import { insertTargetFor, useAppState, type StripRef } from '../state';
import { Knob } from './Knob';

export interface InsertKnobs {
  /** What to draw for one parameter -- the live value while it is being dragged. */
  valueOf(def: ParamDefDto): number;
  /** A knob wired to one of this insert's parameters. */
  Param(props: {
    def: ParamDefDto;
    size?: number;
    /** Defaults to the parameter's name with its band prefix stripped. */
    label?: string;
  }): JSX.Element;
}

export function createInsertKnobs(source: {
  strip: () => StripRef;
  /** What the status-bar readout calls this: `Kick · Compressor`. */
  owner: () => string;
  insert: () => InsertDto;
}): InsertKnobs {
  const app = useAppState();

  /**
   * The parameter being dragged right now, and where the pointer has put it.
   *
   * `set_insert_param` is the per-frame command and answers with a single
   * number; the whole `MixerState` -- which is where `insert().params` comes
   * from -- is only refetched when the gesture ends. Without this the knob would
   * not move under the pointer.
   */
  const [dragging, setDragging] = createSignal<{ id: string; value: number } | null>(null);

  const target = () => insertTargetFor(source.strip());

  function valueOf(def: ParamDefDto): number {
    const held = dragging();
    if (held?.id === def.id) return held.value;
    return (
      source.insert().params.find((value) => value.id === def.id)?.value ?? def.defaultValue
    );
  }

  function Param(props: { def: ParamDefDto; size?: number; label?: string }): JSX.Element {
    return (
      <Knob
        def={props.def}
        typeId={source.insert().typeId}
        label={props.label ?? shortParamName(props.def.name)}
        owner={source.owner()}
        size={props.size ?? 28}
        color={source.insert().bypassed ? 'var(--dim)' : 'var(--live)'}
        value={() => valueOf(props.def)}
        // Per frame, and one atomic store: the engine answers with what it
        // actually kept, which the next whole-mixer reply carries.
        onInput={(value) => {
          setDragging({ id: props.def.id, value });
          void app.backend
            .setInsertParam(target(), source.insert().index, props.def.id, value)
            .catch(app.report);
        }}
        onCommit={() => {
          void app
            .mixerCommand((backend) => backend.getTracks())
            .finally(() => setDragging(null));
        }}
      />
    );
  }

  return { valueOf, Param };
}
