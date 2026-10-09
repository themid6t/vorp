// Pure display helpers: units, tunnel URLs and the agent command line.

export const MIB = 1024 * 1024;

export function formatRate(bytesPerSec: number): string {
  if (bytesPerSec >= MIB) return `${+(bytesPerSec / MIB).toFixed(2)} MiB/s`;
  if (bytesPerSec >= 1024) return `${+(bytesPerSec / 1024).toFixed(1)} KiB/s`;
  return `${bytesPerSec} B/s`;
}

export function formatBytes(bytes: number): string {
  if (bytes >= MIB) return `${+(bytes / MIB).toFixed(1)} MiB`;
  if (bytes >= 1024) return `${+(bytes / 1024).toFixed(1)} KiB`;
  return `${bytes} B`;
}

export const mibToBytes = (mib: number | string) => Math.round(Number(mib) * MIB);
export const bytesToMib = (bytes: number) => +(bytes / MIB).toFixed(3);

export const tunnelUrl = (name: string, baseDomain: string) => `https://${name}.${baseDomain}`;

// The agent dials port 443 of vorp.<base domain>, the relay's own name.
// A relay on another port also needs --relay-addr, which takes an IP address.
export function agentCommand(baseDomain: string, rawToken?: string, names: string[] = []): string {
  const parts = [`VORP_TOKEN=${rawToken || '<your-token>'}`, 'vorp', '--relay-host', `vorp.${baseDomain}`];
  const port = location.port;
  if (port && port !== '443') parts.push('--relay-addr', `${relayIp()}:${port}`);
  for (const name of names) parts.push('--subdomain', name);
  parts.push('--upstream', 'http://127.0.0.1:3000');
  return parts.join(' ');
}

function relayIp(): string {
  const host = location.hostname;
  if (host === 'localhost' || host.endsWith('.localhost')) return '127.0.0.1';
  if (/^[\d.]+$/.test(host) || host.startsWith('[')) return host;
  return '<relay-ip>';
}

export function commandNote(): string {
  const port = location.port;
  return port && port !== '443'
    ? 'This relay is not on port 443, so the agent needs --relay-addr. With a self-signed certificate, also pass --ca-cert <file>.'
    : '';
}

/** A positive whole number, or null when the input is not one. */
export function positiveInt(value: unknown): number | null {
  const n = Number(value);
  return Number.isSafeInteger(n) && n >= 1 ? n : null;
}

/** A bandwidth in MiB/s as bytes per second, or null when it rounds to nothing. */
export function bandwidthBytes(mib: unknown): number | null {
  const bytes = mibToBytes(mib as string);
  return Number.isSafeInteger(bytes) && bytes >= 1 ? bytes : null;
}
