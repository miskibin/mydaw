// Comparing file paths the way the file system that produced them does.

/** A drive (`C:\`, `C:/`) or UNC (`\\server\share`) path: Windows, whose file names ignore case. */
const WINDOWS_PATH = /^(?:[A-Za-z]:[\\/]|\\\\)/;

/**
 * Whether two absolute paths name the same file. Windows paths ignore case and treat `/`
 * like `\` (Explorer, a terminal and the file dialog can spell one file differently);
 * POSIX paths compare exactly, since Linux file names are case-sensitive.
 * The engine (`app/src/project_launch.rs`) already removes `.` and `..` segments.
 */
export function samePath(a: string | null | undefined, b: string | null | undefined): boolean {
  if (a == null || b == null) return false;
  if (a === b) return true;
  if (!WINDOWS_PATH.test(a) || !WINDOWS_PATH.test(b)) return false;
  const key = (p: string) => p.replace(/\//g, '\\').replace(/(?<!^)\\+/g, '\\').replace(/\\$/, '').toLowerCase();
  return key(a) === key(b);
}
