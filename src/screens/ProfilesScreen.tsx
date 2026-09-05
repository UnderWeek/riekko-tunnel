import { useEffect, useState, type DragEvent } from "react";
import type { Group, Profile, Protocol } from "../lib/types";
import { UNGROUPED_ID } from "../lib/types";
import { Button } from "../components/Button";
import { Icon, type IconName } from "../components/Icon";
import "./ProfilesScreen.css";

const PROTOCOL_META: Record<Protocol, { icon: IconName; label: string; className: string }> = {
  VLESS: { icon: "shield-lock", label: "VLESS", className: "protocol-badge--vless" },
  HYSTERIA2: { icon: "bolt", label: "Hysteria2", className: "protocol-badge--hysteria2" },
};

function pluralProfiles(n: number): string {
  const mod10 = n % 10;
  const mod100 = n % 100;
  if (mod100 >= 11 && mod100 <= 14) return "профилей";
  if (mod10 === 1) return "профиль";
  if (mod10 >= 2 && mod10 <= 4) return "профиля";
  return "профилей";
}

function ProtocolBadge({ protocol }: { protocol: Protocol }) {
  const meta = PROTOCOL_META[protocol];
  return (
    <span className={`protocol-badge ${meta.className}`}>
      <Icon name={meta.icon} size={13} />
      {meta.label}
    </span>
  );
}

interface GroupRow {
  id: string;
  name: string;
  editable: boolean;
}

