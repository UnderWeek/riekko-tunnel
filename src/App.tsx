import { useCallback, useEffect, useRef, useState } from "react";
import { backend } from "./lib/backend";
import type { AppState, Section, Settings } from "./lib/types";
import { NavigationRail } from "./components/NavigationRail";
import { ConnectionScreen } from "./screens/ConnectionScreen";
import { ProfilesScreen } from "./screens/ProfilesScreen";
import { SessionScreen } from "./screens/SessionScreen";
import { SettingsScreen } from "./screens/SettingsScreen";
import "./screens/common.css";
import "./App.css";

const same = (s: AppState) => s;

function errorText(err: unknown, fallback: string): string {
  return typeof err === "string" ? err : fallback;
}

export default function App() {
  const [appState, setAppState] = useState<AppState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [section, setSection] = useState<Section>("connection");
  const [connectionError, setConnectionError] = useState<string | null>(null);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [toggling, setToggling] = useState(false);

  // Every backend call answers with a full state snapshot, and answers can
  // arrive out of order: a 1 s tick sent while a slow connect (admin prompt)
  // is pending comes back first. Only a snapshot newer than the last one
  // applied is used, so an old answer can never roll the UI back.
  const issued = useRef(0);
  const applied = useRef(0);
  const track = useCallback(<T,>(request: Promise<T>, pick: (value: T) => AppState): Promise<T> => {
    const seq = ++issued.current;
    return request.then((value) => {
      if (seq > applied.current) {
        applied.current = seq;
        setAppState(pick(value));
      }
      return value;
    });
  }, []);

  const togglingRef = useRef(false);
  const onToggleConnection = useCallback(() => {
    // The button is disabled meanwhile, but a double click can land before
    // React re-renders.
    if (togglingRef.current) return;
    togglingRef.current = true;
    setToggling(true);
    setConnectionError(null);
    track(backend.toggleConnection(), same)
      .catch((err) => setConnectionError(errorText(err, "Не удалось подключиться")))
      .finally(() => {
        togglingRef.current = false;
        setToggling(false);
      });
  }, [track]);

  const autoConnectTried = useRef(false);
  useEffect(() => {
    track(backend.getState(), same)
      .then((state) => {
        // Guarded by a ref: StrictMode runs this effect twice in dev.
        if (autoConnectTried.current) return;
        autoConnectTried.current = true;
        const hasProfile = state.profiles.some((p) => p.id === state.active_profile_id);
        if (state.settings.auto_connect && hasProfile && state.state === "IDLE") {
          onToggleConnection();
        }
      })
      .catch((err) => setLoadError(errorText(err, "Не удалось загрузить состояние")));
  }, [track, onToggleConnection]);

  useEffect(() => {
    // Skip a tick while the previous one is still running instead of piling
    // requests up behind a slow backend.
    let inFlight = false;
    const interval = setInterval(() => {
      if (inFlight) return;
      inFlight = true;
      track(backend.tickSession(), same)
        .catch(() => {})
        .finally(() => {
          inFlight = false;
        });
    }, 1000);
    return () => clearInterval(interval);
  }, [track]);

  const onRefreshSession = useCallback(() => {
    track(backend.refreshSession(), same).catch(() => {});
  }, [track]);

  const onSelectProfile = useCallback(
    (id: string) => {
      track(backend.selectProfile(id), same).catch(() => {});
    },
    [track],
  );

  const onImportProfile = useCallback(
    async (uri: string) => {
      await track(backend.importProfile(uri), same);
    },
    [track],
  );

  const onImportSubscription = useCallback(
    async (url: string) => {
      const result = await track(backend.importSubscription(url), (r) => r.state);
      return { added: result.added, groupName: result.group_name };
    },
    [track],
  );

  const onRemoveProfile = useCallback(
    (id: string) => {
      track(backend.removeProfile(id), same).catch(() => {});
    },
    [track],
  );

  const onCreateGroup = useCallback(
    (name: string) => {
      track(backend.createGroup(name), same).catch(() => {});
    },
    [track],
  );

  const onRenameGroup = useCallback(
    (id: string, name: string) => {
      track(backend.renameGroup(id, name), same).catch(() => {});
    },
    [track],
  );

  const onDeleteGroup = useCallback(
    (id: string) => {
      track(backend.deleteGroup(id), same).catch(() => {});
    },
    [track],
  );

  const onMoveProfile = useCallback(
    (profileId: string, groupId: string) => {
      track(backend.moveProfileToGroup(profileId, groupId), same).catch(() => {});
    },
    [track],
  );

  const onChangeSetting = useCallback(
    (key: keyof Settings, value: boolean) => {
      setSettingsError(null);
      track(backend.updateSetting(key, value), same).catch((err) =>
        setSettingsError(errorText(err, "Не удалось изменить настройку")),
      );
    },
    [track],
  );

  if (!appState) {
    return (
      <div className="app-loading">
        {loadError ? (
          <p className="app-loading__error">{loadError}</p>
        ) : (
          <span className="app-loading__brand">❄</span>
        )}
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
            toggling={toggling}
            error={connectionError ?? (appState.state === "ERROR" ? appState.last_error : null)}
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
          <SettingsScreen settings={appState.settings} onChange={onChangeSetting} error={settingsError} />
        )}
      </main>
    </div>
  );
}
