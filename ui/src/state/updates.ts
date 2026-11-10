// Updates and "What's new" for the views: a signal over the UpdateController (state/updateCtl.ts).
import { createSignal } from 'solid-js';
import { be } from '../api/backend';
import { t } from '../lib/i18n';
import { parseChangelog } from '../lib/changelog';
import changelog from '../../../CHANGELOG.md?raw';
import { guardUnsaved } from './actions';
import { EMPTY_UPD, UpdateController, type UpdState } from './updateCtl';

const [upd, setUpd] = createSignal<UpdState>(EMPTY_UPD);
export { upd };

let ctl: UpdateController | null = null;

/** First look for a new version a few seconds after start (the app is busy loading), then every 6 hours while it stays open. */
const FIRST_CHECK_MS = 8_000;
const EVERY_MS = 6 * 3_600_000;

export async function initUpdates(): Promise<void> {
  if (ctl) return;
  ctl = new UpdateController(be(), parseChangelog(changelog), { confirmRestart: () => guardUnsaved(t('d.upd.guard')) }, setUpd);
  await ctl.init();
  const auto = () => { if (ctl?.state.autoUpdate) void ctl.check(false); };
  setTimeout(auto, FIRST_CHECK_MS);
  setInterval(auto, EVERY_MS);
}

export const checkUpdates = (manual = true) => ctl?.check(manual);
export const startUpdate = () => ctl?.start();
export const installUpdate = () => ctl?.install();
export const openUpdateDialog = () => ctl?.openDialog();
export const closeUpdateDialog = () => ctl?.closeDialog();
export const setAutoUpdate = (on: boolean) => ctl?.setAuto(on);
export const showWhatsNew = () => ctl?.showNotes();
export const closeWhatsNew = () => ctl?.closeNews();
export const openReleasePage = (version?: string | null) => ctl?.openReleasePage(version);