export function ProfilesScreen({
  profiles,
  groups,
  activeProfileId,
  onSelect,
  onImport,
  onImportSubscription,
  onRemove,
  onCreateGroup,
  onRenameGroup,
  onDeleteGroup,
  onMoveProfile,
}: {
  profiles: Profile[];
  groups: Group[];
  activeProfileId: string;
  onSelect: (id: string) => void;
  onImport: (uri: string) => Promise<void>;
  onImportSubscription: (url: string) => Promise<{ added: number; groupName: string }>;
  onRemove: (id: string) => void;
  onCreateGroup: (name: string) => void;
  onRenameGroup: (id: string, name: string) => void;
  onDeleteGroup: (id: string) => void;
  onMoveProfile: (profileId: string, groupId: string) => void;
}) {
  const [link, setLink] = useState("");
  const [importError, setImportError] = useState<string | null>(null);
  const [importSuccess, setImportSuccess] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);

  const [creatingGroup, setCreatingGroup] = useState(false);
  const [newGroupName, setNewGroupName] = useState("");

  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");

  const [draggingId, setDraggingId] = useState<string | null>(null);
  const [dragOverGroupId, setDragOverGroupId] = useState<string | null>(null);

  useEffect(() => {
    if (!importSuccess) return;
    const timeout = setTimeout(() => setImportSuccess(null), 4500);
    return () => clearTimeout(timeout);
  }, [importSuccess]);

  async function submitLink() {
    const value = link.trim();
    if (!value) return;
    setImporting(true);
    setImportError(null);
    setImportSuccess(null);
    try {
      if (/^https?:\/\//i.test(value)) {
        const result = await onImportSubscription(value);
        setImportSuccess(
          `Добавлено ${result.added} ${pluralProfiles(result.added)} в группу «${result.groupName}»`,
        );
      } else {
        await onImport(value);
      }
      setLink("");
    } catch (err) {
      setImportError(typeof err === "string" ? err : "Не удалось добавить профиль");
    } finally {
      setImporting(false);
    }
  }

  function submitNewGroup() {
    const trimmed = newGroupName.trim();
    if (!trimmed) return;
    onCreateGroup(trimmed);
    setNewGroupName("");
    setCreatingGroup(false);
  }

  function toggleCollapsed(groupId: string) {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(groupId)) next.delete(groupId);
      else next.add(groupId);
      return next;
    });
  }

  function commitRename(group: GroupRow) {
    const trimmed = renameValue.trim();
    if (trimmed && trimmed !== group.name) {
      onRenameGroup(group.id, trimmed);
    }
    setRenamingId(null);
  }

  function handleDragStart(e: DragEvent<HTMLDivElement>, profileId: string) {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", profileId);
    setDraggingId(profileId);
  }

  function handleDragEnd() {
    setDraggingId(null);
    setDragOverGroupId(null);
  }

  function handleGroupDragEnter(e: DragEvent<HTMLElement>, groupId: string) {
    e.preventDefault();
    if (dragOverGroupId !== groupId) setDragOverGroupId(groupId);
  }

  function handleGroupDragOver(e: DragEvent<HTMLElement>, groupId: string) {
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    if (dragOverGroupId !== groupId) setDragOverGroupId(groupId);
  }

  function handleGroupDrop(e: DragEvent<HTMLElement>, groupId: string) {
    e.preventDefault();
    const profileId = e.dataTransfer.getData("text/plain");
    setDragOverGroupId(null);
    setDraggingId(null);
    if (profileId) onMoveProfile(profileId, groupId);
  }

  const profilesByGroup = new Map<string, Profile[]>();
  for (const profile of profiles) {
    const key = profile.group_id || UNGROUPED_ID;
    const list = profilesByGroup.get(key) ?? [];
    list.push(profile);
    profilesByGroup.set(key, list);
  }

  const groupRows: GroupRow[] = [
    { id: UNGROUPED_ID, name: "Ungrouped", editable: false },
    ...groups.map((g) => ({ id: g.id, name: g.name, editable: true })),
  ];

  return (
    <div className="screen">
      <div className="screen__header">
        <h1 className="screen__title">Профили</h1>
        <Button
          variant="text"
          icon={<Icon name="create-new-folder" size={18} />}
          onClick={() => setCreatingGroup((v) => !v)}
        >
          Группа
        </Button>
      </div>

      <div className="import-bar card">
        <Icon name="link" size={18} className="import-bar__icon" />
        <input
          value={link}
          onChange={(e) => {
            setLink(e.target.value);
            if (importError) setImportError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") submitLink();
          }}
          placeholder="vless://... hysteria2://... или ссылка на .txt/.md подписку"
          className="import-bar__input"
        />
        <Button
          variant="filled"
          icon={<Icon name="content-paste" size={18} />}
          onClick={submitLink}
          disabled={!link.trim() || importing}
        >
          {importing ? "Добавляем…" : "Добавить"}
        </Button>
      </div>
      {importError && (
        <p className="import-message import-message--error">
          <Icon name="priority-high" size={14} />
          {importError}
        </p>
      )}
      {importSuccess && (
        <p className="import-message import-message--success">
          <Icon name="cloud-download" size={14} />
          {importSuccess}
        </p>
      )}

      {creatingGroup && (
        <div className="create-group-bar card">
          <input
            autoFocus
            value={newGroupName}
            onChange={(e) => setNewGroupName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") submitNewGroup();
              if (e.key === "Escape") setCreatingGroup(false);
            }}
            placeholder="Название группы"
            className="create-group-bar__input"
          />
          <Button variant="text" onClick={() => setCreatingGroup(false)}>
            Отмена
          </Button>
          <Button variant="filled" onClick={submitNewGroup} disabled={!newGroupName.trim()}>
            Создать
          </Button>
        </div>
      )}

      <div className="profiles-scroll">
        {groupRows.map((group) => {
          const groupProfiles = profilesByGroup.get(group.id) ?? [];
          const isCollapsed = collapsed.has(group.id);
          const isDropActive = dragOverGroupId === group.id;
          const isRenaming = renamingId === group.id;

          return (
            <section
              key={group.id}
              className={`profile-group${isDropActive ? " profile-group--drop-active" : ""}`}
              onDragEnter={(e) => handleGroupDragEnter(e, group.id)}
              onDragOver={(e) => handleGroupDragOver(e, group.id)}
              onDrop={(e) => handleGroupDrop(e, group.id)}
            >
              <div className="profile-group__header">
                <button
                  type="button"
                  className={`profile-group__chevron${isCollapsed ? " profile-group__chevron--collapsed" : ""}`}
                  onClick={() => toggleCollapsed(group.id)}
                  aria-label={isCollapsed ? "Развернуть группу" : "Свернуть группу"}
                >
                  <Icon name="expand-more" size={20} />
                </button>

                {isRenaming ? (
                  <input
                    autoFocus
                    className="profile-group__rename-input"
                    value={renameValue}
                    onChange={(e) => setRenameValue(e.target.value)}
                    onBlur={() => commitRename(group)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitRename(group);
                      if (e.key === "Escape") setRenamingId(null);
                    }}
                  />
                ) : (
                  <span
                    className="profile-group__name"
                    onDoubleClick={() => {
                      if (!group.editable) return;
                      setRenamingId(group.id);
                      setRenameValue(group.name);
                    }}
                    title={group.editable ? "Двойной клик — переименовать" : undefined}
                  >
                    {group.name}
                  </span>
                )}

                <span className="profile-group__count">{groupProfiles.length}</span>
                <div className="profile-group__spacer" />

                {group.editable && !isRenaming && (
                  <>
                    <Button
                      variant="icon"
                      title="Переименовать группу"
                      icon={<Icon name="edit" size={16} />}
                      onClick={() => {
                        setRenamingId(group.id);
                        setRenameValue(group.name);
                      }}
                    />
                    <Button
                      variant="icon"
                      danger
                      title="Удалить группу"
                      icon={<Icon name="delete" size={16} />}
                      onClick={() => onDeleteGroup(group.id)}
                    />
                  </>
                )}
              </div>

              <div className={`profile-group__wrapper${isCollapsed ? " profile-group__wrapper--collapsed" : ""}`}>
                <div className="profile-group__body">
                  {groupProfiles.length === 0 ? (
                    <p className="profile-list__empty">Перетащите сюда профиль или добавьте новый.</p>
                  ) : (
                    <div className="profile-list">
                      {groupProfiles.map((profile) => {
                        const selected = profile.id === activeProfileId;
                        const isDragging = draggingId === profile.id;
                        return (
                          <div
                            key={profile.id}
                            draggable
                            onDragStart={(e) => handleDragStart(e, profile.id)}
                            onDragEnd={handleDragEnd}
                            className={`profile-row${selected ? " profile-row--selected" : ""}${isDragging ? " profile-row--dragging" : ""}`}
                            onClick={() => onSelect(profile.id)}
                          >
                            <span className="profile-row__grip">
                              <Icon name="drag-indicator" size={18} />
                            </span>
                            <div className="profile-row__info">
                              <div className="profile-row__name-line">
                                <span className="profile-row__name">{profile.name}</span>
                                <ProtocolBadge protocol={profile.protocol} />
                              </div>
                              <span className="profile-row__meta">
                                {profile.endpoint} · {profile.transport}
                              </span>
                            </div>
                            {selected && (
                              <span className="profile-row__badge">
                                <Icon name="check-circle" size={16} />
                                Активен
                              </span>
                            )}
                            <Button
                              variant="icon"
                              danger
                              title="Удалить профиль"
                              icon={<Icon name="delete" size={18} />}
                              onClick={(e) => {
                                e.stopPropagation();
                                onRemove(profile.id);
                              }}
                            />
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              </div>
            </section>
          );
        })}
      </div>

    </div>
  );
}
