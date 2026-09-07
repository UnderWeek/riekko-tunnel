export function formatUptime(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = Math.floor(totalSeconds % 60);
  const pad = (n: number) => n.toString().padStart(2, "0");
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
}

export function formatMb(value: number): string {
  return `${value.toFixed(2)} MB`;
}

/** `mbPerSec` is a per-tick delta in MB (ticks are ~1s), shown as an
 * adaptive live speed reading (B/s, KB/s or MB/s). */
export function formatSpeed(mbPerSec: number): string {
  const bytesPerSec = Math.max(0, mbPerSec) * 1_000_000;
  if (bytesPerSec < 1_000) return `${bytesPerSec.toFixed(0)} B/s`;
  if (bytesPerSec < 1_000_000) return `${(bytesPerSec / 1_000).toFixed(1)} KB/s`;
  return `${(bytesPerSec / 1_000_000).toFixed(2)} MB/s`;
}
