## Internationalization

Add a namespaced key to `src/i18n/en.ts` and its Polish translation to
`src/i18n/pl.ts`, then call `t(key, vars)` where the text is displayed. Call
`t` at display or drawing time so language changes are reflected. For a new
language, add a matching dictionary and extend `Lang`, the language selector,
and the dictionary lookup in `src/lib/i18n.ts`.
