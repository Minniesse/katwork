import { useEffect, useState } from 'react';
import { api, type Space, type WorkspaceSummary } from './api';

// The workspace the person last chose, per browser. Storage can throw (privacy modes).
const STORAGE_KEY = 'kw-workspace';

export type WorkspaceContext =
  | { state: 'loading' }
  | { state: 'failed'; error: unknown }
  | { state: 'none' }
  | {
      state: 'ready';
      workspaces: WorkspaceSummary[];
      workspace: WorkspaceSummary;
      spaces: Space[];
      /** Null when the workspace has no Space yet. */
      space: Space | null;
      /** The Space's default team: the first one it lists, created with the Space. */
      teamId: string | null;
    };

function saved(): string | null {
  try {
    return localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
}

function save(id: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, id);
  } catch {
    // The choice still applies for this page.
  }
}

/** Loads the signed-in person's workspaces, the chosen one's Spaces, and that Space's team. */
export function useWorkspace(enabled: boolean): [
  WorkspaceContext,
  (workspaceId: string) => void,
  (spaceId: string) => void,
  /** Reads the workspaces again, for a role that changed. */
  () => void,
] {
  const [context, setContext] = useState<WorkspaceContext>({ state: 'loading' });
  const [workspaceId, setWorkspaceId] = useState<string | null>(saved);
  const [spaceId, setSpaceId] = useState<string | null>(null);
  const [reloads, setReloads] = useState(0);

  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const load = async (): Promise<WorkspaceContext> => {
      const workspaces = await api.workspaces();
      const workspace = workspaces.find((w) => w.id === workspaceId) ?? workspaces[0];
      if (!workspace) return { state: 'none' };
      const spaces = await api.spaces(workspace.id);
      const space = spaces.find((s) => s.id === spaceId) ?? spaces[0] ?? null;
      const teams = space ? await api.teams(workspace.id, space.id) : [];
      return { state: 'ready', workspaces, workspace, spaces, space, teamId: teams[0]?.id ?? null };
    };
    load().then(
      (next) => live && setContext(next),
      (error: unknown) => live && setContext({ state: 'failed', error }),
    );
    return () => {
      live = false;
    };
  }, [enabled, workspaceId, spaceId, reloads]);

  const chooseWorkspace = (id: string) => {
    save(id);
    setContext({ state: 'loading' });
    setSpaceId(null);
    setWorkspaceId(id);
  };
  return [context, chooseWorkspace, setSpaceId, () => setReloads((n) => n + 1)];
}
