export interface ProjectRowMenuActions {
  copy: (path: string) => void | Promise<void>;
  fill: (path: string) => void | Promise<void>;
}

export interface ProjectRowMenuItem {
  labelKey: "copy" | "proj_fill_path";
  run: () => void | Promise<void>;
}

export function projectRowMenuItems(
  path: string,
  actions: ProjectRowMenuActions,
): ProjectRowMenuItem[] {
  return [
    { labelKey: "copy", run: () => actions.copy(path) },
    { labelKey: "proj_fill_path", run: () => actions.fill(path) },
  ];
}
