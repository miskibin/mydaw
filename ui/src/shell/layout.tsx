import {
  createContext,
  createSignal,
  onCleanup,
  useContext,
  type Accessor,
  type JSX,
} from 'solid-js';

/**
 * Which of the two bottom panels is showing, and what the editor is editing.
 *
 * These are view state and nothing else -- no command is sent when they change,
 * and nothing about the engine depends on them -- so they live here rather than
 * in `state.tsx`, which is the boundary with Rust. Keeping the two apart is what
 * stops "is the inspector open" from ending up in the project file.
 */
export type BottomTab = 'mixer' | 'editor';
export type EditorMode = 'notes' | 'steps';
export type BrowserTab = 'sounds' | 'devices' | 'projects';

export interface Layout {
  readonly browserOpen: Accessor<boolean>;
  readonly inspectorOpen: Accessor<boolean>;
  readonly bottomTab: Accessor<BottomTab>;
  readonly editorMode: Accessor<EditorMode>;
  readonly browserTab: Accessor<BrowserTab>;
  /** Whether the bottom panel has taken the whole centre column (F7). */
  readonly maximised: Accessor<boolean>;
  /** Whether the device palette is up. */
  readonly paletteOpen: Accessor<boolean>;
  toggleBrowser(): void;
  toggleInspector(): void;
  setBottomTab(tab: BottomTab): void;
  setEditorMode(mode: EditorMode): void;
  setBrowserTab(tab: BrowserTab): void;
  toggleMaximised(): void;
  setPaletteOpen(open: boolean): void;
}

const LayoutContext = createContext<Layout>();

/** Below this the browser folds itself away. Same breakpoint as the mock. */
const BROWSER_BREAKPOINT = 1180;
/** And below this the inspector does. */
const INSPECTOR_BREAKPOINT = 940;

export function LayoutProvider(props: { children: JSX.Element }): JSX.Element {
  const [browserOpen, setBrowserOpen] = createSignal(true);
  const [inspectorOpen, setInspectorOpen] = createSignal(true);
  const [bottomTab, setBottomTab] = createSignal<BottomTab>('mixer');
  const [editorMode, setEditorMode] = createSignal<EditorMode>('notes');
  const [browserTab, setBrowserTab] = createSignal<BrowserTab>('sounds');
  const [maximised, setMaximised] = createSignal(false);
  const [paletteOpen, setPaletteOpen] = createSignal(false);

  /**
   * Fold the side panes away on a narrow window, and put them back when there is
   * room again -- but only the ones *this* folded away.
   *
   * The `auto` flags are what make that distinction: a pane the user closed by
   * hand stays closed when the window is widened, which is the difference
   * between a responsive layout and one that argues with you.
   */
  let autoBrowser = false;
  let autoInspector = false;

  const autofit = () => {
    const width = window.innerWidth;
    if (width < BROWSER_BREAKPOINT && browserOpen()) {
      autoBrowser = true;
      setBrowserOpen(false);
    } else if (width >= BROWSER_BREAKPOINT && autoBrowser && !browserOpen()) {
      autoBrowser = false;
      setBrowserOpen(true);
    }
    if (width < INSPECTOR_BREAKPOINT && inspectorOpen()) {
      autoInspector = true;
      setInspectorOpen(false);
    } else if (width >= INSPECTOR_BREAKPOINT && autoInspector && !inspectorOpen()) {
      autoInspector = false;
      setInspectorOpen(true);
    }
  };

  if (typeof window !== 'undefined') {
    window.addEventListener('resize', autofit);
    autofit();
    onCleanup(() => window.removeEventListener('resize', autofit));
  }

  const layout: Layout = {
    browserOpen,
    inspectorOpen,
    bottomTab,
    editorMode,
    browserTab,
    maximised,
    paletteOpen,
    toggleBrowser() {
      autoBrowser = false;
      setBrowserOpen((open) => !open);
    },
    toggleInspector() {
      autoInspector = false;
      setInspectorOpen((open) => !open);
    },
    setBottomTab,
    setEditorMode,
    setBrowserTab,
    toggleMaximised: () => setMaximised((max) => !max),
    setPaletteOpen,
  };

  return <LayoutContext.Provider value={layout}>{props.children}</LayoutContext.Provider>;
}

export function useLayout(): Layout {
  const layout = useContext(LayoutContext);
  if (!layout) throw new Error('useLayout must be used inside <LayoutProvider>');
  return layout;
}
