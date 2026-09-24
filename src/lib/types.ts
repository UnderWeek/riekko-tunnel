export type TunnelState = "IDLE" | "STARTING" | "CONNECTED" | "RECONNECTING" | "ERROR";

export type Section = "connection" | "profiles" | "session" | "settings";

export type Protocol = "VLESS" | "HYSTERIA2";

export type RoutingMode = "TUN" | "SYSTEM_PROXY";

export const UNGROUPED_ID = "ungrouped";

export interface Group {
  id: string;
  name: string;
  /** Set for subscription groups: the URL they refresh from. */
  source_url?: string;
}

export interface Profile {
  id: string;
  name: string;
  endpoint: string;
  transport: string;
  protocol: Protocol;
  group_id: string;
  uri: string | null;
}

export interface Settings {
  auto_connect: boolean;
  start_with_system: boolean;
  notifications: boolean;
}

export interface SessionInfo {
  endpoint: string;
  latency_ms: number;
  uptime_secs: number;
  rx_mb: number;
  tx_mb: number;
}

export interface AppState {
  /** Backend snapshot order: higher = taken later. */
  revision: number;
  state: TunnelState;
  routing_mode: RoutingMode | null;
  groups: Group[];
  profiles: Profile[];
  /** The profile picked for the next connect. */
  active_profile_id: string;
  /** The profile the running tunnel was started with, if any. */
  connected_profile_id: string | null;
  /** Why the last connect failed or the tunnel dropped. */
  last_error: string | null;
  settings: Settings;
  session: SessionInfo;
}

export interface ImportSubscriptionResult {
  state: AppState;
  added: number;
  group_name: string;
}
