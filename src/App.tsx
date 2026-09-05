import { useCallback, useEffect, useState } from "react";
import { backend } from "./lib/backend";
import type { AppState, Section, Settings } from "./lib/types";
import { NavigationRail } from "./components/NavigationRail";
import { ConnectionScreen } from "./screens/ConnectionScreen";
import { ProfilesScreen } from "./screens/ProfilesScreen";
import { SessionScreen } from "./screens/SessionScreen";
import { SettingsScreen } from "./screens/SettingsScreen";
import "./screens/common.css";
import "./App.css";

export default function App() {
  const [appState, setAppState] = useState<AppState | null>(null);
  const [section, setSection] = useState<Section>("connection");

  useEffect(() => {
    backend.getState().then(setAppState);
  }, []);

  useEffect(() => {
    const interval = setInterval(() => {
      backend.tickSession().then(setAppState);
    }, 1000);
    return () => clearInterval(interval);
  }, []);

  const [connectionError, setConnectionError] = useState<string | null>(null);

  const onToggleConnection = useCallback(() => {
    setConnectionError(null);
    backend
      .toggleConnection()
      .then(setAppState)
      .catch((err) => {
        setConnectionError(typeof err === "string" ? err : "Не удалось подключиться");
      });
  }, []);

  const onRefreshSession = useCallback(() => {
    backend.refreshSession().then(setAppState);
  }, []);

  const onSelectProfile = useCallback((id: string) => {
    backend.selectProfile(id).then(setAppState);
  }, []);

  const onImportProfile = useCallback(async (uri: string) => {
    const next = await backend.importProfile(uri);
    setAppState(next);
  }, []);

  const onImportSubscription = useCallback(async (url: string) => {
    const result = await backend.importSubscription(url);
    setAppState(result.state);
    return { added: result.added, groupName: result.group_name };
  }, []);

  const onRemoveProfile = useCallback((id: string) => {
    backend.removeProfile(id).then(setAppState);
  }, []);

  const onCreateGroup = useCallback((name: string) => {
    backend.createGroup(name).then(setAppState);
  }, []);

  const onRenameGroup = useCallback((id: string, name: string) => {
    backend.renameGroup(id, name).then(setAppState);
  }, []);

  const onDeleteGroup = useCallback((id: string) => {
    backend.deleteGroup(id).then(setAppState);
  }, []);

  const onMoveProfile = useCallback((profileId: string, groupId: string) => {
    backend.moveProfileToGroup(profileId, groupId).then(setAppState);
  }, []);

  const onChangeSetting = useCallback((key: keyof Settings, value: boolean) => {
    backend.updateSetting(key, value).then(setAppState);
  }, []);

  if (!appState) {
    return (
      <div className="app-loading">
        <span className="app-loading__brand">❄</span>
      </div>
    );
  }

  return (
    <div className="app-shell">
      <NavigationRail section={section} onSelect={setSection} />
      <main className="app-content">
        {section === "connection" && (
          <ConnectionScreen
            appState={appState}
            onToggleConnection={onToggleConnection}
            error={connectionError}
          />
        )}
        {section === "profiles" && (
          <ProfilesScreen
            profiles={appState.profiles}
            groups={appState.groups}
            activeProfileId={appState.active_profile_id}
            onSelect={onSelectProfile}
            onImport={onImportProfile}
            onImportSubscription={onImportSubscription}
            onRemove={onRemoveProfile}
            onCreateGroup={onCreateGroup}
            onRenameGroup={onRenameGroup}
            onDeleteGroup={onDeleteGroup}
            onMoveProfile={onMoveProfile}
          />
        )}
        {section === "session" && <SessionScreen appState={appState} onRefresh={onRefreshSession} />}
        {section === "settings" && (
          <SettingsScreen settings={appState.settings} onChange={onChangeSetting} />
        )}
      </main>
    </div>
  );
}
