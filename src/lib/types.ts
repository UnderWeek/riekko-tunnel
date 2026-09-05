export type TunnelState = "IDLE" | "STARTING" | "CONNECTED" | "RECONNECTING" | "ERROR";

export type Section = "connection" | "profiles" | "session" | "settings";

export type Protocol = "VLESS" | "HYSTERIA2";

export type RoutingMode = "TUN" | "SYSTEM_PROXY";

export const UNGROUPED_ID = "ungrouped";

export interface Group {
  id: string;
  name: string;
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
  state: TunnelState;
  routing_mode: RoutingMode | null;
  groups: Group[];
  profiles: Profile[];
  active_profile_id: string;
  settings: Settings;
  session: SessionInfo;
}

export interface ImportSubscriptionResult {
  state: AppState;
  added: number;
  group_name: string;
}
