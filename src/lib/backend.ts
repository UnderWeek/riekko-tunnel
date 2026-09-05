import { invoke } from "@tauri-apps/api/core";
import type { AppState, ImportSubscriptionResult } from "./types";

export const backend = {
  getState: () => invoke<AppState>("get_state"),
  toggleConnection: () => invoke<AppState>("toggle_connection"),
  tickSession: () => invoke<AppState>("tick_session"),
  refreshSession: () => invoke<AppState>("refresh_session"),
  selectProfile: (id: string) => invoke<AppState>("select_profile", { id }),
  importProfile: (uri: string) => invoke<AppState>("import_profile", { uri }),
  importSubscription: (url: string) =>
    invoke<ImportSubscriptionResult>("import_subscription", { url }),
  removeProfile: (id: string) => invoke<AppState>("remove_profile", { id }),
  updateSetting: (key: string, value: boolean) => invoke<AppState>("update_setting", { key, value }),
  createGroup: (name: string) => invoke<AppState>("create_group", { name }),
  renameGroup: (id: string, name: string) => invoke<AppState>("rename_group", { id, name }),
  deleteGroup: (id: string) => invoke<AppState>("delete_group", { id }),
  moveProfileToGroup: (profileId: string, groupId: string) =>
    invoke<AppState>("move_profile_to_group", { profileId, groupId }),
};
