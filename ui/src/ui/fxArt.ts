// Coloured flat effect icons (viewBox 0 0 100 100, no background). Most come from the
// owner's set (2026-11-09); reverb, fet, deess, denoise, voice and vst3 are drawn in the same
// style. Rendered via innerHTML by FxTile, so keep them static markup without ids.
import type { FxKind } from '../api/types';

const svg = (body: string) => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" fill="none" width="100%" height="100%">${body}</svg>`;

export const FX_ART: Record<FxKind, string> = {
  reverb: svg(`
  <polygon points="50,12 86,32 50,52 14,32" fill="#38BDF8"/>
  <polygon points="14,32 50,52 50,88 14,68" fill="#0284C7"/>
  <polygon points="50,52 86,32 86,68 50,88" fill="#0369A1"/>
  <polygon points="50,12 86,32 86,68 50,88 14,68 14,32" stroke="#7DD3FC" stroke-width="3.5" stroke-linejoin="round"/>
  <polyline points="14,32 50,52 86,32" stroke="#7DD3FC" stroke-width="2.5" stroke-linejoin="round"/>
  <line x1="50" y1="52" x2="50" y2="88" stroke="#7DD3FC" stroke-width="2.5"/>
  <circle cx="50" cy="52" r="22" stroke="#BAE6FD" stroke-width="2.5" stroke-opacity="0.5"/>
  <circle cx="50" cy="52" r="13" stroke="#E0F2FE" stroke-width="3" stroke-opacity="0.85"/>
  <circle cx="50" cy="52" r="5" fill="#FFFFFF"/>`),
  delay: svg(`
  <circle cx="67" cy="41" r="21" fill="#BCEAD5" stroke="#59B889" stroke-width="2" stroke-dasharray="3 3"/>
  <polyline points="67,29 67,41 75,45" stroke="#59B889" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="53" cy="46" r="24" fill="#74C69D" stroke="#2D6A4F" stroke-width="2"/>
  <polyline points="53,32 53,46 63,51" stroke="#1B4332" stroke-width="3" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="36" cy="52" r="27" fill="#2DC653" stroke="#1B4332" stroke-width="3"/>
  <circle cx="36" cy="52" r="22" stroke="#208B3A" stroke-width="2"/>
  <line x1="36" y1="29" x2="36" y2="33" stroke="#10451D" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="36" y1="71" x2="36" y2="75" stroke="#10451D" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="13" y1="52" x2="17" y2="52" stroke="#10451D" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="55" y1="52" x2="59" y2="52" stroke="#10451D" stroke-width="2.5" stroke-linecap="round"/>
  <polyline points="36,36 36,52 47,58" stroke="#10451D" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="36" cy="52" r="3" fill="#10451D"/>`),
  eq: svg(`
  <polygon points="20,48 40,34 60,62 80,38 80,82 20,82" fill="#FDE68A"/>
  <rect x="17" y="14" width="6" height="72" rx="3" fill="#B48A1E"/>
  <rect x="37" y="14" width="6" height="72" rx="3" fill="#B48A1E"/>
  <rect x="57" y="14" width="6" height="72" rx="3" fill="#B48A1E"/>
  <rect x="77" y="14" width="6" height="72" rx="3" fill="#B48A1E"/>
  <polyline points="20,48 40,34 60,62 80,38" stroke="#1E1E24" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/>
  <polyline points="20,48 40,34 60,62 80,38" stroke="#FACC15" stroke-width="3.5" stroke-linecap="round" stroke-linejoin="round"/>
  <circle cx="20" cy="48" r="7" fill="#FACC15" stroke="#1E1E24" stroke-width="2"/>
  <circle cx="40" cy="34" r="7" fill="#FACC15" stroke="#1E1E24" stroke-width="2"/>
  <circle cx="60" cy="62" r="7" fill="#FACC15" stroke="#1E1E24" stroke-width="2"/>
  <circle cx="80" cy="38" r="7" fill="#FACC15" stroke="#1E1E24" stroke-width="2"/>`),
  comp: svg(`
  <polygon points="45,12 55,12 55,24 62,24 50,38 38,24 45,24" fill="#F97316"/>
  <polygon points="45,88 55,88 55,76 62,76 50,62 38,76 45,76" fill="#F97316"/>
  <polygon points="10,45 10,55 22,55 22,62 36,50 22,38 22,45" fill="#F97316"/>
  <polygon points="90,45 90,55 78,55 78,62 64,50 78,38 78,45" fill="#F97316"/>
  <polygon points="37,50 40,44 42,56 45,40 48,60 50,37 52,63 55,40 58,56 60,44 63,50 60,53 58,48 55,58 52,42 50,61 48,42 45,58 42,48 40,53" fill="#FB923C" stroke="#F97316" stroke-width="2" stroke-linejoin="round"/>`),
  sat: svg(`
  <polygon points="14,52 19,28 31,28 36,52" fill="#DC2626"/>
  <polygon points="36,52 41,76 53,76 58,52" fill="#DC2626"/>
  <polygon points="58,52 63,28 75,28 80,52" fill="#DC2626"/>
  <polygon points="80,52 85,76 92,76 95,52" fill="#DC2626"/>
  <polyline points="14,52 19,28 31,28 41,76 53,76 63,28 75,28 85,76 92,76 95,52" stroke="#EF4444" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>
  <polygon points="46,12 41,22 46,22 44,30 51,19 46,19" fill="#FACC15"/>
  <polygon points="78,10 70,22 76,22 72,31 83,18 77,18" fill="#FACC15"/>
  <polygon points="28,8 24,17 28,17 26,23 32,14 28,14" fill="#EF4444"/>
  <polygon points="84,26 79,32 83,33 80,38 88,30 84,29" fill="#EF4444"/>
  <polygon points="16,66 24,70 21,73 27,76 18,74 20,71" fill="#EF4444"/>
  <polygon points="32,72 24,84 30,84 26,93 37,80 31,80" fill="#FACC15"/>
  <polygon points="62,76 58,85 62,85 60,92 66,82 62,82" fill="#FACC15"/>
  <polygon points="74,80 71,88 75,88 73,94 79,85 75,85" fill="#EF4444"/>`),
  tune: svg(`
  <line x1="10" y1="22" x2="90" y2="22" stroke="#F472B6" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="10" y1="36" x2="90" y2="36" stroke="#DB2777" stroke-width="2" stroke-linecap="round"/>
  <line x1="10" y1="50" x2="90" y2="50" stroke="#DB2777" stroke-width="2" stroke-linecap="round"/>
  <line x1="10" y1="64" x2="66" y2="64" stroke="#DB2777" stroke-width="2" stroke-linecap="round"/>
  <line x1="84" y1="64" x2="90" y2="64" stroke="#DB2777" stroke-width="2" stroke-linecap="round"/>
  <line x1="10" y1="78" x2="90" y2="78" stroke="#F472B6" stroke-width="2.5" stroke-linecap="round"/>
  <circle cx="18" cy="43" r="4.5" fill="#BE185D"/>
  <path d="M 18,43 C 18,32 32,32 32,45 C 32,58 20,66 13,71" stroke="#BE185D" stroke-width="4.5" stroke-linecap="round"/>
  <circle cx="38" cy="40" r="2.5" fill="#BE185D"/>
  <circle cx="38" cy="52" r="2.5" fill="#BE185D"/>
  <ellipse cx="47" cy="73" rx="9" ry="6.5" transform="rotate(-20 47 73)" fill="#EC4899"/>
  <rect x="52" y="25" width="4.5" height="46" rx="2" fill="#EC4899"/>
  <path d="M 55,26 C 65,30 69,38 66,50 C 64,41 60,36 55,34 Z" fill="#EC4899"/>
  <polygon points="78,25 71,36 75,36 75,47 81,47 81,36 85,36" fill="#EC4899"/>
  <polygon points="78,75 71,64 75,64 75,53 81,53 81,64 85,64" fill="#EC4899"/>`),
  limit: svg(`
  <rect x="10" y="14" width="80" height="10" fill="#EF4444"/>
  <rect x="10" y="24" width="80" height="5" fill="#991B1B"/>
  <path d="M 10,60 Q 13,52 16,60 T 22,58 Q 25,40 28,62 Q 31,74 34,54 Q 38,28 41,58 Q 44,82 47,48 Q 50,24 53,55 Q 56,88 59,45 Q 62,28 65,60 Q 68,80 71,52 Q 74,38 77,62 Q 80,72 83,56 Q 86,48 90,60" stroke="#D97706" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>
  <polygon points="10,58 14,52 18,62 22,50 26,65 30,42 35,72 40,34 44,78 49,31 51,31 55,84 60,34 65,76 70,42 75,68 80,50 85,63 90,56 90,60 85,67 80,54 75,72 70,46 65,80 60,38 55,88 50,35 44,82 40,38 35,76 30,46 26,69 22,54 18,66 14,56 10,60" fill="#FACC15" stroke="#F59E0B" stroke-width="3" stroke-linejoin="round"/>`),
  voice: svg(`
  <path d="M 46,80 L 90,80 C 90,69 78,66 74,61 C 71,57 72,53 75,49 C 79,44 81,36 78,28 C 75,20 65,18 58,22 L 58,80 Z" fill="#F48FB1" stroke="#1E1E24" stroke-width="2.5" stroke-linejoin="round"/>
  <path d="M 30,80 L 76,80 C 76,69 64,66 60,61 C 57,57 58,53 61,49 C 65,44 67,36 64,28 C 61,20 51,18 44,22 L 30,80 Z" fill="#BA68C8" stroke="#1E1E24" stroke-width="2.5" stroke-linejoin="round"/>
  <path d="M 12,80 C 12,69 24,66 28,61 C 31,57 30,53 27,49 C 23,44 21,36 24,28 C 27,22 33,19 38,19 C 43,19 49,22 52,28 C 55,36 53,44 49,49 C 46,53 45,57 48,61 C 52,66 64,69 64,80 Z" fill="#7B2CBF" stroke="#1E1E24" stroke-width="2.5" stroke-linejoin="round"/>`),
  fet: svg(`
  <rect x="8" y="18" width="84" height="64" rx="8" fill="#C2410C"/>
  <rect x="15" y="25" width="70" height="44" rx="4" fill="#FFEDD5"/>
  <path d="M 24,58 A 30,30 0 0 1 76,58" stroke="#9A3412" stroke-width="2.5"/>
  <path d="M 64,40 A 30,30 0 0 1 76,58" stroke="#EF4444" stroke-width="4"/>
  <line x1="27" y1="47" x2="31" y2="50" stroke="#9A3412" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="50" y1="33" x2="50" y2="38" stroke="#9A3412" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="73" y1="47" x2="69" y2="50" stroke="#9A3412" stroke-width="2.5" stroke-linecap="round"/>
  <line x1="50" y1="64" x2="36" y2="36" stroke="#1E1E24" stroke-width="3.5" stroke-linecap="round"/>
  <circle cx="50" cy="64" r="4.5" fill="#1E1E24"/>
  <circle cx="22" cy="76" r="3" fill="#FDBA74"/>
  <circle cx="78" cy="76" r="3" fill="#FDBA74"/>`),
  deess: svg(`
  <path d="M 70,24 C 64,15 55,12 47,12 C 33,12 24,20 24,31 C 24,57 76,45 76,70 C 76,82 65,89 51,89 C 40,89 31,85 25,76" stroke="#0891B2" stroke-width="15" stroke-linecap="round"/>
  <path d="M 70,24 C 64,15 55,12 47,12 C 33,12 24,20 24,31 C 24,57 76,45 76,70 C 76,82 65,89 51,89 C 40,89 31,85 25,76" stroke="#67E8F9" stroke-width="7" stroke-linecap="round"/>
  <rect x="6" y="46" width="88" height="9" rx="4.5" fill="#1E1E24"/>
  <rect x="9" y="48.5" width="82" height="4" rx="2" fill="#F43F5E"/>`),
  denoise: svg(`
  <path d="M 8,50 C 14,30 20,30 26,50 C 32,70 38,70 44,50 C 48,38 52,38 54,50" stroke="#3B82F6" stroke-width="7" stroke-linecap="round"/>
  <path d="M 8,50 C 14,30 20,30 26,50 C 32,70 38,70 44,50 C 48,38 52,38 54,50" stroke="#BFDBFE" stroke-width="2.5" stroke-linecap="round"/>
  <circle cx="62" cy="40" r="4" fill="#93C5FD"/>
  <circle cx="66" cy="60" r="3.5" fill="#93C5FD"/>
  <circle cx="74" cy="30" r="3" fill="#BFDBFE"/>
  <circle cx="76" cy="50" r="3" fill="#BFDBFE"/>
  <circle cx="80" cy="70" r="2.5" fill="#DBEAFE"/>
  <circle cx="86" cy="40" r="2" fill="#DBEAFE"/>
  <circle cx="90" cy="58" r="2" fill="#DBEAFE"/>
  <circle cx="70" cy="76" r="2" fill="#BFDBFE"/>
  <circle cx="84" cy="22" r="1.8" fill="#DBEAFE"/>`),
  vst3: svg(`
  <rect x="14" y="26" width="72" height="58" rx="8" fill="#7C3AED" stroke="#1E1E24" stroke-width="2.5"/>
  <rect x="32" y="10" width="9" height="18" rx="2" fill="#A78BFA" stroke="#1E1E24" stroke-width="2.5"/>
  <rect x="59" y="10" width="9" height="18" rx="2" fill="#A78BFA" stroke="#1E1E24" stroke-width="2.5"/>
  <circle cx="34" cy="55" r="9" fill="#C4B5FD"/>
  <line x1="34" y1="55" x2="34" y2="47" stroke="#4C1D95" stroke-width="3" stroke-linecap="round"/>
  <rect x="52" y="44" width="5" height="26" rx="2.5" fill="#C4B5FD"/>
  <rect x="64" y="44" width="5" height="26" rx="2.5" fill="#C4B5FD"/>
  <rect x="49" y="58" width="11" height="6" rx="2" fill="#EDE9FE"/>
  <rect x="61" y="50" width="11" height="6" rx="2" fill="#EDE9FE"/>`),
};
