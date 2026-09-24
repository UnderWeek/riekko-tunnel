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

  // Every backend answer is a full snapshot stamped with a revision taken
  // under the backend's state lock. Answers arrive out of order — a 1 s
  // tick sent during a slow connect/disconnect can come back before or
  // after it — so a snapshot is applied only if it was *taken* later than
  // the one on screen. (Ordering by when requests were sent would throw the
  // toggle's own final answer away in favor of a mid-operation tick.)
  const shownRevision = useRef(-1);
  const apply = useCallback((next: AppState) => {
    if (next.revision > shownRevision.current) {
      shownRevision.current = next.revision;
      setAppState(next);
    }
  }, []);
  const track = useCallback(
    <T,>(request: Promise<T>, pick: (value: T) => AppState): Promise<T> =>
      request.then((value) => {
        apply(pick(value));
        return value;
      }),
    [apply],
  );
  const refresh = useCallback(() => {
    backend.getState().then(apply).catch(() => {});
  }, [apply]);

  const togglingRef = useRef(false);
  const onToggleConnection = useCallback(() => {
    // The button ignores clicks meanwhile, but a double click can land
    // before React re-renders.
    if (togglingRef.current) return;
    togglingRef.current = true;
    setToggling(true);
    setConnectionError(null);
    track(backend.toggleConnection(), same)
      .catch((err) => {
        setConnectionError(errorText(err, "Не удалось подключиться"));
        // A rejection carries no snapshot; pick up the ERROR state now.
        refresh();
      })
      .finally(() => {
        togglingRef.current = false;
        setToggling(false);
      });
  }, [track, refresh]);

  useEffect(() => {
    // Auto-connect happens in the backend at startup (once per launch, not
    // once per page load), so this only needs to fetch the state.
    track(backend.getState(), same).catch((err) =>
      setLoadError(errorText(err, "Не удалось загрузить состояние")),
    );
  }, [track]);

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

  // A message from a previous attempt must not outlive the state it was
  // about; once the state moves on, the backend's own `last_error` speaks.
  const tunnelState = appState?.state;
  useEffect(() => {
    setConnectionError(null);
  }, [tunnelState]);

  useEffect(() => {
    // With Tauri's native drag-drop handler off (so profile rows can be
    // dragged), a file dropped anywhere outside a drop zone would make the
    // WebView navigate to it — replacing the whole UI.
    const block = (e: DragEvent) => {
      if (e.defaultPrevented) return;
      e.preventDefault();
      if (e.dataTransfer) e.dataTransfer.dropEffect = "none";
    };
    window.addEventListener("dragover", block);
    window.addEventListener("drop", block);
    return () => {
      window.removeEventListener("dragover", block);
      window.removeEventListener("drop", block);
    };
  }, []);

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

  // While idle or failed, the backend's own reason (a dropped tunnel, an
  // unreadable profile file at startup) is the freshest explanation.
  const showBackendError = appState.state === "ERROR" || appState.state === "IDLE";
  const error = showBackendError ? (appState.last_error ?? connectionError) : connectionError;

  // All screens stay mounted (just hidden), so an import running in the
  // background, its result message, the traffic history and half-typed
  // input survive switching tabs.
  return (
    <div className="app-shell">
      <NavigationRail section={section} onSelect={setSection} />
      <main className="app-content">
        <div className="app-section" hidden={section !== "connection"}>
          <ConnectionScreen
            appState={appState}
            onToggleConnection={onToggleConnection}
            toggling={toggling}
            error={error}
          />
        </div>
        <div className="app-section" hidden={section !== "profiles"}>
          <ProfilesScreen
            profiles={appState.profiles}
            groups={appState.groups}
            activeProfileId={appState.active_profile_id}
            connectedProfileId={appState.connected_profile_id}
            onSelect={onSelectProfile}
            onImport={onImportProfile}
            onImportSubscription={onImportSubscription}
            onRemove={onRemoveProfile}
            onCreateGroup={onCreateGroup}
            onRenameGroup={onRenameGroup}
            onDeleteGroup={onDeleteGroup}
            onMoveProfile={onMoveProfile}
          />
        </div>
        <div className="app-section" hidden={section !== "session"}>
          <SessionScreen appState={appState} onRefresh={onRefreshSession} />
        </div>
        <div className="app-section" hidden={section !== "settings"}>
          <SettingsScreen settings={appState.settings} onChange={onChangeSetting} error={settingsError} />
        </div>
      </main>
    </div>
  );
}
